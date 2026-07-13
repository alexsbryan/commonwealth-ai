// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-split from the former monolithic `commands.rs` (PR5). Tauri
//! command handlers grouped by concern; re-exported through
//! `commands/mod.rs` so `commands::<name>` paths in `main.rs`'s
//! `generate_handler!` stay valid.
#![allow(unused_imports)]
use super::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::io::AsyncWriteExt;

use sovereign_tools::atlas_view::FileAtlasReader;
use sovereign_tools::local_corpus::{LocalCorpusConfig, LocalCorpusSourceType};

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
            percent: fraction
                .map(|f| (f * 100.0).clamp(0.0, 100.0))
                .unwrap_or(0.0),
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
pub async fn list_corpora(state: State<'_, Arc<AppState>>) -> Result<Vec<CorpusEntry>, String> {
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
            enrichment_enabled: registry_entry
                .map(|e| e.enrichment_enabled)
                .unwrap_or(false),
            indexed_at: installed_info.map(|i| i.created_at),
            embedding_model: installed_info.map(|i| i.embedding_model.clone()),
            embedding_dimensions: installed_info.map(|i| i.embedding_dimensions),
            vector_index_ready,
            needs_rebuild: installed_info.is_some_and(|i| !i.indexes_built),
            registry_url: registry_entry.map(|e| e.toml_url.clone()),
            schema_version: Some(1),
            parent_corpus_id: b.parent_corpus_id.clone(),
            catalog_status: b.catalog_status.clone(),
        });
    }

    // Local corpora — installed indexes that are NOT in the built-in
    // catalog (recipe-installed, snapshot-restored, or CLI-acquired; a
    // mesh app's `sf-assessor-roll` is the canonical case). This command's
    // contract (see the doc comment above) is a UNION of built-ins and
    // installed indexes. Emitting only built-ins made any such corpus
    // report as *missing* even when fully present on disk, so the
    // mesh-app "Get data" flow re-staged + re-installed it and its
    // completion poll — which waits for `status == "installed"` to appear
    // in this list — never matched, producing a silent ~15-minute hang.
    //
    // We tag these `catalog_status = "hidden"` so they satisfy the
    // installed-status checks (mesh-app readiness, the acquire poll)
    // without crowding the knowledge picker's "Coming soon" rail, which
    // renders the `preview` tier. `installed` is already fetched above —
    // no extra index scan.
    let builtin_ids: std::collections::HashSet<&str> =
        builtins.iter().map(|b| b.id.as_str()).collect();
    for info in &installed {
        if info.is_shard || builtin_ids.contains(info.corpus_id.as_str()) {
            continue;
        }
        entries.push(CorpusEntry {
            id: info.corpus_id.clone(),
            name: if info.corpus_name.is_empty() {
                info.corpus_id.clone()
            } else {
                info.corpus_name.clone()
            },
            description: String::new(),
            size_compressed_gb: 0.0,
            size_indexed_gb: info.index_size_bytes as f64 / 1e9,
            license: String::new(),
            tiers: tiers_for(&info.corpus_id),
            status: "installed".to_string(),
            chunks_count: Some(info.chunk_count),
            enrichment_enabled: info.enrichment_enabled,
            indexed_at: Some(info.created_at),
            embedding_model: Some(info.embedding_model.clone()),
            embedding_dimensions: Some(info.embedding_dimensions),
            vector_index_ready: info.vector_index_built,
            needs_rebuild: !info.indexes_built,
            registry_url: None,
            schema_version: Some(1),
            parent_corpus_id: info.parent_corpus_id.clone(),
            catalog_status: Some("hidden".to_string()),
        });
    }

    // Glassbox: make the union observable. A mesh app reporting its data
    // as "missing" when it is present on disk shows up here as a corpus
    // absent from `local_installed` — set `RUST_LOG=sovereign_desktop=debug`.
    tracing::debug!(
        builtins = builtins.len(),
        installed_on_disk = installed.len(),
        local_installed = entries.len().saturating_sub(builtins.len()),
        "list_corpora: catalog ∪ installed-local corpora",
    );

    Ok(entries)
}

/// Unified Library shelf listing — every *installed* corpus the user can
/// ask or explore, as one deduped [`NotebookSummary`] row.
///
/// Phase 1 of the UX refactor: the Library is the one knowledge home,
/// replacing four scattered listing surfaces (`list_corpora`, `lc_list`,
/// `enrich_list_corpora`, `atlas_list_corpora`). Rather than re-implement
/// their logic, this command *merges their underlying data sources* so
/// there is a single source of truth for "what notebooks do I have":
///
///   - [`installed_indexes()`](corpus_engine::CorpusEngine::installed_indexes)
///     is the deduped, shard-excluded record of what is on disk (id,
///     name, chunk count, freshness, parent). Layer/satellite children
///     (`parent_corpus_id` set) fold under their parent notebook, exactly
///     as the catalog picker hides them — so the shelf lists top-level
///     notebooks only.
///   - the `LocalCorpusManager` configs supply the source-kind
///     discriminator (folder / vault / watched), the user's chosen
///     display name, and scope for locally-ingested corpora.
///   - the atlas readers (`atoms.json` via `FileAtlasReader`, plus
///     conv-tiered enrichment in the SQLite store) decide `explorable`.
///
/// Every secondary lookup degrades gracefully: a missing local-corpus
/// manager, an absent atlas, or an uninitialised sqlite store narrows the
/// metadata for the affected rows rather than failing the whole listing.
#[tauri::command]
pub async fn notebook_list(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<NotebookSummary>, String> {
    let engine = match state.corpus_engine.read().await.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Ok(Vec::new()),
    };

    // The deduped, shard-excluded installed set is the spine of the list.
    let installed = engine.installed_indexes().await.unwrap_or_default();

    // Built-in catalog names, for classifying + naming catalog notebooks.
    let builtins = engine.builtin_corpora();
    let builtin_names: HashMap<&str, &str> = builtins
        .iter()
        .map(|b| (b.id.as_str(), b.name.as_str()))
        .collect();

    // Local-corpus configs: id → config. Clone the Arc and drop the guard
    // before awaiting so we never hold the lock across `list()`. A missing
    // manager (setup incomplete) just leaves rows unclassified-as-local —
    // they fall through to "catalog"/"installed".
    let local_mgr = state.local_corpus.read().await.as_ref().cloned();
    let local_configs: HashMap<String, LocalCorpusConfig> = match local_mgr {
        Some(mgr) => mgr
            .list()
            .await
            .into_iter()
            .map(|c| (c.id.clone(), c))
            .collect(),
        None => HashMap::new(),
    };

    // Explorable set — union of corpora with an `atoms.json` atlas and
    // corpora with conv-tiered enrichment. Both lookups are best-effort.
    let mut explorable: HashSet<String> = HashSet::new();
    let reader = FileAtlasReader::new(engine.index_dir().to_path_buf());
    if let Ok(atom_corpora) = reader.list_corpora().await {
        explorable.extend(atom_corpora.into_iter().map(|c| c.corpus_id));
    }
    let conv_store = state.sqlite_store.read().await.as_ref().map(Arc::clone);
    if let Some(store) = conv_store {
        if let Ok(buckets) = store.list_conv_corpora_with_state_buckets().await {
            explorable.extend(buckets.into_iter().map(|(corpus_id, ..)| corpus_id));
        }
    }

    // Open-conflict counts for governance corpora — those carrying a
    // `governance_oplog.jsonl`. Cheap gate: stat the oplog per corpus;
    // only for the (typically one) governance corpus do we load the view
    // to count open tensions. Best-effort and off the async executor; a
    // read failure just omits the count. A `Some(_)` here is what shows
    // the notebook's Conflicts tab — including `Some(0)` ("all clear").
    let open_conflicts: HashMap<String, u32> = {
        let index_dir = engine.index_dir().to_path_buf();
        let ids: Vec<String> = installed
            .iter()
            .filter(|i| !i.is_shard && i.parent_corpus_id.is_none())
            .map(|i| i.corpus_id.clone())
            .collect();
        tokio::task::spawn_blocking(move || {
            let mut counts = HashMap::new();
            for id in ids {
                let atlas = index_dir.join(&id).join("atlas");
                if !atlas.join("governance_oplog.jsonl").exists() {
                    continue;
                }
                if let Ok(view) = corpus_engine::enrichment::GovernanceView::from_atlas_dir(&atlas)
                {
                    counts.insert(id, view.open_tensions().count() as u32);
                }
            }
            counts
        })
        .await
        .unwrap_or_default()
    };

    let mut notebooks = Vec::new();
    for info in &installed {
        // Shards are storage internals; layer children belong to their
        // parent notebook (the catalog hides them under the parent row).
        if info.is_shard || info.parent_corpus_id.is_some() {
            continue;
        }

        let local = local_configs.get(&info.corpus_id);

        let source_kind = if let Some(cfg) = local {
            match &cfg.source_type {
                LocalCorpusSourceType::ObsidianVault { .. } => "obsidian",
                LocalCorpusSourceType::WatchedFolder(_) => "watched",
                LocalCorpusSourceType::DocumentFolder => "folder",
            }
        } else if builtin_names.contains_key(info.corpus_id.as_str()) {
            "catalog"
        } else {
            // Recipe-installed, CLI-acquired, snapshot-restored, mesh-app,
            // or conversation import — a real notebook with no local-folder
            // config behind it.
            "installed"
        };

        let name = if let Some(cfg) = local {
            cfg.display_name.clone()
        } else if let Some(n) = builtin_names.get(info.corpus_id.as_str()) {
            n.to_string()
        } else if !info.corpus_name.is_empty() {
            info.corpus_name.clone()
        } else {
            info.corpus_id.clone()
        };

        let scope = local
            .map(|c| c.scope.as_recipe_str().to_string())
            .unwrap_or_else(|| "local".to_string());

        notebooks.push(NotebookSummary {
            id: info.corpus_id.clone(),
            name,
            source_kind: source_kind.to_string(),
            doc_count: info.chunk_count,
            explorable: explorable.contains(&info.corpus_id),
            updated_unix: Some(info.created_at),
            scope,
            open_conflicts: open_conflicts.get(&info.corpus_id).copied(),
        });
    }

    // Most-recently-indexed first, then alphabetical — a stable, scannable
    // shelf order that floats fresh ingests to the top.
    notebooks.sort_by(|a, b| {
        b.updated_unix
            .cmp(&a.updated_unix)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    tracing::debug!(
        installed_on_disk = installed.len(),
        notebooks = notebooks.len(),
        local_configs = local_configs.len(),
        explorable = explorable.len(),
        "notebook_list: unified Library shelf",
    );

    Ok(notebooks)
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
        guard
            .as_ref()
            .map(Arc::clone)
            .ok_or("Corpus engine not ready")?
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

    tracing::info!(source = %source, chunks = chunks_created, "document ingested");

    Ok(IngestDocumentResult {
        source,
        chunks_created,
    })
}
