// SPDX-License-Identifier: AGPL-3.0-or-later
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

// ─── Document Asset commands ─────────────────────────────────

#[derive(Serialize)]
pub struct DocumentAssetResponse {
    pub asset: sovereign_core::types::DocumentAsset,
}

#[derive(Serialize)]
pub struct DocumentAskResponse {
    pub response: String,
    /// The document operation used to answer, when the document was involved.
    /// `None` when the question was off-topic and the runtime's normal
    /// conversation pipeline answered it instead (no operation badge shown).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<sovereign_core::types::DocumentAssetOperation>,
    pub sources: Vec<String>,
    /// The PERSISTED assistant-message metadata, returned verbatim so the
    /// live bubble renders identically to a reload from the store —
    /// provenance + retrieved_chunks on the document-op path, and (via the
    /// runtime fallback) `grounding_gate` for the verification receipt.
    /// Dropping this at the Tauri boundary was why live attached-doc
    /// bubbles lacked the routing-meta bar their reloaded twins had.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Upload and ingest a document. The command returns immediately with
/// a Pending asset. The full ingest pipeline (embed + skeleton) runs
/// in a background task and emits `document:progress` events. The
/// frontend shows these via the IngestBanner / DocOpProgress indicator.
#[tauri::command]
pub async fn upload_document_asset(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    file_path: String,
) -> Result<DocumentAssetResponse, String> {
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

    let path = std::path::Path::new(&file_path);
    if !path.exists() {
        return Err(format!("File not found: {file_path}"));
    }

    // `prepare` parses + chunks + persists the Pending asset (no
    // inference — fast). Crucially, the asset id it mints is the SAME
    // id `run_ingest` emits every `document:progress` event under, so
    // the banner we return here and the events the UI later receives
    // agree. (The old path created the asset here AND let `ingest`
    // mint a second id internally — the UI subscribed to the first,
    // events fired under the second, and the banner sat on "Queued…"
    // for the entire ingest while a duplicate record progressed to
    // Ready unseen.)
    let manager =
        sovereign_tools::document_asset::DocumentAssetManager::new(inference, Arc::clone(&store));
    let prepared = manager
        .prepare(path)
        .await
        .map_err(|e| format!("Prepare failed: {e}"))?;
    let response_asset = prepared.asset.clone();

    // Spawn the embed + enrichment pipeline in the background. Progress
    // events update the UI in real time; the asset state transitions
    // Pending → Indexing → PartiallyReady → BuildingSkeleton →
    // MultiHopReady → Ready, all under `response_asset.id`.
    let handle = app_handle.clone();
    let event_asset_id = response_asset.id.clone();
    tauri::async_runtime::spawn(async move {
        match manager
            .run_ingest(prepared, move |progress| {
                // Every event MUST carry asset_id. The frontend listener
                // drops events without one (it keys live state by id),
                // but the high-frequency `Indexing` / `BuildingSkeleton`
                // variants don't embed it — only the milestone events
                // (Started / RagAvailable / MultiHopReady / Ready) do. So
                // inject it here, where the id is known. Without this the
                // per-batch progress that advances the % bar and feeds the
                // ETA never reached the UI: the banner sat on "estimating…"
                // for the entire embed phase, then jumped straight between
                // milestones. `or_insert` preserves the id the milestone
                // variants already carry.
                let mut payload =
                    serde_json::to_value(&progress).unwrap_or_else(|_| serde_json::json!({}));
                if let serde_json::Value::Object(map) = &mut payload {
                    map.entry("asset_id".to_string())
                        .or_insert_with(|| serde_json::Value::String(event_asset_id.clone()));
                }
                let _ = handle.emit("document:progress", &payload);
            })
            .await
        {
            Ok(completed) => {
                tracing::info!(
                    filename = %completed.filename,
                    chunks = completed.chunk_count,
                    entities = completed
                        .skeleton
                        .as_ref()
                        .map(|s| s.main_entities.len())
                        .unwrap_or(0),
                    "document asset ingest complete",
                );
            }
            Err(e) => {
                tracing::warn!("document asset ingest failed: {e}");
            }
        }
    });

    Ok(DocumentAssetResponse {
        asset: response_asset,
    })
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
    if asset.skeleton.is_none() {
        tracing::info!(
            asset_id = %asset_id,
            "ask_document: skeleton missing — spawning background rebuild"
        );
        let inf = Arc::clone(&inference);
        let s = store.clone();
        let aid = asset_id.clone();
        let app = app_handle.clone();
        tokio::spawn(async move {
            let manager = sovereign_tools::document_asset::DocumentAssetManager::new(inf, s);
            match manager.rebuild_skeleton(&aid).await {
                Ok(skeleton) => {
                    tracing::info!(
                        asset_id = %aid,
                        entities = skeleton.main_entities.len(),
                        sections = skeleton.sections.len(),
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
    let user_msg = sovereign_core::types::Message {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conversation_id.clone(),
        role: sovereign_core::types::Role::User,
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
        sovereign_core::types::DocumentAssetOperation::OffTopic { .. }
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
        sovereign_core::types::DocumentAssetOperation::Rag { .. }
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

    let provenance = sovereign_core::types::ResponseProvenance {
        intent: format!("DocumentAsk:{}", operation.label()),
        search_method: Some("document".to_string()),
        sources: vec![sovereign_core::types::SourceSummary {
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

    // Epistemic-humility hook. Detection is now the turn's gate
    // abstention (I4-C retirement of gap.rs's LLM judge) — and the
    // document-op path runs NO grounding gate, so it carries no
    // abstention signal and never fires the card. That is the honest
    // shape: the user attached THE document; the short-answer cases
    // (off-topic, zero-hit RAG) already fell through to the gated
    // runtime pipeline above, which does carry the signal.
    let final_content = {
        let runtime_guard = state.runtime.read().await;
        if let Some(runtime) = runtime_guard.as_ref() {
            // Approval-channel task id kept stamped for parity with the
            // runtime path (a no-op when no card fires).
            state.approval.set_task_id(&conversation_id).await;
            runtime
                .maybe_collaborate(&conversation_id, &question, &output.text, false)
                .await
        } else {
            output.text.clone()
        }
    };

    // Persist the assistant response with document operation metadata
    // (legacy `operation` / `sources` fields) plus the new rich
    // `provenance` / `retrieved_chunks` shape the AssistantMessage
    // component reads for the routing-meta bar and citation popovers.
    let assistant_msg = sovereign_core::types::Message {
        id: assistant_message_id.clone(),
        conversation_id: conversation_id.clone(),
        role: sovereign_core::types::Role::Assistant,
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

    Ok(DocumentAskResponse {
        response: final_content,
        operation: Some(operation),
        sources: sources_content,
        metadata: assistant_msg.metadata.clone(),
    })
}

/// Refresh a single document asset by id. Used by the frontend to pick up
/// state changes (e.g. an auto-heal rebuild that just completed in the
/// background).
#[tauri::command]
pub async fn get_document_asset(
    state: State<'_, Arc<AppState>>,
    asset_id: String,
) -> Result<Option<sovereign_core::types::DocumentAsset>, String> {
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };
    store
        .get_document_asset(&asset_id)
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
) -> Result<sovereign_core::types::DocumentAsset, String> {
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

    let manager = sovereign_tools::document_asset::DocumentAssetManager::new(
        Arc::clone(&inference),
        store.clone(),
    );

    manager
        .rebuild_skeleton(&asset_id)
        .await
        .map_err(|e| format!("Skeleton rebuild failed: {e}"))?;

    // Return the refreshed asset record so the caller can update UI state
    // in-place (skeleton now Some, document_type set, state Ready).
    let refreshed = store
        .get_document_asset(&asset_id)
        .await
        .map_err(|e| format!("Reload failed: {e}"))?
        .ok_or("Asset vanished during rebuild")?;

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
    let runtime = {
        let guard = state.runtime.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Runtime not ready")?
    };

    state.approval.set_task_id(conversation_id).await;

    let response = runtime
        .handle_turn(question, conversation_id)
        .await
        .map_err(|e| format!("Runtime turn failed: {e}"))?;

    // Runtime saved the assistant message itself and spawned auto-title.
    // Emit the list-refresh event the normal send_message command emits.
    let _ = app_handle.emit("conversations:changed", ());

    Ok(DocumentAskResponse {
        response: response.message.content,
        operation: None,
        sources: Vec::new(),
        // Carries the runtime's full message metadata — provenance,
        // retrieved_chunks, and `grounding_gate` (the verification
        // receipt) — to the live bubble.
        metadata: response.message.metadata,
    })
}

#[tauri::command]
pub async fn list_document_assets(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<sovereign_core::types::DocumentAsset>, String> {
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };
    store
        .list_document_assets()
        .await
        .map_err(|e| format!("List failed: {e}"))
}

#[tauri::command]
pub async fn delete_document_asset(
    state: State<'_, Arc<AppState>>,
    asset_id: String,
) -> Result<(), String> {
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

    let manager = sovereign_tools::document_asset::DocumentAssetManager::new(inference, store);
    manager
        .delete(&asset_id)
        .await
        .map_err(|e| format!("Delete failed: {e}"))
}

/// A document from the legacy chunks table (uploaded via the old
/// paperclip path before DocumentAssetManager existed).
#[derive(Serialize)]
pub struct LegacyDocumentEntry {
    pub source: String,
    pub filename: String,
    pub chunk_count: usize,
    pub word_count: usize,
}

/// List documents from the legacy `documents` table that don't have
/// a corresponding DocumentAsset record. These are shown in the picker
/// so users can see and select previously uploaded files.
#[tauri::command]
pub async fn list_legacy_documents(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<LegacyDocumentEntry>, String> {
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };

    let sources = store.list_sources().await.map_err(|e| format!("{e}"))?;
    let assets = store.list_document_assets().await.unwrap_or_default();

    // Filter out sources that already have a DocumentAsset (including
    // the "asset:uuid" sources created by DocumentAssetManager).
    let asset_sources: std::collections::HashSet<String> =
        assets.iter().map(|a| format!("asset:{}", a.id)).collect();

    let mut entries = Vec::new();
    for source in &sources {
        // Skip asset-managed documents and corpus chunks.
        if source.starts_with("asset:") && asset_sources.contains(source) {
            continue;
        }
        // Skip corpus-sourced chunks (Wikipedia, SEP, etc.).
        if source.starts_with("corpus:") {
            continue;
        }

        let chunks = store.get_chunks_by_source(source).await.unwrap_or_default();
        if chunks.is_empty() {
            continue;
        }

        let word_count: usize = chunks
            .iter()
            .map(|c| c.content.split_whitespace().count())
            .sum();
        let filename = source.rsplit('/').next().unwrap_or(source).to_string();

        entries.push(LegacyDocumentEntry {
            source: source.clone(),
            filename,
            chunk_count: chunks.len(),
            word_count,
        });
    }

    Ok(entries)
}

/// Promote a legacy document (from the old chunks table) into a
/// DocumentAsset. This creates the asset record from existing data —
/// no re-upload, no re-embedding. The skeleton is null until built.
#[tauri::command]
pub async fn promote_legacy_document(
    state: State<'_, Arc<AppState>>,
    source: String,
) -> Result<DocumentAssetResponse, String> {
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };

    let chunks = store
        .get_chunks_by_source(&source)
        .await
        .map_err(|e| format!("{e}"))?;

    if chunks.is_empty() {
        return Err(format!("No chunks found for source: {source}"));
    }

    let word_count: usize = chunks
        .iter()
        .map(|c| c.content.split_whitespace().count())
        .sum();
    let filename = source.rsplit('/').next().unwrap_or(&source).to_string();
    let title = filename
        .rsplit_once('.')
        .map(|(name, _)| name)
        .unwrap_or(&filename)
        .replace(['_', '-'], " ");

    let asset = sovereign_core::types::DocumentAsset {
        id: uuid::Uuid::new_v4().to_string(),
        title,
        filename,
        file_size_mb: 0.0, // Unknown for legacy docs.
        word_count,
        chunk_count: chunks.len(),
        document_type: sovereign_core::types::DocumentTypeTag::Unknown,
        ingested_at: chrono::Utc::now(),
        index_id: format!("legacy:{source}"),
        skeleton: None,
        state: sovereign_core::types::AssetState::PartiallyReady,
        owner: None,
    };

    store
        .save_document_asset(&asset)
        .await
        .map_err(|e| format!("{e}"))?;

    Ok(DocumentAssetResponse { asset })
}
