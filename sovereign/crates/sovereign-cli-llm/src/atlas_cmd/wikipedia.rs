//! `sovereign atlas wikipedia ...` — Wikipedia-specific structural
//! enrichment commands. Today Layer 0 only (link graph build).

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::{CorpusEngine, EmbedFn, WikipediaGraph};

use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign atlas wikipedia",
    summary: "Wikipedia link graph and structural enrichment.",
    sections: &[
        HelpSection::Usage("sovereign atlas wikipedia <subcommand> [args]"),
        HelpSection::Subcommands(&[(
            "build-graph",
            "Layer 0: deserialise Wikipedia extractor metadata into a SQLite link graph.",
        )]),
    ],
};

const BUILD_GRAPH_HELP: Help = Help {
    command: "sovereign atlas wikipedia build-graph",
    summary: "Build the Wikipedia link graph for an installed corpus.",
    sections: &[
        HelpSection::Usage(
            "sovereign atlas wikipedia build-graph <corpus-id> [--db-path <path>] [--rebuild]",
        ),
        HelpSection::Flags(&[
            (
                "<corpus-id>",
                "ID of an installed Wikipedia-class corpus (e.g. `wikipedia`).",
            ),
            (
                "--db-path <path>",
                "Override the graph DB location. Default: <data-dir>/indexes/<corpus>/wikipedia_graph.db",
            ),
            (
                "--rebuild",
                "Wipe the corpus's existing graph rows before ingesting. Default is incremental \
                 (re-ingest is idempotent: edge `occurrence_count` is refreshed, not reset).",
            ),
            ("--help, -h", "Show this message."),
        ]),
        HelpSection::Notes(
            "Reads the LanceDB index for <corpus-id> and walks every chunk's `metadata` \
             JSON field, deserialising `WikipediaChunkMetadata`. Aggregates per (article, \
             section) before insert so the chunker emitting N chunks per section does \
             not inflate `occurrence_count`. Layer 0 is zero LLM cost and runs in a few \
             minutes on consumer hardware at Vital L5 scope.",
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
        other => {
            eprintln!("error: unknown wikipedia subcommand `{other}`");
            help::print(&HELP);
            2
        }
    }
}

#[derive(Default)]
struct BuildGraphArgs {
    corpus_id: Option<String>,
    db_path: Option<PathBuf>,
    rebuild: bool,
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
            "--db-path" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("error: --db-path needs a value");
                    return 2;
                };
                a.db_path = Some(PathBuf::from(v));
            }
            "--rebuild" => a.rebuild = true,
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

    // Resolve data dir + indexes dir from the operator's setup config.
    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|cfg| cfg.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        });
    let recipes_dir = data_dir.join("recipes");
    let indexes_dir = data_dir.join("indexes");

    // We never embed during graph build — wire a noop EmbedFn so
    // CorpusEngine's pre-flight passes without requiring a model.
    let noop_embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(Vec::<f32>::new()) }));
    let engine = CorpusEngine::new(recipes_dir, indexes_dir.clone(), noop_embed);

    let index = match engine.open_index_for_corpus(&corpus_id).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!(
                "error: could not open index for corpus `{corpus_id}`: {e}\n\
                 hint: run `sovereign corpus install {corpus_id}` first."
            );
            return 1;
        }
    };

    let db_path = a
        .db_path
        .unwrap_or_else(|| WikipediaGraph::default_db_path(&indexes_dir, &corpus_id));
    eprintln!("opening graph at {}", db_path.display());

    let graph = match WikipediaGraph::open(&db_path, &corpus_id) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: open graph: {e}");
            return 1;
        }
    };

    if a.rebuild {
        eprintln!("--rebuild: clearing existing rows for corpus `{corpus_id}`");
        if let Err(e) = graph.clear_corpus().await {
            eprintln!("error: clear corpus: {e}");
            return 1;
        }
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
    let stream_ms = t_stream.elapsed().as_millis() as u64;
    eprintln!(
        "streamed {} chunk records in {} ms",
        chunks.len(),
        stream_ms
    );

    eprintln!("ingesting into link graph...");
    let t_ingest = std::time::Instant::now();
    let summary = match graph.ingest_from_chunks(chunks).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: ingest: {e}");
            return 1;
        }
    };
    let ingest_ms = t_ingest.elapsed().as_millis() as u64;

    let articles = graph.article_count().await;
    let edges = graph.edge_count().await;

    eprintln!();
    eprintln!("graph build complete:");
    eprintln!(
        "  chunks:   {} with metadata, {} skipped",
        summary.chunks_with_metadata, summary.chunks_without_metadata,
    );
    eprintln!(
        "  articles: {articles} in scope ({} dangling targets)",
        summary.dangling_targets
    );
    eprintln!("  edges:    {edges} unique (source, section, target)");
    eprintln!("  sections: {} signal rows", summary.sections_inserted);
    eprintln!("  ingest:   {ingest_ms} ms");
    eprintln!("  db:       {}", db_path.display());

    0
}
