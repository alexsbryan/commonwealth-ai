// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two reads a turn leaves behind — `/v1/skills` and
//! `/v1/conversations/{id}/provenance` (sv-surface D9).
//!
//! # Why these two, and why they are not in `turn_http`
//!
//! Both answer a question about the process that RAN the turn, and
//! both were answered on the desktop by reading the desktop's own
//! `Runtime`. That was correct while the desktop assembled one. Since
//! sv-surface R5 every turn rides the wire in BOTH boot modes, so in
//! attach the desktop's `Runtime` never saw the turn at all: two
//! commands were reading a register that is structurally empty and
//! reporting the emptiness as "no provenance yet" / "no skills". The
//! deletion ladder calls them two of the three **no-fork
//! degradations** — broken with no `is_attach_mode()` branch to point
//! at, because nobody ever wrote one.
//!
//! They live beside `turn_http` rather than inside it because
//! `turn_http` is the DRIVER's surface (`serve_turn`, the socket, the
//! conversation CRUD the driver writes). These two are reads over the
//! serving `Runtime`'s own registers, and a driver file that also
//! serves its host's registry is the trait-too-wide smell (§5.1).
//!
//! | Route | Runtime register | Desktop command it serves |
//! |---|---|---|
//! | `GET /v1/skills` | `Runtime.skills` (`SkillRegistry`) | `list_skills` |
//! | `GET /v1/conversations/{id}/provenance` | `Runtime::get_last_turn_provenance` | `get_last_turn_provenance` |
//!
//! # One formatter, on this side
//!
//! `trust_level` crosses as the lowercased debug spelling the desktop
//! used to render privately. The desktop had the only copy of that
//! rule and every other surface would have had to re-derive it; the
//! §10.6 fix is one decider, and the daemon is where the registry
//! lives. `TurnProvenance` crosses whole — it is already
//! `Serialize + Deserialize` in `sovereign-core` — so there is no
//! projection twin to keep in step (ARCH §2).
//!
//! # What is NOT here, named rather than half-served
//!
//! **Toggling a skill.** `SkillRegistry::activate`/`deactivate` take
//! `&mut self` and the serving `Runtime` is behind an `Arc`, so a
//! `PUT /v1/skills/{id}/active` cannot be written over today's object:
//! it needs the registry to become swappable (an `ArcSwap`, the shape
//! `admin_http`'s provider reload already uses) and that is a rung of
//! its own. The desktop's `toggle_skill` therefore keeps writing its
//! own config; in attach that config no longer reaches the answer,
//! which was already true before this file and is recorded in the
//! command's own comment.
//!
//! **The session -> conversation lookup.** The daemon owns the session
//! store, but the surface already learns the pairing from the routing
//! cards it receives (`AppState.session_conversations`), so a route
//! would be a second path to a fact the wire already delivers.
//!
//! Loopback posture is `reading_http`'s, unchanged: router-level
//! middleware plus a per-handler `enforce_localhost` (ARCH §5, defence
//! in depth).

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension, Path};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_core::runtime::{Runtime, TurnProvenance};

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::enforce_localhost;

// ─── The wire projection ───────────────────────────────────────

/// One registered skill on the wire.
///
/// Every field the desktop's `SkillEntry` carried, so the surface
/// deserializes into this and renders it — no second struct, no field
/// the caller has to re-derive. `Deserialize` as well as `Serialize`
/// for the `ProjectEntry` reason (`features_http`): a caller parses
/// back into the struct the daemon emitted rather than a hand-copy
/// that can drift.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillWireEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Whether this skill is in the serving runtime's ACTIVE set.
    pub active: bool,
    /// Lowercased [`sovereign_contracts::types::TrustLevel`] — the
    /// spelling the desktop rendered privately, decided here now so
    /// every surface reads the same word (§10.6).
    pub trust_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillListResponse {
    pub skills: Vec<SkillWireEntry>,
}

/// `GET /v1/conversations/{id}/provenance`'s body.
///
/// `provenance: None` is a 200, not a 404: "this conversation has had
/// no witness turn in this runtime's lifetime" is an ANSWER, and the
/// inner-work pane branches on it to show its empty state. A 404 here
/// would collapse that into "no such conversation", which is a
/// different fact (ARCH §18.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceResponse {
    pub provenance: Option<TurnProvenance>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

// ─── Router ────────────────────────────────────────────────────

/// The turn-extras router. Mounted unconditionally on serving daemons;
/// a commission with no `Runtime` cannot exist (`ServingCore.runtime`
/// is not an `Option`), so the 503 arm below is reachable only from a
/// mesh-admin daemon that serves no turns at all — a named reason, not
/// a 404.
pub fn turn_extras_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/skills", get(list_skills))
        .route(
            "/v1/conversations/{id}/provenance",
            get(last_turn_provenance),
        )
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
        .layer(Extension(daemon))
}

// ─── Handlers ──────────────────────────────────────────────────

/// GET `/v1/skills` — every registered skill, with the serving
/// runtime's active set folded in.
///
/// Registration order, which is load order from disk, which is the
/// order the desktop's list already rendered. Sorting here would be a
/// presentation decision the caller owns.
async fn list_skills(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let runtime = match runtime_for(&daemon) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let active_ids: Vec<String> = runtime
        .skills
        .active_skills()
        .iter()
        .map(|s| s.id.clone())
        .collect();
    let skills: Vec<SkillWireEntry> = runtime
        .skills
        .list()
        .iter()
        .map(|s| SkillWireEntry {
            id: s.id.clone(),
            name: s.name.clone(),
            description: s.description.clone(),
            active: active_ids.contains(&s.id),
            trust_level: format!("{:?}", s.trust_level).to_lowercase(),
        })
        .collect();
    tracing::debug!(
        registered = skills.len(),
        active = active_ids.len(),
        "turn_extras_http: skills listed"
    );
    Json(SkillListResponse { skills }).into_response()
}

/// GET `/v1/conversations/{id}/provenance` — the most recent witness
/// turn's provenance frame this runtime captured for the conversation.
///
/// Wire form of `Runtime::get_last_turn_provenance`, which is an
/// in-memory register keyed by conversation and holds only this
/// process's lifetime — so the answer is about the process that RAN
/// the turn, which is exactly why the desktop could not answer it in
/// attach mode.
async fn last_turn_provenance(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let runtime = match runtime_for(&daemon) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let provenance = runtime.get_last_turn_provenance(&id);
    tracing::debug!(
        conversation = %id,
        captured = provenance.is_some(),
        "turn_extras_http: last-turn provenance read"
    );
    Json(ProvenanceResponse { provenance }).into_response()
}

// ─── Helpers ───────────────────────────────────────────────────

/// The daemon's own serving `Runtime`. One lookup site, so neither
/// handler can read a different one than `serve_turn` drives.
fn runtime_for(daemon: &Arc<EmbeddedDaemon>) -> Result<Arc<Runtime>, Response> {
    daemon.runtime().map(Arc::clone).ok_or_else(|| {
        error_body(
            StatusCode::SERVICE_UNAVAILABLE,
            "this daemon serves no turns (it was commissioned without a Runtime)",
        )
    })
}

fn error_body(status: StatusCode, msg: &str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: msg.to_string(),
        }),
    )
        .into_response()
}
