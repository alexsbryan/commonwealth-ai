// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring rail's doors — the surface a ring app writes its own state to.
//!
//! `POST /v1/rail/append`, `GET /v1/rail/log`, `POST+GET /v1/rail/live`,
//! mounted beside the mesh routes in [`crate::api`]. The append and log
//! bodies mirror the inference daemon's `routes_rail` door for door — same
//! paths, same server-assigned fields, same retire rendering, same gap
//! sentences — so a page written against one daemon behaves the same against
//! the other (ARCH §10.6). What is deliberately NOT mirrored is the guest
//! half: there are no grants and no sessions here (loopback IS the auth),
//! so the namespace is always named explicitly and no caller can stamp an
//! act on another's behalf.
//!
//! **The journals live under THIS process's data root**, never the daemon's
//! (§4 rule 1 — one data directory, one owner; a second process never opens
//! the daemon's dir, and the daemon's journals migrate in fp-54's commit, so
//! there is no dual-writer window). The signer is the node key both
//! processes load through the ONE loader
//! (`commonwealth_transport::identity::load_or_generate_node_key`), so a
//! line this daemon writes verifies under the roster every peer already
//! holds — the wire contract five-programs-6 pinned: the signer identity
//! does not change.
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
    admit, Compaction, Ed25519Verifier, Person, RailAct, RailError, RingJournal, RingRail, Roster,
    RosterOrigin, RosterSource,
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
    mesh: Weak<RwLock<Mesh>>,
    self_id: NodeId,
    self_pubkey: Option<NodePubkey>,
}

impl MembershipRosterSource {
    /// Install membership as `rail`'s DEFAULT roster — every ring nobody
    /// narrowed admits everyone in the mesh. Rails registers no namespace of
    /// its own: the daemon's own rings are the daemon's
    /// (`sovereign_mesh::ring_roster::REGISTERED_NAMESPACES`), and a file
    /// `roster.json` still narrows any ring here, as the rail intends.
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

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
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
/// number or actor could write as somebody else. A body that carries
/// `on_behalf_of` is dropped HERE, before anything is signed, and warned:
/// there are no sessions on a rails daemon, so the door signs as the node
/// and a caller cannot stamp — the same drop the daemon's `stamp_from`
/// makes for its session-less callers.
async fn append_act(
    rail: &RingRail,
    journal: &Arc<RingJournal>,
    body: serde_json::Value,
) -> Response {
    if let Some(claimed) = body.get("on_behalf_of").and_then(|v| v.as_str()) {
        tracing::warn!(
            target: "rails",
            namespace = journal.namespace(),
            claimed,
            "rail: dropped an on_behalf_of — this door signs as the node, a caller cannot stamp"
        );
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
    let sealed = matches!(act, RailAct::Seal);
    let appended = if sealed {
        // A seal takes no stamp: it is delivery, not words.
        journal
            .seal(rail.signer(), &roster, &Ed25519Verifier)
            .map(|done| (done.op, Some(retire(&done.retired))))
    } else {
        journal
            .append(act, rail.signer(), &roster, None)
            .map(|op| (op, None))
    };
    match appended {
        Ok((op, retired)) => {
            let mut out = serde_json::json!({
                "id": op.id,
                "seq": op.kind.seq,
                "actor": op.actor,
                "ts_unix": op.ts_unix,
                "namespace": journal.namespace(),
            });
            if let Some(retired) = retired {
                out["retired"] = retired;
            }
            Json(out).into_response()
        }
        Err(e @ RailError::NotInRoster { .. }) => err(
            StatusCode::UNPROCESSABLE_ENTITY,
            not_in_roster_refusal(rail.roster_origin(journal.namespace()), &e),
        ),
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
