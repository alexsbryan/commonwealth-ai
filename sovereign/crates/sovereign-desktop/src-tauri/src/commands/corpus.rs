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

/// Build the IVF-PQ vector index for an installed corpus. `POST
/// /internal/corpus/{corpus}/index/build` since 2026-09-12, then a poll of
/// `GET …/index/progress` re-emitting the same three events the webview has
/// always listened for: `index-build-progress`, `index-build-complete`,
/// `index-build-error`.
///
/// The job, the 409-by-name on a second concurrent build, the
/// `(true, true)` flags that let the recipe decide FTS, and the
/// `vector_index_ready` flip afterwards are all the daemon's
/// (`corpus_catalog_http::index_build`). This command held a full copy of
/// that loop over its OWN `CorpusEngine` and state store until now — the
/// route landed at `f9ecf139f` and `TurnClient::corpus_index_build` with
/// it, and nothing ever called either. Two writers on one LanceDB index
/// was the thing that copy risked; there is one now.
#[tauri::command]
pub async fn build_corpus_index(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    use sovereign_contracts::daemon_wire::{IndexBuildProgress, IndexBuildState, IngestJobAck};

    let client = sovereign_turn_client::TurnClient::new(state.client_base_url());
    // A refused launch is the caller's error — 404 for a corpus that is not
    // installed, 409 for one already building, each naming itself. The old
    // in-process version reported both as an `index-build-error` event
    // AFTER returning Ok, which is a refusal dressed as a failed build.
    let ack: IngestJobAck = client
        .corpus_index_build(&corpus_id)
        .await
        .map_err(|e| format!("build_corpus_index `{corpus_id}`: {e}"))?;
    tracing::debug!(
        corpus_id = %corpus_id,
        job_id = %ack.job_id,
        "build_corpus_index: accepted by the daemon; polling",
    );

    tokio::spawn(async move {
        let emit_error = |error: String| {
            let _ = app_handle.emit(
                "index-build-error",
                serde_json::json!({"corpus_id": &corpus_id, "error": error}),
            );
        };
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let p: IndexBuildProgress = match client.corpus_index_progress(&corpus_id).await {
                Ok(p) => p,
                Err(e) => {
                    emit_error(format!("lost the daemon while polling: {e}"));
                    return;
                }
            };
            match p.state {
                IndexBuildState::Building => {
                    let _ = app_handle.emit(
                        "index-build-progress",
                        serde_json::json!({
                            "corpus_id": &corpus_id,
                            "phase": "building",
                            "pct": p.pct,
                        }),
                    );
                }
                IndexBuildState::Complete => {
                    let _ = app_handle.emit(
                        "index-build-complete",
                        serde_json::json!({"corpus_id": &corpus_id}),
                    );
                    return;
                }
                IndexBuildState::Error => {
                    // The daemon's own sentence, never a re-spelling of it.
                    // `Error` with no text is still an error — reported as
                    // one rather than read as a finished build.
                    emit_error(p.error.unwrap_or_else(|| {
                        // Not a default STANDING IN for the reason — it says
                        // the reason is missing, which is itself the fact.
                        format!(
                            "the daemon reported job {} as failed and gave no reason",
                            ack.job_id
                        )
                    }));
                    return;
                }
                // `Idle` means this daemon has no record of the job we were
                // just acked for — it restarted under us. Not a completion.
                IndexBuildState::Idle => {
                    emit_error(format!(
                        "the daemon no longer knows job {} — it restarted during the build",
                        ack.job_id
                    ));
                    return;
                }
            }
        }
    });

    Ok(())
}

/// The route's own answer shape, parsed and returned verbatim so the
/// webview sees the same bytes — ONE definition, the contracts one
/// (ARCH principle 8); the command's historical name kept for `main.rs`.
pub use sovereign_contracts::daemon_wire::IngestLegacyResponse as IngestDocumentResult;

/// The legacy paperclip path (ChatView's `handleLegacyAttach`): chunk a
/// file into the `documents` table. `POST /v1/documents/legacy` since
/// 2026-09-11 — the daemon's store is the one the legacy listing and
/// promotion read, and it embeds with the Runtime it answers turns with.
#[tauri::command]
pub async fn ingest_document(
    state: State<'_, Arc<AppState>>,
    file_path: String,
) -> Result<IngestDocumentResult, String> {
    let result = sovereign_turn_client::TurnClient::new(state.client_base_url())
        .ingest_legacy_document::<IngestDocumentResult>(&file_path)
        .await
        .map_err(|e| format!("Ingest failed: {e}"))?;
    tracing::info!(source = %result.source, chunks = result.chunks_created, "document ingested");
    Ok(result)
}
