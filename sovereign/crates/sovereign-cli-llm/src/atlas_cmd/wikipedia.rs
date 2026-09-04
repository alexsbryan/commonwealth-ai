// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn atlas wikipedia ...` — Wikipedia structural enrichment (Layer 0: the
//! link graph).
//!
//! WIKIPEDIA_ATLAS_V2 W4 collapsed the two-step build. `build-graph` now writes
//! `atlas/articles.lance` + `atlas/edges.lance` directly from the indexed
//! chunks; the SQLite `wikipedia_graph.db` and the `export-columnar` verb that
//! dumped it into those tables are gone.

use std::sync::Arc;

use corpus_engine::enrichment::atlas::wiki_store::build_wikipedia_columnar_store_from_chunks;
use corpus_engine::{CorpusEngine, EmbedFn};

use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn atlas wikipedia",
    summary: "Wikipedia link graph and structural enrichment.",
    sections: &[
        HelpSection::Usage("svrn atlas wikipedia <subcommand> [args]"),
        HelpSection::Subcommands(&[
            (
                "build-graph",
                "Layer 0: build the columnar link graph from Wikipedia extractor metadata.",
            ),
            (
                "neighbors",
                "Print an article's link-graph neighbors, with read latency.",
            ),
        ]),
    ],
};

const BUILD_GRAPH_HELP: Help = Help {
    command: "svrn atlas wikipedia build-graph",
    summary: "Build the Wikipedia link graph for an installed corpus.",
    sections: &[
        HelpSection::Usage("svrn atlas wikipedia build-graph <corpus-id> [--atlas-dir <path>]"),
        HelpSection::Flags(&[
            (
                "<corpus-id>",
                "ID of an installed Wikipedia-class corpus (e.g. `wikipedia`).",
            ),
            (
                "--atlas-dir <path>",
                "Write the store here instead of <data-dir>/indexes/<corpus>/atlas. \
                 Use it to build BESIDE an installed graph and compare before cutting over.",
            ),
            ("--help, -h", "Show this message."),
        ]),
        HelpSection::Notes(
            "Reads the LanceDB index for <corpus-id> and walks every chunk's `metadata` \
             JSON field, deserialising `WikipediaChunkMetadata`. Aggregates per (article, \
             section) before emitting, so the chunker emitting N chunks per section does \
             not inflate `occurrence_count`. Zero LLM cost and no embedding. \
             \n\nThe write REPLACES both tables, so a build is a full rebuild — there is no \
             incremental mode and no `--rebuild` flag to forget.",
        ),
    ],
};

pub async fn run(args: &[String]) -> i32 {
    if args.is_empty() {
        help::print(&HELP);
        return 2;
    }
    let first = args[0].as_str();
    if first == "--help" || first == "-h" || first == "help" {
        help::print(&HELP);
        return 0;
    }
    match first {
        "build-graph" => cmd_build_graph(&args[1..]).await,
        "neighbors" => cmd_neighbors(&args[1..]).await,
        other => {
            eprintln!("error: unknown wikipedia subcommand `{other}`");
            help::print(&HELP);
            2
        }
    }
}

fn indexes_dir() -> std::path::PathBuf {
    sovereign_core::setup_config::SetupConfig::load()
        .map(|cfg| cfg.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root())
        .join("indexes")
}

/// Print `neighbors(title)` from the columnar store, with read latency. The
/// real-data spot check beside the fixture tests.
async fn cmd_neighbors(args: &[String]) -> i32 {
    let positional: Vec<&str> = args
        .iter()
        .map(|s| s.as_str())
        .filter(|a| !a.starts_with("--"))
        .collect();
    let (Some(corpus_id), Some(title)) = (positional.first(), positional.get(1)) else {
        eprintln!(
            "usage: sovereign atlas wikipedia neighbors <corpus-id> <article-title> [--limit N]"
        );
        return 2;
    };
    let mut limit = 10usize;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--limit" {
            if let Some(v) = it.next() {
                limit = v.parse().unwrap_or(10);
            }
        }
    }
    let atlas_dir = indexes_dir()
        .join(corpus_id)
        .join(corpus_engine::enrichment::atlas::ATLAS_DIRNAME);

    let g = match corpus_engine::ColumnarWikipediaGraph::open(&atlas_dir).await {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: open columnar store at {}: {e}", atlas_dir.display());
            eprintln!("hint: run `sovereign atlas wikipedia build-graph {corpus_id}` first");
            return 1;
        }
    };
    let t = std::time::Instant::now();
    let ns = g.neighbors(title, limit).await;
    println!(
        "neighbors of {title:?} in corpus `{corpus_id}` — {} results in {} ms\n",
        ns.len(),
        t.elapsed().as_millis()
    );
    for x in &ns {
        println!(
            "  {} [{}] occ={} in_scope={}",
            x.title, x.relationship_type, x.occurrence_count, x.in_scope
        );
    }
    if ns.is_empty() {
        eprintln!("(no neighbors — is {title:?} an in-scope article in this corpus?)");
    }
    0
}

#[derive(Default)]
struct BuildGraphArgs {
    corpus_id: Option<String>,
    atlas_dir: Option<std::path::PathBuf>,
}

async fn cmd_build_graph(args: &[String]) -> i32 {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        help::print(&BUILD_GRAPH_HELP);
        return 0;
    }

    let mut a = BuildGraphArgs::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--atlas-dir" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("error: --atlas-dir needs a value");
                    return 2;
                };
                a.atlas_dir = Some(std::path::PathBuf::from(v));
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
            other => {
                if a.corpus_id.is_some() {
                    eprintln!("error: unexpected positional `{other}`");
                    return 2;
                }
                a.corpus_id = Some(other.to_string());
            }
        }
        i += 1;
    }

    let Some(corpus_id) = a.corpus_id else {
        eprintln!("error: <corpus-id> is required");
        help::print(&BUILD_GRAPH_HELP);
        return 2;
    };

    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|cfg| cfg.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root());
    let recipes_dir = data_dir.join("recipes");
    let indexes = data_dir.join("indexes");

    // We never embed during a graph build — a noop EmbedFn satisfies
    // CorpusEngine's pre-flight without requiring a model to be resident.
    let noop_embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(Vec::<f32>::new()) }));
    let engine = CorpusEngine::new(recipes_dir, indexes.clone(), noop_embed);

    let index = match engine.open_index_for_corpus(&corpus_id).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!(
                "error: could not open index for corpus `{corpus_id}`: {e}\n\
                 hint: run `svrn corpus install {corpus_id}` first."
            );
            return 1;
        }
    };

    let atlas_dir = a.atlas_dir.unwrap_or_else(|| {
        indexes
            .join(&corpus_id)
            .join(corpus_engine::enrichment::atlas::ATLAS_DIRNAME)
    });
    if let Err(e) = std::fs::create_dir_all(&atlas_dir) {
        eprintln!("error: create atlas dir {}: {e}", atlas_dir.display());
        return 1;
    }

    eprintln!("streaming chunk metadata from LanceDB...");
    let t_stream = std::time::Instant::now();
    let chunks = match index.all_chunks_with_raw_metadata().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: stream metadata: {e}");
            return 1;
        }
    };
    eprintln!(
        "streamed {} chunk records in {} ms",
        chunks.len(),
        t_stream.elapsed().as_millis()
    );

    eprintln!(
        "building columnar link graph into {} ...",
        atlas_dir.display()
    );
    let t_build = std::time::Instant::now();
    let summary = match build_wikipedia_columnar_store_from_chunks(&atlas_dir, chunks).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: build: {e}");
            return 1;
        }
    };
    let build_ms = t_build.elapsed().as_millis();

    eprintln!();
    eprintln!("graph build complete:");
    eprintln!(
        "  chunks:   {} with metadata, {} skipped",
        summary.chunks_with_metadata, summary.chunks_without_metadata,
    );
    eprintln!(
        "  articles: {} in scope ({} dangling targets)",
        summary.articles, summary.dangling_targets
    );
    eprintln!(
        "  edges:    {} unique (source, section, target)",
        summary.edges
    );
    eprintln!("  sections: {}", summary.sections);
    match summary.revision_id_max {
        Some(r) => eprintln!("  revision: max {r}"),
        // Absence reported, not defaulted (ARCH §18.3): no revision_id in any
        // chunk's metadata means the freshness gate has nothing to compare.
        None => eprintln!("  revision: none present in chunk metadata"),
    }
    eprintln!("  build:    {build_ms} ms");
    eprintln!("  store:    {}", atlas_dir.display());
    0
}
