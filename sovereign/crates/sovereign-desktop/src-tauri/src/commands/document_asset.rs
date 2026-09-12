// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-split from the former monolithic `commands.rs` (PR5). Tauri
//! command handlers grouped by concern; re-exported through
//! `commands/mod.rs` so `commands::<name>` paths in `main.rs`'s
//! `generate_handler!` stay valid.
use super::*;
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
            .document_ingest_progress::<sovereign_mesh::documents_http::DocumentIngestProgress>(
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

#[tauri::command]
pub async fn ask_document(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    asset_id: String,
    question: String,
    conversation_id: String,
) -> Result<DocumentAskResponse, String> {
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };
    let inference = {
        let guard = state.inference.read().await;
        guard
            .as_ref()
            .map(Arc::clone)
            .ok_or("Inference not ready")?
    };

    let asset = store
        .get_document_asset(&asset_id)
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
    // Until 2026-09-11 this arm built a second `DocumentAssetManager` over
    // this process's `inference` + `entity_extractor`, so an auto-heal and
    // a manual rebuild of the same asset could disagree about which
    // extractor ran (ARCH principle 8).
    if asset.skeleton.is_none() {
        tracing::info!(
            asset_id = %asset_id,
            "ask_document: skeleton missing — spawning background rebuild over the wire"
        );
        let base_url = state.client_base_url();
        let aid = asset_id.clone();
        let app = app_handle.clone();
        tokio::spawn(async move {
            match sovereign_turn_client::TurnClient::new(base_url)
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

    // Persist the user's question first. This also upserts the conversations
    // row so the conversation survives navigation and restart, and lets the
    // runtime pipeline (below) see the question when it builds context.
    let user_msg = sovereign_contracts::types::Message {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conversation_id.clone(),
        role: sovereign_contracts::types::Role::User,
        content: question.clone(),
        created_at: now_epoch(),
        metadata: Some(serde_json::json!({
            "attached_asset_id": asset_id,
        })),
        version: now_epoch(),
    };
    store
        .save_message(&user_msg)
        .await
        .map_err(|e| format!("Failed to save user message: {e}"))?;

    let manager = sovereign_tools::document_asset::DocumentAssetManager::new(
        Arc::clone(&inference),
        store.clone(),
    );

    // Route first — a Fast-slot call that decides whether this question is
    // about the document at all.
    let operation = manager
        .route(&asset, &question)
        .await
        .map_err(|e| format!("Routing failed: {e}"))?;

    tracing::info!(
        asset_id = %asset_id,
        operation = %operation.label(),
        "ask_document: routed"
    );

    // When the question isn't about the document, hand it off to the normal
    // conversation pipeline. The runtime will route, search installed corpora,
    // synthesise with layered confidence, and save the assistant message. The
    // user message is already in the conversation (tagged with the asset id,
    // preserving "this turn had a document attached" context).
    if matches!(
        operation,
        sovereign_contracts::types::DocumentAssetOperation::OffTopic { .. }
    ) {
        return run_turn_via_runtime(&app_handle, &state, &question, &conversation_id).await;
    }

    // Document operation path.
    let handle = app_handle.clone();
    let start = std::time::Instant::now();
    let output = manager
        .execute_operation(&asset, &question, &operation, &move |progress| {
            let _ = handle.emit("document:operation", &progress);
        })
        .await
        .map_err(|e| format!("Query failed: {e}"))?;

    // RAG safety net: if retrieval returned zero matching chunks, the router
    // mis-classified. Fall through to the runtime pipeline the same way
    // OffTopic does. `execute_rag` signals this by returning an empty
    // ExecutionOutput.
    if matches!(
        operation,
        sovereign_contracts::types::DocumentAssetOperation::Rag { .. }
    ) && output.citations.is_empty()
        && output.text.is_empty()
    {
        tracing::info!(
            asset_id = %asset_id,
            "ask_document: RAG found no relevant passages — falling back to runtime"
        );
        return run_turn_via_runtime(&app_handle, &state, &question, &conversation_id).await;
    }

    let duration_ms = start.elapsed().as_millis() as u64;
    let assistant_message_id = uuid::Uuid::new_v4().to_string();

    // Build the `retrieved_chunks` + `provenance` shape the frontend expects
    // for the routing-meta bar and rich citation popovers. The frontend
    // matches `[Source: <label>]` spans in the prose against each chunk's
    // `title`, so we use the citation label as the title here.
    let retrieved_chunks: Vec<serde_json::Value> = output
        .citations
        .iter()
        .map(|c| {
            serde_json::json!({
                "title": c.label,
                "corpus_id": asset.title,
                "url": serde_json::Value::Null,
                "snippet": c.snippet,
                "provenance_tier": "document",
            })
        })
        .collect();

    let provenance = sovereign_contracts::types::ResponseProvenance {
        // A document-asset op is not a routed turn — no router decided it, and
        // `None` says exactly that rather than claiming a degraded one.
        router: None,
        intent: format!("DocumentAsk:{}", operation.label()),
        search_method: Some("document".to_string()),
        sources: vec![sovereign_contracts::types::SourceSummary {
            origin: asset.title.clone(),
            count: output.citations.len(),
            from_peer: None,
            display_name: None,
        }],
        inference_backend: if output.model_id.is_empty() {
            "local".to_string()
        } else {
            output.model_id.clone()
        },
        oicp_match: None,
        total_latency_ms: duration_ms,
        tokens_used: output.tokens_used,
        coarse_intent: None,
        self_assessment: None,
        routing_trigger: None,
        coverage: None,
        finish_reason: output.finish_reason.clone(),
        // DocumentAsk uses the same inference_config.max_tokens
        // budget any other handler does; surface it so the cutoff
        // chip can say "hit the N-token limit" honestly. RwLockGuard
        // derefs to `&DesktopConfig` so we can read the field
        // directly — Some() because DesktopConfig.max_tokens is a
        // bare number, not an Option.
        max_tokens_budget: Some(state.config.read().await.max_tokens as usize),
        completion_tokens: output.completion_tokens,
        // DocumentAsk is a self-contained desktop-side path that
        // doesn't share the `self.inference` field other handlers do
        // — the ctx-budget glassbox here would need to thread the
        // provider Arc through `output`. Leave `None` for now; the
        // primary chat path (KnowledgeQuery / DeepQuery / Simple)
        // already surfaces the budget where it matters most.
        context_window: None,
    };

    let sources_content: Vec<String> = output.citations.iter().map(|c| c.content.clone()).collect();

    // The epistemic-humility hook stood here and was a value-preserving
    // identity function, PROVABLY (sv-surface svt-3b).
    //
    // It called `Runtime::maybe_collaborate(.., abstained: false)`, and
    // `run_collaboration` returns `NotAttempted` on `!abstained` before doing
    // anything at all (`sovereign-core/src/runtime/collaboration.rs:177-180`,
    // logging "turn answered — no gap card"), which `maybe_collaborate`
    // flattens straight back to the input string
    // (`runtime/system_message.rs:779-784`). The comment that stood here said
    // as much in prose — "the document-op path runs NO grounding gate, so it
    // carries no abstention signal and never fires the card" — so this is the
    // §15 row "a comment asserting in English what a test could assert in
    // code", except the code could just not do it.
    //
    // Removing it therefore changes no output, and it removes the desktop's
    // last non-chat reason to hold a commissioned `Runtime`. The `set_task_id`
    // stamp goes with it: it existed to key approval cards for a card this
    // path cannot fire.
    let final_content = output.text.clone();

    // Persist the assistant response with document operation metadata
    // (legacy `operation` / `sources` fields) plus the new rich
    // `provenance` / `retrieved_chunks` shape the AssistantMessage
    // component reads for the routing-meta bar and citation popovers.
    let assistant_msg = sovereign_contracts::types::Message {
        id: assistant_message_id.clone(),
        conversation_id: conversation_id.clone(),
        role: sovereign_contracts::types::Role::Assistant,
        content: final_content.clone(),
        created_at: now_epoch(),
        metadata: Some(serde_json::json!({
            "attached_asset_id": asset_id,
            "operation": operation,
            "sources": sources_content,
            "duration_ms": duration_ms,
            "provenance": provenance,
            "retrieved_chunks": retrieved_chunks,
        })),
        version: now_epoch(),
    };
    store
        .save_message(&assistant_msg)
        .await
        .map_err(|e| format!("Failed to save assistant message: {e}"))?;

    // Record the operation for analytics.
    let _ = store
        .save_document_operation(&assistant_message_id, &asset_id, &operation, duration_ms)
        .await;

    // Fire auto-title in the background after the first exchange.
    {
        let inf = Arc::clone(&inference);
        let s = store.clone();
        let cid = conversation_id.clone();
        let app = app_handle.clone();
        tokio::spawn(async move {
            match sovereign_core::title::try_auto_title(inf.as_ref(), s.as_ref(), &cid).await {
                Ok(Some(_)) => {
                    let _ = app.emit("conversations:changed", ());
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        conversation_id = %cid,
                        error = %e,
                        "auto-title: generation failed (ask_document)"
                    );
                }
            }
        });
    }

    let _ = app_handle.emit("conversations:changed", ());

    let metadata = assistant_msg.metadata.clone();
    let (provenance, citations) =
        sovereign_contracts::types::projection::project_message_metadata(&metadata);
    Ok(DocumentAskResponse {
        response: final_content,
        operation: Some(operation),
        sources: sources_content,
        metadata,
        provenance,
        citations,
    })
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
    state.approval.set_task_id(conversation_id).await;

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
