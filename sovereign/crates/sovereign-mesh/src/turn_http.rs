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

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Extension, Path, Query, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::Instant;

use sovereign_contracts::types::projection::{
    project_epistemic_state, project_message_metadata, Citation, Provenance, TaskSummary,
};
use sovereign_contracts::types::{
    ClarificationRequest, InterpretationProposed, ResumeSession, TurnAnswer, TurnFrame, TurnMode,
    TurnNarration, TurnNotice, TurnRequest,
};
use sovereign_core::runtime::Runtime;
use sovereign_core::runtime::{collect_turn, drive_stream_handle, serve_turn, StreamHandle};
use sovereign_core::traits::StateStore;

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::{LocalOnly, LoopbackRouter};
use crate::turn_approval::SocketApprovalChannel;
use sovereign_core::approval_desk::ResolveOutcome;

/// Mount the turn surface. Built from `Arc<Self>` by `start_daemon`, like the
/// mesh, admin and reading routers — so a serving daemon cannot come up
/// without it and `mount_names` reports exactly what it has.
pub fn turn_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    turn_router_with(daemon, SocketTimers::default())
}

/// [`turn_router`] with the socket's two clocks supplied by the composition
/// root instead of taken from the constants.
///
/// The production values are seconds and the property they exist for — the
/// daemon CLOSES a socket the client stopped using (sv-surface RB1) — can
/// only be watched by living through one, so a test that waited out the real
/// idle window would add half a minute to the suite and a test that skipped
/// it would assert nothing. Passing them beats an env override: no ambient
/// state, no cross-test interference, and the values a socket runs on are
/// visible at the seam that built it.
pub fn turn_router_with(daemon: Arc<EmbeddedDaemon>, timers: SocketTimers) -> Router {
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
        .localhost_only_with(daemon)
        .layer(Extension(timers))
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    body: axum::body::Bytes,
) -> Response {
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Query(params): Query<ListQuery>,
) -> Response {
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(conversation_id): Path<String>,
) -> Response {
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(conversation_id): Path<String>,
) -> Response {
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(conversation_id): Path<String>,
    Json(body): Json<SendMessageRequest>,
) -> Response {
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(conversation_id): Path<String>,
) -> Response {
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Query(params): Query<SearchQuery>,
) -> Response {
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(memory_id): Path<String>,
) -> Response {
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(memory_id): Path<String>,
) -> Response {
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(body): Json<ToolOutcomeRequest>,
) -> Response {
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

/// Query for `GET /v1/conversations/{id}/stream`.
#[derive(Debug, Default, Deserialize)]
pub struct StreamParams {
    /// Claim this turn's approvals for the connecting socket
    /// (`?approvals=true`), making it the client the host puts consent
    /// questions to.
    ///
    /// Off by default, and the default is the load-bearing half: a reader
    /// that streams tokens and installs no approval handler — `svrn chat`,
    /// the bench harness, a `websocat` probe — must keep running under the
    /// daemon's own non-interactive channel. Claim it for them and the first
    /// write-effectful step turns an auto-approval into a hang. See
    /// [`crate::turn_approval`].
    #[serde(default)]
    pub approvals: bool,
}

/// `GET /v1/conversations/{id}/stream` — WebSocket upgrade.
async fn ws_handler(
    _: LocalOnly,
    ws: WebSocketUpgrade,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(conversation_id): Path<String>,
    Query(params): Query<StreamParams>,
    Extension(timers): Extension<SocketTimers>,
) -> Response {
    if daemon.runtime().is_none() {
        return service_unavailable("this daemon serves no turns (mesh-admin)");
    }
    ws.on_upgrade(move |socket| {
        handle_ws(socket, daemon, conversation_id, params.approvals, timers)
    })
}

/// How long a SETTLED socket waits for the client's next turn before the
/// daemon closes it.
///
/// A turn socket is a per-conversation resource and a client that has its
/// answer usually wants another turn on it, so the socket is not closed the
/// instant the turn settles — but it does not live forever either, which is
/// what it did until sv-surface RB1: `handle_ws` looped on `ws_rx` and
/// nothing ever ended the loop, so every completed turn left a WebSocket, a
/// writer task and an approval channel alive on BOTH ends. A client that
/// wants another turn simply sends the next request inside this window.
const IDLE_AFTER_SETTLED: Duration = Duration::from_secs(30);

/// The cap on the post-`Complete` window — how long the daemon waits for the
/// turn's own producers to let go of the frame channel before it says
/// `TurnSettled` anyway.
///
/// The wait itself is not a timer (see `Phase::Settling`); this is the bound
/// on a producer that never finishes, so one stuck post-stream spawn costs a
/// socket rather than the process. Reaching it is traced at WARN, which is
/// the instrument that says the signal was wrong rather than the window
/// (ARCH §18.4).
const POST_COMPLETE_SETTLE_MAX: Duration = Duration::from_secs(60);

/// How often the settle predicate is re-read while the post-`Complete`
/// window is open. There is no notification when a sender handle drops, so
/// this is a poll — cheap, and only between turns.
const SETTLE_POLL: Duration = Duration::from_millis(50);

/// How long the daemon gives its writer task to flush what is queued and put
/// the WebSocket Close on the wire.
const WRITER_GOODBYE: Duration = Duration::from_secs(2);

/// The two windows one turn socket runs on. [`SocketTimers::default`] is the
/// production pair; see [`turn_router_with`] for why they are a value.
#[derive(Clone, Copy, Debug)]
pub struct SocketTimers {
    /// How long a settled socket waits for the client's next turn.
    pub idle_after_settled: Duration,
    /// The cap on the post-`Complete` window.
    pub post_complete_settle_max: Duration,
}

impl Default for SocketTimers {
    fn default() -> Self {
        Self {
            idle_after_settled: IDLE_AFTER_SETTLED,
            post_complete_settle_max: POST_COMPLETE_SETTLE_MAX,
        }
    }
}

/// Where a turn socket is between turns — the half of a connection's life
/// with no turn in it, which had no representation at all until sv-surface
/// RB1 and therefore no end (`Phase::Settled` is the end).
#[derive(Clone, Copy)]
enum Phase {
    /// A turn is running, or none has started yet: the socket waits on the
    /// client with no deadline of its own.
    Open,
    /// The turn's frames are out and its post-`Complete` producers may still
    /// be holding the frame channel — `MessageRefined` and `LessonProposed`
    /// fire from a DETACHED post-stream spawn, after the terminal frame (E1),
    /// so "the turn ended" and "the host is done talking" are different
    /// moments and only the second one may close a socket.
    ///
    /// The predicate is not a quiet timer. Every producer that can still
    /// speak here must hold a clone of the frame sender, so the sender's
    /// strong count returning to its resting value IS "nobody can send here
    /// any more" — structural, and true for producers that do not exist yet
    /// (ARCH §7). `give_up_at` bounds a producer that never lets go.
    ///
    /// A new turn arriving mid-settle skips the notice: the client that
    /// started it is not waiting to hear the previous turn settle.
    Settling { give_up_at: Instant },
    /// `TurnSettled` is on the wire. The socket closes at `close_at` unless
    /// the client starts another turn.
    Settled { close_at: Instant },
}

/// The handle counts a socket has when nothing is running on it.
///
/// Measured once, before the first turn, rather than assumed: what "at rest"
/// means depends on how many per-socket sinks were built, and a hard-coded
/// number would be wrong the day a third one appears.
struct RestingHandles {
    /// Clones of the frame sender.
    senders: usize,
    /// Handles on the socket's approval channel.
    approvals: usize,
    /// Handles on the socket's routing-event sink.
    routing: usize,
}

/// How many producer handles are still out past rest — zero means nothing can
/// speak on this socket any more, which is what `TurnSettled` claims.
///
/// THREE counts, not one, and the reason is the whole correctness of RB1: the
/// post-`Complete` producers do NOT all reach the socket the same way. The
/// turn task holds a clone of the frame SENDER, but the detached post-stream
/// refinement spawn holds `Arc::clone(&approval)` — an `Arc<dyn
/// ApprovalChannel>` over this socket's channel (`streaming.rs`, the
/// KnowledgeQuery spawn and the deep-stream one) — and that channel owns ONE
/// sender for the socket's whole life. Cloning the Arc does not clone the
/// sender, so a sender-only predicate does not see the refinement spawn at
/// all: it would settle the instant the turn task's own clone dropped, and
/// `MessageRefined` would arrive AFTER a `TurnSettled` the client has already
/// read as "done" — E1's "Refining your answer forever", hidden behind a
/// frame that says the opposite. Counting the handles the producers actually
/// hold is what makes this a join instead of a guess.
fn producers_outstanding(
    frames: &mpsc::UnboundedSender<TurnFrame>,
    approvals: Option<&Arc<SocketApprovalChannel>>,
    routing: &Arc<dyn sovereign_core::traits::RoutingEventSink>,
    resting: &RestingHandles,
) -> usize {
    frames.strong_count().saturating_sub(resting.senders)
        + approvals
            .map(Arc::strong_count)
            .unwrap_or(0)
            .saturating_sub(resting.approvals)
        + Arc::strong_count(routing).saturating_sub(resting.routing)
}

/// Mint this turn's id and re-base the socket channel's prompt ids on it.
///
/// The channel is per SOCKET and a socket runs turns one after another, so
/// unprefixed ids are step counters: `step:0` on the second turn is spelled
/// exactly like `step:0` on the first, and a late answer to a question that
/// has already gone resolves the CURRENT turn's question instead
/// (sv-surface C2). Minted where the turn is spawned, which is the one place
/// that knows a turn is starting.
fn begin_turn(approvals: Option<&Arc<SocketApprovalChannel>>) {
    if let Some(channel) = approvals {
        let nonce = uuid::Uuid::new_v4().to_string();
        tracing::debug!(nonce = %nonce, "turn_http: a turn begins — prompt ids re-based on it");
        channel.begin_turn(&nonce);
    }
}

async fn handle_ws(
    socket: WebSocket,
    daemon: Arc<EmbeddedDaemon>,
    conversation_id: String,
    claim_approvals: bool,
    timers: SocketTimers,
) {
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
    // The goodbye. `ws_tx` lives in the writer task, so the loop cannot put a
    // Close frame on the wire itself — it asks, and the writer flushes what is
    // still queued before saying it (sv-surface RB1).
    let (close_tx, mut close_rx) = tokio::sync::oneshot::channel::<()>();
    let tx_handle = tokio::spawn(async move {
        // The writer is the ONE place every frame passes through, which is why
        // it — not the loop that decides to settle — stamps `TurnSettled` with
        // the turn's message id: FIFO makes "the Complete has already been
        // written" exact here, where a cell shared with the loop would race the
        // flush and stamp an empty id or a stale one.
        let mut last_complete = String::new();
        let mut closing = false;
        loop {
            let next = if closing {
                out_rx.try_recv().ok()
            } else {
                tokio::select! {
                    // Queued frames first: a client's last notices reach it
                    // before the goodbye does.
                    biased;
                    frame = out_rx.recv() => frame,
                    _ = &mut close_rx => {
                        closing = true;
                        out_rx.try_recv().ok()
                    }
                }
            };
            let Some(frame) = next else {
                if closing {
                    let _ = ws_tx.send(Message::Close(None)).await;
                }
                break;
            };
            if let TurnFrame::Complete { message_id, .. } = &frame {
                last_complete = message_id.clone();
            }
            let frame = if matches!(
                &frame,
                TurnFrame::Notice {
                    notice: TurnNotice::TurnSettled { message_id }
                } if message_id.is_empty()
            ) {
                TurnFrame::Notice {
                    notice: TurnNotice::TurnSettled {
                        message_id: last_complete.clone(),
                    },
                }
            } else {
                frame
            };
            let Ok(json) = serde_json::to_string(&frame) else {
                continue;
            };
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                break; // client disconnected
            }
        }
    });

    // This socket's approval channel, when it claimed them. Per SOCKET and
    // per turn — not a registry keyed by conversation — so another socket's
    // reply has no map to reach these questions through, and a hangup drops
    // the whole channel with the task rather than leaving entries to purge
    // (`turn_approval`'s module docs).
    let approvals = claim_approvals
        .then(|| Arc::new(SocketApprovalChannel::new(out_tx.clone(), &conversation_id)));

    // This socket's routing-event sink (sv-surface G7) — narration, the
    // interpretation banner and the clarification card all leave as frames
    // on THIS socket, installed per turn via `scope_routing_events`: the
    // same per-turn capability shape approval took in C1. A process-wide
    // sink (the runtime's commissioned member) cannot tell which socket's
    // conversation a banner belongs to, and the broadcast bridge the
    // server uses would need a conversation→socket registry to work out
    // what the turn already knows. Unlike approvals this is UNCONDITIONAL
    // — routing events are notices, owed no answer, so there is no claim
    // to forget to make.
    let routing_events: Arc<dyn sovereign_core::traits::RoutingEventSink> =
        Arc::new(SocketRoutingEvents {
            frames: out_tx.clone(),
        });

    // What this socket looks like with nothing running on it. Read HERE,
    // after both sinks exist and before any turn, so `producers_outstanding`
    // is comparing against a measurement rather than a remembered number.
    let resting = RestingHandles {
        senders: out_tx.strong_count(),
        approvals: approvals.as_ref().map(Arc::strong_count).unwrap_or(0),
        routing: Arc::strong_count(&routing_events),
    };

    // The turn's own approval capability, read once per turn and installed
    // around the WHOLE call by each arm that starts one: the executor is built
    // during the ACQUIRE, and a scope that began at the stream handle would
    // leave the turn's steps reading the daemon's commissioned channel
    // instead. `None` is a socket that claimed nothing, and it runs exactly as
    // it did before any of this existed.
    let claimed_channel = || {
        approvals
            .as_ref()
            .map(|a| Arc::clone(a) as Arc<dyn sovereign_core::traits::ApprovalChannel>)
    };

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
    let mut phase = Phase::Open;
    // Why the loop ended: the client hung up, or this daemon closed an idle
    // socket. Only the second one owes a goodbye.
    let mut idle_close = false;
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
        // Copied out of `phase` rather than borrowed from it, so the arms
        // below are free to reassign it.
        let tick_at = match phase {
            Phase::Open => None,
            Phase::Settling { .. } => Some(Instant::now() + SETTLE_POLL),
            Phase::Settled { close_at } => Some(close_at),
        };
        let phase_tick = async move {
            match tick_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        let incoming = tokio::select! {
            _ = turn_finished => {
                in_flight = None;
                // The turn is over; the socket is not. Its post-`Complete`
                // producers get the window (E1) before anything closes.
                phase = Phase::Settling {
                    give_up_at: Instant::now() + timers.post_complete_settle_max,
                };
                continue;
            }
            () = phase_tick => {
                match phase {
                    Phase::Settling { give_up_at } => {
                        let outstanding = producers_outstanding(
                            &out_tx,
                            approvals.as_ref(),
                            &routing_events,
                            &resting,
                        );
                        let out_of_time = Instant::now() >= give_up_at;
                        if outstanding > 0 && !out_of_time {
                            continue;
                        }
                        if outstanding > 0 {
                            tracing::warn!(
                                conversation_id = %conversation_id,
                                outstanding,
                                "turn_http: settling anyway — a post-turn producer still holds \
                                 this socket's frame channel past the cap"
                            );
                        } else {
                            tracing::info!(
                                conversation_id = %conversation_id,
                                "turn_http: the turn settled — every producer let go of the socket"
                            );
                        }
                        // The writer stamps the message id; an empty one here
                        // is the ASK, not a claim about the turn.
                        let _ = out_tx.send(TurnFrame::Notice {
                            notice: TurnNotice::TurnSettled {
                                message_id: String::new(),
                            },
                        });
                        phase = Phase::Settled {
                            close_at: Instant::now() + timers.idle_after_settled,
                        };
                        continue;
                    }
                    Phase::Settled { .. } => {
                        tracing::info!(
                            conversation_id = %conversation_id,
                            idle_secs = timers.idle_after_settled.as_secs_f32(),
                            "turn_http: closing a settled socket the client did not reuse"
                        );
                        idle_close = true;
                        break;
                    }
                    // Unreachable: `Open` produces no tick.
                    Phase::Open => continue,
                }
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
        // The client is alive: any request it sends refreshes the idle
        // clock, and one that starts a turn replaces the clock with the turn.
        if let Phase::Settled { .. } = phase {
            phase = Phase::Settled {
                close_at: Instant::now() + timers.idle_after_settled,
            };
        }
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
                if refuse_second_turn(&in_flight, &out_tx) {
                    continue;
                }
                begin_turn(approvals.as_ref());
                phase = Phase::Open;
                let (rt, st, cid, tx) = (
                    Arc::clone(&runtime),
                    Arc::clone(&store),
                    conversation_id.clone(),
                    out_tx.clone(),
                );
                let turn_approval = claimed_channel();
                let routing = Arc::clone(&routing_events);
                in_flight = Some(tokio::spawn(async move {
                    sovereign_core::runtime::capabilities::scope_turn(
                        turn_approval,
                        Some(routing),
                        async {
                            serve_turn(
                                &rt,
                                st.as_ref(),
                                &cid,
                                &content,
                                mode,
                                intent,
                                // See the module docs — the daemon has no
                                // narration broadcast yet, and a `None`
                                // here is that fact rather than a dropped
                                // channel.
                                None,
                                &tx,
                            )
                            .await;
                        },
                    )
                    .await;
                }));
            }
            // The two SESSION-CONTINUATION turns. Each differs from `Message`
            // only in its ACQUIRE — a synthetic classification the runtime
            // owns one implementation of, plus redirect's sampler cancel and
            // routing-signal write — and is identical to it afterwards: same
            // one-turn guard, same spawn (the receive loop must keep polling
            // or the socket answers no pings), same approval scope, same ONE
            // drain. `serve_turn` cannot serve them because its acquire is the
            // router's; `drive_stream_handle` is its post-acquire half,
            // factored for exactly these two callers (sv-surface rung 0).
            //
            // Until this existed the daemon could answer a question but not a
            // CLARIFICATION of one, so an attached surface had to keep a
            // `Runtime` behind its own cards — which is the construction rung
            // 6 is deleting.
            TurnRequest::Resume {
                content,
                session_id,
                intent_hint,
            } => {
                if refuse_second_turn(&in_flight, &out_tx) {
                    continue;
                }
                begin_turn(approvals.as_ref());
                phase = Phase::Open;
                // A resume names its session for PROVENANCE — the resume
                // acquire reads nothing out of it, only quotes the id in the
                // synthetic classification's rationale — so an id the daemon
                // no longer holds runs normally. That is deliberate and it is
                // the in-process behaviour: a card sits in a transcript, the
                // 30s GC fires, the daemon restarts, and the click must still
                // answer or rung 6 has traded the divergence it is closing for
                // a new one. A FOREIGN session is the different case: the
                // claim "this continues session X" would be written into THIS
                // conversation's routing metadata and be false.
                if let NamedSession::Foreign =
                    named_session(&runtime, &session_id, &conversation_id)
                {
                    tracing::warn!(
                        session_id = %session_id,
                        conversation_id = %conversation_id,
                        "turn_http: resume refused — session belongs to another conversation"
                    );
                    let _ = out_tx.send(TurnFrame::StreamError {
                        message: not_this_sockets_session(&session_id, &conversation_id),
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
                let turn_approval = claimed_channel();
                let routing = Arc::clone(&routing_events);
                in_flight = Some(tokio::spawn(async move {
                    sovereign_core::runtime::capabilities::scope_turn(
                        turn_approval,
                        Some(routing),
                        async {
                            let resume = ResumeSession {
                                session_id,
                                intent_hint,
                            };
                            drive_acquired(
                                rt.resume_session_stream(&content, &cid, resume).await,
                                st.as_ref(),
                                &cid,
                                &tx,
                            )
                            .await;
                        },
                    )
                    .await;
                }));
            }
            TurnRequest::Redirect {
                session_id,
                intent_hint,
            } => {
                if refuse_second_turn(&in_flight, &out_tx) {
                    continue;
                }
                begin_turn(approvals.as_ref());
                phase = Phase::Open;
                // Redirect resolves the turn's MESSAGE and its CONVERSATION
                // off the session, so here the id is a KEY and both ways of
                // missing are refused by name. `Foreign` is the one that would
                // otherwise run a turn in someone else's conversation and
                // stream it down this socket — the same property C1 gave
                // approvals, that a socket cannot reach past its own
                // conversation, applied to the other half of the protocol.
                match named_session(&runtime, &session_id, &conversation_id) {
                    NamedSession::Ours => {}
                    NamedSession::Foreign => {
                        tracing::warn!(
                            session_id = %session_id,
                            conversation_id = %conversation_id,
                            "turn_http: redirect refused — session belongs to another conversation"
                        );
                        let _ = out_tx.send(TurnFrame::StreamError {
                            message: not_this_sockets_session(&session_id, &conversation_id),
                            retry_after_secs: None,
                        });
                        continue;
                    }
                    NamedSession::Gone => {
                        tracing::warn!(
                            session_id = %session_id,
                            conversation_id = %conversation_id,
                            "turn_http: redirect refused — no live session by that id"
                        );
                        let _ = out_tx.send(TurnFrame::StreamError {
                            // Refused rather than re-answered from anything
                            // else this socket knows: a redirect with no
                            // message to re-answer has nothing to substitute
                            // that would not be invented (§18.3).
                            message: format!(
                                "no live session {session_id} on this daemon — a session is \
                                 dropped ~30s after its turn ends, and a redirect re-answers \
                                 the message it holds"
                            ),
                            retry_after_secs: None,
                        });
                        continue;
                    }
                }
                let (rt, st, cid, tx) = (
                    Arc::clone(&runtime),
                    Arc::clone(&store),
                    conversation_id.clone(),
                    out_tx.clone(),
                );
                let turn_approval = claimed_channel();
                let routing = Arc::clone(&routing_events);
                in_flight = Some(tokio::spawn(async move {
                    sovereign_core::runtime::capabilities::scope_turn(
                        turn_approval,
                        Some(routing),
                        async {
                            drive_acquired(
                                rt.redirect_turn_stream(&session_id, &intent_hint).await,
                                st.as_ref(),
                                // The socket's conversation, which the guard
                                // above proved is also the session's — so the
                                // terminal metadata is read from the row the
                                // turn wrote.
                                &cid,
                                &tx,
                            )
                            .await;
                        },
                    )
                    .await;
                }));
            }
            // G2 (sv-surface): the cancel runs the SAME pair the
            // in-process host ran — trip the conversation's session
            // token AND the preparing token — because a task abort would
            // kill the turn without the terminal frame, and a cancel is a
            // fact about how the turn ENDED: the sampler notices the
            // token, stops, and the ordinary Complete carries
            // finish_reason=cancelled down this socket. Both deciders
            // live on the runtime this daemon owns.
            TurnRequest::Cancel {} => {
                let hit_session = runtime
                    .sessions
                    .latest_for_conversation(&conversation_id)
                    .map(|session| {
                        session.cancel.cancel();
                        session.id.clone()
                    });
                let hit_preparing = runtime.sessions.cancel_preparing(&conversation_id);
                // AND unpark the desk. A turn stopped on a consent card is
                // blocked on a `oneshot`, and a cancellation token it will
                // never poll again cannot reach it: without this the executor
                // never returns, no terminal frame is ever emitted, and the
                // user's stop button does nothing at all (sv-surface RB2).
                // The policy is E2's, per prompt KIND, and it is the same one
                // a hangup applies below — one rule, two ways of losing the
                // answerer.
                let unparked = approvals.as_ref().map(|a| a.abandon()).unwrap_or(0);
                if hit_session.is_some() || hit_preparing || unparked > 0 {
                    tracing::info!(
                        conversation_id = %conversation_id,
                        session = ?hit_session,
                        preparing = hit_preparing,
                        unparked,
                        "turn_http: turn cancelled by the client"
                    );
                } else {
                    tracing::debug!(
                        conversation_id = %conversation_id,
                        "turn_http: cancel with nothing in flight — a satisfied no-op"
                    );
                }
            }
            // Rung 6 commit C1 made the refusal a resolve; R1 folded the
            // two reply variants into `Answer { id, answer }` — the id the
            // Prompt arrived under, which is the desk key. Every outcome
            // that is not "the executor is running again" is SAID, because
            // a client that sent an approval and got no frame cannot tell
            // "granted" from "never arrived" (ARCH §18.3). A socket is the
            // address, and this one's questions are the only ones it can
            // reach.
            TurnRequest::Answer { id, answer } => {
                let outcome = match approvals.as_ref() {
                    Some(a) => a.submit(&id, &answer),
                    // Not `NoSuchPending`: this socket owns no desk at all,
                    // which is a claim the client can go and make.
                    None => ResolveOutcome::Unclaimed,
                };
                if outcome == ResolveOutcome::Resolved {
                    // G3b: a search-built information answer carries
                    // its registry rows; the host folds them into the
                    // conversation's cumulative searched_sources as
                    // part of THIS resolve — one user action, one
                    // atomic effect. Soft-fail like the desktop's
                    // in-process copy did: a registry miss costs the
                    // model cumulative-URL awareness for the turn,
                    // never the answer itself.
                    if let TurnAnswer::Information { ref sources, .. } = answer {
                        if !sources.is_empty() {
                            fold_searched_sources(store.as_ref(), &conversation_id, sources).await;
                        }
                    }
                } else if let Some(refusal) = refusal_sentence(outcome, answer_kind(&answer)) {
                    // The daemon's own record of what the client got wrong.
                    // It is a TRACE and not a frame because the frame this
                    // arm sends is the same one either way — see below.
                    tracing::debug!(
                        conversation_id = %conversation_id,
                        id = %id,
                        ?outcome,
                        %refusal,
                        "turn_http: an answer resolved nothing"
                    );
                }
                // G8: EVERY outcome is said, and every one of them rides the
                // NOTICE. A client that answered and heard nothing cannot
                // tell "accepted" from "never arrived" — the bool this
                // replaced collapsed exactly that (§18.3) — and a refusal
                // that rode `StreamError` was worse than silence: both
                // clients treat that frame as TERMINAL, so a double-clicked
                // approve discarded a turn that was running perfectly well
                // (sv-surface RB4). `StreamError` is for a turn that DIED.
                let _ = out_tx.send(TurnFrame::Notice {
                    notice: TurnNotice::ResolveAck { id, outcome },
                });
            }
        }
    }

    if let Some(h) = in_flight {
        // The client hung up mid-turn. Nothing is left to receive the frames,
        // and a turn whose sink is gone is work nobody asked to keep.
        //
        // BEFORE the abort, resolve what the turn is parked on by the same
        // per-kind policy a cancel applies (E2, sv-surface C13): the pre-send
        // window already cancelled a question the socket died before
        // receiving, but a question already SHOWN and parked went through
        // neither — it was torn down by this abort with no policy applied at
        // all, so an information request that should have read as the user's
        // SKIP died as a cancel.
        if let Some(channel) = approvals.as_ref() {
            channel.abandon();
        }
        h.abort();
    }
    if idle_close {
        // Ask the writer for the goodbye, and give it long enough to flush
        // what is queued. A client that already left makes this a no-op.
        let _ = close_tx.send(());
        if tokio::time::timeout(WRITER_GOODBYE, tx_handle)
            .await
            .is_err()
        {
            tracing::debug!(
                conversation_id = %conversation_id,
                "turn_http: the writer did not finish its goodbye in time"
            );
        }
    } else {
        tx_handle.abort();
    }
}

/// The one-turn-per-socket guard, shared by the three variants that START a
/// turn. Returns whether the request was refused.
///
/// Refused, not queued: a client that sent a second turn and heard nothing
/// cannot tell "queued" from "lost". The guard is what keeps that true without
/// the receive loop having to block to enforce it — see the loop's own comment
/// for why blocking is not available here.
fn refuse_second_turn(
    in_flight: &Option<tokio::task::JoinHandle<()>>,
    out: &mpsc::UnboundedSender<TurnFrame>,
) -> bool {
    if in_flight.is_none() {
        return false;
    }
    let _ = out.send(TurnFrame::StreamError {
        message: "a turn is already in flight on this socket".to_string(),
        retry_after_secs: None,
    });
    true
}

/// Where a `session_id` a client named sits relative to THIS socket.
///
/// The socket's conversation is pinned by its URL and a `QuerySession` carries
/// its own, so "is this session mine to name?" is one lookup. It is the same
/// property C1 gave approvals — a socket cannot reach past its own
/// conversation — stated for the half of the protocol that names sessions.
enum NamedSession {
    /// Live, on this socket's own conversation.
    Ours,
    /// Live, on a different conversation. Never this socket's to drive.
    Foreign,
    /// No live session by that id: expired (`SESSION_RETENTION`, 30s past its
    /// turn), dropped by a daemon restart, or never real. One state, because
    /// nothing downstream would do anything different with the three.
    Gone,
}

fn named_session(runtime: &Runtime, session_id: &str, conversation_id: &str) -> NamedSession {
    match runtime.sessions.get(session_id) {
        None => NamedSession::Gone,
        Some(s) if s.conversation_id == conversation_id => NamedSession::Ours,
        Some(_) => NamedSession::Foreign,
    }
}

/// The refusal owed to a client that named a session belonging to someone
/// else's conversation. Names the session and THIS socket's conversation; the
/// owning conversation's id is not the client's to learn from a refusal.
fn not_this_sockets_session(session_id: &str, conversation_id: &str) -> String {
    format!(
        "session {session_id} belongs to another conversation — this socket serves \
         {conversation_id}, and a session is only nameable on the conversation that owns it"
    )
}

/// Drive a turn whose handle was acquired through a richer path than
/// `serve_turn`'s own — session resume and session redirect, whose acquires
/// carry a synthetic classification (and, for redirect, a sampler cancel and a
/// routing-signal write) the plain path has no place for.
///
/// What must never be re-derived is everything AFTER the acquire, so this hands
/// the handle straight to `drive_stream_handle` — THE drain (ARCH §10.6). The
/// recorded lesson is specific: the desktop's hand-rolled drains for these same
/// two calls were missing `present_answer` envelope stripping and the graceful
/// guards the plain path had learned, because a re-derived loop reproduces the
/// gaps of the loop it re-derives (sv-surface rung 0).
///
/// A failed acquire is SAID in the host's own words rather than dropped
/// (§18.3): a client that asked for a turn and received no frame cannot tell
/// "refused" from "lost".
///
/// Takes the acquire's RESULT rather than the acquire itself, which is not a
/// style choice: an `impl Future` parameter means the caller builds that future
/// on its own stack and then moves it through this frame, the capability scope
/// and the spawn — and a debug-built turn future is large enough that the moves
/// alone overflowed the test thread (watched, both continuation turns, before
/// this signature). Awaiting at the call site leaves the big future in one
/// place and passes a `Result<StreamHandle>`.
async fn drive_acquired(
    acquired: sovereign_core::Result<StreamHandle>,
    store: &dyn StateStore,
    conversation_id: &str,
    out: &mpsc::UnboundedSender<TurnFrame>,
) {
    match acquired {
        Ok(handle) => {
            // `None` narration, like the plain turn: the daemon owns no
            // per-connection subscription yet, and this is that fact rather
            // than a dropped channel (see the module docs' known gap).
            drive_stream_handle(handle, store, conversation_id, None, out).await;
        }
        Err(e) => {
            tracing::error!(
                conversation_id = %conversation_id,
                error = %e,
                "turn_http: continuation turn failed to start"
            );
            let _ = out.send(TurnFrame::StreamError {
                message: e.to_string(),
                retry_after_secs: None,
            });
        }
    }
}

/// The daemon's own words for a reply that resolved nothing, or `None` when
/// it resolved a parked question and the turn is running again.
///
/// A TRACE, not a frame, since sv-surface RB4: the wire answer to an answer
/// is `Notice::ResolveAck`, whose typed [`ResolveOutcome`] is what a client
/// branches on. This is what the daemon's log says about the same event, and
/// it is where the sentence a person reads is spelled once.
fn refusal_sentence(outcome: ResolveOutcome, kind: &str) -> Option<String> {
    match outcome {
        ResolveOutcome::Resolved => None,
        ResolveOutcome::NoSuchPending => Some(format!(
            "no {kind} is pending on this socket — it was already answered, or \
             the turn ended"
        )),
        ResolveOutcome::WrongKind => Some(format!(
            "this turn is waiting on the other kind of answer, not a {kind}"
        )),
        ResolveOutcome::WaiterGone => Some(format!(
            "the {kind} arrived after its turn had already let the question go"
        )),
        ResolveOutcome::Unclaimed => Some(format!(
            "this socket did not claim its turn's approvals — reconnect with \
             `?approvals=true` to receive and answer them (a {kind} on an \
             unclaimed socket resolves nothing)"
        )),
    }
}

/// How to name the answer in a refusal — the one place the wire's answer
/// kinds map to the words a client reads.
fn answer_kind(answer: &TurnAnswer) -> &'static str {
    match answer {
        TurnAnswer::Approved(_) => "approval",
        TurnAnswer::Text(_) => "user reply",
        TurnAnswer::Information { .. } => "information response",
    }
}

/// G3b's daemon half: fold an answer's search-registry rows into the
/// conversation, through the ONE merge (`sovereign_core::searched_sources`)
/// the desktop's in-process path also uses. Soft-fail by the same bargain
/// as the desktop copy — the registry is model awareness, never
/// correctness.
async fn fold_searched_sources(
    store: &dyn sovereign_core::traits::StateStore,
    conversation_id: &str,
    sources: &[sovereign_contracts::types::SearchedSourceEntry],
) {
    let fresh = sources
        .iter()
        .map(|s| (s.url.clone(), s.title.clone(), s.search_query.clone()));
    match store.get_conversation(conversation_id).await {
        Ok(conv) => {
            let current_turn = conv.messages.len();
            let merged = sovereign_core::searched_sources::merge_into(
                conv.searched_sources,
                fresh,
                current_turn,
            );
            if let Err(e) = store
                .set_conversation_searched_sources(conversation_id, Some(merged))
                .await
            {
                tracing::warn!(
                    conversation_id = %conversation_id,
                    error = %e,
                    "turn_http: failed to persist searched_sources — the answer resolved, the model loses cumulative-URL awareness this turn"
                );
            }
        }
        Err(e) => {
            tracing::debug!(
                conversation_id = %conversation_id,
                error = %e,
                "turn_http: could not load the conversation for searched_sources — skipping"
            );
        }
    }
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

// ─── The socket's routing-event sink (sv-surface G7) ──────────────────────
//
// Narration, the interpretation banner and the clarification card all leave
// as frames on THIS socket. Installed per turn via
// `capabilities::scope_routing_events` — the per-turn capability shape
// approval took in C1 — because the process-wide `routing_events` member
// cannot tell which socket's conversation a banner belongs to, and the
// broadcast bridge `sovereign-server` uses (narration.rs) would need a
// conversation→socket registry to work out what the turn already knows.

/// The routing-event sink of ONE turn socket: one frame channel, three
/// frame shapes, no answer owed.
struct SocketRoutingEvents {
    frames: mpsc::UnboundedSender<TurnFrame>,
}

impl SocketRoutingEvents {
    /// Say a Notice. Owed no answer, so a closed socket costs a trace.
    fn notice(&self, notice: TurnNotice) {
        if self.frames.send(TurnFrame::Notice { notice }).is_err() {
            tracing::debug!("turn_http: routing event dropped — the socket is closed");
        }
    }
}

#[async_trait::async_trait]
impl sovereign_core::traits::RoutingEventSink for SocketRoutingEvents {
    async fn emit_interpretation_proposed(&self, payload: InterpretationProposed) {
        self.notice(TurnNotice::InterpretationProposed(payload));
    }

    async fn emit_clarification_request(&self, payload: ClarificationRequest) {
        self.notice(TurnNotice::ClarificationRequest(payload));
    }

    /// E3 (sv-surface): narration keeps its OWN frame — it has real readers
    /// today, and folding it into Notice would be a wire break for no gain.
    /// `message_id` is empty: routing narration emits before a stream handle
    /// exists (same as the `TurnFrame::Narration` doc's "before the stream
    /// handle is acquired" case), and the socket already knows which
    /// conversation it serves — the payload's session/conversation ids are
    /// addressing for a broadcast, which this is not.
    async fn emit_turn_narration(&self, payload: TurnNarration) {
        let _ = self.frames.send(TurnFrame::Narration {
            message_id: String::new(),
            phase: payload.event.phase,
            text: payload.event.text,
            elapsed_ms: payload.event.elapsed_ms,
        });
    }
}

#[cfg(test)]
mod settle_predicate_tests {
    use super::*;

    fn socket() -> (
        mpsc::UnboundedSender<TurnFrame>,
        mpsc::UnboundedReceiver<TurnFrame>,
        Arc<SocketApprovalChannel>,
        Arc<dyn sovereign_core::traits::RoutingEventSink>,
        RestingHandles,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let approvals = Arc::new(SocketApprovalChannel::new(tx.clone(), "conv-a"));
        let routing: Arc<dyn sovereign_core::traits::RoutingEventSink> =
            Arc::new(SocketRoutingEvents { frames: tx.clone() });
        let resting = RestingHandles {
            senders: tx.strong_count(),
            approvals: Arc::strong_count(&approvals),
            routing: Arc::strong_count(&routing),
        };
        (tx, rx, approvals, routing, resting)
    }

    /// At rest, nothing is outstanding — the measurement is of THIS socket,
    /// not of a remembered number.
    #[test]
    fn a_socket_with_no_turn_on_it_has_nothing_outstanding() {
        let (tx, _rx, approvals, routing, resting) = socket();
        assert_eq!(
            producers_outstanding(&tx, Some(&approvals), &routing, &resting),
            0
        );
    }

    /// **The post-stream refinement spawn's shape**, and the reason the
    /// predicate counts three things.
    ///
    /// `streaming.rs` carries the turn's approval channel into a DETACHED
    /// spawn as `Arc::clone(&approval)` — an `Arc<dyn ApprovalChannel>` — and
    /// emits `MessageRefined` through it after the terminal frame. The
    /// channel owns ONE frame sender for the socket's whole life, so that
    /// clone does not raise the SENDER count by anything: a predicate reading
    /// only `frames.strong_count()` reports 0 outstanding while this holder
    /// is still alive and about to speak.
    ///
    /// Named failing input (§18.1): drop the `approvals` term from
    /// `producers_outstanding` and the assertion below reads 0 — the daemon
    /// then says `TurnSettled` before the refinement, and the client that
    /// stops on that bookend never repaints the bubble (E1/G4).
    #[tokio::test]
    async fn a_detached_holder_of_the_approval_channel_is_outstanding() {
        let (tx, mut rx, approvals, routing, resting) = socket();

        let held = Arc::clone(&approvals) as Arc<dyn sovereign_core::traits::ApprovalChannel>;
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let spawned = tokio::spawn(async move {
            let _ = release_rx.await;
            held.emit_message_refined(sovereign_contracts::types::MessageRefinedPayload {
                conversation_id: "conv-a".into(),
                message_id: "m1".into(),
                new_content: "the revised answer.".into(),
            });
        });

        assert_eq!(
            producers_outstanding(&tx, Some(&approvals), &routing, &resting),
            1,
            "the detached holder is counted while it can still speak"
        );

        release_tx.send(()).unwrap();
        spawned.await.unwrap();
        assert!(
            matches!(
                rx.try_recv(),
                Ok(TurnFrame::Notice {
                    notice: TurnNotice::MessageRefined(_)
                })
            ),
            "and it did speak — after the point a sender-only predicate would have settled"
        );
        assert_eq!(
            producers_outstanding(&tx, Some(&approvals), &routing, &resting),
            0,
            "and once it is gone the socket is settled"
        );
    }

    /// The other two carriers, so no term is load-free: a turn task's frame
    /// sender clone, and a per-turn clone of the routing sink.
    #[test]
    fn a_turn_task_sender_and_a_routing_sink_clone_are_both_counted() {
        let (tx, _rx, approvals, routing, resting) = socket();
        let turn_tx = tx.clone();
        assert_eq!(
            producers_outstanding(&tx, Some(&approvals), &routing, &resting),
            1
        );
        let turn_routing = Arc::clone(&routing);
        assert_eq!(
            producers_outstanding(&tx, Some(&approvals), &routing, &resting),
            2
        );
        drop(turn_tx);
        drop(turn_routing);
        assert_eq!(
            producers_outstanding(&tx, Some(&approvals), &routing, &resting),
            0
        );
    }
}

#[cfg(test)]
mod routing_event_sink_tests {
    use super::*;
    use sovereign_contracts::types::{NarrationEvent, NarrationPhase};
    use sovereign_core::traits::RoutingEventSink;

    fn sink() -> (SocketRoutingEvents, mpsc::UnboundedReceiver<TurnFrame>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (SocketRoutingEvents { frames: tx }, rx)
    }

    /// The banner and the card ride Notice with their payloads intact; the
    /// id fields survive because Resume needs them — the client's redirect
    /// click names the session the payload carried.
    #[tokio::test]
    async fn the_two_cards_ride_notice_with_payloads_intact() {
        let (sink, mut rx) = sink();
        sink.emit_interpretation_proposed(InterpretationProposed {
            session_id: "s1".into(),
            conversation_id: "c1".into(),
            interpretation: "reading as an overview".into(),
            alternatives: Vec::new(),
            confidence: 0.55,
        })
        .await;
        sink.emit_clarification_request(ClarificationRequest {
            session_id: "s1".into(),
            conversation_id: "c1".into(),
            question: "which way?".into(),
            options: Vec::new(),
        })
        .await;

        let first = rx.try_recv().unwrap();
        let TurnFrame::Notice {
            notice: TurnNotice::InterpretationProposed(p),
        } = &first
        else {
            panic!("the banner rides Notice; got {first:?}")
        };
        assert_eq!((p.session_id.as_str(), p.confidence), ("s1", 0.55));

        let second = rx.try_recv().unwrap();
        let TurnFrame::Notice {
            notice: TurnNotice::ClarificationRequest(c),
        } = &second
        else {
            panic!("the card rides Notice; got {second:?}")
        };
        assert_eq!(c.session_id, "s1", "the resume key crosses intact");
    }

    /// Narration stays the Narration frame (E3) — its phase/text/elapsed
    /// map one-to-one, and its message_id is honestly empty.
    #[tokio::test]
    async fn narration_keeps_its_own_frame() {
        let (sink, mut rx) = sink();
        sink.emit_turn_narration(TurnNarration {
            session_id: "s1".into(),
            conversation_id: "c1".into(),
            event: NarrationEvent {
                phase: NarrationPhase::RetrievalStart,
                text: "Reading 12 chunks".into(),
                elapsed_ms: 340,
            },
        })
        .await;

        assert_eq!(
            rx.try_recv().unwrap(),
            TurnFrame::Narration {
                message_id: String::new(),
                phase: NarrationPhase::RetrievalStart,
                text: "Reading 12 chunks".into(),
                elapsed_ms: 340,
            },
            "not a Notice — folding narration would break its real readers"
        );
    }
}
