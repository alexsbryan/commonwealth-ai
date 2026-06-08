//! `sovereign enrich raptor <corpus>` — retrofit an already-installed
//! corpus with a RAPTOR tier-3 summary tree: one tree per source
//! document, persisted into `conv_raptor_nodes` keyed
//! `(corpus_id, source_doc_id)`.
//!
//! ## Why this exists
//!
//! Chunk-RAG answers "find the passages near this query" — it samples
//! for cosine similarity, not coverage. A "summarize <work>" query
//! needs the *whole document*, which top-k can't give: the answer
//! (a definition, an argument's arc) is a global property no single
//! chunk encodes. A per-document RAPTOR tree's root node *is* that
//! document's own summary. This verb builds those trees over a corpus
//! that already shipped (e.g. SEP), so summarization has something
//! whole to reach for.
//!
//! ## Additive by construction
//!
//! The RAPTOR tree lives in the SQLite state store (`conv_raptor_nodes`);
//! the corpus's atom-graph atlas lives on disk (`atlas/atoms.json`).
//! Different storage, no overlap — this never touches the atom graph.
//! It reuses the existing leaf embeddings from `chunks.lance` (no
//! re-chunk, no re-embed of leaves); only the generated summary nodes
//! are embedded.
//!
//! It drives the same per-document builder the watched-folder path uses
//! ([`FolderTieredProvider`] + `enrich_conversation`), so it inherits
//! that path's per-document checkpointing (resumable — a re-run skips
//! the LLM work for trees already built), `_enrichment_state.json`
//! stamping, and motif extraction. The only difference is the cue:
//! `--doc-type argument` asks the summarizer for claim-level summaries,
//! the right shape for SEP's philosophy essays.
//!
//! ## Furniture filtering
//!
//! SEP entries publish through a fixed template whose copyright / contact
//! / navigation blocks get chunked alongside the prose. Left in, RAPTOR
//! clusters them and wastes a summary producing "this is just metadata"
//! — which would also pollute a whole-document summary. `--strip-furniture`
//! drops those chunks before clustering (at the CHUNK level, because
//! k-means sometimes mixes a content sentence into a furniture cluster —
//! dropping nodes would lose content). `--inspect-furniture` prints
//! exactly what would be dropped, no writes, so the filter can be
//! eyeballed before committing.
//!
//! ## Glassbox
//!
//! Every document prints a one-line record (chunks · bucket · nodes ·
//! wall-time); the run ends with a totals summary. `--dry-run` prints
//! the per-bucket dispatch plan with no inference and no writes, so the
//! cost is legible before committing to the full pass. `--limit N`
//! builds only the N smallest documents — a fast spike to validate the
//! pipeline before the long tail.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use corpus_engine::enrichment::tiered::ConvBucket;
use corpus_engine::index::CorpusIndex;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::DocumentTypeTag;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::conv_tiered_provider::{
    FolderTieredProvider, IndexDirResolver, StaticIndexDirResolver,
};

use crate::chat_cmd::bootstrap::SplitInferenceProvider;
use crate::util::help;

/// Parsed `enrich raptor` invocation.
struct RaptorArgs {
    corpus_id: String,
    doc_type: DocumentTypeTag,
    limit: Option<usize>,
    dry_run: bool,
    strip_furniture: bool,
    inspect_furniture: bool,
    force: bool,
    daemon_base: String,
    chat_model: String,
    embed_model: String,
}

pub async fn cmd_raptor(args: &[String]) -> i32 {
    if help::wants_help(args) {
        print_usage();
        return 0;
    }
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}\n");
            print_usage();
            return 2;
        }
    };

    // Resolve paths exactly as the daemon does: `data_dir` owns BOTH the
    // state DB (`sovereign.db`) and the corpus indexes dir. Matching the
    // daemon's derivation (daemon_cmd.rs) is what guarantees we augment
    // the same store the daemon serves retrieval from.
    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        });
    let indexes_dir = data_dir.join("indexes");
    let db_path = data_dir.join("sovereign.db");
    let index_path = indexes_dir.join(&parsed.corpus_id);

    if !index_path.exists() {
        eprintln!(
            "error: corpus '{}' is not installed at {}",
            parsed.corpus_id,
            index_path.display()
        );
        return 1;
    }

    // Open the index and compute the per-document dispatch plan BEFORE
    // touching inference, so `--dry-run` is free and a bad corpus fails
    // fast with a clear message.
    let index = match CorpusIndex::open(&index_path).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: open index {}: {e}", index_path.display());
            return 1;
        }
    };
    let groups = match index.group_chunks_by_source_doc().await {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: group chunks by source doc: {e}");
            return 1;
        }
    };
    if groups.is_empty() {
        eprintln!(
            "error: corpus '{}' has no source documents to summarize",
            parsed.corpus_id
        );
        return 1;
    }

    let mut docs: Vec<(String, usize)> = groups
        .iter()
        .map(|(id, chunk_ids)| (id.clone(), chunk_ids.len()))
        .collect();
    // Smallest documents first (tie-break by id for determinism): the
    // cheapest trees land early so the operator sees progress — and any
    // failure surfaces — before the long tail of big essays.
    docs.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    if let Some(limit) = parsed.limit {
        docs.truncate(limit);
    }
    let total_docs = docs.len();
    let total_chunks: usize = docs.iter().map(|(_, n)| *n).sum();

    let mut bucket_hist: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();
    for (_, n) in &docs {
        *bucket_hist
            .entry(ConvBucket::classify(*n).label())
            .or_default() += 1;
    }

    println!("RAPTOR retrofit plan for corpus '{}':", parsed.corpus_id);
    println!("  documents:  {total_docs}");
    println!("  chunks:     {total_chunks}");
    println!("  doc-type:   {}", parsed.doc_type.label());
    println!(
        "  furniture:  {}",
        if parsed.strip_furniture {
            "stripping SEP page-template chunks"
        } else {
            "kept (pass --strip-furniture to drop)"
        }
    );
    println!(
        "  by bucket:  {}",
        bucket_hist
            .iter()
            .map(|(b, n)| format!("{b}={n}"))
            .collect::<Vec<_>>()
            .join("  ")
    );
    if let Some(limit) = parsed.limit {
        println!("  (limited to the {limit} smallest documents)");
    }

    if parsed.dry_run {
        println!("\n--dry-run: no inference, no writes. Re-run without --dry-run to build.");
        return 0;
    }

    // Furniture inspection: load the (limited) docs, run the filter, and
    // print exactly what it would drop — so the filter can be verified
    // clean (furniture only, no philosophy) before any build. No
    // inference, no writes.
    if parsed.inspect_furniture {
        println!("\nFurniture inspection (no inference, no writes) — what --strip-furniture would drop:");
        let mut total = 0usize;
        let mut dropped = 0usize;
        let mut samples: Vec<String> = Vec::new();
        for (doc_id, _) in &docs {
            let rows = match index.chunks_for_source_doc_with_embeddings(doc_id).await {
                Ok(r) => r,
                Err(_) => continue,
            };
            for (chunk, _) in &rows {
                total += 1;
                if is_sep_furniture(&chunk.content) {
                    dropped += 1;
                    if samples.len() < 14 {
                        let preview: String = chunk.content.chars().take(95).collect();
                        samples.push(preview.replace('\n', " "));
                    }
                }
            }
        }
        let pct = if total > 0 {
            100.0 * dropped as f64 / total as f64
        } else {
            0.0
        };
        println!("  chunks scanned: {total} across {} docs", docs.len());
        println!("  would drop:     {dropped} ({pct:.0}%)");
        println!("  sample dropped chunks (verify these are ALL furniture, no philosophy):");
        for s in &samples {
            println!("    · {s}");
        }
        return 0;
    }

    // Wire inference (daemon over HTTP) + the doc-keyed provider. WAL mode
    // (migrations.rs) makes the concurrent store handle with the running
    // daemon safe; we keep a clone for post-build node counting.
    let v1 = format!("{}/v1", parsed.daemon_base.trim_end_matches('/'));
    let inference: Arc<dyn InferenceProvider> = Arc::new(SplitInferenceProvider::new(
        &v1,
        parsed.chat_model.clone(),
        parsed.embed_model.clone(),
        8192,
    ));

    let store = match SqliteStateStore::open(&db_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("error: open state db {}: {e}", db_path.display());
            return 1;
        }
    };
    let verify_store = Arc::clone(&store);

    let resolver: Arc<dyn IndexDirResolver> = Arc::new(StaticIndexDirResolver {
        indexes_root: indexes_dir.clone(),
    });
    let provider = FolderTieredProvider::new(store, inference)
        .with_index_dir_resolver(resolver)
        .with_doc_type(parsed.doc_type.clone())
        .into_handle();

    println!(
        "\nBuilding RAPTOR trees via daemon {} (chat={}, embed={})…\n",
        parsed.daemon_base, parsed.chat_model, parsed.embed_model
    );

    let run_start = Instant::now();
    let mut built = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut resumed = 0usize;
    let mut empty = 0usize;
    let mut nodes_total = 0usize;

    for (idx, (doc_id, _)) in docs.into_iter().enumerate() {
        // Doc-level resume. The per-doc RAPTOR checkpoint shares ONE dir
        // per corpus (each document clobbers the previous one's), so it
        // gives no cross-document resume on a batch run. Skipping docs
        // that already have persisted nodes makes a crashed multi-day
        // build restart-cheap: a re-launch flies past completed docs and
        // picks up where it stopped. `--force` rebuilds regardless.
        if !parsed.force {
            let existing = verify_store
                .list_conv_raptor_nodes(&parsed.corpus_id, &doc_id)
                .await
                .map(|n| n.len())
                .unwrap_or(0);
            if existing > 0 {
                if resumed == 0 {
                    println!("  (resuming — skipping documents already built; --force to rebuild)");
                }
                resumed += 1;
                continue;
            }
        }
        let rows = match index.chunks_for_source_doc_with_embeddings(&doc_id).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  [{}/{total_docs}] {doc_id}: chunk fetch failed: {e}", idx + 1);
                failed += 1;
                continue;
            }
        };
        if rows.is_empty() {
            eprintln!("  [{}/{total_docs}] {doc_id}: no embedded chunks; skipping", idx + 1);
            failed += 1;
            continue;
        }
        // Strip SEP page-template furniture before clustering so RAPTOR
        // doesn't spend a summary on copyright/nav/contact blocks (and so
        // those never pollute a whole-document summary). Filtering the
        // CHUNKS, not the resulting nodes — k-means can mix a content
        // sentence into a furniture cluster, so a node-level drop would
        // lose real content.
        let raw_count = rows.len();
        let rows: Vec<_> = if parsed.strip_furniture {
            rows.into_iter()
                .filter(|(c, _)| !is_sep_furniture(&c.content))
                .collect()
        } else {
            rows
        };
        let dropped = raw_count - rows.len();
        if rows.is_empty() {
            eprintln!(
                "  [{}/{total_docs}] {doc_id}: all {raw_count} chunks were furniture; skipping",
                idx + 1
            );
            skipped += 1;
            continue;
        }
        let bucket = ConvBucket::classify(rows.len());
        let kept = rows.len();
        let (chunks, embeddings): (Vec<_>, Vec<_>) = rows.into_iter().unzip();

        let t = Instant::now();
        match provider
            .enrich_conversation(&parsed.corpus_id, &doc_id, chunks, embeddings, bucket)
            .await
        {
            Ok(()) => {
                let node_count = verify_store
                    .list_conv_raptor_nodes(&parsed.corpus_id, &doc_id)
                    .await
                    .map(|n| n.len())
                    .unwrap_or(0);
                let furniture_note = if dropped > 0 {
                    format!(" (-{dropped} furniture)")
                } else {
                    String::new()
                };
                if node_count == 0 {
                    // A non-skipped document that persists ZERO nodes is an
                    // anomaly — almost always the summarizer failing every
                    // cluster (e.g. the daemon's inference slot crashing
                    // mid-run, as on 2026-06-07 when the 92 largest SEP docs
                    // silently produced nothing). Flag it loudly and count it
                    // as a failure so a crash can never masquerade as "built";
                    // resume retries it (it has no nodes) on the next run.
                    empty += 1;
                    eprintln!(
                        "  [{}/{total_docs}] {doc_id}  {kept} chunks{furniture_note} · {} · 0 NODES — summarizer FAILED (will retry on resume) · {:.1}s",
                        idx + 1,
                        bucket.label(),
                        t.elapsed().as_secs_f64(),
                    );
                } else {
                    nodes_total += node_count;
                    built += 1;
                    println!(
                        "  [{}/{total_docs}] {doc_id}  {kept} chunks{furniture_note} · {} · {node_count} nodes · {:.1}s",
                        idx + 1,
                        bucket.label(),
                        t.elapsed().as_secs_f64(),
                    );
                }
            }
            Err(e) => {
                failed += 1;
                eprintln!("  [{}/{total_docs}] {doc_id}: build failed: {e}", idx + 1);
            }
        }
    }

    let elapsed = run_start.elapsed();
    println!("\nRAPTOR retrofit complete for '{}':", parsed.corpus_id);
    println!("  documents built:  {built}");
    if resumed > 0 {
        println!("  documents resumed (already built): {resumed}");
    }
    if skipped > 0 {
        println!("  documents skipped (all furniture): {skipped}");
    }
    if failed > 0 {
        println!("  documents failed: {failed}");
    }
    if empty > 0 {
        println!("  documents with 0 nodes (summarizer FAILED — re-run to retry): {empty}");
    }
    println!(
        "  nodes persisted:  {nodes_total}  (conv_raptor_nodes, corpus_id='{}')",
        parsed.corpus_id
    );
    println!("  elapsed:          {:.1}s", elapsed.as_secs_f64());
    if built > 0 {
        println!("  avg per document: {:.1}s", elapsed.as_secs_f64() / built as f64);
    }
    println!("\nThe atom-graph atlas (atlas/atoms.json) is untouched — RAPTOR nodes are additive.");

    // Total failure (nothing built) is a non-zero exit; partial
    // failures are tolerated like the folder runner — one bad document
    // shouldn't sink a multi-day pass.
    if empty > 0 || (built == 0 && failed > 0) {
        return 1;
    }
    0
}

fn parse_args(args: &[String]) -> Result<RaptorArgs, String> {
    let mut corpus_id: Option<String> = None;
    let mut doc_type = DocumentTypeTag::Unknown;
    let mut limit: Option<usize> = None;
    let mut dry_run = false;
    let mut strip_furniture = false;
    let mut inspect_furniture = false;
    let mut force = false;
    let mut daemon_base = "http://localhost:9741".to_string();
    let mut chat_model = "primary".to_string();
    let mut embed_model = "embed".to_string();

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--doc-type" => {
                i += 1;
                let v = args.get(i).ok_or("--doc-type needs a value")?;
                doc_type = parse_doc_type(v)?;
            }
            "--limit" => {
                i += 1;
                let v = args.get(i).ok_or("--limit needs a value")?;
                limit = Some(
                    v.parse::<usize>()
                        .map_err(|_| format!("--limit: not a number: {v}"))?,
                );
            }
            "--dry-run" => dry_run = true,
            "--strip-furniture" => strip_furniture = true,
            "--inspect-furniture" => {
                inspect_furniture = true;
                strip_furniture = true;
            }
            "--force" => force = true,
            "--daemon" => {
                i += 1;
                daemon_base = args.get(i).ok_or("--daemon needs a value")?.clone();
            }
            "--chat-model" => {
                i += 1;
                chat_model = args.get(i).ok_or("--chat-model needs a value")?.clone();
            }
            "--embed-model" => {
                i += 1;
                embed_model = args.get(i).ok_or("--embed-model needs a value")?.clone();
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_some() {
                    return Err(format!("unexpected argument: {other}"));
                }
                corpus_id = Some(other.to_string());
            }
        }
        i += 1;
    }

    let corpus_id = corpus_id.ok_or("missing <corpus-id>")?;
    Ok(RaptorArgs {
        corpus_id,
        doc_type,
        limit,
        dry_run,
        strip_furniture,
        inspect_furniture,
        force,
        daemon_base,
        chat_model,
        embed_model,
    })
}

fn parse_doc_type(s: &str) -> Result<DocumentTypeTag, String> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "argument" => DocumentTypeTag::Argument,
        "narrative" => DocumentTypeTag::Narrative,
        "evidence" => DocumentTypeTag::Evidence,
        "chronicle" => DocumentTypeTag::Chronicle,
        "technical" => DocumentTypeTag::Technical,
        "unknown" | "document" => DocumentTypeTag::Unknown,
        other => {
            return Err(format!(
                "unknown --doc-type '{other}' (argument|narrative|evidence|chronicle|technical|unknown)"
            ))
        }
    })
}

/// SEP page-template furniture detector. SEP entries publish through a
/// fixed template whose copyright / contact / navigation blocks get
/// chunked alongside the entry prose. This matches those blocks so the
/// RAPTOR pass doesn't waste a summary on "this is metadata" (and so
/// furniture never pollutes a whole-document summary). Deliberately
/// CONSERVATIVE: it matches unambiguous template strings at the chunk
/// start, plus the SEP-wide copyright footer anywhere — so bibliography,
/// block quotes, and entry prose are never dropped.
fn is_sep_furniture(content: &str) -> bool {
    // The SEP-wide copyright footer can sit mid-chunk; match it anywhere.
    if content.contains("The Stanford Encyclopedia of Philosophy is copyright") {
        return true;
    }
    // Chunks are "<slug>\n\n<body>"; the per-entry furniture blocks lead
    // the body with a fixed template string.
    let body = content
        .split_once("\n\n")
        .map(|(_, rest)| rest)
        .unwrap_or(content)
        .trim_start();
    const START_MARKERS: &[&str] = &[
        "[Please contact the author",
        "Please contact the author",
        "Copyright ©",
        "Academic Tools",
        "Other Internet Resources",
        "Related Entries",
        "How to cite this entry",
        "Friends PDF Preview",
        "Author and Citation Info",
        "Preview the PDF version",
    ];
    START_MARKERS.iter().any(|m| body.starts_with(m))
}

fn print_usage() {
    eprintln!(
        "sovereign enrich raptor — retrofit an installed corpus with a per-document RAPTOR tier-3 summary tree."
    );
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("  sovereign enrich raptor <corpus-id> [flags]");
    eprintln!();
    eprintln!("FLAGS:");
    eprintln!("  --doc-type <tag>    Summary cue: argument|narrative|evidence|chronicle|technical|unknown");
    eprintln!("                      (default: unknown). SEP philosophy essays → argument (claim-level).");
    eprintln!("  --limit N           Build only the N smallest documents (by chunk count). Use for a spike.");
    eprintln!("  --strip-furniture   Drop SEP page-template chunks (copyright/contact/nav) before clustering.");
    eprintln!("  --inspect-furniture Show which chunks --strip-furniture would drop, then exit. Implies --strip-furniture.");
    eprintln!("  --dry-run           Print the dispatch plan and exit (no inference, no writes).");
    eprintln!("  --force             Rebuild every document, even ones already built (default: resume/skip them).");
    eprintln!("  --daemon <url>      Daemon base URL (default: http://localhost:9741).");
    eprintln!("  --chat-model <id>   Summarizer model id/alias (default: primary).");
    eprintln!("  --embed-model <id>  Embedding model id/alias for summary nodes (default: embed).");
    eprintln!();
    eprintln!("Additive: does NOT modify the corpus's atom-graph atlas (atlas/atoms.json).");
    eprintln!("Resumable: each document checkpoints under its index dir; re-runs skip completed trees.");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_corpus_and_defaults() {
        let p = parse_args(&sv(&["sep"])).unwrap();
        assert_eq!(p.corpus_id, "sep");
        assert_eq!(p.doc_type, DocumentTypeTag::Unknown);
        assert!(p.limit.is_none());
        assert!(!p.dry_run);
        assert!(!p.strip_furniture);
        assert!(!p.force);
        assert_eq!(p.chat_model, "primary");
        assert_eq!(p.embed_model, "embed");
        assert_eq!(p.daemon_base, "http://localhost:9741");
    }

    #[test]
    fn parses_all_flags() {
        let p = parse_args(&sv(&[
            "sep",
            "--doc-type",
            "argument",
            "--limit",
            "5",
            "--dry-run",
            "--strip-furniture",
            "--chat-model",
            "Qwen3.6-35B-A3B-MTP-UD-Q6_K",
            "--embed-model",
            "Qwen3-Embedding-0.6B-Q8_0",
        ]))
        .unwrap();
        assert_eq!(p.doc_type, DocumentTypeTag::Argument);
        assert_eq!(p.limit, Some(5));
        assert!(p.dry_run);
        assert!(p.strip_furniture);
        assert_eq!(p.chat_model, "Qwen3.6-35B-A3B-MTP-UD-Q6_K");
        assert_eq!(p.embed_model, "Qwen3-Embedding-0.6B-Q8_0");
    }

    #[test]
    fn doc_type_is_case_insensitive() {
        assert_eq!(parse_doc_type("Argument").unwrap(), DocumentTypeTag::Argument);
        assert_eq!(parse_doc_type("NARRATIVE").unwrap(), DocumentTypeTag::Narrative);
        assert_eq!(parse_doc_type("document").unwrap(), DocumentTypeTag::Unknown);
    }

    #[test]
    fn rejects_unknown_doc_type() {
        assert!(parse_doc_type("philosophy").is_err());
    }

    #[test]
    fn rejects_missing_corpus() {
        assert!(parse_args(&sv(&["--limit", "5"])).is_err());
    }

    #[test]
    fn rejects_unknown_flag() {
        assert!(parse_args(&sv(&["sep", "--bogus"])).is_err());
    }

    #[test]
    fn rejects_second_positional() {
        assert!(parse_args(&sv(&["sep", "wikipedia"])).is_err());
    }

    #[test]
    fn limit_requires_number() {
        assert!(parse_args(&sv(&["sep", "--limit", "lots"])).is_err());
    }

    #[test]
    fn parses_furniture_flags() {
        let p = parse_args(&sv(&["sep", "--strip-furniture"])).unwrap();
        assert!(p.strip_furniture);
        assert!(!p.inspect_furniture);
        let p2 = parse_args(&sv(&["sep", "--inspect-furniture"])).unwrap();
        assert!(p2.inspect_furniture);
        assert!(p2.strip_furniture, "inspect implies strip");
    }

    #[test]
    fn parses_force_flag() {
        assert!(parse_args(&sv(&["sep", "--force"])).unwrap().force);
        assert!(!parse_args(&sv(&["sep"])).unwrap().force);
    }

    #[test]
    fn furniture_detects_template_blocks() {
        assert!(is_sep_furniture(
            "david\n\n[Please contact the author with suggestions"
        ));
        assert!(is_sep_furniture(
            "holes\n\nCopyright © 2019 by   Roberto Casati <casati@ehess>"
        ));
        assert!(is_sep_furniture(
            "holes\n\nThe Stanford Encyclopedia of Philosophy is copyright © 2021 by The Metaphysics Research Lab"
        ));
        assert!(is_sep_furniture("x\n\nRelated Entries\n\natomism | Plato"));
    }

    #[test]
    fn furniture_keeps_real_content() {
        assert!(!is_sep_furniture(
            "leucippus\n\nLeucippus is recognized as the founder of ancient Greek atomism."
        ));
        // bibliography is content, not furniture
        assert!(!is_sep_furniture(
            "leucippus\n\nThe standard scholarly edition of the ancient reports concerning the Presocratics is Diels-Kranz' work (DK)."
        ));
        // a passing mention of copyright mid-sentence is not furniture
        assert!(!is_sep_furniture(
            "ip\n\nThe modern concept of copyright © emerged in the 18th century as a legal response to printing."
        ));
    }
}
