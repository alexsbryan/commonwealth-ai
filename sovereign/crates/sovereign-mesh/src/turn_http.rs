// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon serves the turn — `POST /v1/conversations`,
//! `GET /v1/conversations/{id}/stream` — and, since sv-surface rung 3,
//! the conversation CRUD surface around it: `GET /v1/conversations`
//! (list), `GET /v1/conversations/{id}` (get with history),
//! `DELETE /v1/conversations/{id}`, and the one-shot
//! `POST /v1/conversations/{id}/messages`.
//!
//! # Why this exists
//!
//! `quality/TOPOLOGY.md` §3.5 draws every surface — desktop, `svrn chat`, the
//! server, the bench — above ONE process that assembles a `Runtime`, with a
//! turn protocol between them. Phase 5b made the protocol a value
//! (`sovereign_contracts::types::{TurnRequest, TurnFrame}`), phase 5c made
//! driving a turn a library function (`sovereign_core::runtime::serve_turn`),
//! and this is the door: the first place the daemon itself answers one.
//!
//! Before it, `sovereign daemon run` held the corpus engine, the state store
//! and the routed inference provider — every ingredient of an answer — and
//! served none, so the only way to get a turn was to be a host that had built
//! its own `Runtime`. That is what made "one process assembles" unstatable.
//!
//! # The wire form is `sovereign-server`'s, deliberately
//!
//! Same paths, same frames. A client that speaks to the server speaks to the
//! daemon without knowing which it reached, which is the property that lets a
//! host stop assembling and start connecting (phase 6). The differences are
//! the two the daemon genuinely has and the server does not:
//!
//! - **No tenant scoping.** The server prefixes `{tenant}:{conv}` because it
//!   is a multi-tenant hub; a local daemon has one principal. `serve_turn`
//!   takes an ALREADY-SCOPED id for exactly this reason — prefixing is a host
//!   policy — so the daemon passes the id through and the server keeps its
//!   `TenantRuntime`.
//! - **No fair scheduler, no reciprocity.** Those price a shared hub's
//!   contention between strangers. Loopback callers are one user's own
//!   surfaces.
//!
//! The rung-3 CRUD routes extend that sentence from "same turn frames" to
//! "same conversation envelopes": every response struct below is
//! `sovereign-server`'s `routes.rs` shape field-for-field (declaration order
//! included, so the bytes equal on the same row), and the message projections
//! are not re-implementations at all — `project_message_metadata` /
//! `project_epistemic_state` / `TaskSummary` are imported from
//! `sovereign_contracts::types::projection`, the same deciders the server's
//! handlers call. The envelope structs themselves cannot be imported the same
//! way (`sovereign-server` is a bin crate, and `sovereign-contracts` holds the
//! projections rather than the envelopes), so they are mirrored here the way
//! `reading_http` mirrors the server's reading shapes, and the parity test in
//! `tests/main/loopback_parity.rs` pins the mirror: the route's bytes are the
//! canonical projection's bytes on the same fixture rows.
//!
//! # Loopback only
//!
//! Both layers, mirroring `reading_http` and `admin_http`: the router-level
//! [`crate::loopback_guard::loopback_only`] middleware and a per-handler peer
//! check. A turn runs this host's tools against this host's corpora; it is not
//! a peer-facing surface, and `/v1/chat/completions` — which IS peer-facing —
//! deliberately remains raw completion with none of it.
//!
//! # Known gap, stated rather than discovered
//!
//! Narration frames are NOT emitted. `serve_turn` takes the narration
//! broadcast when a host installed one, and the daemon's `Runtime` carries the
//! recipe's default no-op sink: §3.5 lists `routing_events` among the five
//! capabilities that leave the `Runtime` entirely, because it is a
//! per-connection wire concern, and the daemon does not yet own the
//! per-connection subscription that would make it one. A client therefore sees
//! `Token`s and a terminal `Complete`, and no progress in between. Nothing
//! regresses — no host served a turn from the daemon before this file — but a
//! reader should know it is missing on purpose.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{ConnectInfo, Extension, Path, Query, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use sovereign_contracts::types::projection::{
    project_epistemic_state, project_message_metadata, Citation, Provenance, TaskSummary,
};
use sovereign_contracts::types::{TurnFrame, TurnMode, TurnRequest};
use sovereign_core::runtime::{collect_turn, serve_turn};

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::enforce_localhost;

/// Mount the turn surface. Built from `Arc<Self>` by `start_daemon`, like the
/// mesh, admin and reading routers — so a serving daemon cannot come up
/// without it and `mount_names` reports exactly what it has.
pub fn turn_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/conversations", post(create_conversation))
        .route("/v1/conversations", get(list_conversations))
        .route("/v1/conversations/search", get(search_conversations))
        .route("/v1/conversations/{id}", get(get_conversation))
        .route("/v1/conversations/{id}", delete(delete_conversation))
        .route("/v1/conversations/{id}/messages", post(send_message))
        .route("/v1/conversations/{id}/stream", get(ws_handler))
        .route("/v1/conversations/{id}/end", post(end_conversation))
        .route("/v1/memories/{id}", delete(delete_memory))
        .route("/v1/memories/{id}/weaken", post(weaken_memory))
        .route("/v1/notes/tool-outcome", post(record_tool_outcome))
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
        .layer(Extension(daemon))
}

#[derive(Debug, Default, Deserialize)]
pub struct CreateConversationRequest {
    /// Workspace skill to tag the conversation with, so
    /// `Runtime::resolve_active_mode` routes it into that agent loop from the
    /// first message rather than after one untagged turn.
    #[serde(default)]
    pub skill_id: Option<String>,
    /// The per-conversation retrieval allow-list
    /// (`Conversation::enabled_corpora`), so a surface can scope a turn to
    /// named corpora the way the desktop's chip strip does. Absent means
    /// "every installed corpus". Validated by `Runtime::seed_conversation`
    /// against the corpora this daemon would actually search: an unknown id
    /// is a 400 naming it and the installed list, never a silent widen.
    #[serde(default)]
    pub enabled_corpora: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct CreateConversationResponse {
    pub id: String,
    pub created_at: i64,
    /// The allow-list that was seeded, echoed back VERBATIM when one was
    /// sent and omitted otherwise. The echo is what lets a client tell a
    /// daemon that scoped the conversation from one that predates the field
    /// and ignored it — serde drops unknown keys, so without this a stale
    /// daemon would mint an unscoped conversation and say nothing (§18.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_corpora: Option<Vec<String>>,
}

/// `POST /v1/conversations`
///
/// Seeds the row before the first message, which is what makes the skill tag
/// load-bearing (same reason `sovereign-server`'s create route seeds rather
/// than letting the first turn create the conversation). A missing or
/// malformed body yields an untagged conversation rather than a 4xx — the
/// server's behaviour, kept so one client works against both.
async fn create_conversation(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(runtime) = daemon.runtime() else {
        return service_unavailable("this daemon serves no turns (mesh-admin)");
    };
    let req: CreateConversationRequest = if body.is_empty() {
        CreateConversationRequest::default()
    } else {
        serde_json::from_slice(&body).unwrap_or_default()
    };
    let id = uuid::Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    if let Err(e) = runtime
        .seed_conversation(
            &id,
            now,
            req.skill_id.as_deref(),
            req.enabled_corpora.as_deref(),
        )
        .await
    {
        // A bad allow-list is the CALLER's mistake and carries its own remedy
        // (the installed ids); a store failure is the daemon's. Different
        // status codes so a client can tell "fix your flag" from "the daemon
        // is unwell" without parsing prose. 500 on the store failure is the
        // SERVER's spelling (routes.rs maps every non-InvalidInput seed error
        // to INTERNAL_SERVER_ERROR); until rung 3 this branch said 503, which
        // was the one byte a client could use to tell which host it had
        // reached — the exact opposite of the wire-compat this file exists
        // to hold.
        return match e {
            sovereign_core::error::Error::InvalidInput(msg) => bad_request(&msg),
            other => internal_error(&format!("seed conversation: {other}")),
        };
    }
    Json(CreateConversationResponse {
        id,
        created_at: now,
        enabled_corpora: req.enabled_corpora,
    })
    .into_response()
}

// ─── Conversation CRUD wire types (sv-surface rung 3) ──────────────
//
// Every struct here mirrors `sovereign-server`'s `routes.rs` field-for-field,
// including `skip_serializing_if` and field DECLARATION ORDER — serde emits
// declaration order, so order equality is byte equality on the same row. The
// projections inside them are not mirrors: they are the server's own deciders,
// imported from `sovereign_contracts::types::projection`.

/// `GET /v1/conversations?limit=&offset=` — the server's `ListQuery`, whose
/// defaults (20 / 0) the handler applies identically.
#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ConversationListResponse {
    pub conversations: Vec<ConversationListEntry>,
}

#[derive(Debug, Serialize)]
pub struct ConversationListEntry {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize)]
pub struct ConversationResponse {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub messages: Vec<MessageEntry>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize)]
pub struct MessageEntry {
    pub id: String,
    pub role: String,
    pub content: String,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    /// The typed epistemic ledger (EPISTEMIC_STATE.md); see the server's
    /// twin field. `None` on old messages / kill switch off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epistemic_state: Option<sovereign_contracts::types::EpistemicState>,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub message_id: String,
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskSummary>,
    /// Host-side provenance (model + serving node, routing tier, latency).
    /// `None` on turns whose handler doesn't persist provenance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// Corpus-grounded citations carrying the host's `(corpus_id, chunk_id)`
    /// handle. Empty when the answer wasn't grounded in an installed corpus.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    /// The typed epistemic ledger, when the turn stamped one; see
    /// [`MessageEntry::epistemic_state`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epistemic_state: Option<sovereign_contracts::types::EpistemicState>,
}

/// `GET /v1/conversations`
///
/// The server filters to the caller's tenant and strips the `tenant:` prefix;
/// this daemon has one principal and stores bare ids, so the rows pass
/// through verbatim — the same wire ANSWER the server's client would see for
/// its own tenant, which is the compat that matters.
async fn list_conversations(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Query(params): Query<ListQuery>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(store) = daemon.state_store() else {
        return service_unavailable("this daemon holds no conversation store (mesh-admin)");
    };
    let limit = params.limit.unwrap_or(20);
    let offset = params.offset.unwrap_or(0);
    match store.list_conversations(limit, offset).await {
        Ok(convos) => Json(ConversationListResponse {
            conversations: convos
                .into_iter()
                .map(|c| ConversationListEntry {
                    id: c.id,
                    title: c.title,
                    created_at: c.created_at,
                    updated_at: c.updated_at,
                })
                .collect(),
        })
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// `GET /v1/conversations/{id}`
///
/// Serves the row's history through the same projections the server's
/// `get_conversation` runs — `role_str` for the role, the contracts-layer
/// projection functions for provenance / citations / epistemic state — so a
/// client rendering a resumed conversation cannot tell which host answered.
/// A missing row is the server's exact 404 sentence, not a generic one.
async fn get_conversation(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(conversation_id): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(store) = daemon.state_store() else {
        return service_unavailable("this daemon holds no conversation store (mesh-admin)");
    };
    match store.get_conversation(&conversation_id).await {
        Ok(convo) => Json(ConversationResponse {
            id: conversation_id,
            title: convo.title,
            messages: convo
                .messages
                .into_iter()
                .map(|m| {
                    let role = m.role_str().to_string();
                    let (provenance, citations) = project_message_metadata(&m.metadata);
                    MessageEntry {
                        id: m.id,
                        role,
                        content: m.content,
                        created_at: m.created_at,
                        provenance,
                        citations,
                        epistemic_state: project_epistemic_state(&m.metadata),
                    }
                })
                .collect(),
            created_at: convo.created_at,
            updated_at: convo.updated_at,
        })
        .into_response(),
        Err(sovereign_core::error::Error::NotFound(_)) => not_found("Conversation not found"),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// `DELETE /v1/conversations/{id}`
///
/// The server's contract: `204 No Content` on success, no body — deletion is
/// idempotent from the client's point of view, and the row's absence
/// afterward is observable via the 404 the get route now answers.
async fn delete_conversation(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(conversation_id): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(store) = daemon.state_store() else {
        return service_unavailable("this daemon holds no conversation store (mesh-admin)");
    };
    match store.delete_conversation(&conversation_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// `POST /v1/conversations/{id}/messages` — the one-shot REST turn.
///
/// The SAME driver the WebSocket route streams through (`collect_turn` wraps
/// `serve_turn` with a collecting sink), which is also the driver the
/// server's REST route calls. One client can therefore mix transports —
/// stream a long turn, one-shot a short one — and the conversation reads
/// identically either way, because there was only ever one writer.
///
/// The server wraps its call in the fair scheduler and stamps the approval
/// channel's task id; this daemon has neither (see the module docs), and the
/// approval refusal the WebSocket route enforces applies here implicitly: a
/// turn that would pause for an approval cannot complete on this route, which
/// is the same named gap, not a new one.
async fn send_message(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(conversation_id): Path<String>,
    Json(body): Json<SendMessageRequest>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let (Some(runtime), Some(store)) = (daemon.runtime(), daemon.state_store()) else {
        return service_unavailable("this daemon serves no turns (mesh-admin)");
    };
    match collect_turn(
        runtime,
        store.as_ref(),
        &conversation_id,
        &body.content,
        TurnMode::Grounded,
        None,
    )
    .await
    {
        Ok(turn) => Json(MessageResponse {
            message_id: turn.message_id,
            // Always the assistant: this endpoint returns the reply to the
            // message the caller just sent (the server's spelling).
            role: "assistant".to_string(),
            content: turn.text,
            task: turn.task,
            provenance: turn.provenance,
            citations: turn.citations,
            epistemic_state: turn.epistemic_state,
        })
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// `POST /v1/conversations/{id}/end`
///
/// Runs the conversation-end memory-extraction pass. This is a lifecycle
/// operation, not a turn, which is why it is a REST route beside `create`
/// rather than a `TurnRequest` variant — `TurnRequest`'s own doc says it is
/// "Client → host, for ONE turn", and a client that quits its REPL is not
/// taking a turn.
///
/// It exists because `svrn chat session` called `Runtime::end_conversation`
/// on `quit`, and phase 6 turns that host into a client. Without a wire form
/// the conversion would have silently stopped extracting long-term memories
/// on the one interactive CLI surface — a capability disappearing because the
/// process that used to hold it stopped holding it, which is the failure mode
/// phase 6 has to not have.
async fn end_conversation(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(conversation_id): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(runtime) = daemon.runtime() else {
        return service_unavailable("this daemon serves no turns (mesh-admin)");
    };
    match runtime.end_conversation(&conversation_id).await {
        Ok(()) => Json(serde_json::json!({ "ended": conversation_id })).into_response(),
        // Reported, not swallowed: the extraction pass runs a model, and a
        // caller told "ok" for a pass that never ran cannot tell the
        // difference (ARCH §18.3).
        Err(e) => service_unavailable(&format!("end conversation: {e}")),
    }
}

/// `GET /v1/conversations/search?q=...` — full-text message search across
/// conversations (sv-surface rung 6, commit A).
///
/// The decider is `StateStore::search_messages` — the SAME trait call the
/// desktop's in-process `search_messages` command makes — so the wire's
/// answer and the local answer cannot drift. The route caps at 50 rows,
/// the desktop's own cap, moved here so the cap has one home by the time
/// the desktop repoints (rung 6 commit D).
#[derive(Debug, Default, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchEntry>,
}

#[derive(Debug, Serialize)]
pub struct SearchEntry {
    pub content: String,
    pub conversation_id: String,
}

async fn search_conversations(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Query(params): Query<SearchQuery>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(store) = daemon.state_store() else {
        return service_unavailable("this daemon holds no conversation store (mesh-admin)");
    };
    let Some(q) = params.q.filter(|q| !q.is_empty()) else {
        return bad_request("the q parameter is required and must not be empty");
    };
    match store.search_messages(&q).await {
        Ok(messages) => Json(SearchResponse {
            results: messages
                .into_iter()
                .take(50)
                .map(|m| SearchEntry {
                    content: m.content,
                    conversation_id: m.conversation_id,
                })
                .collect(),
        })
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// `DELETE /v1/memories/{id}` — tombstone a memory the user flagged as
/// wrong (soft delete; the row is preserved for audit and excluded from
/// recall). The desktop's `forget_memory` repoints here in rung 6 commit D.
async fn delete_memory(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(memory_id): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(store) = daemon.state_store() else {
        return service_unavailable("this daemon holds no conversation store (mesh-admin)");
    };
    match store.delete_memory(&memory_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// `POST /v1/memories/{id}/weaken` — halve a memory's confidence with the
/// standard decay floor (sv-surface rung 6).
///
/// THE one decider for the halving: the formula lived in the desktop's
/// `weaken_memory` command until this route, and a second copy is exactly
/// the §10.6 twin this campaign deletes. The read-modify-write is honest
/// here rather than client-side because the confidence value the floor is
/// applied to must be the daemon's row, not a snapshot a client read
/// earlier. Answers the new confidence so a caller can render it.
#[derive(Debug, Serialize)]
pub struct WeakenResponse {
    pub confidence: f64,
}

async fn weaken_memory(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(memory_id): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(store) = daemon.state_store() else {
        return service_unavailable("this daemon holds no conversation store (mesh-admin)");
    };
    let all = match store.get_all_memories().await {
        Ok(all) => all,
        Err(e) => return internal_error(&e.to_string()),
    };
    let Some(current) = all.iter().find(|m| m.id == memory_id) else {
        return not_found(&format!("memory {memory_id} not found"));
    };
    let new_confidence = (current.confidence * 0.5).max(0.0);
    match store
        .update_memory_confidence(&memory_id, new_confidence)
        .await
    {
        Ok(()) => Json(WeakenResponse {
            confidence: new_confidence,
        })
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// `POST /v1/notes/tool-outcome` — record a tool-decision outcome into the
/// daemon's notes.db dossier (sv-surface rung 6, commit A).
///
/// The desktop's `submit_information_search` writes this BEFORE running
/// the user's escape-hatch web search; in attach mode the NoteStore lives
/// daemon-side, so the write crosses here. `record_tool_outcome` in
/// sovereign-core is the one decider — the same function the in-process
/// path calls. A daemon without a mounted notes surface answers 503 with
/// the named reason; the in-process path's "missing NoteStore is silently
/// skipped" posture is preserved by the CLIENT choosing to treat the 503
/// as non-fatal, which is the desktop repoint's call to make, not this
/// route's (the route must not swallow the fact — ARCH §18.3).
#[derive(Debug, Deserialize)]
pub struct ToolOutcomeRequest {
    /// Per-conversation-turn opaque id — the approval `key` the desktop
    /// minted, used as the session-id proxy so the audit trail traces back
    /// to the originating INFORMATION REQUEST card.
    pub session_id: String,
    #[serde(default)]
    pub conversation_id: Option<String>,
    pub tool_id: String,
    pub outcome: sovereign_core::memory::ToolDecisionOutcome,
    #[serde(default)]
    pub reasoning: String,
    #[serde(default)]
    pub extras: sovereign_core::memory::ToolDecisionExtras,
}

async fn record_tool_outcome(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(body): Json<ToolOutcomeRequest>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(notes) = daemon.notes_store() else {
        return service_unavailable(
            "this daemon serves no notes surface (notes.db unavailable — /mcp not mounted)",
        );
    };
    sovereign_core::dossier::record_tool_outcome(
        Some(notes),
        &body.session_id,
        body.conversation_id.as_deref(),
        &body.tool_id,
        body.outcome,
        &body.reasoning,
        body.extras,
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

/// `GET /v1/conversations/{id}/stream` — WebSocket upgrade.
async fn ws_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(conversation_id): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    if daemon.runtime().is_none() {
        return service_unavailable("this daemon serves no turns (mesh-admin)");
    }
    ws.on_upgrade(move |socket| handle_ws(socket, daemon, conversation_id))
}

async fn handle_ws(socket: WebSocket, daemon: Arc<EmbeddedDaemon>, conversation_id: String) {
    let (Some(runtime), Some(store)) = (daemon.runtime(), daemon.state_store()) else {
        // Re-checked after the upgrade because the borrow cannot cross it.
        // Unreachable in practice — `ws_handler` refused above.
        return;
    };
    let runtime = Arc::clone(runtime);
    let store = Arc::clone(store);

    let (mut ws_tx, mut ws_rx) = socket.split();

    // One writer to the socket, fed by the per-turn frame channel. There is no
    // second source here: the server multiplexes a fan-out approval broadcast
    // onto the same socket, and phase 5b split those into a different type
    // precisely because mixing them leaked one tenant's tokens to every
    // client. The daemon has no fan-out events, so it has one channel.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<TurnFrame>();
    let tx_handle = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            let Ok(json) = serde_json::to_string(&frame) else {
                continue;
            };
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                break; // client disconnected
            }
        }
    });

    // ONE in-flight turn per socket, held as a task rather than awaited inline.
    //
    // The obvious shape — `serve_turn(..).await` right here — is what
    // `sovereign-server`'s WebSocket handler does, and it has a defect this
    // route would have inherited: while the turn runs, the receive stream is
    // never polled, so the connection answers no PINGS. Every standards-
    // compliant client with keepalive (python `websockets` defaults to a 20s
    // ping with a 20s deadline) therefore drops mid-answer — and a grounded
    // turn over a large corpus routinely runs minutes on a contended host.
    // Observed here on the very first real turn against a deployed daemon:
    // `keepalive ping timeout` at 20s while the daemon's own log showed the
    // turn's retrieval completing normally. Spawning it and continuing to poll
    // `ws_rx` is what keeps the socket alive; the `in_flight` guard is what
    // keeps "one turn per socket" true without the receive loop having to
    // block to enforce it.
    let mut in_flight: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        let turn_finished = async {
            match in_flight.as_mut() {
                Some(h) => {
                    let _ = h.await;
                }
                // No turn running: never resolve, so the select waits on the
                // socket alone.
                None => std::future::pending().await,
            }
        };
        let incoming = tokio::select! {
            _ = turn_finished => {
                in_flight = None;
                continue;
            }
            incoming = ws_rx.next() => incoming,
        };
        let Some(Ok(msg)) = incoming else {
            break;
        };
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            // Ping/Pong are handled beneath us by the WebSocket codec — but
            // only while this stream is being polled, which is the whole
            // reason the turn above is a task.
            _ => continue,
        };
        let event: TurnRequest = match serde_json::from_str(&text) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("turn_http: invalid WebSocket message: {e}");
                let _ = out_tx.send(TurnFrame::StreamError {
                    message: format!("could not parse that as a TurnRequest: {e}"),
                    retry_after_secs: None,
                });
                continue;
            }
        };
        match event {
            TurnRequest::Message {
                content,
                mode,
                intent,
            } => {
                if in_flight.is_some() {
                    // Refused, not queued: a client that sent a second turn
                    // and heard nothing cannot tell "queued" from "lost".
                    let _ = out_tx.send(TurnFrame::StreamError {
                        message: "a turn is already in flight on this socket".to_string(),
                        retry_after_secs: None,
                    });
                    continue;
                }
                let (rt, st, cid, tx) = (
                    Arc::clone(&runtime),
                    Arc::clone(&store),
                    conversation_id.clone(),
                    out_tx.clone(),
                );
                in_flight = Some(tokio::spawn(async move {
                    serve_turn(
                        &rt,
                        st.as_ref(),
                        &cid,
                        &content,
                        mode,
                        intent,
                        // See the module docs — the daemon has no narration
                        // broadcast yet, and a `None` here is that fact rather
                        // than a dropped channel.
                        None,
                        &tx,
                    )
                    .await;
                }));
            }
            // v1 has no daemon-side session owner to route an approval to
            // (TOPOLOGY hazard 12, a phase 5 deliverable). Refusing loudly
            // beats accepting and silently doing nothing: a client that sent
            // an approval and got no frame cannot tell "granted" from "never
            // arrived" (ARCH §18.3).
            TurnRequest::Approve { .. } | TurnRequest::UserReply { .. } => {
                let _ = out_tx.send(TurnFrame::StreamError {
                    message: "this daemon does not accept mid-turn approvals or \
                              user replies (no daemon-side session owner yet)"
                        .to_string(),
                    retry_after_secs: None,
                });
            }
        }
    }

    if let Some(h) = in_flight {
        // The client hung up mid-turn. Nothing is left to receive the frames,
        // and a turn whose sink is gone is work nobody asked to keep.
        h.abort();
    }
    tx_handle.abort();
}

fn bad_request(reason: &str) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": reason })),
    )
        .into_response()
}

/// The server's `ErrorResponse` shape on a 404 — same status, same
/// `{"error": "..."}` body, same sentence the server's route writes.
fn not_found(reason: &str) -> Response {
    (
        axum::http::StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": reason })),
    )
        .into_response()
}

/// 500 with the server's `ErrorResponse` body. The server maps store and
/// turn failures to INTERNAL_SERVER_ERROR; a daemon replying 503 to the same
/// failure would be a distinguishing byte (§5c).
fn internal_error(reason: &str) -> Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": reason })),
    )
        .into_response()
}

fn service_unavailable(reason: &str) -> Response {
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": reason })),
    )
        .into_response()
}
