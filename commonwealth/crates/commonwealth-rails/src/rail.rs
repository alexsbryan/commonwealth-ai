// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring rail's doors — the surface a ring app writes its own state to.
//!
//! `POST /v1/rail/append`, `GET /v1/rail/log`, `POST+GET /v1/rail/live`,
//! mounted beside the mesh routes in [`crate::api`], and — since fp-54's
//! serve half — the sync doors the ring round reads its journals through
//! (`digest`, `missing`, `ingest`, `roster`, `read`, `admit`, `compact`,
//! `namespaces`, `actor`), all rail-core-typed. The append and log
//! bodies mirror the inference daemon's `routes_rail` door for door — same
//! paths, same server-assigned fields, same retire rendering, same gap
//! sentences — so a page written against one daemon behaves the same against
//! the other (ARCH §10.6). What is deliberately NOT mirrored is the guest
//! half: there are no grants and no sessions here (loopback IS the auth),
//! so the namespace is always named explicitly and a caller can stamp an act
//! on another's behalf only with a roster member's signed
//! `GuestAttestation` (decision five-programs-34; see `append_act`).
//!
//! **The journals live under THIS process's data root**, never the daemon's
//! (§4 rule 1 — one data directory, one owner; a second process never opens
//! the daemon's dir, and the daemon's journals migrate in fp-54's commit, so
//! there is no dual-writer window). The signer is THIS process's node key —
//! one loader, but two dirs, so on a default install it is NOT the
//! daemon's key. A line still verifies at every peer because rails is a
//! member in its own right (`run` refuses without a mesh; `join` stamps its
//! key), and it renders as the same person because both processes name a
//! member by hostname and the roster groups keys by name — pinned by
//! `rails_and_the_daemon_sign_with_two_keys_under_one_person`.
//!
//! **The live lane is the drain half only.** The daemon's `live_push` fans
//! each payload out to every online peer over sovereign-mesh's fabric,
//! which this daemon deliberately does not carry (package-closure clean is
//! its own Cargo description). The buffer, the caps and the answer shape
//! are the daemon's (`peers: []`, `delivered: 0` — the fan-out's absence is
//! reported in the body, not papered over); peer delivery arrives with
//! fp-54's dial.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_core::mesh::Mesh;
use commonwealth_rail::{
    admit, AttestRefusal, Compaction, Digest, Ed25519Verifier, GuestAttestation, Op, Person,
    RailAct, RailError, RingJournal, RingRail, Roster, RosterOrigin, RosterSource, SignedOp,
    NO_BUDGET,
};
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::RailsDaemon;

// ── The roster ───────────────────────────────────────────────

/// Read the membership this node holds as a ring roster — the same
/// derivation, rule for rule, the inference daemon's `MeshRoster::derive`
/// runs (sovereign-mesh cannot be named here, and the rail's
/// [`RosterSource`] is the extension point the rail provides for exactly
/// this: the derivation lives in the application):
///
/// - **A tombstone is kept.** A departed member's journal lines still count;
///   dropping the row would turn its whole history into `UnknownSigner` gaps
///   the day it leaves.
/// - **The self pubkey is passed in, not read from the row.** The row's key
///   is stamped by gossip, so a freshly booted node has a key and no stamp
///   for the first round; taking it from the row would make the node unable
///   to author on its own journal until then.
/// - **No placeholder for an unidentified row.** A member with no key is
///   counted out, never defaulted into a shared identity; the report is the
///   gap admission emits.
/// - **A blank name falls back to the node id** — the roster decides
///   membership, the name is only how an admitted op renders.
/// - **Keys are sorted**, so the derivation is a function of the membership
///   set and not of hash-map iteration order.
pub fn derive_roster(mesh: &Mesh, self_id: NodeId, self_pubkey: Option<NodePubkey>) -> Roster {
    let mut members: std::collections::BTreeMap<Person, Vec<String>> =
        std::collections::BTreeMap::new();
    for record in mesh.members.values() {
        let pubkey = if record.node_id == self_id {
            self_pubkey.or(record.node_pubkey)
        } else {
            record.node_pubkey
        };
        let Some(pubkey) = pubkey else { continue };
        let actor = pubkey.to_string();
        let person = if record.name.trim().is_empty() {
            Person::from(record.node_id.to_string())
        } else {
            Person::from(record.name.trim())
        };
        let keys = members.entry(person).or_default();
        if !keys.contains(&actor) {
            keys.push(actor);
        }
    }
    for keys in members.values_mut() {
        keys.sort();
    }
    Roster::new(members)
}

/// Membership as every ring's default roster, held WEAKLY.
///
/// [`RailsDaemon`](crate::RailsDaemon) owns the mesh strongly and the rail
/// beside it; a strong reference here would be a cycle, and a source that
/// outlives the membership it derives from has nothing true to say — it
/// reports that, rather than an empty ring.
pub struct MembershipRosterSource {
    pub(crate) mesh: Weak<RwLock<Mesh>>,
    pub(crate) self_id: NodeId,
    pub(crate) self_pubkey: Option<NodePubkey>,
}

impl MembershipRosterSource {
    /// Install membership as `rail`'s DEFAULT roster — every ring nobody
    /// narrowed admits everyone in the mesh. Rails registers no namespace of
    /// its own; a registered origin's namespaces are held by
    /// `crate::origins` (pb-rails-origins), and a file `roster.json` still
    /// narrows any other ring here, as the rail intends.
    pub fn install(
        rail: &RingRail,
        mesh: &Arc<RwLock<Mesh>>,
        self_id: NodeId,
        self_pubkey: Option<NodePubkey>,
    ) {
        rail.default_roster(Arc::new(Self {
            mesh: Arc::downgrade(mesh),
            self_id,
            self_pubkey,
        }));
        tracing::debug!(target: "rails", "rail: membership is every ring's default roster");
    }
}

impl RosterSource for MembershipRosterSource {
    fn roster(&self) -> Pin<Box<dyn Future<Output = Result<Roster, RailError>> + Send + '_>> {
        Box::pin(async move {
            let Some(mesh) = self.mesh.upgrade() else {
                return Err(RailError::Io(
                    "the mesh state this roster derives from is gone".into(),
                ));
            };
            let mesh = mesh.read().await;
            Ok(derive_roster(&mesh, self.self_id, self.self_pubkey))
        })
    }
}

// ── The doors ────────────────────────────────────────────────

/// Query parameters common to every rail route here. There are no grants on
/// a rails daemon, so the namespace is always the operator's explicit one.
#[derive(Debug, Deserialize)]
pub struct RailQuery {
    #[serde(default)]
    pub namespace: Option<String>,
}

pub(crate) fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// What a not-in-roster refusal tells the operator to DO, in the words of
/// whoever owns this namespace's roster. Derived names this daemon's join
/// verb; a file roster's sentence is the rail's own (it names
/// `svrn ring roster add`, which is right for a ring written by hand).
/// The origin is asked of the rail, never matched out of the error's prose.
fn not_in_roster_refusal(origin: RosterOrigin, e: &RailError) -> String {
    match origin {
        RosterOrigin::Derived => "this node is not in a mesh yet, so every op it writes would be \
             unreadable to every peer — `cw-rails join <invite>` first."
            .into(),
        RosterOrigin::File => e.to_string(),
    }
}

/// Render what [`RingJournal::seal`]'s prune did, for the append body — the
/// daemon's rendering verbatim: a seal and the prune it authorises are one
/// act to the caller, and a refused prune is not a failed seal, so the
/// outcome is reported in the body instead of a status code.
fn retire(retired: &Result<Compaction, RailError>) -> serde_json::Value {
    match retired {
        Ok(done) => serde_json::json!({
            "removed": done.removed,
            "kept": done.kept,
            "gaps_cleared": done.gaps_cleared,
            "floors": done.floors,
        }),
        Err(e) => serde_json::json!({ "refused": e.to_string() }),
    }
}

fn namespace_of(q: &RailQuery) -> Result<String, Response> {
    q.namespace.clone().ok_or_else(|| {
        err(
            StatusCode::BAD_REQUEST,
            "the namespace must be named explicitly — pass ?namespace=<id>",
        )
    })
}

fn journal_of(rail: &RingRail, namespace: &str) -> Result<Arc<RingJournal>, Response> {
    rail.journal(namespace)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))
}

/// POST /v1/rail/append — sign and append one act to the named namespace.
///
/// The body is the act alone. `seq`, the signature, the timestamp and the id
/// are all this daemon's to assign; an app that could choose its own sequence
/// number or actor could write as somebody else.
///
/// There are no sessions on a rails daemon, so a caller cannot stamp by
/// saying so: a bare `on_behalf_of` is dropped HERE, before anything is
/// signed, and warned — the same drop the daemon's `stamp_from` makes for
/// its session-less callers. A body may instead carry an `attestation` (a
/// [`GuestAttestation`] the daemon's guest door signed per session,
/// decision five-programs-34): it is verified against this namespace's
/// roster and the clock, and on success the act is signed as the node with
/// `on_behalf_of` = the attested name. A refused attestation is a 403
/// naming the [`AttestRefusal`], and nothing is written.
pub(crate) async fn append_act(
    rail: &RingRail,
    journal: &Arc<RingJournal>,
    body: serde_json::Value,
) -> Response {
    let attestation = match body.get("attestation") {
        None => None,
        Some(v) => match serde_json::from_value::<GuestAttestation>(v.clone()) {
            Ok(a) => Some(a),
            Err(e) => {
                tracing::warn!(
                    target: "rails",
                    namespace = journal.namespace(),
                    error = %e,
                    "rail: refused an append — the attestation is not one"
                );
                return err(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("the attestation is malformed: {e}"),
                );
            }
        },
    };
    if attestation.is_none() {
        if let Some(claimed) = body.get("on_behalf_of").and_then(|v| v.as_str()) {
            tracing::warn!(
                target: "rails",
                namespace = journal.namespace(),
                claimed,
                "rail: dropped an on_behalf_of — no attestation, so this door signs as the node"
            );
        }
    }
    // Taken as a `Value` and converted here rather than as `Json<RailAct>`,
    // so a refusal is the rail's own sentence instead of axum's rejection
    // prose (ARCH §10.6).
    let act = match RailAct::from_json(body) {
        Ok(act) => act,
        Err(e) => return err(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
    };
    // Through the rail's ONE roster reader — never the file directly.
    let roster = match rail.roster(journal).await {
        Ok(r) => r,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let stamp = match &attestation {
        None => None,
        Some(a) => {
            let now = commonwealth_core::clock::unix_now_secs() as i64;
            match a.verify(&roster, journal.namespace(), now) {
                Ok(()) => {
                    tracing::debug!(
                        target: "rails",
                        namespace = journal.namespace(),
                        name = %a.name,
                        signer = %a.signer,
                        "rail: attestation honoured — signing as the node on the guest's behalf"
                    );
                    Some(a.name.as_str())
                }
                Err(refusal) => {
                    tracing::warn!(
                        target: "rails",
                        namespace = journal.namespace(),
                        name = %a.name,
                        signer = %a.signer,
                        refusal = refusal.name(),
                        "rail: refused an append — the attestation does not hold"
                    );
                    let body = serde_json::json!({
                        "error": refusal.to_string(),
                        "kind": refusal.name(),
                        "namespace": journal.namespace(),
                    });
                    return (StatusCode::FORBIDDEN, Json(body)).into_response();
                }
            }
        }
    };
    let sealed = matches!(act, RailAct::Seal);
    let appended = if sealed {
        // A seal takes no stamp: it is delivery, not words.
        journal
            .seal(rail.signer(), &roster, &Ed25519Verifier)
            .map(|done| (done.op, Some(retire(&done.retired))))
    } else {
        journal
            .append(act, rail.signer(), &roster, stamp)
            .map(|op| (op, None))
    };
    match appended {
        Ok((op, retired)) => {
            // The whole op rides beside the flat fields, so a dialing client
            // that speaks the rail's TYPES (the mesh round's port, since the
            // journals moved here) reads back an `Op<SignedOp>` instead of
            // re-deriving one from the render (ARCH §10.6). The flat fields
            // stay: they are what a page renders, and the daemon's append
            // door answers with exactly them.
            let mut out = serde_json::json!({
                "id": op.id,
                "seq": op.kind.seq,
                "actor": op.actor,
                "ts_unix": op.ts_unix,
                "namespace": journal.namespace(),
                "op": op,
            });
            if let Some(retired) = retired {
                out["retired"] = retired;
            }
            Json(out).into_response()
        }
        Err(e @ RailError::NotInRoster { .. }) => {
            // `kind` carries the refusal TYPED, not only as prose: the KV
            // pump's defer-on-not-in-roster is a real decision that must
            // survive the dial, and a client matching on the sentence would
            // be prose matching (ARCH principle 9). `actor` names whose key
            // was refused.
            let (actor, namespace) = match &e {
                RailError::NotInRoster { actor, namespace } => (actor.clone(), namespace.clone()),
                _ => (String::new(), String::new()),
            };
            let refusal = serde_json::json!({
                "error": not_in_roster_refusal(rail.roster_origin(journal.namespace()), &e),
                "kind": "not_in_roster",
                "actor": actor,
                "namespace": namespace,
            });
            (StatusCode::UNPROCESSABLE_ENTITY, Json(refusal)).into_response()
        }
        Err(RailError::Rejected(why)) => err(StatusCode::UNPROCESSABLE_ENTITY, why),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// GET /v1/rail/log — the namespace's acts, in one order, and its gaps:
/// admitted ops, each gap's own sentence (the rail's `Display`, so the page
/// and the terminal say the same words), the held count, completeness, and
/// the roster the ops were admitted against.
async fn log_answer(rail: &RingRail, journal: &Arc<RingJournal>) -> Response {
    let roster = match rail.roster(journal).await {
        Ok(r) => r,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    // ONE read, admitted from those exact bytes — reading the journal twice
    // would let a write land between and ship an answer that does not match
    // the ops beside it.
    let (ops, skipped) = match journal.read() {
        Ok(pair) => pair,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let admission = admit(
        &ops,
        &skipped,
        &roster,
        journal.namespace(),
        &Ed25519Verifier,
    );
    let gaps: Vec<serde_json::Value> = admission
        .gaps
        .iter()
        .map(|g| {
            let mut v = serde_json::to_value(g).unwrap_or_default();
            if let Some(obj) = v.as_object_mut() {
                obj.insert("message".into(), serde_json::Value::String(g.to_string()));
            }
            v
        })
        .collect();
    let ops_json: Vec<serde_json::Value> = admission
        .ops
        .iter()
        .map(|op| serde_json::to_value(op).unwrap_or_default())
        .collect();
    Json(serde_json::json!({
        "namespace": journal.namespace(),
        "ops": ops_json,
        "gaps": gaps,
        "held": admission.held,
        "complete": admission.is_complete(),
        "roster": roster,
    }))
    .into_response()
}

/// POST /v1/rail/append.
pub async fn append(
    State(daemon): State<Arc<RailsDaemon>>,
    Query(q): Query<RailQuery>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let namespace = match namespace_of(&q) {
        Ok(ns) => ns,
        Err(refusal) => return refusal,
    };
    let journal = match journal_of(&daemon.rail, &namespace) {
        Ok(j) => j,
        Err(refusal) => return refusal,
    };
    append_act(&daemon.rail, &journal, body).await
}

/// GET /v1/rail/log.
pub async fn log(State(daemon): State<Arc<RailsDaemon>>, Query(q): Query<RailQuery>) -> Response {
    let namespace = match namespace_of(&q) {
        Ok(ns) => ns,
        Err(refusal) => return refusal,
    };
    let journal = match journal_of(&daemon.rail, &namespace) {
        Ok(j) => j,
        Err(refusal) => return refusal,
    };
    log_answer(&daemon.rail, &journal).await
}

// ── The sync doors ───────────────────────────────────────────
//
// The anti-entropy surface a peer's ring round reads and writes the journal
// through: digest, missing, ingest, plus the roster/origin/admit/compact
// reads the round and the KV pump decide by. Every body and answer is
// rail-core JSON (`Digest`, `Op<SignedOp>`, `Roster`, `Admission`) — the
// daemon-to-daemon exchange bodies (`sovereign_peer_wire::RingSyncRequest`)
// are typed over these same shapes, so a receiver can compose its wire
// answer from these doors field for field without a second spelling of any
// of them (ARCH §10.6). The wire struct itself stays daemon-side: this
// binary is built and lifted outside the monorepo and names no
// sovereign-* crate.
//
// They are primitives, not the exchange: the caller composes its round from
// them, the way the local `RingJournal` calls composed it before the
// journals moved here. A `missing` answer carries `more` for exactly the
// reason `ops_missing_from_within` returns it — "everything you lack" and
// "as much as fits" must not be confusable.

/// GET /v1/rail/namespaces — every ring this daemon holds.
pub async fn namespaces(State(daemon): State<Arc<RailsDaemon>>) -> Response {
    match daemon.rail.namespaces() {
        Ok(ns) => Json(serde_json::json!({ "namespaces": ns })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// GET /v1/rail/actor — the identity every line this daemon signs carries.
///
/// The one "who am I talking to" read the KV pump's actor-key parse and a
/// peer's roster reasoning both start from; it is the hex pubkey, so the
/// answer is exactly what a roster names a member by.
pub async fn actor(State(daemon): State<Arc<RailsDaemon>>) -> Response {
    Json(serde_json::json!({ "actor": daemon.rail.signer().actor() })).into_response()
}

/// GET /v1/rail/digest?namespace= — the journal's per-actor high-water marks.
fn digest_answer(journal: &Arc<RingJournal>) -> Response {
    match journal.digest() {
        Ok(d) => Json(serde_json::json!({
            "namespace": journal.namespace(),
            "digest": d,
        }))
        .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// POST /v1/rail/missing?namespace= — what this journal holds that the
/// caller's digest says it lacks, stopped at the caller's byte budget.
#[derive(Debug, Deserialize)]
pub struct MissingBody {
    /// What the caller holds. Absent means "I hold nothing", which asks for
    /// everything — the peer-wire body's own default.
    #[serde(default)]
    pub digest: Digest,
    /// Wire-byte budget for the returned ops. Absent is the budget that
    /// never binds (`NO_BUDGET`); the ring round always names one.
    #[serde(default)]
    pub budget: Option<usize>,
}

fn missing_answer(journal: &Arc<RingJournal>, body: MissingBody) -> Response {
    match journal.ops_missing_from_within(&body.digest, body.budget.unwrap_or(NO_BUDGET)) {
        Ok((ops, more)) => Json(serde_json::json!({
            "namespace": journal.namespace(),
            "ops": ops,
            "more": more,
        }))
        .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// POST /v1/rail/ingest?namespace= — admit the caller's ops as-signed and
/// report how many were new. Ingestion stays signature-checked by the rail;
/// this door adds no second judgement beside the fold (ARCH §10.6).
#[derive(Debug, Default, Deserialize)]
pub struct IngestBody {
    #[serde(default)]
    pub ops: Vec<Op<SignedOp>>,
}

/// A namespace that took new ops is marked dirty on `kv`, so the pump's next
/// tick folds them into the store (fp-109); the fold itself is never done here.
pub(crate) fn ingest_answer(
    journal: &Arc<RingJournal>,
    kv: &crate::kv::KvHost,
    body: IngestBody,
) -> Response {
    match journal.ingest_all(&body.ops) {
        Ok(n) => {
            if n > 0 {
                kv.mark_dirty(journal.namespace());
            }
            Json(serde_json::json!({
                "namespace": journal.namespace(),
                "ingested": n,
            }))
            .into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// GET /v1/rail/roster?namespace= — the namespace's roster AND the origin
/// that answered, in one body. The origin is the one decider a caller needs
/// to know whether the answer was read from a hand-written file or derived
/// from membership; shipping both keeps a caller from pairing two reads that
/// could disagree.
async fn roster_answer(rail: &RingRail, journal: &Arc<RingJournal>) -> Response {
    let origin = match rail.roster(journal).await {
        Ok(r) => {
            let origin = rail.roster_origin(journal.namespace());
            Json(serde_json::json!({
                "namespace": journal.namespace(),
                "origin": match origin {
                    RosterOrigin::File => "file",
                    RosterOrigin::Derived => "derived",
                },
                "roster": r,
            }))
        }
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    origin.into_response()
}

fn read_answer(journal: &Arc<RingJournal>) -> Response {
    match journal.read() {
        Ok((ops, skipped)) => Json(serde_json::json!({
            "namespace": journal.namespace(),
            "ops": ops,
            "skipped": skipped.len(),
        }))
        .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// The roster a caller's admit/compact runs against, in the body.
///
/// The caller's roster, not a fresh server-side read: the round reads the
/// roster once through the roster door and then decides by it, and admit
/// against a DIFFERENT roster than the one the decision used would make the
/// door's answer and the caller's log disagree. The roster travels with the
/// request the way it travelled with the local call.
#[derive(Debug, Deserialize)]
pub struct RosterBody {
    pub roster: Roster,
}

fn admit_answer(journal: &Arc<RingJournal>, body: RosterBody) -> Response {
    match journal.admit(&body.roster, &Ed25519Verifier) {
        Ok(admission) => {
            // `Admission` serialises (ops, gaps, held, floors); `complete`
            // rides beside them because a UI that hid it would be hiding a
            // subset — the log door's own rule.
            let mut v = serde_json::to_value(&admission).unwrap_or_default();
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "complete".into(),
                    serde_json::Value::Bool(admission.is_complete()),
                );
                obj.insert(
                    "namespace".into(),
                    serde_json::Value::String(journal.namespace().to_string()),
                );
            }
            Json(v).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

fn compact_answer(journal: &Arc<RingJournal>, body: RosterBody) -> Response {
    match journal.compact(&body.roster, &Ed25519Verifier) {
        Ok(done) => Json(serde_json::json!({
            "namespace": journal.namespace(),
            "removed": done.removed,
            "kept": done.kept,
            "gaps_cleared": done.gaps_cleared,
            "floors": done.floors,
        }))
        .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// The routed POST forms: namespace/journal extraction once, then the
/// answer fn — the same shape the append door is.
macro_rules! journal_route {
    ($name:ident, $answer:ident, $body:ty) => {
        journal_route!($name, $answer, $body, |_daemon, journal, body| $answer(
            &journal, body
        ));
    };
    ($name:ident, $answer:ident, $body:ty, |$d:ident, $j:ident, $b:ident| $call:expr) => {
        pub async fn $name(
            State(daemon): State<Arc<RailsDaemon>>,
            Query(q): Query<RailQuery>,
            Json(body): Json<$body>,
        ) -> Response {
            let namespace = match namespace_of(&q) {
                Ok(ns) => ns,
                Err(refusal) => return refusal,
            };
            let journal = match journal_of(&daemon.rail, &namespace) {
                Ok(j) => j,
                Err(refusal) => return refusal,
            };
            let ($d, $j, $b) = (&daemon, journal, body);
            $call
        }
    };
}

journal_route!(journal_missing, missing_answer, MissingBody);
journal_route!(
    journal_ingest,
    ingest_answer,
    IngestBody,
    |daemon, journal, body| { ingest_answer(&journal, &daemon.kv, body) }
);
journal_route!(journal_admit, admit_answer, RosterBody);
journal_route!(journal_compact, compact_answer, RosterBody);

/// The routed GET forms take no body.
macro_rules! journal_get_route {
    ($name:ident, $answer:ident) => {
        pub async fn $name(
            State(daemon): State<Arc<RailsDaemon>>,
            Query(q): Query<RailQuery>,
        ) -> Response {
            let namespace = match namespace_of(&q) {
                Ok(ns) => ns,
                Err(refusal) => return refusal,
            };
            let journal = match journal_of(&daemon.rail, &namespace) {
                Ok(j) => j,
                Err(refusal) => return refusal,
            };
            $answer(&journal)
        }
    };
}

journal_get_route!(journal_digest, digest_answer);
journal_get_route!(journal_read, read_answer);

/// GET /v1/rail/roster.
pub async fn journal_roster(
    State(daemon): State<Arc<RailsDaemon>>,
    Query(q): Query<RailQuery>,
) -> Response {
    let namespace = match namespace_of(&q) {
        Ok(ns) => ns,
        Err(refusal) => return refusal,
    };
    let journal = match journal_of(&daemon.rail, &namespace) {
        Ok(j) => j,
        Err(refusal) => return refusal,
    };
    roster_answer(&daemon.rail, &journal).await
}

// ── The live lane ────────────────────────────────────────────

/// How many payloads ONE namespace's buffer holds before the oldest is
/// evicted — the daemon's cap. Bounded because this is memory an untrusted
/// local process can fill.
const LIVE_BUFFER_CAPACITY: usize = 256;

/// The largest live payload, in wire bytes — the daemon's cap, and ONE
/// number on this daemon too: read by the push door that accepts and by no
/// one else, so a payload this daemon would accept is exactly the one it
/// keeps.
const LIVE_PAYLOAD_MAX_BYTES: usize = 4096;

/// The arrived-payload buffer: bounded, in memory, and the only place a
/// live payload ever sits. Keyed by namespace, the way append and log are.
#[derive(Debug, Default)]
pub struct LiveBuffer {
    inner: Mutex<HashMap<String, LiveBufferInner>>,
}

#[derive(Debug, Default)]
struct LiveBufferInner {
    payloads: VecDeque<String>,
    /// Payloads evicted because the buffer was full, since the last drain.
    /// Eviction is a real loss and it is REPORTED, never silent.
    dropped: usize,
}

impl LiveBuffer {
    /// Take one arrived payload for `namespace`. Evicts that namespace's
    /// oldest when full.
    pub fn push(&self, namespace: &str, payload: String) {
        let mut buffers = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let inner = buffers.entry(namespace.to_string()).or_default();
        if inner.payloads.len() == LIVE_BUFFER_CAPACITY {
            inner.payloads.pop_front();
            inner.dropped += 1;
            tracing::warn!(
                target: "rails",
                namespace,
                capacity = LIVE_BUFFER_CAPACITY,
                dropped = inner.dropped,
                "rail live: buffer full, evicted the oldest payload — nobody \
                 is draining GET /v1/rail/live fast enough"
            );
        }
        inner.payloads.push_back(payload);
    }

    /// Empty `namespace`'s buffer, returning what was in it and how many
    /// were lost. Every other namespace's buffer is untouched.
    fn drain(&self, namespace: &str) -> (Vec<String>, usize) {
        let mut buffers = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match buffers.remove(namespace) {
            Some(inner) => (inner.payloads.into(), inner.dropped),
            None => (Vec::new(), 0),
        }
    }
}

/// POST /v1/rail/live — take one ephemeral payload for the named namespace.
///
/// Nothing is written anywhere, and nothing is fanned out: the peer round
/// runs over sovereign-mesh's fabric, which this daemon does not carry, so
/// the answer reports the absence honestly — `peers: []`, `delivered: 0` —
/// rather than a success that would read as "everyone saw it".
fn push_answer(live: &LiveBuffer, namespace: &str, body: axum::body::Bytes) -> Response {
    if body.len() > LIVE_PAYLOAD_MAX_BYTES {
        tracing::debug!(
            target: "rails",
            namespace,
            bytes = body.len(),
            "rail live: refused an oversize payload"
        );
        return err(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "live payload is {} bytes; the lane carries at most {} — \
                 send presence, not state",
                body.len(),
                LIVE_PAYLOAD_MAX_BYTES
            ),
        );
    }
    let payload = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(target: "rails", namespace, error = %e, "rail live: refused a non-UTF-8 payload");
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "live payload is not UTF-8 ({e}) — the lane carries text \
                     (base64 your bytes, as a rail act does)"
                ),
            );
        }
    };
    live.push(namespace, payload.to_string());
    tracing::debug!(
        target: "rails",
        namespace,
        bytes = body.len(),
        "rail live: buffered one payload (no peer fan-out on this daemon)"
    );
    Json(serde_json::json!({
        "bytes": body.len(),
        "peers": [],
        "delivered": 0,
    }))
    .into_response()
}

/// GET /v1/rail/live — drain the caller's namespace since the last call.
fn drain_answer(live: &LiveBuffer, namespace: &str) -> Response {
    let (payloads, dropped) = live.drain(namespace);
    if dropped > 0 {
        tracing::warn!(
            target: "rails",
            namespace,
            dropped,
            drained = payloads.len(),
            "rail live: drain reports evicted payloads — the presence map has \
             a hole in it"
        );
    }
    Json(serde_json::json!({
        "payloads": payloads,
        "dropped": dropped,
    }))
    .into_response()
}

/// POST /v1/rail/live.
pub async fn live_push(
    State(daemon): State<Arc<RailsDaemon>>,
    Query(q): Query<RailQuery>,
    body: axum::body::Bytes,
) -> Response {
    let namespace = match namespace_of(&q) {
        Ok(ns) => ns,
        Err(refusal) => return refusal,
    };
    push_answer(&daemon.rail_live, &namespace, body)
}

/// GET /v1/rail/live.
pub async fn live_drain(
    State(daemon): State<Arc<RailsDaemon>>,
    Query(q): Query<RailQuery>,
) -> Response {
    let namespace = match namespace_of(&q) {
        Ok(ns) => ns,
        Err(refusal) => return refusal,
    };
    drain_answer(&daemon.rail_live, &namespace)
}

#[cfg(test)]
mod tests;
