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
            (
                "seed-table",
                "Build the ANN seed table by borrowing each article's chunk vector.",
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

const NEIGHBORS_HELP: Help = Help {
    command: "svrn atlas wikipedia neighbors",
    summary: "Print an article's link-graph neighbors, with read latency.",
    sections: &[
        HelpSection::Usage(
            "svrn atlas wikipedia neighbors <corpus-id> <article-title> \
             [--limit N] [--atlas-dir <path>]",
        ),
        HelpSection::Flags(&[
            ("<corpus-id>", "ID of an installed Wikipedia-class corpus."),
            (
                "<article-title>",
                "Exact article title. Case matters — a Wikipedia title IS the identifier.",
            ),
            ("--limit N", "Maximum neighbors to print (default 10)."),
            (
                "--atlas-dir <path>",
                "Read this store instead of <data-dir>/indexes/<corpus>/atlas. \
                 The counterpart of `build-graph --atlas-dir`: it is how you read a store \
                 built BESIDE the installed one, which an A/B has to do.",
            ),
            ("--help, -h", "Show this message."),
        ]),
        HelpSection::Notes(
            "The store actually read is printed on every run. That is not decoration: \
             until 2026-09-04 this verb accepted `--atlas-dir` and silently ignored it, \
             so a probe intended for a freshly built store reported on the installed one \
             and read as evidence for a rebuild it had never touched. \
             \n\nAn empty result is disambiguated rather than left to the reader: an \
             article that exists with no in-store outgoing edges exits 0, and a title \
             absent from the store exits 1. Those are different facts and used to look \
             identical.",
        ),
    ],
};

const SEED_TABLE_HELP: Help = Help {
    command: "svrn atlas wikipedia seed-table",
    summary: "Build a wiki atlas's ANN seed table from borrowed chunk vectors (0 embeds).",
    sections: &[
        HelpSection::Usage("svrn atlas wikipedia seed-table <corpus-id> [--atlas-dir <path>]"),
        HelpSection::Flags(&[
            (
                "<corpus-id>",
                "ID of an installed Wikipedia-class corpus (e.g. `wikipedia`).",
            ),
            (
                "--atlas-dir <path>",
                "Seed the store here instead of <data-dir>/indexes/<corpus>/atlas. \
                 The counterpart of `build-graph --atlas-dir`: seed the store you built \
                 beside the installed one, not the installed one.",
            ),
            ("--help, -h", "Show this message."),
        ]),
        HelpSection::Notes(
            "A NAMED SUBSTITUTION, not an equivalence. Each article's atom is seeded with \
             the vector of the chunk it already cites — its lead passage — because a wiki \
             atom's own embed text is a bare title (214 of 221 sampled articles have an \
             empty description). Median cosine between the two is 0.323, so the walk seeds \
             on the lead passage rather than on the title. The alternative was ~1 hour \
             of fresh embedding over the least informative string in the store (the probe's \
             33.1 h is the v1 atom set's 1.67M, not this store's 51,781 in-scope articles). \
             \n\nZero embed calls and no model has to be resident. Reads `articles.lance` \
             for the atom_id -> chunk_id join and `chunks.lance` for the vectors, and \
             writes `atoms_ann.lance` through the same writer every other atlas uses. \
             Articles with no chunk anchor and chunks with no vector are reported \
             separately — they are different failures.",
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
        "seed-table" => cmd_seed_table(&args[1..]).await,
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

/// Parsed `neighbors` arguments.
///
/// Split out of [`cmd_neighbors`] so the flag handling can be tested without a
/// store, because the flag handling is what was broken. The parser it replaces
/// took positionals as `args.filter(|a| !a.starts_with("--"))` and then scanned
/// separately for `--limit`, which meant every other flag was DROPPED without a
/// word — `--atlas-dir /nonexistent` ran happily against the installed store —
/// and `--limit abc` became `unwrap_or(10)`. Two silent substitutions in one
/// function (ARCH §18.3). The loop below is the one `cmd_build_graph` already
/// used; this is that shape reused, not a second parser invented (§19).
#[derive(Debug, PartialEq, Eq)]
struct NeighborsArgs {
    corpus_id: String,
    title: String,
    limit: usize,
    /// `None` = the installed store for `corpus_id`.
    atlas_dir: Option<std::path::PathBuf>,
}

fn parse_neighbors_args(args: &[String]) -> Result<NeighborsArgs, String> {
    let mut corpus_id: Option<String> = None;
    let mut title: Option<String> = None;
    let mut limit: usize = 10;
    let mut atlas_dir: Option<std::path::PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--atlas-dir" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return Err("--atlas-dir needs a value".to_string());
                };
                atlas_dir = Some(std::path::PathBuf::from(v));
            }
            "--limit" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return Err("--limit needs a value".to_string());
                };
                // Refuse, never default: `unwrap_or(10)` here meant a typo'd
                // limit silently produced a different query than the one asked
                // for, and reported it as if it were the one asked for.
                limit = v
                    .parse()
                    .map_err(|_| format!("--limit needs a number, got `{v}`"))?;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}`"));
            }
            other if corpus_id.is_none() => corpus_id = Some(other.to_string()),
            other if title.is_none() => title = Some(other.to_string()),
            other => return Err(format!("unexpected positional `{other}`")),
        }
        i += 1;
    }

    match (corpus_id, title) {
        (Some(corpus_id), Some(title)) => Ok(NeighborsArgs {
            corpus_id,
            title,
            limit,
            atlas_dir,
        }),
        _ => Err("<corpus-id> and <article-title> are both required".to_string()),
    }
}

/// Print `neighbors(title)` from the columnar store, with read latency. The
/// real-data spot check beside the fixture tests.
async fn cmd_neighbors(args: &[String]) -> i32 {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        help::print(&NEIGHBORS_HELP);
        return 0;
    }
    let a = match parse_neighbors_args(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            help::print(&NEIGHBORS_HELP);
            return 2;
        }
    };
    let atlas_dir = a.atlas_dir.clone().unwrap_or_else(|| {
        indexes_dir()
            .join(&a.corpus_id)
            .join(corpus_engine::enrichment::atlas::ATLAS_DIRNAME)
    });

    let g = match corpus_engine::ColumnarWikipediaGraph::open(&atlas_dir).await {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: open columnar store at {}: {e}", atlas_dir.display());
            eprintln!(
                "hint: run `sovereign atlas wikipedia build-graph {}` first",
                a.corpus_id
            );
            return 1;
        }
    };
    let t = std::time::Instant::now();
    let ns = g.neighbors(&a.title, a.limit).await;
    // NAME THE STORE, always. A read that does not say what it read is how a
    // probe of a freshly built store came back describing the installed one.
    println!(
        "neighbors of {:?} in corpus `{}` — {} results in {} ms\n  store: {}\n",
        a.title,
        a.corpus_id,
        ns.len(),
        t.elapsed().as_millis(),
        atlas_dir.display()
    );
    for x in &ns {
        println!(
            "  {} [{}] occ={} in_scope={}",
            x.title, x.relationship_type, x.occurrence_count, x.in_scope
        );
    }
    if ns.is_empty() {
        // EMPTY AND ABSENT ARE DIFFERENT FACTS (ARCH §18.3). `record` already
        // answers this — no new accessor (§19) — and the old code asked the
        // reader instead: "(no neighbors — is it an in-scope article?)".
        return match g.record(&a.title).await {
            Some(_) => {
                eprintln!("(article present in this store, 0 in-store outgoing edges)");
                0
            }
            None => {
                eprintln!("(no such article in this store: {:?})", a.title);
                1
            }
        };
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
    let summary =
        match build_wikipedia_columnar_store_from_chunks(&atlas_dir, &corpus_id, chunks).await {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// THE DEFECT, as its failing input.
    ///
    /// ei-7c's rebuild staged a `probe` leg to show the newly built store
    /// answering. It called this verb, which had no `--atlas-dir`, so it read
    /// the INSTALLED store and its numbers were reported as evidence for a
    /// rebuild they had never touched. Before the fix the flag was dropped by
    /// `filter(|arg| !arg.starts_with("--"))` and `/nonexistent/path` was
    /// accepted in silence.
    #[test]
    fn atlas_dir_is_honoured_and_not_swallowed() {
        let p = parse_neighbors_args(&a(&[
            "wikipedia",
            "Jigsaw puzzle",
            "--atlas-dir",
            "/some/where/else/atlas",
        ]))
        .expect("a well-formed invocation must parse");
        assert_eq!(
            p.atlas_dir,
            Some(std::path::PathBuf::from("/some/where/else/atlas")),
            "the store the caller named must survive parsing"
        );
        assert_eq!(p.corpus_id, "wikipedia");
        assert_eq!(p.title, "Jigsaw puzzle");
        assert_eq!(p.limit, 10, "unstated limit keeps its documented default");
        // Omitted means "the installed store", which is a decision the caller
        // can see rather than a path baked into the parse.
        let d = parse_neighbors_args(&a(&["wikipedia", "Jigsaw puzzle"])).unwrap();
        assert_eq!(d.atlas_dir, None);
    }

    /// An unrecognised flag is REFUSED, not dropped. This is the general form
    /// of the bug above: `--atlas-dir` was only one of the flags that vanished.
    #[test]
    fn an_unknown_flag_is_refused_by_name() {
        let e = parse_neighbors_args(&a(&["wikipedia", "T", "--nope"]))
            .expect_err("an unknown flag must not be silently dropped");
        assert!(e.contains("--nope"), "the error must name the flag: {e}");
        // Including one that merely LOOKS like a supported flag.
        let e = parse_neighbors_args(&a(&["wikipedia", "T", "--atlas_dir", "/x"]))
            .expect_err("a near-miss flag is still unknown");
        assert!(e.contains("--atlas_dir"), "{e}");
    }

    /// The second silent substitution in the same function: `--limit abc` was
    /// `unwrap_or(10)`, so a typo'd limit ran a different query than the one
    /// asked for and reported it as the one asked for.
    #[test]
    fn a_non_numeric_limit_is_refused_rather_than_defaulted() {
        let e = parse_neighbors_args(&a(&["wikipedia", "T", "--limit", "abc"]))
            .expect_err("a non-numeric limit must be refused");
        assert!(e.contains("abc"), "the error must quote the bad value: {e}");
        assert_eq!(
            parse_neighbors_args(&a(&["wikipedia", "T", "--limit", "3"]))
                .unwrap()
                .limit,
            3
        );
    }

    /// A flag consuming the next token must not silently eat the end of the
    /// argument list.
    #[test]
    fn a_flag_missing_its_value_is_refused() {
        for flag in ["--atlas-dir", "--limit"] {
            let e = parse_neighbors_args(&a(&["wikipedia", "T", flag]))
                .expect_err("a dangling flag must be refused");
            assert!(e.contains("needs a value"), "{flag}: {e}");
        }
    }

    /// Positionals stay positional, and a third one is a mistake worth naming
    /// rather than ignoring — the old parser took `.first()` and `.get(1)` and
    /// discarded whatever else arrived, which is how a swallowed flag's VALUE
    /// could land as a silent extra positional.
    #[test]
    fn missing_or_extra_positionals_are_refused() {
        assert!(parse_neighbors_args(&a(&["wikipedia"])).is_err());
        assert!(parse_neighbors_args(&a(&[])).is_err());
        let e = parse_neighbors_args(&a(&["wikipedia", "T", "extra"])).unwrap_err();
        assert!(e.contains("extra"), "{e}");
    }
}

/// `svrn atlas wikipedia seed-table <corpus-id> [--atlas-dir <path>]`
///
/// Same argument shape as `build-graph`, deliberately: they are the two halves
/// of preparing one store, and a flag that means one thing in the first and
/// another in the second is how a probe ends up reading the installed atlas
/// while believing it read the rebuilt one.
async fn cmd_seed_table(args: &[String]) -> i32 {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        help::print(&SEED_TABLE_HELP);
        return 0;
    }

    let mut corpus_id: Option<String> = None;
    let mut atlas_dir_arg: Option<std::path::PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--atlas-dir" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("error: --atlas-dir needs a value");
                    return 2;
                };
                atlas_dir_arg = Some(std::path::PathBuf::from(v));
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
            other => {
                if corpus_id.is_some() {
                    eprintln!("error: unexpected positional `{other}`");
                    return 2;
                }
                corpus_id = Some(other.to_string());
            }
        }
        i += 1;
    }

    let Some(corpus_id) = corpus_id else {
        eprintln!("error: <corpus-id> is required");
        help::print(&SEED_TABLE_HELP);
        return 2;
    };

    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|cfg| cfg.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root());
    let indexes = data_dir.join("indexes");

    // Never embeds — that is the whole point of the borrowed table — so a noop
    // EmbedFn satisfies CorpusEngine's pre-flight with no model resident.
    let noop_embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(Vec::<f32>::new()) }));
    let engine = CorpusEngine::new(data_dir.join("recipes"), indexes.clone(), noop_embed);
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

    let atlas_dir = atlas_dir_arg.unwrap_or_else(|| {
        indexes
            .join(&corpus_id)
            .join(corpus_engine::enrichment::atlas::ATLAS_DIRNAME)
    });
    // Printed on EVERY run, like `neighbors` — the store a build touched is
    // the one fact a later A/B cannot recover if it was not recorded.
    eprintln!("seeding atlas store: {}", atlas_dir.display());

    let t = std::time::Instant::now();
    match corpus_engine::enrichment::atlas::wiki_store::build_borrowed_ann_seed_table(
        &atlas_dir, &index, &corpus_id,
    )
    .await
    {
        Ok(stats) => {
            println!("seed-table {corpus_id}: {}", stats.describe());
            println!(
                "  substitution: seeds are the article's LEAD-PASSAGE vector, borrowed from \
                 chunks.lance — not an embedding of the atom's own text. 0 embed calls."
            );
            eprintln!("built in {} ms", t.elapsed().as_millis());
            0
        }
        Err(e) => {
            eprintln!("seed-table {corpus_id}: {e}");
            1
        }
    }
}
