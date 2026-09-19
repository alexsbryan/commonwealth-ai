// SPDX-License-Identifier: AGPL-3.0-or-later
//! Notes CRUD over the wire — `/v1/notes/…` (sv-surface D6, first half).
//!
//! Six routes over [`crate::daemon::EmbeddedDaemon::notes_store`] — the same
//! `notes.db` `/mcp` and `POST /v1/notes/tool-outcome` already write, and
//! which the desktop's lesson pane opened a SECOND handle on. `retire` is the
//! sixth because a supersede writes the successor AND retires the
//! predecessor, and a retired row survives struck through where a delete does
//! not. The LIST is a POST: `read_notes` filters on three lists whose members
//! may contain commas, so no flat query string expresses it (§10.6).
//!
//! Loopback posture is `reading_http`'s, unchanged.
//!
//! Stays in the caller: the LESSON semantics — per-rung supersede, the
//! `payload_json` → `LessonRow` projection, the `drafted_display` consent
//! rule — every one a fold over bytes this store treats as opaque.

use std::sync::Arc;

use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use corpus_engine_notes::{Note, NoteScope, NoteSource, NoteStore};

use crate::daemon::EmbeddedDaemon;
use crate::http_response::{bad_request, internal_error, not_found, Absence};
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

/// Rows a list read returns when the caller names no limit. The store
/// caps internally at 100; a caller that wants the whole history says so
/// (`list_lessons` asks for 500).
const LIST_LIMIT_DEFAULT: usize = 50;

// ─── The wire projection ───────────────────────────────────────

/// One note on the wire. Defined in `sovereign-contracts` so a client can
/// parse a note without linking this crate; re-exported here so the routes
/// below and their tests keep naming it at this path (sv-surface svt-3).
///
/// The PROJECTION stays here — see [`note_entry`]. `corpus_engine_notes::Note`
/// is a store type three layers above `sovereign-contracts`, so the `From`
/// impl cannot travel with the struct.
pub use sovereign_contracts::daemon_wire::NoteEntry;

/// Project a stored note onto the wire shape.
///
/// A free function rather than `impl From<Note> for NoteEntry`: both types
/// are foreign to this crate now that [`NoteEntry`] lives in
/// `sovereign-contracts`, and the orphan rule forbids the impl. Still the ONE
/// projection for the family — the two routes below are its only callers.
pub fn note_entry(n: Note) -> NoteEntry {
    NoteEntry {
        id: n.id,
        kind: n.kind,
        content: n.content,
        symbols: n.symbols,
        files: n.files,
        session_id: n.session_id,
        created_at: n.created_at,
        tool_name: n.tool_name,
        retired_at: n.retired_at,
        retired_by: n.retired_by,
        scope: n.scope,
        feature_id: n.feature_id,
        promoted_from: n.promoted_from,
        related_entity: n.related_entity,
        source: n.source,
        supersedes: n.supersedes,
        payload_json: n.payload_json,
        origin_node_id: n.origin_node_id,
        sent_at: n.sent_at,
        received_at: n.received_at,
    }
}

// ─── Request / response shapes ─────────────────────────────────

/// Body of `POST /v1/notes/query` — `NoteStore::read_notes`'s six
/// arguments. Every key defaults, so `{}` is "the most recent notes,
/// unfiltered".
#[derive(Debug, Default, Deserialize)]
pub struct NoteQuery {
    /// Free-text relevance query. Absent = order by recency, newest first.
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub symbols: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// `false` (the default) hides retired rows. The lesson pane passes
    /// `true` — it is the trust story and renders the whole supersede
    /// chain.
    #[serde(default)]
    pub include_retired: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NoteListResponse {
    pub notes: Vec<NoteEntry>,
}

/// `POST /v1/notes` — the twelve arguments of
/// `NoteStore::write_note_full_v9`, which is the store's single write
/// chokepoint. Nothing here is defaulted on the caller's behalf beyond
/// the serde defaults named below; `scope` and `source` are REQUIRED
/// because guessing either is how a note ends up on the mesh that was
/// meant to stay local.
#[derive(Debug, Deserialize)]
pub struct CreateNoteRequest {
    pub kind: String,
    pub content: String,
    #[serde(default)]
    pub symbols: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    pub session_id: String,
    /// `"global"` | `"feature"` | `"session"`.
    pub scope: String,
    #[serde(default)]
    pub feature_id: Option<String>,
    #[serde(default)]
    pub related_entity: Option<String>,
    /// `"agent"` | `"committed"` | `"extracted"` | `"inferred"` | `"observed"`.
    pub source: String,
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub payload_json: Option<String>,
    /// Persisted locally, never gossiped. Defaults to `false`, which is
    /// what `write_note_full_v9` documents as the safe default.
    #[serde(default)]
    pub private: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateNoteResponse {
    /// The id the store minted.
    pub id: String,
}

/// `PATCH /v1/notes/{id}/payload` — replace the opaque payload blob.
#[derive(Debug, Deserialize)]
pub struct PayloadPatch {
    pub payload_json: String,
}

/// `POST /v1/notes/{id}/retire` — strike a note through, keeping the row.
#[derive(Debug, Deserialize)]
pub struct RetireRequest {
    /// Human-readable reason, rendered in the pane (`"superseded by X"`).
    pub reason: String,
}

/// The answer to the three routes whose store op returns "did that row
/// exist?" — retire, patch, delete.
///
/// A bool in a named field rather than a bare `204`: "the row was not
/// there" is a real answer to a delete, and the desktop commands it
/// replaces return `Ok(false)` for exactly that. Collapsing it into a
/// status would make the caller infer an absence it is currently told
/// (ARCH §18.3).
#[derive(Debug, Serialize, Deserialize)]
pub struct AffectedResponse {
    pub existed: bool,
}

// ─── Router ────────────────────────────────────────────────────

/// The notes CRUD router. Mounted unconditionally on serving daemons
/// beside `reading_http`; a commission whose `notes.db` would not open
/// answers 503 with that named reason — the same refusal
/// `McpSurface::Unavailable` makes rather than conflating "no tool
/// surface" with "the file would not open".
pub fn notes_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/notes", post(create_note))
        .route("/v1/notes/query", post(list_notes))
        .route("/v1/notes/{id}", get(get_note).delete(delete_note))
        .route("/v1/notes/{id}/payload", patch(patch_payload))
        .route("/v1/notes/{id}/retire", post(retire_note))
        .localhost_only_with(daemon)
}

// ─── Handlers ──────────────────────────────────────────────────

/// POST `/v1/notes/query` — filtered read. Wire form of `read_notes`.
///
/// A POST for the reason above the DTO. An absent body is the empty
/// filter, not a 400: "give me the recent notes" is a legitimate ask and
/// should not require a `{}` nobody can forget to send.
///
/// An empty list is a legitimate answer for every filter combination, so
/// this never 404s. The store caps `limit` internally; the value that
/// reached it is traced.
async fn list_notes(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    body: Option<Json<NoteQuery>>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    let q = body.map(|Json(q)| q).unwrap_or_default();
    let limit = q.limit.unwrap_or(LIST_LIMIT_DEFAULT);
    Ok(
        match store
            .read_notes(
                q.query.as_deref(),
                &q.symbols,
                &q.files,
                &q.kinds,
                limit,
                q.include_retired,
            )
            .await
        {
            Ok(rows) => {
                tracing::debug!(
                    kinds = ?q.kinds,
                    limit,
                    include_retired = q.include_retired,
                    returned = rows.len(),
                    "notes_http: notes listed",
                );
                Json(NoteListResponse {
                    notes: rows.into_iter().map(note_entry).collect(),
                })
                .into_response()
            }
            Err(e) => internal_error(&e.to_string()),
        },
    )
}

/// GET `/v1/notes/{id}` — one note. Wire form of `read_note_by_id`.
///
/// 404 when the id is not in the store, with that reason — never an
/// empty-shaped 200, which a caller cannot tell from a note whose every
/// field happens to be blank.
async fn get_note(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    Ok(match store.read_note_by_id(&id).await {
        Ok(Some(note)) => Json(note_entry(note)).into_response(),
        Ok(None) => not_found(&format!("no note `{id}`")),
        Err(e) => internal_error(&e.to_string()),
    })
}

/// POST `/v1/notes` — create. Wire form of `write_note_full_v9`.
///
/// `scope` and `source` are parsed through the enums' own
/// `parse` — one decider for what those closed sets contain (ARCH §2,
/// §10.6) — and an unrecognised value is a 400 naming the value, not a
/// silent fall back to `Global`/`Agent`. The store applies its own
/// lifecycle policy on top (operational-exhaust kinds are forced to
/// `Session` regardless of what is asked for); that policy stays where
/// every writer already meets it.
async fn create_note(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(body): Json<CreateNoteRequest>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    let Some(scope) = NoteScope::parse(&body.scope) else {
        return Ok(bad_request(&format!(
            "scope `{}` is not one of global/feature/session",
            body.scope
        )));
    };
    let Some(source) = NoteSource::parse(&body.source) else {
        return Ok(bad_request(&format!(
            "source `{}` is not one of agent/committed/extracted/inferred/observed",
            body.source
        )));
    };
    Ok(
        match store
            .write_note_full_v9(
                &body.kind,
                &body.content,
                body.symbols,
                body.files,
                &body.session_id,
                scope,
                body.feature_id.as_deref(),
                body.related_entity.as_deref(),
                source,
                body.supersedes.as_deref(),
                body.payload_json.as_deref(),
                body.private,
            )
            .await
        {
            Ok(id) => {
                tracing::info!(
                    note_id = %id,
                    kind = %body.kind,
                    scope = scope.as_str(),
                    private = body.private,
                    "notes_http: note written",
                );
                (StatusCode::CREATED, Json(CreateNoteResponse { id })).into_response()
            }
            Err(e) => internal_error(&e.to_string()),
        },
    )
}

/// PATCH `/v1/notes/{id}/payload` — replace the structured payload.
/// Wire form of `update_note_payload`.
///
/// The body is a JSON STRING field, not raw JSON: the store holds this
/// blob opaquely and the caller owns its schema, so re-encoding it
/// through this router would put a second decider on a shape neither
/// side here understands.
async fn patch_payload(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
    Json(body): Json<PayloadPatch>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    Ok(
        match store.update_note_payload(&id, &body.payload_json).await {
            Ok(existed) => {
                tracing::debug!(note_id = %id, existed, "notes_http: payload patched");
                Json(AffectedResponse { existed }).into_response()
            }
            Err(e) => internal_error(&e.to_string()),
        },
    )
}

/// POST `/v1/notes/{id}/retire` — strike through, keep the row. Wire
/// form of `retire_by_id`.
///
/// NOT a delete: the lesson pane renders the supersede chain, and a
/// retired predecessor has to still be there to be struck through.
async fn retire_note(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
    Json(body): Json<RetireRequest>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    Ok(match store.retire_by_id(&id, &body.reason).await {
        Ok(existed) => {
            tracing::info!(note_id = %id, existed, reason = %body.reason,
                "notes_http: note retired");
            Json(AffectedResponse { existed }).into_response()
        }
        Err(e) => internal_error(&e.to_string()),
    })
}

/// DELETE `/v1/notes/{id}` — hard delete. Wire form of `delete_note`.
///
/// Real deletion, no tombstone (TEACHABLE §5, which the lesson pane's
/// delete implements). `existed: false` for an unknown id is the
/// answer, not a 404: the caller asked for the row to be gone and it
/// is, and the desktop command it replaces returns `Ok(false)` here.
async fn delete_note(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    Ok(match store.delete_note(&id).await {
        Ok(existed) => {
            tracing::info!(note_id = %id, existed, "notes_http: note deleted");
            Json(AffectedResponse { existed }).into_response()
        }
        Err(e) => internal_error(&e.to_string()),
    })
}

// ─── Helpers ───────────────────────────────────────────────────

/// The daemon's own `NoteStore`. One lookup site, so no handler can
/// reach a different `notes.db` than the `/mcp` surface and
/// `/v1/notes/tool-outcome` already write to.
fn store_for(daemon: &Arc<EmbeddedDaemon>) -> Result<Arc<NoteStore>, Absence> {
    daemon.notes_store().map(Arc::clone).ok_or_else(|| {
        Absence::unavailable("this daemon has no note store (notes.db did not open)")
    })
}
