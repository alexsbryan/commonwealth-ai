// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reads a turn leaves behind, the skill toggle that steers the next one,
//! and whether this process can serve one at all (sv-surface D9, D9a).
//!
//! | Route | Runtime register |
//! |---|---|
//! | `GET /v1/skills` | `Runtime.skills` (`SkillRegistry`) |
//! | `PUT /v1/skills/{id}/active` | `SkillRegistry::activate`/`deactivate` |
//! | `GET /v1/conversations/{id}/provenance` | `Runtime::get_last_turn_provenance` |
//! | `GET /v1/ready` | the serving `Runtime` itself |
//!
//! Each was answered on the desktop by reading the DESKTOP's `Runtime`, which
//! since R5 never sees a turn in attach mode. They sit beside `turn_http`
//! rather than in it because that file is the DRIVER's surface, and a driver
//! that also serves its host's registry is the §5.1 smell. `trust_level`
//! crosses as the lowercased debug spelling, decided here so nothing
//! re-derives it (§10.6); `TurnProvenance` crosses whole. `/v1/ready` is
//! deliberately not an identity probe — WHO answers is `/status.process.pid`.
//!
//! Loopback posture is `reading_http`'s, unchanged.
//!
//! NOT here: the session → conversation lookup — the surface already learns
//! that pairing from the routing cards it receives.

use std::sync::Arc;

use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_core::runtime::{Runtime, TurnProvenance};

use crate::daemon::EmbeddedDaemon;
use crate::http_response::{json_error, Absence};
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

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
        .localhost_only_with(daemon)
}

// ─── Handlers ──────────────────────────────────────────────────

/// GET `/v1/skills` — every registered skill, with the serving
/// runtime's active set folded in.
///
/// Registration order, which is load order from disk, which is the
/// order the desktop's list already rendered. Sorting here would be a
/// presentation decision the caller owns.
async fn list_skills(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Result<Response, Absence> {
    let runtime = runtime_for(&daemon)?;
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
    Ok(Json(SkillListResponse { skills }).into_response())
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Result<Response, Absence> {
    let runtime = runtime_for(&daemon)?;
    let provenance = runtime.get_last_turn_provenance(&id);
    tracing::debug!(
        conversation = %id,
        captured = provenance.is_some(),
        "turn_extras_http: last-turn provenance read"
    );
    Ok(Json(ProvenanceResponse { provenance }).into_response())
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
    Json(req): Json<SetSkillActiveRequest>,
) -> Result<Response, Absence> {
    let runtime = runtime_for(&daemon)?;
    if runtime.skills.skill_by_id(&id).is_none() {
        return Ok(json_error(
            StatusCode::NOT_FOUND,
            &format!("no skill '{id}' is registered on this daemon"),
        ));
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
            return Ok(json_error(
                StatusCode::NOT_FOUND,
                &format!("no skill '{id}' is registered on this daemon"),
            ))
        }
    };
    let entry = wire_entry(skill, runtime.skills.is_active(&id));
    tracing::debug!(
        skill = %id,
        active = entry.active,
        requested = req.active,
        "turn_extras_http: skill activation set"
    );
    Ok(Json(entry).into_response())
}

/// GET `/v1/ready` — is the backend that will answer my turns up.
///
/// 200 with `ready: true` when this daemon holds a serving `Runtime`;
/// 503 with `ready: false` and the same named reason the sibling
/// routes give when it does not. The status line and the body agree,
/// so neither a caller that gates on `resp.status()` nor one that
/// reads `body.ready` can get the wrong answer.
async fn ready(_: LocalOnly, Extension(daemon): Extension<Arc<EmbeddedDaemon>>) -> Response {
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
fn runtime_for(daemon: &Arc<EmbeddedDaemon>) -> Result<Arc<Runtime>, Absence> {
    daemon.runtime().map(Arc::clone).ok_or_else(|| {
        Absence::unavailable("this daemon serves no turns (it was commissioned without a Runtime)")
    })
}
