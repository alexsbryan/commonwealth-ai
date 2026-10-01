// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn corpus catalog <subcommand>`
//!
//! Catalog-corpus probes and the on-demand single-work simulator.
//! Pairs with the `gutenberg` catalog recipe and the `gutenberg-work`
//! on-demand content recipe.
//!
//! Subcommands:
//!
//! - `query <text>` — FTS-search every installed catalog corpus and
//!   print the partitioned results: full-text hits in one section,
//!   catalog-aware hits (with metadata-only context) in another.
//!   No LLM, no network, no embedder required — useful as a
//!   smoke-test of the partition logic against a live install.
//!
//! - `simulate <text>` — run `query` then, if a catalog hit is
//!   present, prompt `[y/N]` to fire an on-demand single-work
//!   ingest for the top hit. Streams progress events to the
//!   terminal so an operator can watch download / extract /
//!   chunk / index phases. Skips enrichment by default for a
//!   fast demo (`--enrich` opts in).

use std::sync::Arc;

use corpus_index::ingest_port::daemon::IngestPort;
use corpus_index::recipe::CatalogConfig;
use corpus_index::source::CorpusReadPort;
use corpus_index::types::{CorpusKind, ScoredChunk};
use sovereign_tools::catalog::{partition_hits_by_kind, CatalogResolutionContext};
use sovereign_tools::catalog_ingest::{
    run_catalog_ingest, CatalogIngestEvent, CatalogIngestRequest,
};

const HELP_CATALOG: &str = "\
svrn corpus catalog — Catalog-corpus probes and on-demand ingest demo.

USAGE:
    svrn corpus catalog <subcommand>

SUBCOMMANDS:
    query <text>      Search installed catalog corpora and print
                      partitioned results (full-text vs catalog-aware).
                      No LLM / no network — pure FTS.

    simulate <text>   Run `query`, then prompt to ingest the top
                      catalog hit on-demand. Streams ingest progress.
                      Pairs with the `gutenberg` + `gutenberg-work`
                      recipes for the demo flow.

FLAGS (simulate):
    --enrich          Run literary_atlas enrichment after ingest.
                      Off by default — keeps the demo fast.
    --yes             Auto-confirm the ingest prompt (for scripting).

EXAMPLES:
    svrn corpus catalog query \"moby dick\"
    svrn corpus catalog simulate \"obsession in 19th century american literature\"
";

pub async fn run_catalog(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        println!("{HELP_CATALOG}");
        return if args.is_empty() { 1 } else { 0 };
    }

    match args[0].as_str() {
        "query" => cmd_query(&args[1..]).await,
        "simulate" => cmd_simulate(&args[1..]).await,
        other => {
            eprintln!("Unknown catalog subcommand: {other}");
            println!("{HELP_CATALOG}");
            1
        }
    }
}

async fn cmd_query(args: &[String]) -> i32 {
    let query = match args.first() {
        Some(q) => q.clone(),
        None => {
            eprintln!("Usage: svrn corpus catalog query <text>");
            return 1;
        }
    };
    let engine = match build_engine() {
        Ok(e) => e,
        Err(code) => return code,
    };
    let report = match search_catalog(engine.as_ref(), &query).await {
        Ok(r) => r,
        Err(code) => return code,
    };
    print_query_report(&query, &report);
    0
}

async fn cmd_simulate(args: &[String]) -> i32 {
    let mut query: Option<String> = None;
    let mut enrich = false;
    let mut auto_yes = false;
    for a in args {
        match a.as_str() {
            "--enrich" => enrich = true,
            "--yes" | "-y" => auto_yes = true,
            "--help" | "-h" => {
                println!("{HELP_CATALOG}");
                return 0;
            }
            other if !other.starts_with('-') => {
                if query.is_none() {
                    query = Some(other.to_string());
                }
            }
            other => {
                eprintln!("Unknown flag: {other}");
                return 1;
            }
        }
    }
    let Some(query) = query else {
        eprintln!("Usage: svrn corpus catalog simulate <text> [--enrich] [--yes]");
        return 1;
    };
    let engine = match build_engine() {
        Ok(e) => e,
        Err(code) => return code,
    };
    let atlas = match crate::chat_cmd::ingest::atlas() {
        Ok(a) => a,
        Err(why) => {
            eprintln!("error: {why}");
            return 1;
        }
    };
    let report = match search_catalog(engine.as_ref(), &query).await {
        Ok(r) => r,
        Err(code) => return code,
    };
    print_query_report(&query, &report);

    let Some(top) = report.catalog_hits.first() else {
        println!();
        println!(
            "No catalog-aware hits — nothing to offer. Install the \
             `gutenberg` catalog and try a query that names a public-domain work."
        );
        return 0;
    };
    if let Some(corpus_id) = &top.already_ingested_corpus_id {
        println!();
        println!(
            "`{title}` is already ingested as `{corpus_id}` — query \
             that corpus directly for full-text results.",
            title = top.title
        );
        return 0;
    }

    if !auto_yes {
        let mins = top
            .estimated_ingest_minutes
            .map(|m| format!("~{m} min"))
            .unwrap_or_else(|| "a few minutes".to_string());
        let prompt = format!(
            "\nIngest \"{}\" ({title_id})? {mins} [y/N]: ",
            top.title,
            title_id = top.work_id,
        );
        let confirmed = sovereign_cli_base::prompts::confirm(&prompt, false);
        if !confirmed {
            println!("Skipping ingest. Run again later when you're ready.");
            return 0;
        }
    }

    println!();
    println!("Ingesting {} ({})…", top.title, top.work_id);
    let progress = Arc::new(|evt: CatalogIngestEvent| {
        print_ingest_event(&evt);
    }) as sovereign_tools::catalog_ingest::CatalogIngestProgressFn;
    let req = CatalogIngestRequest {
        catalog_corpus_id: top.catalog_corpus_id.clone(),
        work_id: top.work_id.clone(),
        enrich,
        progress: Some(progress),
        cancel: None,
        // Demo simulator runs synchronously — disable expansion so
        // the user sees a single deterministic ingest.
        expand_links: false,
    };
    match run_catalog_ingest(engine as _, atlas, req).await {
        Ok(corpus_id) => {
            println!();
            println!("✓ Ingested → corpus_id = {corpus_id}");
            0
        }
        Err(e) => {
            eprintln!();
            eprintln!("ingest failed: {e}");
            1
        }
    }
}

// ─── Helpers ───────────────────────────────────────────

struct QueryReport {
    full_text: Vec<ScoredChunk>,
    catalog_hits: Vec<sovereign_tools::catalog::CatalogHit>,
    catalogs_present: Vec<String>,
}

async fn search_catalog(engine: &dyn CorpusReadPort, query: &str) -> Result<QueryReport, i32> {
    let indexes = match engine.installed_indexes().await {
        Ok(ix) => ix,
        Err(e) => {
            eprintln!("installed_indexes() failed: {e}");
            return Err(1);
        }
    };
    if indexes.is_empty() {
        eprintln!(
            "No installed corpora found. Install the catalog with:\n\
             \n\
             \tsovereign corpus install gutenberg\n"
        );
        return Err(1);
    }

    // Per-corpus kind map for partition_hits_by_kind.
    let kinds: std::collections::HashMap<String, CorpusKind> = indexes
        .iter()
        .map(|i| (i.corpus_id.clone(), i.kind))
        .collect();

    // Resolve [catalog] blocks for each catalog corpus. Best-effort —
    // a missing block drops the corpus's hits back into full-text
    // formatting but doesn't error.
    let mut catalog_configs: std::collections::HashMap<String, CatalogConfig> =
        std::collections::HashMap::new();
    let mut catalogs_present: Vec<String> = Vec::new();
    for info in &indexes {
        if info.kind == CorpusKind::Catalog {
            catalogs_present.push(info.corpus_id.clone());
            if let Ok(Some(cat)) = engine.catalog_config(&info.corpus_id).await {
                catalog_configs.insert(info.corpus_id.clone(), cat);
            }
        }
    }
    let ctx = CatalogResolutionContext::from_indexes(&indexes, catalog_configs);

    let mut full_text = Vec::new();
    let mut catalog_hits = Vec::new();
    for info in &indexes {
        let idx = match engine.open_index(&info.path).await {
            Ok(i) => i,
            Err(_) => continue,
        };
        // Empty embedding → FTS-only. The catalog corpus is small
        // enough to flat-scan if FTS isn't built; for a freshly-installed
        // gutenberg catalog Tantivy fires immediately.
        let scored = match idx.search(&[], query, 5).await {
            Ok(s) => s,
            Err(_) => continue,
        };
        let (ft, cat) = partition_hits_by_kind(scored, &kinds, &ctx);
        full_text.extend(ft);
        catalog_hits.extend(cat);
    }

    // Sort each list by score desc.
    full_text.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    catalog_hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(QueryReport {
        full_text,
        catalog_hits,
        catalogs_present,
    })
}

fn print_query_report(query: &str, report: &QueryReport) {
    println!("Query: {query:?}");
    if !report.catalogs_present.is_empty() {
        println!("Catalogs available: {}", report.catalogs_present.join(", "));
    }
    println!();
    if report.full_text.is_empty() && report.catalog_hits.is_empty() {
        println!("No hits.");
        return;
    }
    if !report.full_text.is_empty() {
        println!("FULL-TEXT HITS:");
        for (i, h) in report.full_text.iter().take(5).enumerate() {
            let title = h.title.clone().unwrap_or_else(|| h.corpus_id.clone());
            let preview = &h.content[..h.content.len().min(160)].replace('\n', " ");
            println!("  [{}] [{:.2}] {} :: {}", i + 1, h.score, title, preview);
        }
        println!();
    }
    if !report.catalog_hits.is_empty() {
        println!("CATALOG-AWARE HITS (metadata only — full text NOT yet ingested):");
        for (i, h) in report.catalog_hits.iter().take(5).enumerate() {
            let mut line = format!("  [C{}] [{:.2}] {}", i + 1, h.score, h.title);
            if let Some(a) = &h.authors {
                line.push_str(&format!(" — {a}"));
            }
            if let Some(y) = &h.year {
                line.push_str(&format!(" ({y})"));
            }
            if let Some(corpus_id) = &h.already_ingested_corpus_id {
                line.push_str(&format!("\n         ALREADY INGESTED → {corpus_id}"));
            } else if let Some(mins) = h.estimated_ingest_minutes {
                line.push_str(&format!(
                    "\n         Ingest estimate: ~{mins} min · download: {}",
                    h.download_url
                ));
            } else {
                line.push_str(&format!("\n         download: {}", h.download_url));
            }
            if let Some(s) = &h.subjects {
                let trimmed: String = s.chars().take(140).collect();
                line.push_str(&format!("\n         Subjects: {trimmed}"));
            }
            println!("{line}");
        }
    }
}

fn print_ingest_event(evt: &CatalogIngestEvent) {
    use sovereign_contracts::daemon_wire::IngestProgress;
    match evt {
        CatalogIngestEvent::Resolving {
            catalog_corpus_id,
            work_id,
        } => {
            println!("  ↳ resolving {work_id} in {catalog_corpus_id}…");
        }
        CatalogIngestEvent::Resolved {
            title,
            download_url,
            new_corpus_id,
        } => {
            println!("  ↳ resolved: \"{title}\"");
            println!("     download: {download_url}");
            println!("     target corpus: {new_corpus_id}");
        }
        CatalogIngestEvent::Ingest(p) => match p {
            IngestProgress::Downloading { percent, .. } => {
                print!("\r  ↳ download… {:.1}%", percent);
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
            IngestProgress::Extracting {
                documents_processed,
            } => {
                println!("\n  ↳ extract: {documents_processed} docs");
            }
            IngestProgress::Chunking { chunks_created } => {
                println!("  ↳ chunked: {chunks_created} chunks");
            }
            IngestProgress::Embedding {
                chunks_embedded,
                total,
                ..
            } => {
                print!("\r  ↳ embed: {chunks_embedded}/{total} chunks");
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
            IngestProgress::Indexing {
                chunks_indexed,
                total,
            } => {
                println!("\n  ↳ index: {chunks_indexed}/{total}");
            }
            IngestProgress::OptimizingIndex { current_chunks } => {
                println!("  ↳ optimize ({current_chunks} chunks)…");
            }
            IngestProgress::Enriching {
                detail, fraction, ..
            } => match fraction {
                Some(f) => println!("  ↳ enrich: {detail} ({:.0}%)", f * 100.0),
                None => println!("  ↳ enrich: {detail}"),
            },
            IngestProgress::Complete {
                total_chunks,
                duration_secs,
            } => {
                println!("  ↳ complete: {total_chunks} chunks in {duration_secs}s");
            }
            // Print the engine's message in full — for an authorisation
            // refusal it carries the remedy, which is the one thing the
            // operator running this command needs.
            IngestProgress::Failed { message } => {
                println!("  ↳ FAILED: {message}");
            }
        },
        CatalogIngestEvent::Enrich(_) => {
            println!("  ↳ enrich…");
        }
        CatalogIngestEvent::Complete {
            new_corpus_id,
            chunks_created,
            atlas_summary,
        } => {
            println!();
            println!("  ✓ {new_corpus_id} ({chunks_created} chunks indexed)");
            if let Some(a) = atlas_summary {
                println!(
                    "  ✓ atlas: {atoms} atoms, {edges} edges, {themes} themes, {q} questions",
                    atoms = a.atoms,
                    edges = a.edges,
                    themes = a.themes,
                    q = a.questions,
                );
            }
        }
        CatalogIngestEvent::Failed { stage, message } => {
            eprintln!();
            eprintln!("  ✗ {stage:?}: {message}");
        }
    }
}

/// Ingest's engine for the catalog verbs and `corpus pull`'s unpack, through
/// the process's one composition (`chat_cmd::ingest`). `None` there is the
/// named absence.
pub(crate) fn build_engine() -> Result<Arc<dyn IngestPort>, i32> {
    let data_dir = sovereign_contracts::setup_config::SetupConfig::load()
        .map(|cfg| cfg.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root());

    // Catalog query is FTS-only — we never call the embed function.
    // For simulate, the on-demand ingest path embeds the per-work
    // corpus's chunks; that requires a real model. We wire a noop
    // here so `corpus catalog query` works on any install, and a
    // simulate that needs embeddings will fail-fast at the engine's
    // pre-flight (clear error instead of silent zero-vector ingest).
    // The engine writes no chunk entities here (no NER is composed), so
    // its entity store is an in-memory one: these verbs never opened
    // sovereign.db.
    let store = match sovereign_store::sqlite::SqliteStateStore::open_in_memory() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("error: open in-memory store: {e}");
            return Err(1);
        }
    };
    match crate::chat_cmd::ingest::compose(
        data_dir,
        Arc::new(FtsOnly),
        "qwen-embedding-0.6b",
        store,
    ) {
        Some(mount) => Ok(mount.port),
        None => {
            eprintln!("error: {}", crate::chat_cmd::ingest::NO_INGEST);
            Err(1)
        }
    }
}

/// The catalog verbs' inference: the noop embed they always had (an empty
/// vector, which the engine's pre-flight refuses), and no generation.
struct FtsOnly;

#[async_trait::async_trait]
impl sovereign_core::traits::InferenceProvider for FtsOnly {
    async fn complete(
        &self,
        _request: &sovereign_core::types::CompletionRequest,
    ) -> sovereign_core::error::Result<sovereign_core::types::CompletionResponse> {
        Err(sovereign_core::Error::Inference(
            "the catalog verbs carry no generation".to_string(),
        ))
    }
    async fn complete_stream(
        &self,
        _request: &sovereign_core::types::CompletionRequest,
    ) -> sovereign_core::error::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = sovereign_core::error::Result<String>> + Send>,
        >,
    > {
        Err(sovereign_core::Error::Inference(
            "the catalog verbs carry no generation".to_string(),
        ))
    }
    async fn embed(&self, _text: &str) -> sovereign_core::error::Result<Vec<f32>> {
        Ok(Vec::new())
    }
    fn capabilities(&self) -> sovereign_core::types::ProviderCapabilities {
        sovereign_core::types::ProviderCapabilities {
            max_context_tokens: 0,
            supports_structured_output: false,
            relative_speed: sovereign_core::types::Speed::Fast,
            relative_reasoning: sovereign_core::types::Depth::Shallow,
        }
    }
}
