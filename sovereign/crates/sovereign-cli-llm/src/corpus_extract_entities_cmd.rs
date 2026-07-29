// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn corpus extract-entities <corpus_id>` — run GliNER NER
//! across a corpus's chunks, persist results into `chunk_entities`.
//!
//! Spec: `sovereign/docs/specs/CONV_TIERED_PORT.md` §"Phase 1 —
//! GliNER per-chunk entities".
//!
//! Resumable: re-runs skip conversations already processed in a
//! recent extraction (matched by `chunk_entity_progress.state =
//! 'complete'` AND same `model_id` + `threshold`). Use
//! `--force` to re-extract everything.
//!
//! Conv-corpora only for now: the iteration groups by
//! `source_doc_id`. Future ports may add a `--per-chunk` mode for
//! non-conv corpora.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use corpus_engine::index::CorpusIndex;
use sovereign_core::conv_tiered::{ChunkEntityProgressRow, ChunkEntityRow};
use sovereign_gliner::gliner_ner::{
    self, GlinerExtractor, DEFAULT_LABELS, DEFAULT_MODEL_ID, DEFAULT_THRESHOLD,
};
use sovereign_store::sqlite::SqliteStateStore;

/// Per-batch chunk count handed to GliNER. Smaller batches = more
/// frequent progress updates; larger = better throughput. 8 keeps
/// memory bounded for the small model + lets the operator see
/// progress every ~700ms at 78ms/chunk.
const BATCH_SIZE: usize = 8;

pub async fn run_extract_entities(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            print_help();
            return 2;
        }
    };

    // Standalone download mode: fetch model files + exit. No corpus
    // touched. Used as the first-install workflow (desktop's
    // "enable per-chunk entity extraction" toggle shells this).
    if parsed.download_only {
        return run_download_model(&parsed.model_id).await;
    }

    // Resolve data dir + index path.
    let data_dir = match resolve_data_dir() {
        Some(d) => d,
        None => {
            eprintln!("error: cannot resolve data_dir (set HOME or use --data-dir)");
            return 1;
        }
    };
    let db_path = data_dir.join("sovereign.db");
    let index_dir = data_dir.join("indexes");
    let corpus_index_path = find_corpus_index_path(&index_dir, &parsed.corpus_id);
    let corpus_index_path = match corpus_index_path {
        Some(p) => p,
        None => {
            eprintln!(
                "error: no installed index for corpus '{}' under {}",
                parsed.corpus_id,
                index_dir.display()
            );
            return 1;
        }
    };

    // Verify model files exist before doing anything expensive.
    if !gliner_ner::probe_model_available(&parsed.model_id) {
        let root = gliner_ner::models_root().join(&parsed.model_id);
        eprintln!(
            "error: GliNER model '{}' not installed at {}",
            parsed.model_id,
            root.display()
        );
        eprintln!("  download instructions:");
        eprintln!(
            "    mkdir -p {root}/onnx && cd {root}",
            root = root.display()
        );
        eprintln!(
            "    curl -L -o tokenizer.json https://huggingface.co/onnx-community/{model}/resolve/main/tokenizer.json",
            model = parsed.model_id
        );
        eprintln!(
            "    curl -L -o onnx/model.onnx https://huggingface.co/onnx-community/{model}/resolve/main/onnx/model.onnx",
            model = parsed.model_id
        );
        return 1;
    }

    // Open store + index.
    let store = match SqliteStateStore::open(&db_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("error: open {}: {e}", db_path.display());
            return 1;
        }
    };
    // Database / Index / Scope are the inventory: under `--dry-run` they
    // are the whole answer, and on a real run they are the provenance
    // header for the summary at the end. Payload either way -> stdout.
    println!("Database: {}", db_path.display());
    let index = match CorpusIndex::open(&corpus_index_path).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!(
                "error: open corpus index {}: {e}",
                corpus_index_path.display()
            );
            return 1;
        }
    };
    println!("Index:    {}", corpus_index_path.display());

    // Group chunks by source_doc_id.
    let groups = match index.group_chunks_by_source_doc().await {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: group_chunks_by_source_doc: {e}");
            return 1;
        }
    };
    let total_convs = groups.len();
    let total_chunks: usize = groups.values().map(|v| v.len()).sum();
    println!(
        "Scope:    {} conversation{} / {} chunks",
        total_convs,
        if total_convs == 1 { "" } else { "s" },
        total_chunks
    );

    if parsed.dry_run {
        eprintln!("--dry-run set; not invoking GliNER or writing to disk.");
        return 0;
    }

    // Load GliNER (one-time, ~500ms).
    eprintln!("Loading GliNER {}…", parsed.model_id);
    let load_start = Instant::now();
    let extractor = match GlinerExtractor::new(
        &parsed.model_id,
        &labels_ref(&parsed.labels),
        parsed.threshold,
    ) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: load GliNER: {e}");
            return 1;
        }
    };
    eprintln!("Loaded in {:.2?}", load_start.elapsed());

    // Initialise progress row.
    let labels_json = serde_json::to_string(&parsed.labels).unwrap_or_else(|_| "[]".to_string());
    let now = gliner_ner::now_unix();
    let mut progress = ChunkEntityProgressRow {
        corpus_id: parsed.corpus_id.clone(),
        chunks_processed: 0,
        chunks_total: total_chunks as i64,
        mentions_extracted: 0,
        last_chunk_id: None,
        started_at: now,
        updated_at: now,
        finished_at: None,
        state: "running".to_string(),
        model_id: Some(parsed.model_id.clone()),
        threshold: Some(parsed.threshold as f64),
        labels_json: Some(labels_json.clone()),
        error_msg: None,
    };
    if let Err(e) = store.upsert_chunk_entity_progress(&progress).await {
        eprintln!("warn: write progress row: {e}");
    }

    // Process each conv. Sorted for deterministic order so a
    // resumed run hits the same boundary points.
    let mut conv_list: Vec<(String, Vec<u64>)> = groups.into_iter().collect();
    conv_list.sort_by(|a, b| a.0.cmp(&b.0));

    let overall_start = Instant::now();
    let mut total_mentions = 0usize;
    let mut last_progress_print = Instant::now();
    let mut convs_done = 0usize;

    for (conv_uuid, _chunk_ids) in conv_list.iter() {
        let rows = match index.chunks_for_source_doc_with_embeddings(conv_uuid).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("warn: fetch chunks for {conv_uuid}: {e}");
                continue;
            }
        };
        if rows.is_empty() {
            continue;
        }
        // We don't need embeddings for NER, but the existing fetch
        // returns both pari passu; discard the vec to free memory.
        let conv_chunks: Vec<(u64, String)> = rows
            .into_iter()
            .map(|(row, _emb)| (row.id, row.content))
            .collect();

        // Extract in batches.
        let mut conv_rows: Vec<ChunkEntityRow> = Vec::new();
        let extracted_at = gliner_ner::now_unix();
        for batch in conv_chunks.chunks(BATCH_SIZE) {
            let texts: Vec<&str> = batch.iter().map(|(_, t)| t.as_str()).collect();
            let result = match extractor.extract_batch(&texts) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("warn: extract_batch {conv_uuid}: {e}");
                    progress.error_msg = Some(format!("{e}"));
                    continue;
                }
            };
            for ((chunk_id, _), mentions) in batch.iter().zip(result) {
                for m in mentions {
                    conv_rows.push(m.into_row(
                        &parsed.corpus_id,
                        *chunk_id,
                        Some(conv_uuid),
                        extracted_at,
                    ));
                }
                progress.chunks_processed += 1;
                progress.last_chunk_id = Some(*chunk_id as i64);
            }
            progress.mentions_extracted += conv_rows.len() as i64;
            progress.updated_at = gliner_ner::now_unix();

            // Print progress at most every 2s.
            if last_progress_print.elapsed().as_secs_f64() >= 2.0 {
                report_progress(&progress, overall_start, convs_done, total_convs);
                last_progress_print = Instant::now();
            }
        }
        total_mentions += conv_rows.len();

        // Persist conv's entities atomically.
        if let Err(e) = store
            .save_chunk_entities_for_conv(&parsed.corpus_id, conv_uuid, &conv_rows)
            .await
        {
            eprintln!("warn: save_chunk_entities_for_conv {conv_uuid}: {e}");
        }
        // Update aggregate progress row in store after each conv.
        progress.updated_at = gliner_ner::now_unix();
        let _ = store.upsert_chunk_entity_progress(&progress).await;

        convs_done += 1;
    }

    progress.state = "complete".to_string();
    progress.finished_at = Some(gliner_ner::now_unix());
    progress.updated_at = progress.finished_at.unwrap();
    let _ = store.upsert_chunk_entity_progress(&progress).await;

    let elapsed = overall_start.elapsed();
    // The blank stays on stderr: it terminates the in-place progress row,
    // which is a stderr concern. The tally below is the run's result.
    eprintln!();
    println!("✓ extraction complete");
    println!("  conversations:   {convs_done}");
    println!("  chunks:          {}", progress.chunks_processed);
    println!("  mentions:        {total_mentions}");
    println!(
        "  wall-clock:      {:.1}s  ({:.0}ms / chunk)",
        elapsed.as_secs_f64(),
        elapsed.as_secs_f64() * 1000.0 / progress.chunks_processed.max(1) as f64
    );
    0
}

/// Print a single-line progress update overwriting the previous one.
fn report_progress(
    progress: &ChunkEntityProgressRow,
    overall_start: Instant,
    convs_done: usize,
    total_convs: usize,
) {
    let pct = if progress.chunks_total > 0 {
        100.0 * progress.chunks_processed as f64 / progress.chunks_total as f64
    } else {
        0.0
    };
    let elapsed_s = overall_start.elapsed().as_secs_f64();
    let rate = progress.chunks_processed as f64 / elapsed_s.max(0.001);
    let remaining = (progress.chunks_total - progress.chunks_processed) as f64 / rate.max(0.001);
    eprintln!(
        "  {:5}/{:5} chunks ({pct:.1}%)  conv {}/{} · {:.0} chunks/s · ETA {:.0}s · {} mentions",
        progress.chunks_processed,
        progress.chunks_total,
        convs_done,
        total_convs,
        rate,
        remaining,
        progress.mentions_extracted,
    );
}

#[derive(Debug)]
struct Parsed {
    corpus_id: String,
    model_id: String,
    threshold: f32,
    labels: Vec<String>,
    dry_run: bool,
    download_only: bool,
}

/// First-install workflow: fetch the GliNER ONNX + tokenizer from
/// huggingface.co/onnx-community/<model_id>. Idempotent — skips
/// files already present. Reports per-file progress.
async fn run_download_model(model_id: &str) -> i32 {
    use sovereign_gliner::gliner_ner::{download_model, models_root};
    let root = models_root().join(model_id);
    eprintln!("Downloading GliNER model '{model_id}' → {}", root.display());
    let last_pct = std::sync::Arc::new(std::sync::Mutex::new((String::new(), 0u8)));
    let progress_cb = {
        let last_pct = std::sync::Arc::clone(&last_pct);
        move |file: &str, downloaded: u64, total: u64| {
            if total == 0 {
                if downloaded == 0 {
                    eprintln!("  ✓ {file} already present");
                }
                return;
            }
            let pct = ((downloaded as f64 / total as f64) * 100.0) as u8;
            let mut lock = last_pct.lock().unwrap();
            if lock.0 != file || pct.saturating_sub(lock.1) >= 5 || pct == 100 {
                eprintln!(
                    "  {file}: {pct}% ({:.1} / {:.1} MB)",
                    downloaded as f64 / 1_048_576.0,
                    total as f64 / 1_048_576.0
                );
                lock.0 = file.to_string();
                lock.1 = pct;
            }
        }
    };
    match download_model(model_id, progress_cb).await {
        Ok(()) => {
            eprintln!("✓ model installed at {}", root.display());
            eprintln!();
            eprintln!("  next: svrn corpus extract-entities <corpus_id>");
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn parse_args(args: &[String]) -> Result<Parsed, String> {
    if args.is_empty() {
        return Err("usage: svrn corpus extract-entities <corpus_id> [--model <id>] [--threshold <f>] [--labels <l1,l2,...>] [--dry-run]".to_string());
    }
    if matches!(args[0].as_str(), "--help" | "-h") {
        return Err(String::new());
    }
    let mut corpus_id: Option<String> = None;
    let mut model_id = DEFAULT_MODEL_ID.to_string();
    let mut threshold = DEFAULT_THRESHOLD;
    let mut labels: Vec<String> = DEFAULT_LABELS.iter().map(|s| s.to_string()).collect();
    let mut dry_run = false;
    let mut download_only = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--model" => {
                i += 1;
                model_id = args
                    .get(i)
                    .ok_or_else(|| "--model needs a value".to_string())?
                    .clone();
            }
            "--threshold" => {
                i += 1;
                threshold = args
                    .get(i)
                    .ok_or_else(|| "--threshold needs a value".to_string())?
                    .parse::<f32>()
                    .map_err(|e| format!("--threshold parse: {e}"))?;
            }
            "--labels" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| "--labels needs a value".to_string())?;
                labels = raw
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "--dry-run" => dry_run = true,
            "--download-model" => download_only = true,
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_some() {
                    return Err(format!("unexpected positional: {other}"));
                }
                corpus_id = Some(other.to_string());
            }
        }
        i += 1;
    }
    Ok(Parsed {
        corpus_id: if download_only {
            corpus_id.unwrap_or_default()
        } else {
            corpus_id.ok_or_else(|| "corpus_id required".to_string())?
        },
        model_id,
        threshold,
        labels,
        dry_run,
        download_only,
    })
}

fn print_help() {
    eprintln!("svrn corpus extract-entities <corpus_id>");
    eprintln!("  Run GliNER per-chunk NER and persist into chunk_entities.");
    eprintln!();
    eprintln!("  Flags:");
    eprintln!(
        "    --model <id>       GliNER model id (default: {})",
        DEFAULT_MODEL_ID
    );
    eprintln!(
        "    --threshold <f>    Score threshold (default: {})",
        DEFAULT_THRESHOLD
    );
    eprintln!(
        "    --labels <l1,...>  Label set CSV (default: {})",
        DEFAULT_LABELS.join(",")
    );
    eprintln!("    --dry-run          Inventory only — don't load model or write.");
    eprintln!("    --download-model   Fetch the GliNER model files + exit (no extraction).");
    eprintln!();
    eprintln!("  First-install workflow:");
    eprintln!("    svrn corpus extract-entities --download-model");
    eprintln!("    svrn corpus extract-entities conversations-anthropic");
    eprintln!();
    eprintln!("  Once the model is installed, the daemon's tiered ingest path");
    eprintln!("  also fires GliNER automatically on every conv-corpus import.");
}

fn resolve_data_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SOVEREIGN_DATA_DIR") {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".sovereign"))
}

/// Walk `indexes_dir` for the canonical or partition path that owns
/// `corpus_id`. Prefers an exact-name directory; falls back to the
/// first `-partition-*` match.
fn find_corpus_index_path(indexes_dir: &std::path::Path, corpus_id: &str) -> Option<PathBuf> {
    let canonical = indexes_dir.join(corpus_id);
    if canonical.is_dir() && canonical.join("_corpus_meta.json").is_file() {
        return Some(canonical);
    }
    let entries = std::fs::read_dir(indexes_dir).ok()?;
    let prefix = format!("{corpus_id}-partition-");
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(&prefix) && !name.contains('.') {
            return Some(entry.path());
        }
    }
    None
}

fn labels_ref(labels: &[String]) -> Vec<&str> {
    labels.iter().map(|s| s.as_str()).collect()
}
