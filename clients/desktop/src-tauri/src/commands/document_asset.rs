// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-split from the former monolithic `commands.rs` (PR5). Tauri
//! command handlers grouped by concern; re-exported through
//! `commands/mod.rs` so `commands::<name>` paths in `main.rs`'s
//! `generate_handler!` stay valid.
use std::sync::Arc;

use serde::Serialize;
use tauri::{Emitter, State};

use crate::state::AppState;

// ─── Document Asset commands ─────────────────────────────────

#[derive(Serialize)]
pub struct DocumentAssetResponse {
    pub asset: sovereign_contracts::types::DocumentAsset,
}

#[derive(Serialize)]
pub struct DocumentAskResponse {
    pub response: String,
    /// The document operation used to answer, when the document was involved.
    /// `None` when the question was off-topic and the runtime's normal
    /// conversation pipeline answered it instead (no operation badge shown).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<sovereign_contracts::types::DocumentAssetOperation>,
    pub sources: Vec<String>,
    /// The PERSISTED assistant-message metadata, returned verbatim so the
    /// live bubble renders identically to a reload from the store —
    /// provenance + retrieved_chunks on the document-op path, and (via the
    /// runtime fallback) `grounding_gate` for the verification receipt.
    /// Dropping this at the Tauri boundary was why live attached-doc
    /// bubbles lacked the routing-meta bar their reloaded twins had.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// sv-surface D7/G9 — the TYPED projection of `metadata`, carried
    /// BESIDE it rather than instead of it. `metadata` above stays the
    /// verbatim blob because the frontend types it as `unknown` and
    /// reads it with pointers; retyping that contract is a later rung.
    /// These two are what a wire-attached client receives on a
    /// `TurnFrame::Complete`, so anything built on them — the answer
    /// export, the routing footer — renders identically in both boot
    /// modes. Absent when the blob carries no provenance (the
    /// documented graceful-degradation contract).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<sovereign_contracts::types::projection::Provenance>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<sovereign_contracts::types::projection::Citation>,
}

/// Upload and ingest a document. The command returns immediately with
/// a Pending asset. The full ingest pipeline (embed + skeleton) runs on
/// the DAEMON as a job — `POST /v1/documents` (2026-09-11) — and this
/// command follows it over `GET /v1/documents/{id}/progress`, re-emitting
/// every frame as `document:progress`. The frontend shows these via the
/// IngestBanner / DocOpProgress indicator, unchanged: the frames are the
/// manager's own `IngestProgress` values with `asset_id` stamped on
/// (the route stamps it now; this command used to, per event), so the
/// bytes on the event are the ones the banner already keys on.
///
/// The asset returned is the one `prepare` minted on the daemon, and it
/// is the id every frame carries — the banner the UI shows and the
/// events it receives agree on one id, which is the contract the old
/// in-process split existed to keep.
#[tauri::command]
pub async fn upload_document_asset(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    file_path: String,
) -> Result<DocumentAssetResponse, String> {
    let base_url = state.client_base_url();
    let asset = sovereign_turn_client::TurnClient::new(base_url.clone())
        .upload_document::<sovereign_contracts::types::DocumentAsset>(&file_path)
        .await
        .map_err(|e| format!("Upload failed: {e}"))?;
    tracing::info!(
        asset_id = %asset.id,
        filename = %asset.filename,
        "upload_document_asset: daemon accepted the ingest job"
    );
    tokio::spawn(follow_document_ingest(
        base_url,
        asset.id.clone(),
        app_handle.clone(),
    ));
    Ok(DocumentAssetResponse { asset })
}

/// Follow a daemon-side document ingest, emitting each of the host's
/// frames as `document:progress`. Same cadence and give-up rule as the
/// local-corpus jobs (`INGEST_POLL_INTERVAL` / `INGEST_POLL_MAX_FAILURES`
/// — one decider). Losing the daemon mid-ingest emits a `Failed` frame
/// naming it, so the banner does not sit on "estimating…" forever.
async fn follow_document_ingest(base_url: String, asset_id: String, app: tauri::AppHandle) {
    use crate::local_corpus_commands::{INGEST_POLL_INTERVAL, INGEST_POLL_MAX_FAILURES};
    let client = sovereign_turn_client::TurnClient::new(base_url);
    let mut failures = 0u32;
    let mut cursor = 0usize;
    loop {
        tokio::time::sleep(INGEST_POLL_INTERVAL).await;
        let p = match client
            .document_ingest_progress::<sovereign_contracts::daemon_wire::DocumentIngestProgress>(
                &asset_id, cursor,
            )
            .await
        {
            Ok(p) => {
                failures = 0;
                p
            }
            Err(e) => {
                failures += 1;
                tracing::warn!(
                    %asset_id, failures,
                    "upload_document_asset: progress poll failed: {e}"
                );
                if failures >= INGEST_POLL_MAX_FAILURES {
                    let _ = app.emit(
                        "document:progress",
                        &serde_json::json!({
                            "type": "Failed",
                            "asset_id": asset_id,
                            "reason": format!(
                                "lost contact with the daemon while ingesting \
                                 ({failures} consecutive failures): {e}"
                            ),
                        }),
                    );
                    return;
                }
                continue;
            }
        };
        cursor = p.next;
        for frame in p.frames {
            let _ = app.emit("document:progress", &frame);
        }
        if p.finished {
            tracing::info!(%asset_id, "upload_document_asset: ingest job finished");
            return;
        }
    }
}

/// Ask a question of an attached document. Since 2026-09-11 the
/// document-operation half is the daemon's JOB — `POST
/// /v1/documents/{id}/ask` persists the question and runs route +
/// execute + persist on the manager that holds the chunks; this command
/// follows it over `GET /v1/documents/{id}/ask/{job_id}` and re-emits the
/// host's `OperationProgress` frames as `document:operation`, verbatim.
/// The two fall-throughs (off-topic, empty RAG) come back as an outcome
/// and run the ordinary turn over the wire exactly as before.
///
/// `DocumentAskResponse` is built from the PERSISTED assistant message
/// the outcome carries, so the live bubble renders identically to a
/// reload — the contract the `metadata` field documents.
#[tauri::command]
pub async fn ask_document(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    asset_id: String,
    question: String,
    conversation_id: String,
) -> Result<DocumentAskResponse, String> {
    let base_url = state.client_base_url();
    let client = sovereign_turn_client::TurnClient::new(base_url.clone());

    let asset = client
        .get_document::<sovereign_contracts::types::DocumentAsset>(&asset_id)
        .await
        .map_err(|e| format!("Load failed: {e}"))?
        .ok_or("Document not found")?;

    if !asset.state.is_queryable() {
        return Err(format!(
            "Document is not ready for queries (state: {})",
            asset.state.label()
        ));
    }

    // Self-heal: if the skeleton never persisted (common when ingest was
    // interrupted — app quit mid-build, backend crash, etc.), kick off a
    // rebuild in the background. The current turn still proceeds with the
    // skeleton-less asset (routing will be slightly less accurate); every
    // subsequent turn benefits from the rebuilt skeleton.
    //
    // The rebuild is `POST /v1/documents/{id}/skeleton` — the same route
    // the user-initiated `rebuild_document_skeleton` command below takes,
    // for the reason its comment gives: the manager that matters is the
    // one on the store the chunks are in, with the daemon's own NER lane.
    if asset.skeleton.is_none() {
        tracing::info!(
            asset_id = %asset_id,
            "ask_document: skeleton missing — spawning background rebuild over the wire"
        );
        let heal_url = base_url.clone();
        let aid = asset_id.clone();
        let app = app_handle.clone();
        tokio::spawn(async move {
            match sovereign_turn_client::TurnClient::new(heal_url)
                .rebuild_document_skeleton::<sovereign_contracts::types::DocumentAsset>(&aid)
                .await
            {
                Ok(refreshed) => {
                    tracing::info!(
                        asset_id = %aid,
                        entities = refreshed
                            .skeleton
                            .as_ref()
                            .map(|s| s.main_entities.len())
                            .unwrap_or(0),
                        sections = refreshed
                            .skeleton
                            .as_ref()
                            .map(|s| s.sections.len())
                            .unwrap_or(0),
                        "auto-heal: skeleton rebuilt"
                    );
                    let _ = app.emit("document:skeleton_rebuilt", &aid);
                }
                Err(e) => {
                    tracing::warn!(
                        asset_id = %aid,
                        error = %e,
                        "auto-heal: skeleton rebuild failed"
                    );
                }
            }
        });
    }

    // The job: the daemon persists the question (tagged with the asset id)
    // before it answers 202, then routes + executes on its own manager.
    let ack = client
        .ask_document::<sovereign_contracts::daemon_wire::AskJobAck>(
            &asset_id,
            &question,
            &conversation_id,
        )
        .await
        .map_err(|e| format!("Routing failed: {e}"))?;
    tracing::info!(
        asset_id = %asset_id,
        job_id = %ack.job_id,
        "ask_document: daemon accepted the ask job"
    );

    let outcome = follow_ask_job(&client, &asset_id, &ack.job_id, &app_handle).await?;
    use sovereign_contracts::daemon_wire::AskOutcome;
    match outcome {
        AskOutcome::Answered {
            operation,
            message,
            sources,
        } => {
            tracing::info!(
                asset_id = %asset_id,
                operation = %operation.label(),
                "ask_document: answered by the document"
            );
            let _ = app_handle.emit("conversations:changed", ());
            let metadata = message.metadata.clone();
            let (provenance, citations) =
                sovereign_contracts::types::projection::project_message_metadata(&metadata);
            Ok(DocumentAskResponse {
                response: message.content,
                operation: Some(operation),
                sources,
                metadata,
                provenance,
                citations,
            })
        }
        // When the question isn't about the document (or RAG found nothing),
        // hand it to the normal conversation pipeline. The user message is
        // already in the conversation, tagged with the asset id.
        AskOutcome::FellThrough { operation, reason } => {
            tracing::info!(
                asset_id = %asset_id,
                operation = %operation.label(),
                reason,
                "ask_document: falling back to the runtime turn"
            );
            run_turn_via_runtime(&app_handle, &state, &question, &conversation_id).await
        }
        AskOutcome::Failed { error } => Err(error),
    }
}

/// Follow an ask job to its outcome, re-emitting each of the host's
/// `OperationProgress` frames as `document:operation`. Same cadence and
/// give-up rule as the ingest follows (one decider); the job is short,
/// so the cadence is also the upper bound on how late the answer lands.
async fn follow_ask_job(
    client: &sovereign_turn_client::TurnClient,
    asset_id: &str,
    job_id: &str,
    app: &tauri::AppHandle,
) -> Result<sovereign_contracts::daemon_wire::AskOutcome, String> {
    use crate::local_corpus_commands::{INGEST_POLL_INTERVAL, INGEST_POLL_MAX_FAILURES};
    let mut failures = 0u32;
    let mut cursor = 0usize;
    loop {
        tokio::time::sleep(INGEST_POLL_INTERVAL).await;
        let p = match client
            .ask_document_progress::<sovereign_contracts::daemon_wire::AskProgress>(
                asset_id, job_id, cursor,
            )
            .await
        {
            Ok(p) => {
                failures = 0;
                p
            }
            Err(e) => {
                failures += 1;
                tracing::warn!(%asset_id, %job_id, failures, "ask_document: progress poll failed: {e}");
                if failures >= INGEST_POLL_MAX_FAILURES {
                    return Err(format!(
                        "lost contact with the daemon while answering \
                         ({failures} consecutive failures): {e}"
                    ));
                }
                continue;
            }
        };
        cursor = p.next;
        for frame in p.frames {
            let _ = app.emit("document:operation", &frame);
        }
        if p.finished {
            // `end` sets the outcome before it flips `finished`, so this
            // arm is a host contradiction, reported rather than answered
            // with an empty success (§18.3).
            return p.outcome.ok_or_else(|| {
                format!("the daemon reported ask job '{job_id}' finished with no outcome")
            });
        }
    }
}

/// Refresh a single document asset by id. Used by the frontend to pick up
/// state changes (e.g. an auto-heal rebuild that just completed in the
/// background).
#[tauri::command]
pub async fn get_document_asset(
    state: State<'_, Arc<AppState>>,
    asset_id: String,
) -> Result<Option<sovereign_contracts::types::DocumentAsset>, String> {
    // sv-surface D9b: the daemon's store is the one that holds the asset,
    // so the read crosses the wire in BOTH modes. `Ok(None)` is the route's
    // 404 and only the 404 — "no such asset" and "the store would not
    // answer" stay different facts (ARCH §18.3), which is what the local
    // `ok_or("Store not ready")` collapsed the moment this process stopped
    // owning the store.
    sovereign_turn_client::TurnClient::new(state.client_base_url())
        .get_document::<sovereign_contracts::types::DocumentAsset>(&asset_id)
        .await
        .map_err(|e| format!("Load failed: {e}"))
}

/// User-initiated skeleton rebuild. Works from stored chunks (no file
/// required) — handy for assets whose skeleton never persisted because the
/// original ingest was interrupted, and for documents opened from history.
#[tauri::command]
pub async fn rebuild_document_skeleton(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    asset_id: String,
) -> Result<sovereign_contracts::types::DocumentAsset, String> {
    // sv-surface D9b — `POST /v1/documents/{id}/skeleton`. The rebuild is
    // the DocumentAssetManager's, and the manager that matters is the one
    // sitting on the store the chunks are in. Building a second manager
    // here over this process's own `inference` + `entity_extractor` meant
    // an attached desktop rebuilt a skeleton with a different extractor
    // than the daemon would (the daemon has no `EntityExtractor` on
    // `ServingCore`, so it takes `build_skeleton`'s documented LLM
    // fallback — named in 2a9a9e91e, and now the one answer).
    //
    // The route answers the REFRESHED record, which is what the reload
    // below used to ask a second question for.
    let refreshed = sovereign_turn_client::TurnClient::new(state.client_base_url())
        .rebuild_document_skeleton::<sovereign_contracts::types::DocumentAsset>(&asset_id)
        .await
        .map_err(|e| format!("Skeleton rebuild failed: {e}"))?;

    let _ = app_handle.emit("document:skeleton_rebuilt", &asset_id);

    Ok(refreshed)
}

/// Helper used by `ask_document` when the routed question is off-topic
/// (or when RAG retrieval comes up empty). Delegates to the runtime's
/// normal conversation pipeline — router, corpus search, layered-confidence
/// synthesis, auto-title — and returns a `DocumentAskResponse` with no
/// `DocumentAssetOperation` attribution since the document wasn't used.
///
/// The user message has already been saved as the latest message in the
/// conversation, so we use `handle_turn` (not `handle_message`) to avoid
/// saving it twice.
async fn run_turn_via_runtime(
    app_handle: &tauri::AppHandle,
    state: &State<'_, Arc<AppState>>,
    question: &str,
    conversation_id: &str,
) -> Result<DocumentAskResponse, String> {
    // sv-surface R5: a document question is a turn, and every turn rides
    // the wire — the REST one-shot (same driver the socket serves, through
    // the client family), exactly as the chat one-shot does.
    let turn = sovereign_turn_client::TurnClient::new(state.client_base_url())
        .send_message(conversation_id, question)
        .await
        .map_err(|e| format!("turn failed: {e}"))?;

    // Runtime saved the assistant message itself and spawned auto-title.
    // Emit the list-refresh event the normal send_message command emits.
    let _ = app_handle.emit("conversations:changed", ());

    // The wire's TYPED projection (the Complete frame's provenance /
    // epistemic metadata) where the in-process read returned the raw
    // persisted blob — the same named G9 delta the chat one-shot
    // carries.
    let metadata = turn
        .metadata
        .map(|m| serde_json::to_value(&m).unwrap_or(serde_json::Value::Null));
    let (provenance, citations) =
        sovereign_contracts::types::projection::project_message_metadata(&metadata);
    Ok(DocumentAskResponse {
        response: turn.text,
        operation: None,
        sources: Vec::new(),
        metadata,
        provenance,
        citations,
    })
}

#[tauri::command]
pub async fn list_document_assets(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<sovereign_contracts::types::DocumentAsset>, String> {
    // sv-surface D9b — `GET /v1/documents`. One shelf, the daemon's.
    sovereign_turn_client::TurnClient::new(state.client_base_url())
        .list_documents::<sovereign_contracts::types::DocumentAsset>()
        .await
        .map_err(|e| format!("List failed: {e}"))
}

#[tauri::command]
pub async fn delete_document_asset(
    state: State<'_, Arc<AppState>>,
    asset_id: String,
) -> Result<(), String> {
    // sv-surface D9b — `DELETE /v1/documents/{id}`. The delete removes the
    // asset row AND its chunks, so it must run against the store that holds
    // them. Note what the local arm needed to do that: a whole
    // `DocumentAssetManager` over an `InferenceProvider` — a model handle,
    // to delete rows — because the manager's constructor demands one. The
    // route needs no such thing.
    sovereign_turn_client::TurnClient::new(state.client_base_url())
        .delete_document(&asset_id)
        .await
        .map_err(|e| format!("Delete failed: {e}"))
}

/// A document from the legacy chunks table (uploaded via the old paperclip
/// path before DocumentAssetManager existed) — the route's own type, which
/// `list_legacy_documents` parses with and returns verbatim. Named from
/// `sovereign-contracts`, which is where `documents_http`'s route names it
/// too (sv-surface svt-3).
pub use sovereign_contracts::daemon_wire::LegacyDocumentEntry;

/// List documents from the legacy `documents` table that don't have
/// a corresponding DocumentAsset record. These are shown in the picker
/// so users can see and select previously uploaded files.
#[tauri::command]
pub async fn list_legacy_documents(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<LegacyDocumentEntry>, String> {
    // sv-surface D9b — `GET /v1/documents/legacy`. The three skips (asset-
    // managed sources, `corpus:` chunks, empty sources) and the word count
    // moved DOWN with the route in 2a9a9e91e; this process held the only
    // copy of those rules, and running them here as well would have made
    // two deciders for one shelf (ARCH §10.6).
    sovereign_turn_client::TurnClient::new(state.client_base_url())
        .list_legacy_documents::<LegacyDocumentEntry>()
        .await
        .map_err(|e| format!("{e}"))
}

/// Promote a legacy document (from the old chunks table) into a
/// DocumentAsset. This creates the asset record from existing data —
/// no re-upload, no re-embedding. The skeleton is null until built.
#[tauri::command]
pub async fn promote_legacy_document(
    state: State<'_, Arc<AppState>>,
    source: String,
) -> Result<DocumentAssetResponse, String> {
    // sv-surface D9b — `POST /v1/documents/legacy/promote`. The title rule
    // (strip the extension, `_`/`-` to spaces) went down with the route;
    // the asset is minted against the store that owns the chunks, so the
    // `index_id`/`word_count` it records describe rows that are actually
    // there.
    let asset = sovereign_turn_client::TurnClient::new(state.client_base_url())
        .promote_legacy_document::<sovereign_contracts::types::DocumentAsset>(&source)
        .await
        .map_err(|e| format!("{e}"))?;

    Ok(DocumentAssetResponse { asset })
}
