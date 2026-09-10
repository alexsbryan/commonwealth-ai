// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reads a turn leaves behind, the skill toggle that steers the
//! next one, and whether this process can serve one at all
//! (sv-surface D9, D9a).
//!
//! # Why these, and why they are not in `turn_http`
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
//! | `PUT /v1/skills/{id}/active` | `SkillRegistry::activate`/`deactivate` | `toggle_skill` |
//! | `GET /v1/conversations/{id}/provenance` | `Runtime::get_last_turn_provenance` | `get_last_turn_provenance` |
//! | `GET /v1/ready` | the serving `Runtime` itself | `is_backend_ready` |
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
//! # The toggle, and the lock that made it possible (D9a)
//!
//! This file used to say a toggle "cannot be written over today's
//! object", because `SkillRegistry::activate` took `&mut self` behind
//! an `Arc`. D9a moved the registry's ACTIVE SET behind a
//! `std::sync::RwLock` in `sovereign-contracts` — the crate's own
//! interior-mutability shape, not a new `arc-swap` dep on the bottom
//! of the layer map (§19; the reasoning is on `SkillRegistry::active`)
//! — so `activate`/`deactivate` take `&self` and the toggle reaches
//! the registry the serving `Runtime` actually reads. The desktop's
//! `toggle_skill` no longer writes a config that, in attach, reached
//! nothing.
//!
//! # Readiness is a fact about the SERVING process, not a needle
//!
//! `GET /v1/ready` answers "is the backend that will answer my turns
//! up". The desktop answered it by reading `state.runtime.is_some()`
//! — its OWN bootstrap's needle — which in attach reports on a
//! Runtime that will never see a turn, and which D9b deletes
//! (a `None` there hangs the splash forever: the `backend-ready`
//! Tauri event has no replay). It is deliberately NOT an identity
//! probe: WHO answers is `/status.process.pid`'s single answer
//! (3c7ad5933, and B4's `probe_daemon_identity` is its one caller
//! shape), and a second pid on a second route would be the §10.6 twin
//! this campaign exists to delete. Compose the two — the desktop
//! already resolved identity at boot.
//!
//! # What is NOT here, named rather than half-served
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
use axum::routing::{get, put};
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

/// `PUT /v1/skills/{id}/active`'s body — the DESIRED state, not a
/// verb.
///
/// `{"active": true}` rather than a `/activate` + `/deactivate` pair,
/// because the desktop's control is a switch: a surface that sends
/// "make it so" is idempotent under a double-tap and under a retry,
/// where two verbs make the caller track which one it owes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetSkillActiveRequest {
    pub active: bool,
}

/// `GET /v1/ready`'s body — is the process that answers turns up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadyResponse {
    /// True when this daemon holds a serving `Runtime`. A 503 carries
    /// `false` and a reason rather than a bare status, so a caller
    /// that only reads the body still learns the fact (§18.3).
    pub ready: bool,
    /// The model id the Fast slot would answer with, from the serving
    /// provider itself — `"unknown"` when the provider declines to
    /// say (the trait's documented default), never an invented name.
    pub model_id: String,
    /// Why not, when `ready` is false. `None` on a ready daemon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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
        .route("/v1/skills/{id}/active", put(set_skill_active))
        .route(
            "/v1/conversations/{id}/provenance",
            get(last_turn_provenance),
        )
        .route("/v1/ready", get(ready))
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
    let skills: Vec<SkillWireEntry> = runtime
        .skills
        .list()
        .iter()
        .map(|s| wire_entry(s, runtime.skills.is_active(&s.id)))
        .collect();
    let active = skills.iter().filter(|s| s.active).count();
    tracing::debug!(
        registered = skills.len(),
        active,
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

/// PUT `/v1/skills/{id}/active` — put one skill in, or out of, the
/// serving runtime's ACTIVE set.
///
/// Reaches the registry `serve_turn` reads, which is the whole point:
/// `SkillRegistry::primary_skill_id_for_conversation` decides which
/// agent loop the NEXT turn routes into, and until D9a the desktop's
/// toggle wrote a config file that, on an attached boot, no serving
/// process ever read.
///
/// A 404 for an unregistered id, not a silent no-op: `activate` is
/// deliberately tolerant of an id it does not know (it records the
/// intent so a later-loaded skill honours it), and a surface that
/// switched a toggle on for a skill that does not exist would render
/// "on" over an answer nobody can give (§18.3).
///
/// The response is the SAME `SkillWireEntry` the list serves, read
/// back AFTER the write — so the caller's rendered state comes from
/// the registry rather than from its own optimistic guess.
async fn set_skill_active(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
    Json(req): Json<SetSkillActiveRequest>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let runtime = match runtime_for(&daemon) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if runtime.skills.skill_by_id(&id).is_none() {
        return error_body(
            StatusCode::NOT_FOUND,
            &format!("no skill '{id}' is registered on this daemon"),
        );
    }
    if req.active {
        runtime.skills.activate(&id);
    } else {
        runtime.skills.deactivate(&id);
    }
    // Read back through the registry, not from `req`: the write is
    // the decider and the echo must not be able to disagree with it.
    let skill = match runtime.skills.skill_by_id(&id) {
        Some(s) => s,
        None => {
            return error_body(
                StatusCode::NOT_FOUND,
                &format!("no skill '{id}' is registered on this daemon"),
            )
        }
    };
    let entry = wire_entry(skill, runtime.skills.is_active(&id));
    tracing::debug!(
        skill = %id,
        active = entry.active,
        requested = req.active,
        "turn_extras_http: skill activation set"
    );
    Json(entry).into_response()
}

/// GET `/v1/ready` — is the backend that will answer my turns up.
///
/// 200 with `ready: true` when this daemon holds a serving `Runtime`;
/// 503 with `ready: false` and the same named reason the sibling
/// routes give when it does not. The status line and the body agree,
/// so neither a caller that gates on `resp.status()` nor one that
/// reads `body.ready` can get the wrong answer.
async fn ready(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let runtime = match daemon.runtime() {
        Some(r) => Arc::clone(r),
        None => {
            tracing::debug!("turn_extras_http: readiness probed on a daemon with no Runtime");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ReadyResponse {
                    ready: false,
                    model_id: "unknown".to_string(),
                    reason: Some(
                        "this daemon serves no turns (it was commissioned without a Runtime)"
                            .to_string(),
                    ),
                }),
            )
                .into_response();
        }
    };
    let model_id = runtime
        .inference
        .model_id_for(sovereign_core::types::Speed::Fast);
    tracing::debug!(%model_id, "turn_extras_http: readiness probed — serving");
    Json(ReadyResponse {
        ready: true,
        model_id,
        reason: None,
    })
    .into_response()
}

// ─── Helpers ───────────────────────────────────────────────────

/// One `Skill` -> `SkillWireEntry` projection, so the list and the
/// toggle's echo cannot spell `trust_level` (or anything else) two
/// ways (§10.6).
fn wire_entry(skill: &sovereign_contracts::skills::Skill, active: bool) -> SkillWireEntry {
    SkillWireEntry {
        id: skill.id.clone(),
        name: skill.name.clone(),
        description: skill.description.clone(),
        active,
        trust_level: format!("{:?}", skill.trust_level).to_lowercase(),
    }
}

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
