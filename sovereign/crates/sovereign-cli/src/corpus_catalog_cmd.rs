//! `sovereign corpus catalog <subcommand>`
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

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::recipe::CatalogConfig;
use corpus_engine::types::CorpusKind;
use corpus_engine::{CorpusEngine, EmbedFn, ScoredChunk};
use sovereign_tools::catalog::{
    partition_hits_by_kind, CatalogResolutionContext,
};
use sovereign_tools::catalog_ingest::{
    run_catalog_ingest, CatalogIngestEvent, CatalogIngestRequest,
};

const HELP_CATALOG: &str = "\
sovereign corpus catalog — Catalog-corpus probes and on-demand ingest demo.

USAGE:
    sovereign corpus catalog <subcommand>

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
    sovereign corpus catalog query \"moby dick\"
    sovereign corpus catalog simulate \"obsession in 19th century american literature\"
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
            eprintln!("Usage: sovereign corpus catalog query <text>");
            return 1;
        }
    };
    let engine = match build_engine() {
        Ok(e) => e,
        Err(code) => return code,
    };
    let report = match search_catalog(&engine, &query).await {
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
        eprintln!("Usage: sovereign corpus catalog simulate <text> [--enrich] [--yes]");
        return 1;
    };
    let engine = match build_engine() {
        Ok(e) => e,
        Err(code) => return code,
    };
    let engine = Arc::new(engine);
    let report = match search_catalog(&engine, &query).await {
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
        let confirmed = crate::util::prompts::confirm(&prompt, false);
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
    match run_catalog_ingest(engine, req).await {
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

async fn search_catalog(
    engine: &CorpusEngine,
    query: &str,
) -> Result<QueryReport, i32> {
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
            if let Ok(recipe) =
                engine.registry().fetch_recipe(&info.corpus_id).await
            {
                if let Some(cat) = recipe.catalog {
                    catalog_configs.insert(info.corpus_id.clone(), cat);
                }
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
        println!(
            "Catalogs available: {}",
            report.catalogs_present.join(", ")
        );
    }
    println!();
    if report.full_text.is_empty() && report.catalog_hits.is_empty() {
        println!("No hits.");
        return;
    }
    if !report.full_text.is_empty() {
        println!("FULL-TEXT HITS:");
        for (i, h) in report.full_text.iter().take(5).enumerate() {
            let title = h
                .title
                .clone()
                .unwrap_or_else(|| h.corpus_id.clone());
            let preview = &h.content[..h.content.len().min(160)].replace('\n', " ");
            println!(
                "  [{}] [{:.2}] {} :: {}",
                i + 1,
                h.score,
                title,
                preview
            );
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
                line.push_str(&format!("\n         Ingest estimate: ~{mins} min · download: {}", h.download_url));
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
    use corpus_engine::progress::IngestProgress;
    match evt {
        CatalogIngestEvent::Resolving { catalog_corpus_id, work_id } => {
            println!("  ↳ resolving {work_id} in {catalog_corpus_id}…");
        }
        CatalogIngestEvent::Resolved { title, download_url, new_corpus_id } => {
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
            IngestProgress::Extracting { documents_processed } => {
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
            IngestProgress::Indexing { chunks_indexed, total } => {
                println!("\n  ↳ index: {chunks_indexed}/{total}");
            }
            IngestProgress::OptimizingIndex { current_chunks } => {
                println!("  ↳ optimize ({current_chunks} chunks)…");
            }
            IngestProgress::Enriching { detail, fraction, .. } => {
                match fraction {
                    Some(f) => println!("  ↳ enrich: {detail} ({:.0}%)", f * 100.0),
                    None => println!("  ↳ enrich: {detail}"),
                }
            }
            IngestProgress::Complete { total_chunks, duration_secs } => {
                println!(
                    "  ↳ complete: {total_chunks} chunks in {duration_secs}s"
                );
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
            println!(
                "  ✓ {new_corpus_id} ({chunks_created} chunks indexed)"
            );
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

fn build_engine() -> Result<CorpusEngine, i32> {
    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|cfg| cfg.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        });
    let recipes_dir = data_dir.join("recipes");
    let index_dir = data_dir.join("indexes");

    // Catalog query is FTS-only — we never call the embed function.
    // For simulate, the on-demand ingest path embeds the per-work
    // corpus's chunks; that requires a real model. We wire a noop
    // here so `corpus catalog query` works on any install, and a
    // simulate that needs embeddings will fail-fast at the engine's
    // pre-flight (clear error instead of silent zero-vector ingest).
    let noop_embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(Vec::<f32>::new()) }));
    let engine = CorpusEngine::new(recipes_dir, index_dir, noop_embed)
        .with_embedding_model("qwen-embedding-0.6b");
    Ok(engine)
}
