// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-split from the former monolithic `commands.rs` (PR5). Tauri
//! command handlers grouped by concern; re-exported through
//! `commands/mod.rs` so `commands::<name>` paths in `main.rs`'s
//! `generate_handler!` stay valid.
use super::*;
use std::sync::Arc;

use tauri::{Emitter, State};

use crate::state::AppState;

// ─── Corpus Management ──────────────────────────────────────
//
// All corpus operations route through the shared `CorpusEngine` stored
// in `AppState::corpus_engine`. The catalog of available corpora comes
// from the `RecipeRegistry` bundled snapshot (registry_snapshot.toml),
// and installed state comes from `installed_indexes()` scanning
// `~/.sovereign/indexes`. The legacy `CorpusManager` /
// `CorpusRegistry` / `data/corpora.toml` path has been removed.

/// The coverage card for an installed corpus whose recipe DECLARES a
/// typed authoritative store — FINANCIAL_CORPORA §7.7, bars F5 and F6.
///
/// `Ok(None)` for every corpus that declares none. Absence is reported,
/// never defaulted (ARCH §18.3): a corpus with no typed store gets no
/// card rather than an empty or invented one.
///
/// Loaded on demand, like `get_corpus_health`, so `notebook_list` keeps
/// its shape and stays fast — this adds a command, not a change to an
/// existing response.
///
/// Discovery and content both come from `corpus_engine`'s `sec_facts`
/// module: the SAME `authoritative_store` accessor the `sec_facts` tool
/// resolves through, and the SAME `coverage_card` derivation. One
/// implementation of each (ARCH §10.6), so the card cannot advertise a
/// corpus or a period the tool would refuse.
#[tauri::command]
pub async fn corpus_coverage_card(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Option<corpus_engine::enrichment::atlas::analysis::sec_facts::CoverageCard>, String> {
    // sv-surface D9b — `GET /internal/corpus/{corpus}/coverage-card`. The
    // card is derived from the SAME `authoritative_store` accessor the
    // `sec_facts` tool resolves through, and the tool runs on the serving
    // runtime. Deriving it here as well made two answers to "what periods
    // does this corpus authoritatively cover", one of which the tool would
    // then refuse.
    //
    // `Ok(None)` stays "this corpus has no authoritative store", the
    // route's own 200-with-null; a daemon that will not answer is an
    // `Err` (ARCH §18.3).
    sovereign_turn_client::TurnClient::new(state.client_base_url())
        .corpus_coverage_card::<corpus_engine::enrichment::atlas::analysis::sec_facts::CoverageCard>(
            &corpus_id,
        )
        .await
        .map_err(|e| e.to_string())
}

/// List all corpora available to the user — a union of:
/// - Built-in recipes (Wikipedia, SEP, …) from `corpus_engine::builtin_corpora()`
/// - Locally-installed indexes from `corpus_engine::installed_indexes()`
///
/// Built-in entries that are also installed get their `status` set to
/// "installed" with the live chunk count from the on-disk index.
#[tauri::command]
pub async fn list_corpora(state: State<'_, Arc<AppState>>) -> Result<Vec<CorpusEntry>, String> {
    // sv-surface D9b — `GET /internal/corpus/catalog`. The whole union
    // (built-in catalogue ∪ every installed index it does not name, the
    // `catalog_status: "hidden"` tag, the two-source `vector_index_ready`
    // rule, `tiers_for`) moved down with the route in 2a9a9e91e; the
    // ~170 lines that ran it here were the second copy.
    //
    // The `None => Ok(Vec::new())` this replaces was the sharpest of the
    // three no-fork degradations the D9 correction named: an attached
    // desktop whose `corpus_engine` had not opened rendered an EMPTY
    // picker and called it success. A daemon that will not answer is now
    // an `Err` with the daemon's own words (ARCH §18.3).
    //
    // `"installing"` is composed HERE and only here: the daemon's
    // catalogue answers `installed` / `not_installed`, and this process's
    // `install_progress` register is the in-flight fact (rung 1's
    // `/internal/corpus/status` is the daemon's own; overlaying the local
    // register keeps the picker's optimistic transition for installs THIS
    // window started).
    let mut entries: Vec<CorpusEntry> =
        sovereign_turn_client::TurnClient::new(state.client_base_url())
            .corpus_catalog::<CorpusEntry>()
            .await
            .map_err(|e| format!("list_corpora: {e}"))?;

    let installing = state.install_progress.read().await;
    let mut overlaid = 0usize;
    for entry in &mut entries {
        if entry.status == "not_installed"
            && installing
                .get(&entry.id)
                .is_some_and(|p| p.phase != "complete" && p.phase != "failed")
        {
            entry.status = "installing".to_string();
            overlaid += 1;
        }
    }
    drop(installing);

    // Glassbox: the union is the daemon's now, so what is observable here
    // is the overlay — how many rows this window's own in-flight register
    // moved off `not_installed`.
    tracing::debug!(
        rows = entries.len(),
        installing_overlaid = overlaid,
        "list_corpora: daemon catalogue + local in-flight overlay",
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
///   - the local-corpus REGISTRY (`GET /internal/corpus/local`, over the
///     daemon's own manager since sv-surface D8) supplies the source-kind
///     discriminator (folder / vault / watched), the user's chosen
///     display name, and scope for locally-ingested corpora.
///   - the atlas readers (`atoms.json` via `FileAtlasReader`, plus
///     conv-tiered enrichment in the SQLite store) decide `explorable`.
///
/// The atlas and sqlite lookups degrade gracefully: an absent atlas or an
/// uninitialised store narrows the metadata for the affected rows rather
/// than failing the listing. The registry read does NOT — see its comment
/// below.
#[tauri::command]
pub async fn notebook_list(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<NotebookSummary>, String> {
    // sv-surface D9b — `GET /internal/corpus/notebooks`. The five-source
    // fold this replaces (installed indexes, the local-corpus registry,
    // the file atlas, conv-tiered enrichment, the governance oplog), its
    // four naming rules, the collection-parent prefix walk and the sort
    // ALL moved down with the route in 2a9a9e91e — serving only the
    // inputs would have kept the copy here and added five round trips.
    //
    // The `None => Ok(Vec::new())` this replaces was the second silent
    // degradation the D9 correction named: an attached desktop rendered an
    // EMPTY Library shelf and reported success. A daemon that will not
    // answer is an `Err` now.
    let notebooks = sovereign_turn_client::TurnClient::new(state.client_base_url())
        .corpus_notebooks::<NotebookSummary>()
        .await
        .map_err(|e| format!("notebook_list: {e}"))?;

    tracing::debug!(
        notebooks = notebooks.len(),
        "notebook_list: unified Library shelf, served",
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
