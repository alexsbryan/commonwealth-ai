// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `work` plane's doors — `GET /v1/work/projection` (five-programs
//! fp-45, §12 decision 2), and since pb-work-doors the four a submitter
//! needs: seal, submit, refusals and attribution.
//!
//! The queue is a fold over the `work` journal ("there is no queue server …
//! the queue is a fold over Admission" — commonwealth-work's own doc), and
//! since fp-54 that journal lives under THIS process's root. So the fold runs
//! here: roster through the same derivation the roster door answers with,
//! admission, then `commonwealth_work::projection::fold` — the one fold. A
//! donor receives the folded queue and asks `may_take` / `lease_state` of it;
//! it never reads the admission itself.
//!
//! **The submitter's doors (pb-work-doors).** A client that links no
//! `commonwealth-work` cannot seal a unit (`unit_hash` canonicalises through
//! rail-core's `Payload::new`), sign a `Submit`, run `may_take`, or read the
//! donor's attribution method — so each is a door over the ONE
//! implementation here, never a second copy on the client:
//!
//! - `POST /v1/work/seal` answers sealed `JobUnit`s for a kind and a list of
//!   `(payload, requirements)`;
//! - `POST /v1/work/submit` verifies every seal, appends the `Submit` act as
//!   this node through the append door's own path, and answers the handoff
//!   and the unit refs beside the append's answer;
//! - `POST /v1/work/refusals` runs `may_take` for every published offer over
//!   the named units at the named instant;
//! - `GET /v1/work/attribution` answers `attribution::of_sandbox` for a rev
//!   and the image the caller names.
//!
//! Absence is reported, never defaulted: a roster or journal that will not
//! answer is a 500 naming why, never an empty projection — an empty queue and
//! an unreadable one are different facts to a donor deciding what to take.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use commonwealth_rail::{Ed25519Verifier, RailAct};
use commonwealth_work::act::{Submission, WorkAct};
use commonwealth_work::projection::WorkProjection;
use commonwealth_work::refusal::{may_take, WorkRefusal};
use commonwealth_work::sandbox::Sandbox;
use commonwealth_work::{seal, ActorKey, HandoffId, UnitRef, WORK_NAMESPACE};
use oicp_types::{JobKind, JobRequirements, JobUnit};
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::rail::err;
use crate::RailsDaemon;

/// The `work` namespace, folded now — or the 500 that names why not.
async fn folded(daemon: &RailsDaemon) -> Result<WorkProjection, Response> {
    let journal = match daemon.rail.journal(WORK_NAMESPACE) {
        Ok(j) => j,
        Err(e) => return Err(err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };
    let roster = match daemon.rail.roster(&journal).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, "work projection: the `work` roster is unreadable");
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the `work` roster is unreadable: {e}"),
            ));
        }
    };
    let admission = match journal.admit(&roster, &Ed25519Verifier) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "work projection: the `work` journal would not admit");
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the `work` journal would not admit: {e}"),
            ));
        }
    };
    Ok(commonwealth_work::projection::fold(&admission))
}

/// GET /v1/work/projection — the `work` namespace, folded now.
pub async fn projection(State(daemon): State<Arc<RailsDaemon>>) -> Response {
    match folded(&daemon).await {
        Ok(proj) => Json(proj).into_response(),
        Err(refusal) => refusal,
    }
}

/// A request body in this door's own words: taken as a `Value` and decoded
/// here, so a malformed body is a 422 carrying serde's sentence rather than
/// axum's rejection prose (the rail door's rule, `rail::append_act`).
fn decode<T: DeserializeOwned>(door: &str, body: serde_json::Value) -> Result<T, Response> {
    serde_json::from_value(body).map_err(|e| {
        tracing::debug!(door, error = %e, "work: refused a malformed body");
        err(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("the {door} body is malformed: {e}"),
        )
    })
}

/// One unit to seal: its payload and what a host must satisfy to run it.
#[derive(Deserialize)]
struct SealItem {
    payload: serde_json::Value,
    requirements: JobRequirements,
}

#[derive(Deserialize)]
struct SealRequest {
    kind: JobKind,
    units: Vec<SealItem>,
}

/// POST /v1/work/seal — `seal::seal` over each `(payload, requirements)`,
/// in order. The first unit that cannot be sealed refuses the whole request
/// by its index and the seal's own sentence: half a selection sealed is a
/// submission that silently drops rows.
pub async fn seal_units(Json(body): Json<serde_json::Value>) -> Response {
    let req: SealRequest = match decode("seal", body) {
        Ok(r) => r,
        Err(refusal) => return refusal,
    };
    let mut units: Vec<JobUnit> = Vec::with_capacity(req.units.len());
    for (i, item) in req.units.into_iter().enumerate() {
        match seal::seal(req.kind.clone(), item.payload, item.requirements, None) {
            Ok(unit) => units.push(unit),
            Err(e) => {
                tracing::debug!(index = i, error = %e, "work seal: refused a unit");
                return err(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("unit {i} could not be sealed: {e}"),
                );
            }
        }
    }
    tracing::debug!(kind = %req.kind, units = units.len(), "work seal: sealed");
    Json(serde_json::json!({ "units": units })).into_response()
}

#[derive(Deserialize)]
struct SubmitRequest {
    kind: JobKind,
    units: Vec<JobUnit>,
    #[serde(default)]
    allowed: Option<Vec<ActorKey>>,
    #[serde(default)]
    ttl_secs: Option<u64>,
}

/// POST /v1/work/submit — open a handoff for these sealed units.
///
/// `to_payload` is the check: every unit's seal verified, every unit's kind
/// the handoff's, and a refusal is its sentence at 422 with nothing written.
/// The act then goes through the append door's own path (`rail::append_act`),
/// signed as this node — one append, whose answer this door returns with the
/// handoff and the unit refs beside it.
pub async fn submit(
    State(daemon): State<Arc<RailsDaemon>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let req: SubmitRequest = match decode("submit", body) {
        Ok(r) => r,
        Err(refusal) => return refusal,
    };
    let handoff = HandoffId::generate();
    let refs: Vec<UnitRef> = req
        .units
        .iter()
        .map(|u| UnitRef {
            handoff,
            unit_hash: u.unit_hash.clone(),
        })
        .collect();
    let act = WorkAct::Submit(Submission::new(
        handoff,
        req.kind,
        req.units,
        req.allowed,
        req.ttl_secs,
    ));
    let payload = match commonwealth_work::to_payload(&act) {
        Ok(p) => p,
        Err(why) => {
            tracing::debug!(%why, "work submit: refused before the rail");
            return err(StatusCode::UNPROCESSABLE_ENTITY, why);
        }
    };
    let journal = match daemon.rail.journal(WORK_NAMESPACE) {
        Ok(j) => j,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let record = match serde_json::to_value(RailAct::Record { payload }) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let appended = crate::rail::append_act(&daemon.rail, &journal, record).await;
    if !appended.status().is_success() {
        tracing::debug!(status = %appended.status(), "work submit: the append door refused");
        return appended;
    }
    let bytes = match axum::body::to_bytes(appended.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let mut out: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    out["handoff"] = serde_json::json!(handoff);
    out["units"] = serde_json::json!(refs);
    tracing::debug!(
        handoff = %handoff.to_hex(),
        units = refs.len(),
        seq = ?out.get("seq"),
        "work submit: appended"
    );
    Json(out).into_response()
}

#[derive(Deserialize)]
struct RefusalsRequest {
    handoff: HandoffId,
    units: Vec<String>,
    at_ms: u64,
}

/// POST /v1/work/refusals — what every donor that has published an offer
/// says about each named unit at `at_ms`: `may_take`, the one predicate, run
/// over the fold at this instant. Keyed by unit hash; each value is the
/// `(actor, verdict)` list, `Ok` meaning the rail's half is satisfied.
pub async fn refusals(
    State(daemon): State<Arc<RailsDaemon>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let req: RefusalsRequest = match decode("refusals", body) {
        Ok(r) => r,
        Err(refusal) => return refusal,
    };
    let proj = match folded(&daemon).await {
        Ok(p) => p,
        Err(refusal) => return refusal,
    };
    let mut out: BTreeMap<String, Vec<(ActorKey, Result<(), WorkRefusal>)>> = BTreeMap::new();
    for hash in req.units {
        let unit = UnitRef {
            handoff: req.handoff,
            unit_hash: hash.clone(),
        };
        let verdicts = proj
            .offers
            .iter()
            .map(|(actor, offer)| {
                (
                    actor.clone(),
                    may_take(&proj, actor, offer, &unit, req.at_ms),
                )
            })
            .collect();
        out.insert(hash, verdicts);
    }
    tracing::debug!(
        handoff = %req.handoff.to_hex(),
        units = out.len(),
        offers = proj.offers.len(),
        at_ms = req.at_ms,
        "work refusals: surveyed"
    );
    Json(out).into_response()
}

/// The foreground deadline a program on this node published: until
/// `until_ms` its operator is at the keyboard, and a donor whose offer says
/// `yield_to_foreground` takes no new unit (pb-work-donor, phase-b-38 fork 3).
///
/// cw-rails owns the take and this deadline, never the foreground: the svrn
/// daemon owns its turns and posts the deadline here. With nobody posting,
/// nothing yields, which is a daemon-less node's behaviour today.
#[derive(Debug, Default)]
pub struct ForegroundYield {
    until_ms: std::sync::atomic::AtomicU64,
}

impl ForegroundYield {
    /// Hold new takes until `until_ms`. A later deadline wins; an earlier one
    /// never shortens a window already published.
    pub fn hold_until(&self, until_ms: u64) {
        self.until_ms
            .fetch_max(until_ms, std::sync::atomic::Ordering::SeqCst);
    }

    /// The deadline in force at `now_ms`, or `None` once it has passed.
    pub fn yielding_at(&self, now_ms: u64) -> Option<u64> {
        let until = self.until_ms.load(std::sync::atomic::Ordering::SeqCst);
        (until > now_ms).then_some(until)
    }
}

#[derive(Deserialize)]
struct YieldRequest {
    until_ms: u64,
}

/// POST /v1/work/yield `{until_ms}` — a program on this node publishes its
/// foreground deadline. Loopback is the auth, as for every door here.
pub async fn hold_yield(
    State(daemon): State<Arc<RailsDaemon>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let req: YieldRequest = match decode("yield", body) {
        Ok(r) => r,
        Err(refusal) => return refusal,
    };
    daemon.work_yield.hold_until(req.until_ms);
    tracing::debug!(target: commonwealth_work::TRACE_TARGET, until_ms = req.until_ms,
                    "work yield: the foreground holds new takes until this deadline");
    Json(serde_json::json!({ "until_ms": req.until_ms })).into_response()
}

#[derive(Deserialize)]
pub struct AttributionQuery {
    repo_rev: String,
    #[serde(default)]
    image: Option<String>,
}

/// GET /v1/work/attribution?repo_rev=<rev>[&image=<image>] — what a unit run
/// at `repo_rev` inside `image` (or on this host, with none) is attributed
/// to: `attribution::of_sandbox` over `Sandbox::probe`, the method every
/// donor reads its own provenance with. Probing a container runtime is
/// blocking I/O, so it runs off the async threads.
pub async fn attribution(Query(q): Query<AttributionQuery>) -> Response {
    let answer = tokio::task::spawn_blocking(move || {
        let (sandbox, why) = Sandbox::probe(q.image.as_deref());
        tracing::debug!(
            image = ?q.image,
            sandbox = ?sandbox,
            no_sandbox = ?why,
            "work attribution: probed"
        );
        commonwealth_work::attribution::of_sandbox(q.repo_rev, &sandbox)
    })
    .await;
    match answer {
        Ok(a) => Json(a).into_response(),
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the attribution probe did not finish: {e}"),
        ),
    }
}
