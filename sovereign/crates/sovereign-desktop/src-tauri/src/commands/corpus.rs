//! Auto-split from the former monolithic `commands.rs` (PR5). Tauri
//! command handlers grouped by concern; re-exported through
//! `commands/mod.rs` so `commands::<name>` paths in `main.rs`'s
//! `generate_handler!` stay valid.
#![allow(unused_imports)]
use super::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::io::AsyncWriteExt;

use crate::state::{self, AppState, DesktopConfig};

// ─── Corpus Management ──────────────────────────────────────
//
// All corpus operations route through the shared `CorpusEngine` stored
// in `AppState::corpus_engine`. The catalog of available corpora comes
// from the `RecipeRegistry` bundled snapshot (registry_snapshot.toml),
// and installed state comes from `installed_indexes()` scanning
// `~/.sovereign/indexes`. The legacy `CorpusManager` /
// `CorpusRegistry` / `data/corpora.toml` path has been removed.

/// Map a `corpus_engine::IngestProgress` variant to a frontend-friendly
/// `CorpusProgressPayload`. Covers the full pipeline including the
/// optional enrichment phases so progress reporting doesn't go silent
/// during the (often long) claim/relationship extraction stages.
pub(crate) fn ingest_progress_to_payload(
    corpus_id: &str,
    progress: &corpus_engine::IngestProgress,
) -> CorpusProgressPayload {
    use corpus_engine::IngestProgress;
    match progress {
        IngestProgress::Downloading {
            percent,
            bytes_downloaded,
            ..
        } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            phase: "downloading".into(),
            percent: *percent,
            chunks_processed: 0,
            message: Some(format!("{:.1} MB", *bytes_downloaded as f64 / 1_048_576.0)),
        },
        IngestProgress::Extracting {
            documents_processed,
        } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            phase: "extracting".into(),
            percent: 0.0,
            chunks_processed: *documents_processed,
            message: Some(format!("{} documents", documents_processed)),
        },
        IngestProgress::Chunking { chunks_created } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            phase: "chunking".into(),
            percent: 0.0,
            chunks_processed: *chunks_created,
            message: None,
        },
        IngestProgress::Embedding {
            chunks_embedded,
            total,
            docs_processed,
            chunks_per_sec,
            expected_docs,
        } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            phase: "embedding".into(),
            // Live-event path has no shard-scan signal, so all we can
            // do is the legacy chunk-total ratio (0 until the pipeline
            // knows the chunk count). The polling path
            // (`status_entry_to_payload`) carries shard-scan progress
            // via `entry.estimated_fraction` and is the primary signal
            // the desktop banner consumes.
            //
            // We deliberately do NOT compute `docs_processed /
            // expected_docs` here, even with a clamp. For Wikipedia
            // JSONL one accepted article emits ~10× sections; the
            // ratio hits 100% within minutes of an embed run that has
            // hours left. The "X / Y articles" message below carries
            // the filter-scope context without lying about completion.
            percent: if *total > 0 {
                (*chunks_embedded as f32 / *total as f32) * 100.0
            } else {
                0.0
            },
            chunks_processed: *chunks_embedded,
            message: Some(format_embed_message(
                *chunks_embedded,
                *docs_processed,
                *chunks_per_sec,
                *expected_docs,
            )),
        },
        IngestProgress::Indexing {
            chunks_indexed,
            total,
        } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            phase: "indexing".into(),
            percent: if *total > 0 {
                (*chunks_indexed as f32 / *total as f32) * 100.0
            } else {
                0.0
            },
            chunks_processed: *chunks_indexed,
            message: None,
        },
        IngestProgress::OptimizingIndex { current_chunks } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            phase: "optimizing_index".into(),
            // The rebuild is one-shot — no incremental progress to
            // report. Surface it as in-flight (50%) so the banner's
            // bar doesn't snap from 100% (Indexing) back to 0% (no
            // bar) and disorient the user mid-expansion.
            percent: 50.0,
            chunks_processed: *current_chunks,
            message: Some(format!(
                "Retraining vector index over {} chunks",
                pretty_count(*current_chunks)
            )),
        },
        IngestProgress::Enriching {
            phase,
            detail,
            fraction,
        } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            // Prefix with `enriching_` so frontend selectors can
            // distinguish enrichment sub-phases from the embed/index
            // phases above. The full sub-phase token (e.g.
            // `skeleton-extraction`, `clustering`,
            // `cluster-labeling-complete`) is carried verbatim so
            // the UI can render specific copy without parsing
            // `detail`.
            phase: format!("enriching_{phase}"),
            // Fraction lands on the bar when the underlying phase
            // reports it (Phase 1b batch progress, clustering
            // milestones). Otherwise 0% — the UI is expected to
            // show a spinner-mode rendering when phase is
            // `enriching_*`.
            percent: fraction.map(|f| (f * 100.0).clamp(0.0, 100.0)).unwrap_or(0.0),
            chunks_processed: 0,
            message: Some(detail.clone()),
        },
        IngestProgress::Complete {
            total_chunks,
            duration_secs,
        } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            phase: "complete".into(),
            percent: 100.0,
            chunks_processed: *total_chunks,
            message: Some(format!("Done in {duration_secs}s")),
        },
    }
}

/// List all corpora available to the user — a union of:
/// - Built-in recipes (Wikipedia, SEP, …) from `corpus_engine::builtin_corpora()`
/// - Locally-installed indexes from `corpus_engine::installed_indexes()`
///
/// Built-in entries that are also installed get their `status` set to
/// "installed" with the live chunk count from the on-disk index.
#[tauri::command]
pub async fn list_corpora(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<CorpusEntry>, String> {
    let engine_guard = state.corpus_engine.read().await;
    let engine = match engine_guard.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Ok(Vec::new()),
    };
    drop(engine_guard);

    // Pull built-in catalog from registry snapshot (no network required).
    //
    // Layer/satellite corpora (those with a `parent_corpus_id` —
    // `wikipedia-simple`, `wikipedia-newsworthy`) are still returned
    // here. The desktop frontend hides them from the top-level picker
    // and re-renders them as toggleable layers under the parent's row.
    // Returning them in this list keeps the install/remove/progress
    // wiring uniform — every layer is still a real corpus with its own
    // id, status, and progress payload.
    let builtins: Vec<_> = engine.builtin_corpora().into_iter().collect();

    // Look up live install status. Failure here is non-fatal — we still
    // want to render the catalog so the user can choose what to install.
    let installed = engine.installed_indexes().await.unwrap_or_default();

    let installing = state.install_progress.read().await;

    // Snapshot vector index readiness from the store for all installed corpora.
    let store_guard = state.store.read().await;
    let store_opt = store_guard.as_ref().map(Arc::clone);
    drop(store_guard);

    let mut entries = Vec::new();
    for b in &builtins {
        let registry_entry = engine.registry().find_entry(&b.id);
        // `installed_indexes()` already filters partial/abandoned-shell
        // indices by `ingestion_in_progress=true` (a crashed install
        // never reaches `mark_ingestion_complete()`). Any entry that
        // survives that filter is semantically installed — including
        // watcher-driven corpora like `wikipedia-newsworthy` whose
        // steady state is chunk_count=0 until the first watcher tick.
        // Re-introducing a `chunk_count > 0` gate here would hide
        // those forever, leaving the layer chip stuck on "Add".
        let installed_info = installed
            .iter()
            .find(|i| i.corpus_id == b.id && !i.is_shard);
        let is_installing = installing
            .get(&b.id)
            .is_some_and(|p| p.phase != "complete" && p.phase != "failed");

        let status = if installed_info.is_some() {
            "installed"
        } else if is_installing {
            "installing"
        } else {
            "not_installed"
        };

        // `vector_index_ready` is what the UI uses to decide whether
        // to show "Build Index" or "Hybrid search ready". Two sources
        // of truth historically, easy to drift apart:
        //
        //   1. `_corpus_meta.json.vector_index_built` — written by the
        //      ingest pipeline when IVF-PQ actually finishes.
        //   2. The SQLite `vector_index_ready` flag — set ONLY by the
        //      explicit `build_corpus_index` Tauri command.
        //
        // A regular ingest that builds the index never writes (2), so
        // the UI shows "Keyword search only / Build Index" even though
        // the vector index is on disk and live. Trust the on-disk meta
        // first; fall back to the SQLite cache for installs that
        // happened before this field was populated.
        let vector_index_ready = if let Some(info) = installed_info {
            if info.vector_index_built {
                true
            } else if let Some(ref s) = store_opt {
                s.get_vector_index_ready(&b.id).await.unwrap_or(false)
            } else {
                false
            }
        } else {
            false
        };

        entries.push(CorpusEntry {
            id: b.id.clone(),
            name: b.name.clone(),
            description: b.description.clone(),
            size_compressed_gb: b.size_compressed_gb,
            size_indexed_gb: b.size_indexed_gb,
            license: b.license.clone(),
            tiers: tiers_for(&b.id),
            status: status.to_string(),
            chunks_count: installed_info.map(|i| i.chunk_count),
            enrichment_enabled: registry_entry.map(|e| e.enrichment_enabled).unwrap_or(false),
            indexed_at: installed_info.map(|i| i.created_at),
            embedding_model: installed_info.map(|i| i.embedding_model.clone()),
            embedding_dimensions: installed_info.map(|i| i.embedding_dimensions),
            vector_index_ready,
            registry_url: registry_entry.map(|e| e.toml_url.clone()),
            schema_version: Some(1),
            parent_corpus_id: b.parent_corpus_id.clone(),
            catalog_status: b.catalog_status.clone(),
        });
    }

    Ok(entries)
}

/// Build the IVF-PQ vector index for an installed corpus in the background.
/// Emits `index-build-progress`, `index-build-complete`, or `index-build-error`
/// events to the frontend. Sets `vector_index_ready` on the store when done.
#[tauri::command]
pub async fn build_corpus_index(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let engine = {
        let guard = state.corpus_engine.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Corpus engine not ready")?
    };
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };

    let cid = corpus_id.clone();
    tokio::spawn(async move {
        let indexes = match engine.installed_indexes().await {
            Ok(v) => v,
            Err(e) => {
                let _ = app_handle.emit(
                    "index-build-error",
                    serde_json::json!({"corpus_id": cid, "error": e.to_string()}),
                );
                return;
            }
        };
        let Some(info) = indexes.iter().find(|i| i.corpus_id == cid) else {
            let _ = app_handle.emit(
                "index-build-error",
                serde_json::json!({"corpus_id": cid, "error": "Corpus not found"}),
            );
            return;
        };
        let idx = match engine.open_index(&info.path).await {
            Ok(i) => i,
            Err(e) => {
                let _ = app_handle.emit(
                    "index-build-error",
                    serde_json::json!({"corpus_id": cid, "error": e.to_string()}),
                );
                return;
            }
        };

        let progress_handle = app_handle.clone();
        let progress_cid = cid.clone();
        let on_progress: Box<dyn Fn(u64, u64) + Send + Sync> = Box::new(move |done, total| {
            let pct = if total > 0 { done * 100 / total } else { 0 };
            let _ = progress_handle.emit(
                "index-build-progress",
                serde_json::json!({"corpus_id": &progress_cid, "phase": "building", "pct": pct}),
            );
        });

        // Build both vector and FTS indexes. The recipe controls which
        // are enabled; passing (true, true) lets the index builder respect
        // those flags rather than hardcoding FTS off (which would corrupt
        // the metadata by marking FTS as built without building it).
        match idx.build_indexes(true, true, Some(&*on_progress)).await {
            Ok(()) => {
                let _ = store.set_vector_index_ready(&cid, true).await;
                let _ = app_handle.emit(
                    "index-build-complete",
                    serde_json::json!({"corpus_id": cid}),
                );
            }
            Err(e) => {
                let _ = app_handle.emit(
                    "index-build-error",
                    serde_json::json!({"corpus_id": cid, "error": e.to_string()}),
                );
            }
        }
    });

    Ok(())
}

#[derive(serde::Serialize)]
pub struct IngestDocumentResult {
    pub source: String,
    pub chunks_created: usize,
}

#[tauri::command]
pub async fn ingest_document(
    state: State<'_, Arc<AppState>>,
    file_path: String,
) -> Result<IngestDocumentResult, String> {
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };
    let inference = {
        let guard = state.inference.read().await;
        guard.as_ref().map(Arc::clone)
    };

    let path = std::path::Path::new(&file_path);
    if !path.exists() {
        return Err(format!("File not found: {file_path}"));
    }

    let chunks_created = sovereign_tools::rag::ingest::ingest_file(
        path,
        store.as_ref(),
        inference.as_ref().map(|i| i.as_ref()),
    )
    .await
    .map_err(|e| format!("Ingest failed: {e}"))?;

    let source = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&file_path)
        .to_string();

    eprintln!("[ingest] {} -> {} chunks", source, chunks_created);

    Ok(IngestDocumentResult {
        source,
        chunks_created,
    })
}

