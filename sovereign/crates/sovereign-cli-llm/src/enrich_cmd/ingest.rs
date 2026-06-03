//! `sovereign enrich ingest <corpus> [--strategy <id>] [--source-corpus <id>] [--limit-articles <N>]`
//!
//! Drives an `AtlasIngestion` strategy end-to-end: open the source
//! corpus index, run the strategy, write the resulting `AtlasData`
//! bundle to `~/.sovereign/indexes/<corpus>/atlas/`.
//!
//! Today's only strategy with a working `ingest()` is
//! `structure_first` — the deterministic Wikipedia parser.
//! `extraction_first` is registered for forward compatibility but its
//! `ingest()` is scaffolded; that pipeline is still driven by the
//! per-phase subcommands (`extract`, `cluster`, `name`, ...).

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::enrichment::atlas::{AtlasData, AtlasIngestionConfig, AtlasIngestionRegistry};
use corpus_engine::{CorpusEngine, EmbedFn, IngestProgress, ProgressCallback};

use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich ingest",
    summary: "Run an atlas ingestion strategy end-to-end (today: structure_first for Wikipedia).",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich ingest <corpus-id> --source-corpus <id> [--strategy structure_first] [--limit-articles <N>]",
        ),
        HelpSection::Flags(&[
            (
                "--source-corpus <id>",
                "Already-ingested corpus to read chunks from (required). Typically `wikipedia` or a code-corpus id like `commonwealth-ai`.",
            ),
            (
                "--strategy <id>",
                "Strategy id from the AtlasIngestionRegistry. Default: structure_first. Use `--list-strategies` to see what's registered.",
            ),
            (
                "--limit-articles <N>",
                "Wikipedia branch only: cap on the number of articles processed. Useful for fast-iteration validation. Sorted by article title for stable ordering.",
            ),
            (
                "--include-functions",
                "Code branch only: emit Entity atoms for `pub fn` / methods. Off by default — function-tier atoms inflate the demo atlas without paying back.",
            ),
            (
                "--include-private",
                "Code branch only: include non-`pub` items. Off by default — public surface is the architectural shape; private internals are implementation detail.",
            ),
            (
                "--list-strategies",
                "Print every registered strategy id and exit.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign enrich ingest wiki-rep-struct --source-corpus wikipedia --limit-articles 25",
                "Validate the structure-first parser on a 25-article slice.",
            ),
            (
                "sovereign enrich ingest wikipedia --source-corpus wikipedia",
                "Build the structural atlas over the full installed corpus.",
            ),
            (
                "sovereign enrich ingest sovereign-self-atlas --source-corpus commonwealth-ai",
                "Build the structural code atlas over the workspace's indexed source.",
            ),
        ]),
        HelpSection::Notes(
            "Output goes to ~/.sovereign/indexes/<corpus-id>/atlas/{atoms.json, edges.json, schema_validation.json}. \
             No daemon required — the structure_first strategy is pure Rust.",
        ),
    ],
};

pub async fn cmd_ingest(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }

    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };

    let registry = AtlasIngestionRegistry::builtin();

    if parsed.list_strategies {
        let mut ids: Vec<&str> = registry.strategy_ids();
        ids.sort();
        println!("Registered atlas ingestion strategies:");
        for id in ids {
            let s = registry.get(id).expect("registered id resolves");
            println!("  {:<24} {}", s.id(), s.name());
        }
        return 0;
    }

    let Some(corpus_id) = parsed.corpus_id.as_deref() else {
        eprintln!("error: missing <corpus-id>");
        eprintln!();
        help::print(&HELP);
        return 2;
    };
    let Some(source_corpus) = parsed.source_corpus.as_deref() else {
        eprintln!(
            "error: --source-corpus is required (the already-ingested corpus to read chunks from)"
        );
        return 2;
    };

    let strategy_id = parsed.strategy.as_deref().unwrap_or("structure_first");
    let strategy = match registry.get(strategy_id) {
        Some(s) => s,
        None => {
            eprintln!(
                "error: unknown strategy '{strategy_id}'. Run with --list-strategies to see registered ids."
            );
            return 2;
        }
    };

    println!("  using strategy '{}' ({})", strategy.id(), strategy.name());
    println!("  source corpus = {source_corpus}");
    if let Some(n) = parsed.limit_articles {
        println!("  limit_articles = {n}");
    }

    // Resolve indexes dir from setup config (mirrors how the
    // --from-corpus init path does it).
    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        });
    let recipes_dir = data_dir.join("recipes");
    let indexes_dir = data_dir.join("indexes");

    // structure_first doesn't embed; wire a no-op EmbedFn so the
    // engine constructor succeeds without a model. A future strategy
    // that needs embeddings can override this.
    let noop_embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(Vec::<f32>::new()) }));
    let engine = Arc::new(CorpusEngine::new(
        recipes_dir,
        indexes_dir,
        noop_embed.clone(),
    ));

    // Strategy config blob — strategy-internal shape.
    let mut strategy_config = serde_json::json!({
        "source_corpus_id": source_corpus,
    });
    if let Some(n) = parsed.limit_articles {
        if let Some(obj) = strategy_config.as_object_mut() {
            obj.insert("limit_articles".into(), serde_json::json!(n));
        }
    }
    if parsed.include_functions {
        if let Some(obj) = strategy_config.as_object_mut() {
            obj.insert("include_functions".into(), serde_json::json!(true));
        }
    }
    if parsed.include_private {
        if let Some(obj) = strategy_config.as_object_mut() {
            obj.insert("include_private".into(), serde_json::json!(true));
        }
    }
    let cfg = AtlasIngestionConfig {
        strategy_id: strategy_id.to_string(),
        strategy_config,
    };

    // Progress sink — log typed IngestProgress events (the strategy
    // ALSO emits free-form `tracing::info!` messages between events
    // for richer status; both surface together when RUST_LOG=info).
    let started = std::time::Instant::now();
    let progress: Arc<ProgressCallback> = Arc::new(Box::new(move |ev: IngestProgress| {
        let secs = started.elapsed().as_secs();
        tracing::info!(elapsed_s = secs, ?ev, "ingest progress");
    }));

    let inference_fn: Option<corpus_engine::InferenceFn> = None;

    let t_start = std::time::Instant::now();
    let result = strategy
        .ingest(engine, noop_embed, inference_fn, cfg, progress)
        .await;
    let elapsed = t_start.elapsed();

    let data: AtlasData = match result {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: ingestion failed: {e}");
            return 1;
        }
    };

    let atlas_dir = paths::index_root(corpus_id).join("atlas");
    if let Err(e) = std::fs::create_dir_all(&atlas_dir) {
        eprintln!("error: creating {}: {e}", atlas_dir.display());
        return 1;
    }
    let atoms_path = atlas_dir.join("atoms.json");
    let edges_path = atlas_dir.join("edges.json");
    let schema_path = atlas_dir.join("schema_validation.json");

    if let Err(e) = write_atomic_json(&atoms_path, &data.atoms) {
        eprintln!("error: writing {}: {e}", atoms_path.display());
        return 1;
    }
    if let Err(e) = write_atomic_json(&edges_path, &data.edges) {
        eprintln!("error: writing {}: {e}", edges_path.display());
        return 1;
    }
    if let Err(e) = write_atomic_json(&schema_path, &data.schema_validation) {
        eprintln!("error: writing {}: {e}", schema_path.display());
        return 1;
    }

    let secs = elapsed.as_secs_f64();
    println!();
    println!("  ✓ wrote {}", atoms_path.display());
    println!("  ✓ wrote {}", edges_path.display());
    println!("  ✓ wrote {}", schema_path.display());
    println!("  ✓ dominant_depth = {:?}", data.dominant_depth);
    println!("  ✓ wall time     = {secs:.2}s");

    // Inline summary from schema_validation if the strategy provided one.
    if let Some(stats) = data.schema_validation.get("stats") {
        println!();
        println!("  stats:");
        if let Some(obj) = stats.as_object() {
            for (k, v) in obj {
                println!("    {k:<28} {v}");
            }
        }
    }

    0
}

fn write_atomic_json(path: &std::path::Path, value: &serde_json::Value) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[derive(Debug, Default)]
struct ParsedIngest {
    corpus_id: Option<String>,
    source_corpus: Option<String>,
    strategy: Option<String>,
    limit_articles: Option<usize>,
    list_strategies: bool,
    /// Code-corpus branch only: emit Entity atoms for `pub fn` /
    /// methods. Off by default to keep demo atlases tractable.
    include_functions: bool,
    /// Code-corpus branch only: include non-`pub` items.
    include_private: bool,
}

fn parse_args(args: &[String]) -> Result<ParsedIngest, String> {
    let mut out = ParsedIngest::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source-corpus" => {
                out.source_corpus = Some(
                    args.get(i + 1)
                        .ok_or("--source-corpus requires a value".to_string())?
                        .clone(),
                );
                i += 2;
            }
            "--strategy" => {
                out.strategy = Some(
                    args.get(i + 1)
                        .ok_or("--strategy requires a value".to_string())?
                        .clone(),
                );
                i += 2;
            }
            "--limit-articles" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--limit-articles requires a value".to_string())?;
                let n: usize = v
                    .parse()
                    .map_err(|e| format!("--limit-articles must be a positive integer: {e}"))?;
                if n == 0 {
                    return Err("--limit-articles must be > 0".into());
                }
                out.limit_articles = Some(n);
                i += 2;
            }
            "--list-strategies" => {
                out.list_strategies = true;
                i += 1;
            }
            "--include-functions" => {
                out.include_functions = true;
                i += 1;
            }
            "--include-private" => {
                out.include_private = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if out.corpus_id.is_none() {
                    out.corpus_id = Some(other.to_string());
                    i += 1;
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_invocation() {
        let p = parse_args(&[
            "wiki-rep-struct".into(),
            "--source-corpus".into(),
            "wikipedia".into(),
        ])
        .unwrap();
        assert_eq!(p.corpus_id.as_deref(), Some("wiki-rep-struct"));
        assert_eq!(p.source_corpus.as_deref(), Some("wikipedia"));
        assert!(p.strategy.is_none());
        assert!(p.limit_articles.is_none());
    }

    #[test]
    fn parse_with_limit_and_strategy() {
        let p = parse_args(&[
            "x".into(),
            "--source-corpus".into(),
            "wikipedia".into(),
            "--strategy".into(),
            "structure_first".into(),
            "--limit-articles".into(),
            "100".into(),
        ])
        .unwrap();
        assert_eq!(p.strategy.as_deref(), Some("structure_first"));
        assert_eq!(p.limit_articles, Some(100));
    }

    #[test]
    fn parse_rejects_zero_limit() {
        let err = parse_args(&[
            "x".into(),
            "--source-corpus".into(),
            "w".into(),
            "--limit-articles".into(),
            "0".into(),
        ])
        .unwrap_err();
        assert!(err.contains("> 0"));
    }

    #[test]
    fn parse_list_strategies_flag() {
        let p = parse_args(&["--list-strategies".into()]).unwrap();
        assert!(p.list_strategies);
        assert!(p.corpus_id.is_none());
    }
}
