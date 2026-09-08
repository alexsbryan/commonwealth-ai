// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring journal's own replication loop — slower than gossip, and by
//! digest rather than by snapshot.
//!
//! # Why this is a digest and not a snapshot
//!
//! `gossip.rs` used to carry a fourth step that shipped a **full mesh-store
//! snapshot to every online peer every ten seconds** — 8,640 rounds a day. A
//! household writes on the order of 3,500 journal ops a year, call it 1.5 MB,
//! so riding that push would have cost roughly **246 GB/day of egress per
//! node** and taxed every other namespace on the same body forever. Bandwidth
//! is the binding constraint for this feature and it binds on day one.
//!
//! So: a sixty-second cadence (ample for money), and an exchange whose
//! request is a ~600-byte digest rather than the journal.
//!
//! At cw-lift rung 2e that argument stopped being a comparison and became the
//! whole story: the push, its `/internal/app/state` route and the
//! event-driven `broadcast_now` beside it are deleted, `MeshStore` is a
//! projection of these journals on both sides of the wire, and this is the ONE
//! sender of replicated state in the workspace. The nudge is what keeps that
//! affordable for a latency-sensitive writer — `AppState::ring_write_nudge`
//! starts a round when a local write reaches a journal, so the sixty seconds
//! is a ceiling on idleness rather than on a write.
//!
//! # The exchange
//!
//! Two calls per chunk per peer per namespace, both idempotent:
//!
//! 1. **`{digest_mine, ops: []}`** → the peer ingests nothing, answers with
//!    its own digest and one budget's worth of the ops our digest says we
//!    lack. We ingest those.
//! 2. **`{digest_mine', ops: one budget of what_they_lack}`** → computed from
//!    the digest they just gave us. They ingest; we read the count back.
//!
//! A dropped call costs one round of convergence and never a duplicate entry,
//! because ingest is keyed on the content-addressed op id.
//!
//! # …and it repeats, because one body is not the unit of convergence
//!
//! Both `ops` arrays are stopped at
//! [`RING_SYNC_OPS_BUDGET_BYTES`](commonwealth_api::routes_internal::RING_SYNC_OPS_BUDGET_BYTES),
//! and [`exchange`] repeats the pair until neither side moves. Nothing on the
//! wire changed shape for that: the exchange was always idempotent, so a
//! partial one is safe and the second half is just the next call.
//!
//! Before the budget, one exchange carried the whole selection and the
//! receiver's `DefaultBodyLimit` refused it at ~9,599 ops of the measured
//! 594-byte fixture. The refusal was answered at the extractor, so the
//! handler never ran and its gauge could not fire; this loop mapped the 413
//! to a string, logged it at DEBUG and counted the peer as UNREACHABLE. A
//! peer that had been refused the journal then reported zero ops, zero gaps
//! and a complete ring.
//!
//! **Why repeating terminates** (and it must, or a ceiling that was at least
//! measurable becomes a silent spin): a chunk's first op is always one the
//! receiver provably lacks — a contiguous mark of `n` means they do not hold
//! `n + 1`, and the selection is ordered and filtered so `n + 1` is the
//! lowest element it can yield. Every non-empty chunk therefore moves the
//! receiver's mark. Holdings are finite, so the loop runs out of work; and
//! [`MAX_CHUNKS_PER_EXCHANGE`] bounds it anyway, because a bound you can name
//! beats an argument you have to trust.
//!
//! # Everyone republishes everything they hold
//!
//! Call 2 sends what the PEER lacks out of everything WE hold, with no filter
//! on who authored it. Three failure modes die at once: the author's node
//! dying before anyone else came online, a peer restart wiping in-memory
//! buffers, and a housemate leaving the ring with half the journal. It is also
//! why there is no own-origin skip to get wrong here — the mesh store's
//! `origin` names the last republisher rather than the author, and this path
//! has no origin field at all because the op carries its author in a
//! signature.

use std::sync::Arc;
use std::time::{Duration, Instant};

use commonwealth_api::routes_internal::{
    RingSyncRequest, RingSyncResponse, RING_SYNC_OPS_BUDGET_BYTES,
};
use commonwealth_api::state::AppState;
use commonwealth_core::mesh::NodeStatus;
use commonwealth_transport::{peer_contact, TrafficClass};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// Money does not need ten-second convergence, and the bandwidth argument in
/// the module docs says it must not have it.
pub const DEFAULT_RING_SYNC_INTERVAL: Duration = Duration::from_secs(60);

/// How many chunked round-trips one [`exchange`] makes before it hands the
/// round back.
///
/// A **safety valve, not a throughput knob.** The termination argument in the
/// module docs is the real reason the loop stops; this is what makes the loop
/// stop even if that argument is one day wrong, which is the whole difference
/// between a ceiling that is measurable and a sync loop that silently spins.
///
/// Sixteen chunks of the four-megabyte budget is 64 MiB per peer per
/// namespace per round — about 76,800 ops of the 594-byte fixture the ceiling
/// was measured against, or twenty years of the ~3,500 ops/year household
/// journal the module docs price, moved in one sixty-second round. Tripping
/// it costs nothing but time: every op ingested is already on disk, so the
/// next round resumes where this one stopped.
const MAX_CHUNKS_PER_EXCHANGE: usize = 16;

/// Handle to the spawned loop. Aborts the task on drop, matching
/// [`GossipHandle`](crate::gossip::GossipHandle) so the daemon tears both
/// down the same way.
pub struct RingSyncHandle {
    _task: tokio::task::JoinHandle<()>,
}

impl Drop for RingSyncHandle {
    fn drop(&mut self) {
        self._task.abort();
    }
}

/// What one round moved. Returned rather than only logged so a test can
/// assert convergence instead of asserting on log lines.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RoundOutcome {
    pub namespaces: usize,
    pub peers_reached: usize,
    pub peers_unreachable: usize,
    /// Peers that ANSWERED and refused our body as too large.
    ///
    /// Counted apart from `peers_unreachable` because they are different
    /// facts, and collapsing them is what kept the convergence ceiling
    /// invisible: a peer saying "that body is over my limit" is a peer that
    /// received the request, and filing it as one that could not be dialled
    /// reports the wrong problem to whoever goes looking (ARCH §18.3).
    ///
    /// With the exchange budgeted against the same limit, this reads zero
    /// against peers on this build. A non-zero count means a peer's limit is
    /// lower than this build's budget — which is worth seeing, and used not
    /// to be visible at any level.
    pub peers_refused: usize,
    pub ops_pulled: usize,
    pub ops_pushed: usize,
    /// Namespaces whose journal was folded back into the mesh store this round.
    /// Counted apart from `namespaces` because a measurements ring, a journal
    /// that would not admit and a namespace the store refuses are all "not
    /// projected" and none of them is "nothing to project" (ARCH §18.2).
    pub namespaces_projected: usize,
}

/// Spawn the periodic ring-sync task. Call once per daemon start.
///
/// Runs one round **immediately** before entering the interval, because the
/// first thing a node that has been offline owes its ring is everything it
/// holds — waiting a full minute to boot-republish would leave a freshly
/// restarted peer confidently reporting a total over a subset for that whole
/// minute.
///
/// `nudge` is the wake-up [`crate::rail_kv_pump`] fires after it signs a local
/// write onto a journal. Sixty seconds is the right cadence for anti-entropy
/// and the wrong one for "I just wrote something", and the two do not have to
/// be the same number: the nudge starts a round now, and the round is the same
/// round. **Nothing about the sender census changes** — the pump does not talk
/// to a peer, it asks this loop to.
pub fn spawn_ring_sync_loop(
    app_state: AppState,
    interval: Duration,
    nudge: Arc<Notify>,
) -> RingSyncHandle {
    let task = tokio::spawn(async move {
        info!(
            interval_secs = interval.as_secs(),
            "ring sync: loop started"
        );
        loop {
            let started = Instant::now();
            let outcome = run_one_round(&app_state).await;
            if outcome.namespaces > 0 {
                debug!(
                    namespaces = outcome.namespaces,
                    peers_reached = outcome.peers_reached,
                    peers_unreachable = outcome.peers_unreachable,
                    peers_refused = outcome.peers_refused,
                    ops_pulled = outcome.ops_pulled,
                    ops_pushed = outcome.ops_pushed,
                    projected = outcome.namespaces_projected,
                    round_ms = started.elapsed().as_millis() as u64,
                    "ring sync: round"
                );
            }
            // `Notify::notify_one` stores one permit when nobody is waiting, so
            // a write that lands mid-round wakes the NEXT sleep rather than
            // being lost — which is exactly the write that most needs to go.
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = nudge.notified() => {
                    debug!("ring sync: woken by a local write rather than the interval");
                }
            }
        }
    });
    RingSyncHandle { _task: task }
}

/// One anti-entropy pass over every namespace this node holds, against every
/// online peer.
pub async fn run_one_round(app_state: &AppState) -> RoundOutcome {
    let mut outcome = RoundOutcome::default();
    let Some(rail) = app_state.ring_rail() else {
        return outcome;
    };
    let namespaces = match rail.namespaces() {
        Ok(n) => n,
        Err(e) => {
            warn!(error = %e, "ring sync: cannot enumerate namespaces");
            return outcome;
        }
    };
    if namespaces.is_empty() {
        return outcome;
    }
    let http = match crate::gossip::gossip_client() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "ring sync: no http client");
            return outcome;
        }
    };

    let self_id = *app_state.inner.self_node_id_swap.load_full().as_ref();
    let peers: Vec<commonwealth_transport::PeerContact> = {
        let mesh = app_state.inner.mesh.read().await;
        mesh.members
            .values()
            .filter(|m| m.node_id != self_id && m.status == NodeStatus::Online)
            .map(peer_contact)
            .collect()
    };
    if peers.is_empty() {
        return outcome;
    }
    let transport = app_state.peer_transport();

    for namespace in &namespaces {
        outcome.namespaces += 1;
        let journal = match rail.journal(namespace) {
            Ok(l) => l,
            Err(e) => {
                warn!(namespace, error = %e, "ring sync: cannot open journal");
                continue;
            }
        };
        for contact in &peers {
            let endpoints = transport.endpoints(contact, TrafficClass::Gossip).await;
            let mut reached = false;
            let mut refused = false;
            for ep in &endpoints {
                let url = format!("{}/internal/ring/sync", ep.base_url);
                let ex = exchange(http, &url, &rail, &journal).await;
                // Counted BEFORE the verdict is read. `ingest_all` has already
                // written these lines to disk, so they are progress whether or
                // not a later call in the same exchange failed — the old shape
                // returned `Err` and threw the pulled count away, which made
                // `ops_pulled` undercount exactly in the failure case.
                outcome.ops_pulled += ex.pulled;
                outcome.ops_pushed += ex.pushed;
                match ex.stop {
                    None => {
                        reached = true;
                        break; // one working address is enough
                    }
                    Some(ExchangeStop::Refused { sent_bytes }) => {
                        refused = true;
                        warn!(
                            peer = %contact.node_id,
                            url = %url,
                            sent_bytes,
                            budget_bytes = RING_SYNC_OPS_BUDGET_BYTES,
                            "ring sync: peer refused the body as too large — it \
                             ANSWERED, so this is not an unreachable peer: its \
                             body limit is below this build's exchange budget"
                        );
                    }
                    Some(ExchangeStop::Failed(detail)) => {
                        debug!(
                            peer = %contact.node_id,
                            url = %url,
                            detail,
                            "ring sync: exchange failed, trying next address"
                        );
                    }
                }
            }
            if reached {
                outcome.peers_reached += 1;
            } else if refused {
                outcome.peers_refused += 1;
            } else {
                outcome.peers_unreachable += 1;
            }
        }

        // ── The journal is truth; the store is its projection.
        //
        // ONCE per namespace per round, after every peer, and NOT inside
        // `exchange`. Two reasons, and the second is the one that matters:
        //
        // - Folding is one Ed25519 verify per op, so doing it per chunk would
        //   pay a bootstrap's whole read cost sixteen times over for one
        //   answer that does not change until the last chunk lands.
        // - **Half the ops this node receives never pass through `exchange`
        //   at all.** A peer PUSHES on call 2 of its own exchange, and those
        //   ops arrive through `/internal/ring/sync` — a route, in another
        //   crate, that this loop never runs. A projection hung off our own
        //   pull would be blind to exactly the direction the pump's nudge
        //   creates, and a write would reach a peer's disk immediately and its
        //   store never.
        if let Ok(j) = rail.journal(namespace) {
            if crate::rail_kv_pump::project_namespace(app_state, &rail, &j)
                .await
                .is_some()
            {
                outcome.namespaces_projected += 1;
            }
        }
    }
    outcome
}

/// Why an exchange with one peer address stopped.
#[derive(Debug)]
enum ExchangeStop {
    /// The peer **answered**, and refused our body as too large.
    ///
    /// Its own variant because the alternative is the collapse this rung
    /// exists to undo: a 413 came back as `Err("HTTP 413")`, indistinguishable
    /// from a dead socket, and the round filed a reachable peer under
    /// `peers_unreachable` at DEBUG.
    Refused { sent_bytes: usize },
    /// Everything else this address could stop on: no route, a timeout, a
    /// 5xx, an answer this build could not read, or **this node's own journal
    /// refusing to read**. They are one variant because the round does the
    /// same thing with all of them — try the next address, then count the
    /// peer unreachable — and the local case is spelled `local journal: …` in
    /// the detail so the log still says whose fault it was.
    Failed(String),
}

/// What one exchange moved, and why it stopped.
///
/// **Progress and failure are reported together, and that is the point.**
/// `ingest_all` writes to the journal, so ops pulled in call 1 are durably
/// held whether or not a later call fails. The old signature was
/// `Result<(usize, usize), String>`, so a failing call 2 returned `Err` and
/// threw the pulled count away — `ops_pulled` undercounted exactly in the
/// failure case, which is the case anyone reading the metric is looking for.
#[derive(Debug, Default)]
struct ExchangeOutcome {
    pulled: usize,
    pushed: usize,
    stop: Option<ExchangeStop>,
}

impl ExchangeOutcome {
    /// Keep what moved; record why it stopped.
    fn stopped(mut self, stop: ExchangeStop) -> Self {
        self.stop = Some(stop);
        self
    }
}

/// Delete what a peer's seal just retired, on THIS node's disk.
///
/// **The half that actually bounds storage.** The author's own node prunes
/// when it seals (`routes_rail::append`), but that bounds the writer's disk and
/// nobody else's — every housemate would still keep a full copy of everyone's
/// history forever, which is the growth this exists to stop. A seal is a signed
/// statement in the one total order, so it binds whoever admits it, and
/// `RingJournal::compact` is already author-blind.
///
/// **Only when a seal actually arrived**, and that gate is not frugality for
/// its own sake: compaction re-admits, which is one Ed25519 verify per op
/// (2a measured the fold at 38.4 µs/op, 94% of it that verify), and a floor
/// cannot move without a seal. Running it every round would pay the journal's
/// whole read cost every sixty seconds to discover nothing changed.
///
/// It sits INSIDE the chunk loop rather than after it, deliberately. The
/// trigger is the seal and not the chunk, so a bootstrap pulling sixteen
/// chunks pays this once per seal it actually meets — and compaction keeps
/// that number at roughly one, since every seal but the highest is itself
/// below the floor and goes. Hoisting it out would be cheaper only in a case
/// the mechanism prevents, and would skip the prune entirely on every path
/// where a later chunk fails after the seal has already landed.
///
/// **Fails closed, everywhere it can fail.** The floor comes from
/// `Admission::floors`, so a seal this node cannot authenticate against the
/// roster it holds retires nothing — a stale or partial roster under-prunes
/// rather than over-prunes. The roster is the rail's one reader
/// (`RingRail::roster`), so `mesh-measurements` — whose roster is derived from
/// membership and whose file is empty — prunes exactly as a hand-rostered ring
/// does; until it went through that door this path read the file and retired
/// nothing on the daemon's own namespace. A refusal is logged and the exchange
/// carries on: the ops are ingested and durable, and a journal that stayed
/// long is a worse outcome than a stalled round, not a reason to make one.
async fn prune_what_the_peer_retired(
    rail: &commonwealth_rail::RingRail,
    journal: &commonwealth_rail::RingJournal,
) {
    let namespace = journal.namespace();
    let roster = match rail.roster(journal).await {
        Ok(r) => r,
        Err(e) => {
            warn!(namespace, error = %e, "ring sync: a seal arrived and the roster is unreadable, so nothing was pruned");
            return;
        }
    };
    match journal.compact(&roster, &commonwealth_rail::Ed25519Verifier) {
        Ok(done) if done.removed > 0 => {
            info!(
                namespace,
                removed = done.removed,
                kept = done.kept,
                "ring sync: a peer's seal retired lines on this node"
            );
        }
        Ok(_) => {}
        Err(e) => {
            warn!(namespace, error = %e, "ring sync: the prune a peer's seal authorises was refused")
        }
    }
}

/// The chunked exchange with one peer address: two calls per chunk, repeated
/// until neither side moves or [`MAX_CHUNKS_PER_EXCHANGE`] is spent.
///
/// See the module docs for why repeating terminates. The two stopping
/// conditions below are the honest ones and neither is silent: *converged*
/// (nothing came, nothing left to send) returns quietly, and *stalled*
/// (something outstanding, nothing moved anywhere) warns — repeating that
/// would repeat verbatim.
async fn exchange(
    http: &reqwest::Client,
    url: &str,
    rail: &commonwealth_rail::RingRail,
    journal: &commonwealth_rail::RingJournal,
) -> ExchangeOutcome {
    let namespace = journal.namespace();
    let mut out = ExchangeOutcome::default();
    let mut last_peer_digest: Option<commonwealth_rail::Digest> = None;

    for chunk in 1..=MAX_CHUNKS_PER_EXCHANGE {
        // ── Call 1 — learn what they have, take one chunk of what we lack.
        let mine = match journal.digest() {
            Ok(d) => d,
            Err(e) => return out.stopped(ExchangeStop::Failed(format!("local journal: {e}"))),
        };
        let first = match post(
            http,
            url,
            &RingSyncRequest {
                namespace: namespace.to_string(),
                digest: mine,
                ops: Vec::new(),
            },
        )
        .await
        {
            Ok(r) => r,
            Err(stop) => return out.stopped(stop),
        };
        let pulled = match journal.ingest_all(&first.ops) {
            Ok(n) => n,
            Err(e) => return out.stopped(ExchangeStop::Failed(format!("local journal: {e}"))),
        };
        out.pulled += pulled;
        if pulled > 0
            && first
                .ops
                .iter()
                .any(|o| matches!(o.kind.act, commonwealth_rail::RailAct::Seal))
        {
            prune_what_the_peer_retired(rail, journal).await;
        }

        // The peer's OWN report of what it holds — the one progress signal
        // that cannot be faked by a count an older build did not send.
        let peer_digest_moved = last_peer_digest.as_ref() != Some(&first.digest);
        last_peer_digest = Some(first.digest.clone());

        // ── Call 2 — give them one chunk of what they lack, out of
        // everything we now hold.
        let (for_peer, more_for_peer) =
            match journal.ops_missing_from_within(&first.digest, RING_SYNC_OPS_BUDGET_BYTES) {
                Ok(v) => v,
                Err(e) => return out.stopped(ExchangeStop::Failed(format!("local journal: {e}"))),
            };
        let offered = for_peer.len();
        let pushed = if for_peer.is_empty() {
            0
        } else {
            let refreshed = match journal.digest() {
                Ok(d) => d,
                Err(e) => return out.stopped(ExchangeStop::Failed(format!("local journal: {e}"))),
            };
            match post(
                http,
                url,
                &RingSyncRequest {
                    namespace: namespace.to_string(),
                    digest: refreshed,
                    ops: for_peer,
                },
            )
            .await
            {
                Ok(second) => second.ingested,
                // `out.pulled` is KEPT: call 1's ops are on disk already.
                Err(stop) => return out.stopped(stop),
            }
        };
        out.pushed += pushed;

        debug!(
            namespace,
            url,
            chunk,
            pulled,
            pushed,
            peer_sent = first.ops.len(),
            offered,
            more_for_peer,
            "ring sync: chunk"
        );

        if pulled == 0 && offered == 0 {
            return out; // converged
        }
        if pulled == 0 && pushed == 0 && !peer_digest_moved {
            warn!(
                namespace,
                url,
                chunk,
                offered,
                "ring sync: exchange stalled — the peer's digest did not move \
                 and nothing was ingested, so repeating would repeat verbatim; \
                 leaving the rest to the next round"
            );
            return out;
        }
    }
    warn!(
        namespace,
        url,
        chunks = MAX_CHUNKS_PER_EXCHANGE,
        pulled = out.pulled,
        pushed = out.pushed,
        "ring sync: exchange hit its chunk bound with work outstanding — every \
         op moved is already on disk, so the next round resumes here"
    );
    out
}

async fn post(
    http: &reqwest::Client,
    url: &str,
    body: &RingSyncRequest,
) -> Result<RingSyncResponse, ExchangeStop> {
    // Serialised here rather than handed to `.json(body)` so the byte count
    // the receiver's limit judges is a number THIS side can name in a log.
    let payload = match serde_json::to_vec(body) {
        Ok(p) => p,
        Err(e) => return Err(ExchangeStop::Failed(format!("local encode: {e}"))),
    };
    let sent_bytes = payload.len();
    let resp = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(payload)
        .send()
        .await
        .map_err(|e| ExchangeStop::Failed(e.to_string()))?;
    let status = resp.status();
    if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        return Err(ExchangeStop::Refused { sent_bytes });
    }
    if !status.is_success() {
        return Err(ExchangeStop::Failed(format!("HTTP {status}")));
    }
    resp.json::<RingSyncResponse>()
        .await
        .map_err(|e| ExchangeStop::Failed(e.to_string()))
}

#[cfg(test)]
mod tests {
    //! The loop itself, against a live listener.
    //!
    //! `exchange` needs a `reqwest` client and a real socket, so these bind
    //! `commonwealth_api::server::internal_router` on an ephemeral port — the
    //! same shape `tests/main/gossip_integration.rs` uses for the gossip loop,
    //! and the only way to drive the production loop rather than a second
    //! spelling of it (ARCH §10.6).

    use super::*;
    use axum::response::IntoResponse;
    use commonwealth_api::server::internal_router;
    use commonwealth_core::ids::{MeshId, NodeId};
    use commonwealth_core::mesh::Mesh;
    use commonwealth_rail::{
        actor_of, body_json, sign_ring_op, Ed25519Verifier, Op, Payload, Person, RailAct,
        RingJournal, RingRail, Roster, SignedOp, SigningKey,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    const NS: &str = "house-expenses";

    /// The fixture 2a's ceiling table is quoted against: a 594-byte
    /// serialised body, ~873 B/op on the wire.
    const FIXTURE_BODY_BYTES: usize = 594;

    fn bare_state() -> AppState {
        let mesh = Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id: MeshId::from_u128(7),
            name: "Test".into(),
            invite_key_hash: [3u8; 32],
            invite_version: 0,
            require_encryption: false,
            members: HashMap::new(),
            peers: vec![],
        };
        AppState::new(NodeId::from_u128(1), mesh)
    }

    fn body_of_size(target: usize) -> Payload {
        let mut filler = target.saturating_sub(40);
        loop {
            let p = Payload::new(serde_json::json!({ "b": "x".repeat(filler) })).unwrap();
            let n = body_json(&RailAct::Record { payload: p.clone() }).len();
            if n >= target {
                return p;
            }
            filler += target - n;
        }
    }

    /// One op signed for its own `(namespace, ts, seq)`, so its `OpId` is
    /// distinct and a fixture of clones cannot make convergence look real.
    fn signed(key: &SigningKey, seq: u64, act: RailAct) -> Op<SignedOp> {
        signed_in(NS, key, seq, act)
    }

    /// A signature binds the namespace, so an op signed for `NS` is a gap on
    /// any other ring — a fixture reused across namespaces would make every
    /// test on the second ring pass or fail for that reason alone.
    fn signed_in(ns: &str, key: &SigningKey, seq: u64, act: RailAct) -> Op<SignedOp> {
        let ts = 1_700_000_000i64 + seq as i64;
        let sig = sign_ring_op(key, ns, ts, seq, &body_json(&act));
        Op::new(SignedOp { seq, sig, act }, ts, actor_of(key))
    }

    fn ops(key: &SigningKey, n: usize) -> Vec<Op<SignedOp>> {
        ops_in(NS, key, n)
    }

    fn ops_in(ns: &str, key: &SigningKey, n: usize) -> Vec<Op<SignedOp>> {
        let payload = body_of_size(FIXTURE_BODY_BYTES);
        (0..n as u64)
            .map(|seq| {
                signed_in(
                    ns,
                    key,
                    seq,
                    RailAct::Record {
                        payload: payload.clone(),
                    },
                )
            })
            .collect()
    }

    fn solo_roster(key: &SigningKey) -> Roster {
        let mut m = std::collections::BTreeMap::new();
        m.insert(Person::from("alex"), vec![actor_of(key)]);
        Roster::new(m)
    }

    /// A node holding `n` signed ops. `n = 0` is a node that has never seen
    /// this ring — the bootstrap case, which can only ever be TOLD.
    fn node(
        dir: &std::path::Path,
        key: &SigningKey,
        n: usize,
    ) -> (AppState, Arc<RingJournal>, Arc<RingRail>) {
        let state = bare_state();
        let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
        let journal = rail.journal(NS).unwrap();
        journal.set_roster(&solo_roster(key)).unwrap();
        if n > 0 {
            assert_eq!(journal.ingest_all(&ops(key, n)).unwrap(), n);
        }
        state.install_ring_rail(rail.clone());
        (state, journal, rail)
    }

    async fn serve(router: axum::Router) -> String {
        let addr = serve_at(router).await;
        format!("http://{addr}/internal/ring/sync")
    }

    /// [`serve`] for a caller that needs the ADDRESS rather than the route —
    /// `run_one_round` dials a member's `addresses`, so a test that drives the
    /// round has to put a real one in the mesh.
    async fn serve_at(router: axum::Router) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        addr
    }

    /// **The gate, and the thing 2a measured.** A 10,000-op journal — past
    /// the 9,599-op one-exchange ceiling — converges onto a peer that has
    /// never seen this ring, through the real route and the real loop.
    ///
    /// The bootstrap case matters because it is the ONLY one the ceiling
    /// could break: `run_one_round` enumerates `rail.namespaces()` from disk
    /// and returns before dialling anyone when the list is empty, so a node
    /// with no `rings/<ns>/` never asks — it can only be told, over the one
    /// direction that has a body limit.
    ///
    /// Watched RED by raising `RING_SYNC_OPS_BUDGET_BYTES` above the body
    /// limit, which is exactly the unbudgeted shape this replaced: the first
    /// push is refused 413, `pushed` is 0, and the peer folds a ring that is
    /// empty and calls itself complete.
    #[tokio::test]
    async fn a_journal_past_the_one_exchange_ceiling_converges_onto_a_fresh_peer() {
        const N: usize = 10_000;
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let (sender_dir, peer_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (_sender, journal, rail) = node(sender_dir.path(), &key, N);
        let (peer_state, peer_journal, _r2) =
            node(peer_dir.path(), &SigningKey::from_bytes(&[2u8; 32]), 0);

        let url = serve(internal_router(peer_state)).await;
        let out = exchange(&reqwest::Client::new(), &url, &rail, &journal).await;

        assert!(out.stop.is_none(), "the exchange failed: {:?}", out.stop);
        assert_eq!(out.pushed, N, "every op landed on the peer");
        assert_eq!(out.pulled, 0, "a peer holding nothing has nothing to give");

        let admitted = peer_journal
            .admit(&solo_roster(&key), &Ed25519Verifier)
            .unwrap();
        assert_eq!(admitted.ops.len(), N);
        assert!(admitted.is_complete(), "gaps: {:?}", admitted.gaps);
        assert_eq!(
            peer_journal.digest().unwrap(),
            journal.digest().unwrap(),
            "two nodes, one claim"
        );
    }

    /// **A peer's seal shortens THIS node's disk, in the round it arrives.**
    ///
    /// The author's own node prunes when it seals, and that bounds the writer's
    /// disk and nobody else's — without this, every housemate keeps a full copy
    /// of everyone's history forever and the retention rung buys one node's
    /// storage instead of the ring's. A seal is a signed statement in the one
    /// total order, so it binds whoever admits it.
    ///
    /// The assertion is on what is on DISK afterwards, not on a return value:
    /// `exchange` reports ops moved, and a prune that silently did nothing
    /// would leave every count in this test identical.
    #[tokio::test]
    async fn a_peers_seal_prunes_this_nodes_disk_in_the_round_it_arrives() {
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let (mine, theirs) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (_me, journal, rail) = node(mine.path(), &key, 3);
        let (peer_state, peer_journal, _r2) = node(theirs.path(), &key, 3);
        // The peer has sealed; we have not heard about it yet.
        peer_journal
            .ingest(&signed(&key, 3, RailAct::Seal))
            .unwrap();
        assert_eq!(journal.read().unwrap().0.len(), 3, "control: we hold three");

        let url = serve(internal_router(peer_state)).await;
        let out = exchange(&reqwest::Client::new(), &url, &rail, &journal).await;

        assert!(out.stop.is_none(), "the exchange failed: {:?}", out.stop);
        assert_eq!(out.pulled, 1, "one op came over, and it was the seal");
        let held = journal.read().unwrap().0;
        assert_eq!(held.len(), 1, "the retired prefix left our disk: {held:?}");
        assert!(matches!(held[0].kind.act, RailAct::Seal));

        // And a pruned node is not a broken one, on either side of the wire.
        let admitted = journal.admit(&solo_roster(&key), &Ed25519Verifier).unwrap();
        assert!(admitted.is_complete(), "gaps: {:?}", admitted.gaps);
        assert_eq!(
            journal.digest().unwrap(),
            peer_journal.digest().unwrap(),
            "two nodes, one claim"
        );
    }

    /// **The daemon's own namespace prunes too.** `mesh-measurements` has no
    /// `roster.json`; its roster is derived from membership. Before the rail
    /// had one roster reader the prune read the file, found nobody, and a
    /// peer's seal retired nothing on that ring — the control half of this
    /// test, kept so the fix is watched to matter. With the membership source
    /// installed beside the rail, the same exchange retires the prefix exactly
    /// as it does on a hand-rostered ring.
    #[tokio::test]
    async fn a_peers_seal_prunes_the_daemons_own_namespace_whose_roster_is_derived() {
        use crate::ring_roster::tests::{member, mesh_of, pubkey_of};
        use crate::ring_roster::MeshRosterSource;
        const OWN: &str = sovereign_core::mesh_measurements::MEASUREMENTS_APP_ID;
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let me = NodeId::from_u128(1);

        // A node on the daemon's namespace: no roster file, ever.
        let node_on_own = |dir: &std::path::Path, with_source: bool| {
            let state = AppState::new(me, mesh_of(vec![member(me, "me", Some(pubkey_of(&key)))]));
            let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
            let journal = rail.journal(OWN).unwrap();
            assert_eq!(journal.ingest_all(&ops_in(OWN, &key, 3)).unwrap(), 3);
            if with_source {
                MeshRosterSource::install(&rail, &state).unwrap();
            }
            state.install_ring_rail(rail.clone());
            (state, journal, rail)
        };
        let sealed_peer = |dir: &std::path::Path| {
            let (state, journal, _r) = node_on_own(dir, false);
            journal
                .ingest(&signed_in(OWN, &key, 3, RailAct::Seal))
                .unwrap();
            state
        };

        // Control: the file is the reader, and the file is empty.
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (_s, journal, rail) = node_on_own(a.path(), false);
        let url = serve(internal_router(sealed_peer(b.path()))).await;
        let out = exchange(&reqwest::Client::new(), &url, &rail, &journal).await;
        assert_eq!(out.pulled, 1, "{:?}", out.stop);
        assert_eq!(
            journal.read().unwrap().0.len(),
            4,
            "control: without the source the seal arrives and retires nothing"
        );

        // The fix: the membership answers, and the prefix goes.
        let (c, d) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (_s, journal, rail) = node_on_own(c.path(), true);
        // The door admits this node's own key on its own ring — the refusal
        // `svrn ring seal mesh-measurements` used to hit. Asserted first so a
        // failure below is about the prune and not about the roster.
        let roster = rail.roster(&journal).await.unwrap();
        assert!(
            roster.person_for(&actor_of(&key)).is_some(),
            "the derived roster does not claim our key: {roster:?}"
        );
        let url = serve(internal_router(sealed_peer(d.path()))).await;
        let out = exchange(&reqwest::Client::new(), &url, &rail, &journal).await;
        assert_eq!(out.pulled, 1, "{:?}", out.stop);
        let held = journal.read().unwrap().0;
        assert_eq!(
            held.len(),
            1,
            "the retired prefix stayed on disk ({} lines); a direct compaction says: {:?}",
            held.len(),
            journal
                .compact(&roster, &Ed25519Verifier)
                .map(|c| c.removed)
        );
        assert!(matches!(held[0].kind.act, RailAct::Seal));
    }

    /// The control for the test above. The identical exchange with the seal
    /// replaced by an ordinary act pulls the same one op and deletes NOTHING —
    /// so the prune there is the seal's doing and not something the sync path
    /// does to any journal it touches.
    #[tokio::test]
    async fn an_ordinary_op_arriving_prunes_nothing() {
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let (mine, theirs) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (_me, journal, rail) = node(mine.path(), &key, 3);
        let (peer_state, peer_journal, _r2) = node(theirs.path(), &key, 3);
        peer_journal
            .ingest(&signed(
                &key,
                3,
                RailAct::Record {
                    payload: body_of_size(FIXTURE_BODY_BYTES),
                },
            ))
            .unwrap();

        let url = serve(internal_router(peer_state)).await;
        let out = exchange(&reqwest::Client::new(), &url, &rail, &journal).await;

        assert_eq!(out.pulled, 1);
        assert_eq!(journal.read().unwrap().0.len(), 4, "nothing was retired");
    }

    /// The control for the test above: that journal really does need more
    /// than one exchange, so convergence there is evidence about the LOOP and
    /// not about a body that happened to fit.
    #[tokio::test]
    async fn ten_thousand_ops_do_not_fit_one_chunk() {
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let dir = tempfile::tempdir().unwrap();
        let (_state, journal, rail) = node(dir.path(), &key, 10_000);
        let (chunk, more) = journal
            .ops_missing_from_within(
                &commonwealth_rail::Digest::new(),
                RING_SYNC_OPS_BUDGET_BYTES,
            )
            .unwrap();
        assert!(more, "the budget must cut a 10,000-op journal short");
        assert!(
            chunk.len() < 10_000,
            "one chunk carried all {} ops — the budget stopped binding and the \
             convergence test above stopped testing convergence",
            chunk.len()
        );
        assert!(
            serde_json::to_vec(&chunk).unwrap().len()
                <= commonwealth_api::server::MAX_REQUEST_BODY_BYTES,
            "a chunk must fit the limit it was budgeted against"
        );
    }

    /// **2f-4: a real pull is not discarded by a later failure.** Call 1
    /// hands over ops and they are written to disk; call 2 then fails. The
    /// old signature returned `Err` and threw the pulled count away, so
    /// `ops_pulled` undercounted exactly in the failure case.
    ///
    /// The peer here answers call 1 with ops and refuses any request that
    /// carries ops of its own, which is the shape of a peer whose body limit
    /// is below ours.
    #[tokio::test]
    async fn a_second_call_that_fails_still_reports_what_the_first_call_pulled() {
        let peer_key = SigningKey::from_bytes(&[9u8; 32]);
        let gift = ops(&peer_key, 5);
        let gift_for_route = gift.clone();

        let router = axum::Router::new().route(
            "/internal/ring/sync",
            axum::routing::post(move |body: axum::body::Bytes| {
                let gift = gift_for_route.clone();
                async move {
                    let req: RingSyncRequest = serde_json::from_slice(&body).unwrap();
                    if !req.ops.is_empty() {
                        // Call 2 — refuse it, and refuse it the way a peer
                        // with a smaller limit would.
                        return axum::http::StatusCode::PAYLOAD_TOO_LARGE.into_response();
                    }
                    axum::Json(RingSyncResponse {
                        namespace: req.namespace,
                        digest: commonwealth_rail::Digest::new(),
                        ops: gift,
                        ingested: 0,
                    })
                    .into_response()
                }
            }),
        );
        let url = serve(router).await;

        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let (_state, journal, rail) = node(dir.path(), &key, 3);

        let out = exchange(&reqwest::Client::new(), &url, &rail, &journal).await;
        assert!(
            matches!(out.stop, Some(ExchangeStop::Refused { .. })),
            "a 413 is a refusal, not an unreachable peer: {:?}",
            out.stop
        );
        assert_eq!(
            out.pulled, 5,
            "the five ops call 1 pulled are on disk and must be counted"
        );
        assert_eq!(
            journal.read().unwrap().0.len(),
            8,
            "and they really are on disk: 3 held + 5 pulled"
        );
    }

    /// A peer that answers nothing but 413 is REFUSED, not unreachable —
    /// the distinction the round counts on, and the one whose absence made
    /// the ceiling silent.
    #[tokio::test]
    async fn a_peer_that_answers_413_is_refused_rather_than_unreachable() {
        let router = axum::Router::new().route(
            "/internal/ring/sync",
            axum::routing::post(|| async { axum::http::StatusCode::PAYLOAD_TOO_LARGE }),
        );
        let url = serve(router).await;
        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let (_state, journal, rail) = node(dir.path(), &key, 3);

        let out = exchange(&reqwest::Client::new(), &url, &rail, &journal).await;
        match out.stop {
            Some(ExchangeStop::Refused { sent_bytes }) => {
                assert!(
                    sent_bytes > 0,
                    "the refused size is what makes it actionable"
                )
            }
            other => panic!("expected Refused, got {other:?}"),
        }

        // The negative control: a peer that is not there at all is Failed,
        // so the variant above is evidence about the STATUS and not about
        // every failure being labelled a refusal.
        let out = exchange(
            &reqwest::Client::new(),
            "http://127.0.0.1:1/internal/ring/sync",
            &rail,
            &journal,
        )
        .await;
        assert!(
            matches!(out.stop, Some(ExchangeStop::Failed(_))),
            "an unreachable address is not a refusal: {:?}",
            out.stop
        );
    }

    /// **K9.** A peer that answers a well-formed exchange but never ingests
    /// anything cannot make this loop spin: the stall check sees an unmoved
    /// digest and hands the round back.
    #[tokio::test]
    async fn a_peer_whose_digest_never_moves_stops_the_loop_instead_of_spinning() {
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        let router = axum::Router::new().route(
            "/internal/ring/sync",
            axum::routing::post(move |body: axum::body::Bytes| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let req: RingSyncRequest = serde_json::from_slice(&body).unwrap();
                    // Always "I hold nothing, and I ingested nothing" — a
                    // black hole that stays reachable.
                    axum::Json(RingSyncResponse {
                        namespace: req.namespace,
                        digest: commonwealth_rail::Digest::new(),
                        ops: Vec::new(),
                        ingested: 0,
                    })
                }
            }),
        );
        let url = serve(router).await;
        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let (_state, journal, rail) = node(dir.path(), &key, 3);

        let out = exchange(&reqwest::Client::new(), &url, &rail, &journal).await;
        assert!(out.stop.is_none());
        assert_eq!(out.pulled, 0);
        assert_eq!(out.pushed, 0);
        assert!(
            hits.load(std::sync::atomic::Ordering::SeqCst) <= 4,
            "the stall check must stop after the second chunk, not run all \
             {MAX_CHUNKS_PER_EXCHANGE} — {} calls",
            hits.load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    // ── The mesh store as a projection of the rail (cw-lift 4) ──
    //
    // Two nodes, the REAL internal router, the REAL pump and the REAL fold.
    // Every helper below is deliberately built from the production pieces:
    // a fixture that appended its own ops or projected with its own roster
    // would pass whatever the two halves happened to agree on.

    /// The namespace these tests replicate. A `MeshStore` app_id verbatim, and
    /// one of `DAEMON_OWN_NAMESPACES` — a namespace not on that list has no
    /// derived roster and would be refused at the door for that reason alone,
    /// which is a different test.
    const KV: &str = commonwealth_inference::INFERENCE_APP_ID;

    /// One mesh both nodes see. Each member carries the pubkey of the key its
    /// node signs with, because that equality is the whole bridge between a
    /// signature and a `NodeId` — a fixture whose membership is empty makes
    /// every projected row `unattributed` for that reason and nothing else.
    fn kv_mesh(
        ka: &SigningKey,
        kb: &SigningKey,
        a: NodeId,
        b: NodeId,
    ) -> commonwealth_core::mesh::Mesh {
        use crate::ring_roster::tests::{member, mesh_of, pubkey_of};
        mesh_of(vec![
            member(a, "a", Some(pubkey_of(ka))),
            member(b, "b", Some(pubkey_of(kb))),
        ])
    }

    /// A node whose rail derives its roster from that membership — what the
    /// daemon installs, through the same call the daemon makes.
    fn kv_node(
        dir: &std::path::Path,
        key: &SigningKey,
        self_id: NodeId,
        mesh: commonwealth_core::mesh::Mesh,
    ) -> (AppState, Arc<RingRail>) {
        let state = AppState::new(self_id, mesh);
        let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
        crate::ring_roster::MeshRosterSource::install(&rail, &state).unwrap();
        state.install_ring_rail(rail.clone());
        (state, rail)
    }

    /// One store write as the act a node would have signed, with the write
    /// time chosen by the caller. The pump builds these from the outbox; these
    /// are for the history a test needs to already exist.
    fn kv_op(
        ns: &str,
        key: &SigningKey,
        seq: u64,
        k: &str,
        v: Option<&[u8]>,
        t: u64,
    ) -> Op<SignedOp> {
        signed_in(
            ns,
            key,
            seq,
            RailAct::Record {
                payload: commonwealth_state::rail_kv::to_payload(k, v, t).unwrap(),
            },
        )
    }

    /// The floor named by the snapshot mark on these ops, if one is there.
    fn mark_of(ops: &[Op<SignedOp>]) -> Option<u64> {
        ops.iter().find_map(|o| match &o.kind.act {
            RailAct::Record { payload } => commonwealth_state::rail_kv::read_snapshot_mark(payload),
            _ => None,
        })
    }

    /// Whether these ops carry a store write for `key` — a tombstone included.
    /// Used to assert that a delete really is OFF a disk, rather than trusting
    /// a line count to have taken the right line.
    fn carries_write_for(ops: &[Op<SignedOp>], key: &str) -> bool {
        ops.iter().any(|o| match &o.kind.act {
            RailAct::Record { payload } => {
                commonwealth_state::rail_kv::from_payload(payload).is_some_and(|kv| kv.key == key)
            }
            _ => false,
        })
    }

    /// The 2,000 superseded writes that trip `SEAL_AFTER_OWN_OPS`, signed by
    /// `key` from `first_seq`. Old `t`s, so nothing here can win a key.
    fn filler(k: &SigningKey, first_seq: u64, keys: &[&str]) -> Vec<Op<SignedOp>> {
        let old = commonwealth_core::clock::unix_now_secs() - 10_000;
        (0..crate::rail_kv_pump::SEAL_AFTER_OWN_OPS as u64)
            .map(|i| {
                kv_op(
                    KV,
                    k,
                    first_seq + i,
                    keys[i as usize % keys.len()],
                    Some(format!("old-{i}").as_bytes()),
                    old + i,
                )
            })
            .collect()
    }

    fn value_at(state: &AppState, app_id: &str, key: &str) -> Option<Vec<u8>> {
        state
            .inner
            .mesh_store
            .get(app_id, key)
            .unwrap()
            .map(|e| e.value.to_vec())
    }

    /// **(a) A local store write reaches a peer's store, over the ring.**
    ///
    /// The whole mechanism end to end: `set` queues, the pump signs, the
    /// exchange carries, the fold projects. The origin assertion is the one
    /// that cannot be faked — B never sees a `NodeId` on the wire, only a
    /// signature, and the roster is what turns one into the other.
    ///
    /// Watched RED by deleting the `journal.append` arm's `acked.push(row.id)`
    /// and returning before the append: `pumped.appended` is 0 and B's store
    /// answers `None`.
    #[tokio::test]
    async fn a_local_store_write_reaches_a_peers_store_through_the_ring() {
        let (ka, kb) = (
            SigningKey::from_bytes(&[1u8; 32]),
            SigningKey::from_bytes(&[2u8; 32]),
        );
        let (a_id, b_id) = (NodeId::from_u128(1), NodeId::from_u128(2));
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mesh = kv_mesh(&ka, &kb, a_id, b_id);
        let (a_state, a_rail) = kv_node(da.path(), &ka, a_id, mesh.clone());
        let (b_state, _b_rail) = kv_node(db.path(), &kb, b_id, mesh);

        assert!(a_state
            .inner
            .mesh_store
            .set(KV, "plan", bytes::Bytes::from_static(b"v1"), a_id)
            .unwrap());
        assert_eq!(
            a_state.inner.mesh_store.outbox_len().unwrap(),
            1,
            "a local write queues for the rail"
        );

        let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
        assert_eq!(pumped.appended, 1, "{pumped:?}");
        assert_eq!(pumped.deferred + pumped.refused, 0, "{pumped:?}");
        assert_eq!(
            a_state.inner.mesh_store.outbox_len().unwrap(),
            0,
            "an appended row is acked"
        );

        let url = serve(internal_router(b_state.clone())).await;
        let journal = a_rail.journal(KV).unwrap();
        let out = exchange(&reqwest::Client::new(), &url, &a_rail, &journal).await;
        assert!(out.stop.is_none(), "the exchange failed: {:?}", out.stop);
        assert_eq!(out.pushed, 1, "the write landed on the peer's journal");

        assert_eq!(
            crate::rail_kv_pump::project_all_on_disk(&b_state).await,
            1,
            "the peer folds the namespace it just received"
        );
        let got = b_state
            .inner
            .mesh_store
            .get(KV, "plan")
            .unwrap()
            .expect("the peer's store holds the write");
        assert_eq!(got.value.as_ref(), b"v1");
        assert_eq!(
            got.origin, a_id,
            "the origin comes from the roster placing a signature, never from \
             anything the sender supplied"
        );
    }

    /// **(b) A delete travels, and an older write does not undo it.**
    ///
    /// Two claims in one journal, because they are the same claim: the fold
    /// orders on the payload's `t`, so a tombstone at `t` beats every write
    /// below it no matter when the line arrived. The lower-`t` write here is
    /// signed by the OTHER node, which is the case an arrival-order rule
    /// cannot get right.
    ///
    /// Watched RED by folding on `op.ts_unix` instead of the payload's `t` in
    /// `rail_kv::project`: B's store gets `stale` back and the final assertion
    /// fails.
    #[tokio::test]
    async fn a_delete_travels_and_an_older_write_does_not_resurrect_the_key() {
        let (ka, kb) = (
            SigningKey::from_bytes(&[3u8; 32]),
            SigningKey::from_bytes(&[4u8; 32]),
        );
        let (a_id, b_id) = (NodeId::from_u128(11), NodeId::from_u128(12));
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mesh = kv_mesh(&ka, &kb, a_id, b_id);
        let (a_state, a_rail) = kv_node(da.path(), &ka, a_id, mesh.clone());
        let (b_state, b_rail) = kv_node(db.path(), &kb, b_id, mesh);
        let now = commonwealth_core::clock::unix_now_secs();

        // A already holds the key, from a write older than the wall clock —
        // so the delete below is unambiguously later. `set` stamps `now`, and
        // a set/delete pair inside one second is a tie the fold breaks by
        // actor and id rather than by intent.
        let a_journal = a_rail.journal(KV).unwrap();
        assert_eq!(
            a_journal
                .ingest_all(&[kv_op(KV, &ka, 0, "k", Some(b"live"), now - 100)])
                .unwrap(),
            1
        );
        crate::rail_kv_pump::project_all_on_disk(&a_state).await;
        assert_eq!(value_at(&a_state, KV, "k").as_deref(), Some(&b"live"[..]));

        let b_url = serve(internal_router(b_state.clone())).await;
        let client = reqwest::Client::new();
        let out = exchange(&client, &b_url, &a_rail, &a_journal).await;
        assert!(out.stop.is_none(), "{:?}", out.stop);
        crate::rail_kv_pump::project_all_on_disk(&b_state).await;
        assert_eq!(
            value_at(&b_state, KV, "k").as_deref(),
            Some(&b"live"[..]),
            "control: the key reached the peer before it was deleted"
        );

        // The delete, through the store, through the pump, over the ring.
        assert!(a_state.inner.mesh_store.delete(KV, "k").unwrap());
        let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
        assert_eq!(pumped.appended, 1, "the tombstone is an act like any other");
        let out = exchange(&client, &b_url, &a_rail, &a_journal).await;
        assert!(out.stop.is_none(), "{:?}", out.stop);
        crate::rail_kv_pump::project_all_on_disk(&b_state).await;
        assert_eq!(
            value_at(&b_state, KV, "k"),
            None,
            "the peer lost the key the tombstone names"
        );

        // A write B signed, older than the tombstone, arriving after it.
        let b_journal = b_rail.journal(KV).unwrap();
        assert_eq!(
            b_journal
                .ingest_all(&[kv_op(KV, &kb, 0, "k", Some(b"stale"), now - 50)])
                .unwrap(),
            1
        );
        crate::rail_kv_pump::project_all_on_disk(&b_state).await;
        assert_eq!(
            value_at(&b_state, KV, "k"),
            None,
            "a lower-t write does not resurrect a deleted key"
        );
        // …and it does not resurrect it on the node that deleted it either,
        // once the op gets there.
        let out = exchange(&client, &b_url, &a_rail, &a_journal).await;
        assert!(out.stop.is_none(), "{:?}", out.stop);
        assert_eq!(out.pulled, 1, "B's older write came over");
        crate::rail_kv_pump::project_all_on_disk(&a_state).await;
        assert_eq!(value_at(&a_state, KV, "k"), None);
    }

    /// **(c) A seal bounds the ring, and the snapshot keeps every live key.**
    ///
    /// The filler here is SUPERSEDED history of the same four keys — 2,000
    /// older writes the live set has already overwritten, which is exactly
    /// what a seal is for. Afterwards the peer's disk holds the seal and the
    /// snapshot and nothing else, and its store still answers for every key.
    ///
    /// The assertion is on what is on DISK and on what the STORE answers, not
    /// on a return value: a seal that retired nothing, or a snapshot that
    /// dropped the live set, would leave `PumpOutcome` looking identical.
    ///
    /// Watched RED by deleting the `snapshot()` call from `seal_if_due`: the
    /// disk assertion passes (one line, the seal) and every value assertion
    /// below it fails — the seal became a delete.
    #[tokio::test]
    async fn a_seal_bounds_the_ring_and_the_snapshot_keeps_every_live_key() {
        const KEYS: [&str; 4] = ["k0", "k1", "k2", "k3"];
        let (ka, kb) = (
            SigningKey::from_bytes(&[5u8; 32]),
            SigningKey::from_bytes(&[6u8; 32]),
        );
        let (a_id, b_id) = (NodeId::from_u128(21), NodeId::from_u128(22));
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mesh = kv_mesh(&ka, &kb, a_id, b_id);
        let (a_state, a_rail) = kv_node(da.path(), &ka, a_id, mesh.clone());
        let (b_state, b_rail) = kv_node(db.path(), &kb, b_id, mesh);

        for k in KEYS {
            assert!(a_state
                .inner
                .mesh_store
                .set(KV, k, bytes::Bytes::from(format!("live-{k}")), a_id)
                .unwrap());
        }
        // ── The control: four ops is not two thousand, and nothing seals.
        let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
        assert_eq!(pumped.appended, 4);
        assert_eq!(
            (pumped.sealed, pumped.snapshot_rows),
            (0, 0),
            "below the threshold the pump seals nothing"
        );
        let a_journal = a_rail.journal(KV).unwrap();
        assert_eq!(a_journal.read().unwrap().0.len(), 4);

        // ── 2,000 older writes of the same keys: history, already superseded.
        let filler = filler(&ka, 4, &KEYS);
        let total = 4 + filler.len();
        assert_eq!(a_journal.ingest_all(&filler).unwrap(), filler.len());

        // The peer takes the whole history first, so the prune below has
        // something to remove — otherwise "B's disk is short" would be true
        // because B was never told anything.
        let a_url = serve(internal_router(a_state.clone())).await;
        let client = reqwest::Client::new();
        let b_journal = b_rail.journal(KV).unwrap();
        let out = exchange(&client, &a_url, &b_rail, &b_journal).await;
        assert!(out.stop.is_none(), "{:?}", out.stop);
        assert_eq!(
            b_journal.read().unwrap().0.len(),
            total,
            "control: the peer holds the unsealed history"
        );

        // ── One more local write, and the seal fires on the same tick.
        assert!(a_state
            .inner
            .mesh_store
            .set(KV, "k0", bytes::Bytes::from_static(b"newest"), a_id)
            .unwrap());
        let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
        assert_eq!(pumped.appended, 1);
        assert_eq!(pumped.sealed, 1, "{pumped:?}");
        assert_eq!(
            pumped.snapshot_rows,
            KEYS.len(),
            "every live row is re-appended above the new floor"
        );
        let held = a_journal.read().unwrap().0;
        assert_eq!(
            held.len(),
            1 + KEYS.len() + 1,
            "the writer's own disk is the seal, the snapshot, and the mark that \
             closes it: {held:?}"
        );
        assert!(matches!(held[0].kind.act, RailAct::Seal));
        assert_eq!(
            mark_of(&held),
            Some(held[0].kind.seq),
            "the snapshot ends with the mark naming the seal it completes — \
             without it no peer may retire anything of ours"
        );

        // ── The peer meets the seal and retires the same prefix.
        let out = exchange(&client, &a_url, &b_rail, &b_journal).await;
        assert!(out.stop.is_none(), "{:?}", out.stop);
        assert_eq!(
            b_journal.read().unwrap().0.len(),
            1 + KEYS.len() + 1,
            "the peer's disk holds only the seal, the snapshot and its mark"
        );
        assert_eq!(
            a_journal.digest().unwrap(),
            b_journal.digest().unwrap(),
            "two nodes, one claim"
        );

        // ── …and every live key is still readable on the peer.
        crate::rail_kv_pump::project_all_on_disk(&b_state).await;
        assert_eq!(
            value_at(&b_state, KV, "k0").as_deref(),
            Some(&b"newest"[..]),
            "the newest write survives its own snapshot"
        );
        for k in &KEYS[1..] {
            assert_eq!(
                value_at(&b_state, KV, k).as_deref(),
                Some(format!("live-{k}").as_bytes()),
                "{k} did not survive the seal"
            );
        }
    }

    /// **(c2) A delete keeps travelling past the seal that retired it.**
    ///
    /// The cost ea4da7b68 recorded and priced rather than paid: a snapshot
    /// carries LIVE rows, a tombstone is not one, so a peer that was away for
    /// the delete used to keep the value forever — the KV shape of K7. Here B
    /// holds all three keys, A deletes one while B is not listening, and the
    /// seal that fires on the same tick takes the tombstone off A's disk before
    /// any exchange could carry it. The middle assertion is the one that makes
    /// this a real reproduction rather than a slow round: the delete is
    /// provably UNREACHABLE, not merely late.
    ///
    /// What B has afterwards is the seal, the snapshot and its mark — and that
    /// is a claim about A's WHOLE live set, which is what retires the row.
    ///
    /// Watched RED by dropping the reconciliation loop from
    /// `MeshStore::apply_projection`: every assertion above the last passes and
    /// B keeps `gone` at its stale value.
    #[tokio::test]
    async fn a_seal_carries_a_delete_the_peer_never_received() {
        const KEYS: [&str; 3] = ["k0", "k1", "gone"];
        let (ka, kb) = (
            SigningKey::from_bytes(&[15u8; 32]),
            SigningKey::from_bytes(&[16u8; 32]),
        );
        let (a_id, b_id) = (NodeId::from_u128(61), NodeId::from_u128(62));
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mesh = kv_mesh(&ka, &kb, a_id, b_id);
        let (a_state, a_rail) = kv_node(da.path(), &ka, a_id, mesh.clone());
        let (b_state, b_rail) = kv_node(db.path(), &kb, b_id, mesh);

        for k in KEYS {
            assert!(a_state
                .inner
                .mesh_store
                .set(KV, k, bytes::Bytes::from(format!("live-{k}")), a_id)
                .unwrap());
        }
        let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
        assert_eq!((pumped.appended, pumped.sealed), (3, 0), "{pumped:?}");

        // B takes the whole live set, including the key that is about to go.
        let a_url = serve(internal_router(a_state.clone())).await;
        let client = reqwest::Client::new();
        let a_journal = a_rail.journal(KV).unwrap();
        let b_journal = b_rail.journal(KV).unwrap();
        let out = exchange(&client, &a_url, &b_rail, &b_journal).await;
        assert!(out.stop.is_none(), "{:?}", out.stop);
        crate::rail_kv_pump::project_all_on_disk(&b_state).await;
        assert_eq!(
            value_at(&b_state, KV, "gone").as_deref(),
            Some(&b"live-gone"[..]),
            "control: the peer held the key before it was deleted"
        );

        // ── A deletes it, and seals on the same tick. B is not listening.
        assert_eq!(
            a_journal.ingest_all(&filler(&ka, 3, &KEYS)).unwrap(),
            crate::rail_kv_pump::SEAL_AFTER_OWN_OPS
        );
        assert!(a_state.inner.mesh_store.delete(KV, "gone").unwrap());
        let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
        assert_eq!(pumped.appended, 1, "the tombstone was appended: {pumped:?}");
        assert_eq!(pumped.sealed, 1, "{pumped:?}");
        assert_eq!(pumped.snapshot_rows, 2, "two live rows, not three");

        let held = a_journal.read().unwrap().0;
        assert!(
            !carries_write_for(&held, "gone"),
            "THE REPRODUCTION: the tombstone is below the floor and off the \
             disk, so no exchange can ever carry it to B: {held:?}"
        );
        assert_eq!(mark_of(&held), Some(held[0].kind.seq));

        // ── The round that meets the seal.
        let out = exchange(&client, &a_url, &b_rail, &b_journal).await;
        assert!(out.stop.is_none(), "{:?}", out.stop);
        assert!(
            !carries_write_for(&b_journal.read().unwrap().0, "gone"),
            "B never receives a delete for it — the seal is what says so"
        );
        crate::rail_kv_pump::project_all_on_disk(&b_state).await;

        assert_eq!(
            value_at(&b_state, KV, "gone"),
            None,
            "the key A stopped asserting is gone from the peer that never saw \
             the tombstone"
        );
        for k in ["k0", "k1"] {
            assert_eq!(
                value_at(&b_state, KV, k).as_deref(),
                Some(format!("live-{k}").as_bytes()),
                "{k} was in the snapshot and must survive the same pass"
            );
        }
    }

    /// **(c3) A snapshot that arrives in two chunks retires nothing until the
    /// mark lands.** The control for (c2), and the reason the mark exists.
    ///
    /// `exchange` pulls in chunks, so a seal can land in one and its snapshot
    /// in the next — and a round can end in between (the chunk bound, a peer
    /// that stops answering on call 2). At that instant the seal is on disk and
    /// the live set folds EMPTY, so an unguarded reconciliation would retire
    /// every row this node holds on that actor's behalf. `admit` reports the
    /// journal COMPLETE there, because the hole audit runs from the floor and
    /// the seal is the floor — which is why completeness cannot be the gate.
    ///
    /// The two chunks are fed by hand rather than over the wire: what is under
    /// test is the fold's verdict on a partly-arrived journal, and driving the
    /// split through HTTP would make the test's own chunking the thing being
    /// asserted.
    ///
    /// Watched RED by dropping `marked` from the completeness test in
    /// `rail_kv::project`: the first half retires all three of A's keys.
    #[tokio::test]
    async fn a_snapshot_that_arrives_in_two_chunks_retires_nothing_until_the_mark() {
        const KEYS: [&str; 3] = ["k0", "k1", "gone"];
        let (ka, kb) = (
            SigningKey::from_bytes(&[17u8; 32]),
            SigningKey::from_bytes(&[18u8; 32]),
        );
        let (a_id, b_id) = (NodeId::from_u128(71), NodeId::from_u128(72));
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mesh = kv_mesh(&ka, &kb, a_id, b_id);
        let (a_state, a_rail) = kv_node(da.path(), &ka, a_id, mesh.clone());
        let (b_state, b_rail) = kv_node(db.path(), &kb, b_id, mesh);

        for k in KEYS {
            assert!(a_state
                .inner
                .mesh_store
                .set(KV, k, bytes::Bytes::from(format!("live-{k}")), a_id)
                .unwrap());
        }
        assert_eq!(crate::rail_kv_pump::pump_once(&a_state).await.appended, 3);
        let a_journal = a_rail.journal(KV).unwrap();
        let b_journal = b_rail.journal(KV).unwrap();

        // B holds A's pre-seal history.
        let pre = a_journal.read().unwrap().0;
        assert_eq!(b_journal.ingest_all(&pre).unwrap(), 3);
        crate::rail_kv_pump::project_all_on_disk(&b_state).await;
        for k in KEYS {
            assert!(value_at(&b_state, KV, k).is_some(), "{k}");
        }

        // A deletes one key and seals.
        a_journal.ingest_all(&filler(&ka, 3, &KEYS)).unwrap();
        assert!(a_state.inner.mesh_store.delete(KV, "gone").unwrap());
        let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
        assert_eq!((pumped.sealed, pumped.snapshot_rows), (1, 2), "{pumped:?}");
        let after = a_journal.read().unwrap().0;

        // ── Chunk one: the seal, and nothing above it.
        let (seal, rest): (Vec<_>, Vec<_>) = after
            .into_iter()
            .partition(|o| matches!(o.kind.act, RailAct::Seal));
        assert_eq!(seal.len(), 1);
        assert_eq!(b_journal.ingest_all(&seal).unwrap(), 1);
        let admitted = b_journal
            .admit(
                &crate::ring_roster::MeshRoster::from_app_state(&b_state)
                    .await
                    .roster()
                    .clone(),
                &Ed25519Verifier,
            )
            .unwrap();
        assert!(
            admitted.is_complete(),
            "the journal reports COMPLETE at exactly the instant its live set \
             is a lie: {:?}",
            admitted.gaps
        );
        crate::rail_kv_pump::project_all_on_disk(&b_state).await;
        for k in KEYS {
            assert!(
                value_at(&b_state, KV, k).is_some(),
                "{k} was retired on the strength of half a snapshot"
            );
        }

        // ── Chunk two: the rows and the mark.
        assert_eq!(b_journal.ingest_all(&rest).unwrap(), rest.len());
        crate::rail_kv_pump::project_all_on_disk(&b_state).await;
        assert_eq!(
            value_at(&b_state, KV, "gone"),
            None,
            "now the claim is whole"
        );
        for k in ["k0", "k1"] {
            assert!(value_at(&b_state, KV, k).is_some(), "{k}");
        }
    }

    /// **(d) A namespace that never leaves a machine never leaves it.**
    ///
    /// The sender-side guard is inside the store's own transaction, so this
    /// asserts on the two places a private write could surface if it slipped:
    /// this node's outbox and its journals, and the peer's store. The control
    /// in the same test is a public write on the same tick — without it, an
    /// entirely broken pump would pass.
    ///
    /// Watched RED by deleting the `is_gossip_excluded` guard from
    /// `backend::enqueue_on`: the row queues, the pump appends it, a
    /// `notes-private` journal appears on disk, and the peer refuses it at
    /// `apply_projection` — the last layer, and the first three assertions all
    /// go red on the way there.
    #[tokio::test]
    async fn an_excluded_namespace_never_enters_the_outbox_nor_a_peers_store() {
        const PRIVATE: &str = "notes-private";
        assert!(
            commonwealth_state::GOSSIP_EXCLUDED_APP_IDS.contains(&PRIVATE),
            "this test is about an excluded namespace"
        );
        let (ka, kb) = (
            SigningKey::from_bytes(&[7u8; 32]),
            SigningKey::from_bytes(&[8u8; 32]),
        );
        let (a_id, b_id) = (NodeId::from_u128(31), NodeId::from_u128(32));
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mesh = kv_mesh(&ka, &kb, a_id, b_id);
        let (a_state, a_rail) = kv_node(da.path(), &ka, a_id, mesh.clone());
        let (b_state, _b_rail) = kv_node(db.path(), &kb, b_id, mesh);

        assert!(a_state
            .inner
            .mesh_store
            .set(PRIVATE, "secret", bytes::Bytes::from_static(b"mine"), a_id)
            .unwrap());
        assert!(a_state
            .inner
            .mesh_store
            .set(KV, "public", bytes::Bytes::from_static(b"shared"), a_id)
            .unwrap());
        assert_eq!(
            a_state.inner.mesh_store.outbox_len().unwrap(),
            1,
            "only the public write queued"
        );

        let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
        assert_eq!(pumped.appended, 1, "{pumped:?}");
        let namespaces = a_rail.namespaces().unwrap();
        assert!(
            !namespaces.iter().any(|n| n == PRIVATE),
            "a private namespace has no journal at all: {namespaces:?}"
        );

        let url = serve(internal_router(b_state.clone())).await;
        let journal = a_rail.journal(KV).unwrap();
        let out = exchange(&reqwest::Client::new(), &url, &a_rail, &journal).await;
        assert!(out.stop.is_none(), "{:?}", out.stop);
        crate::rail_kv_pump::project_all_on_disk(&b_state).await;

        assert_eq!(
            value_at(&b_state, PRIVATE, "secret"),
            None,
            "the private write is nowhere on the peer"
        );
        assert_eq!(
            value_at(&b_state, KV, "public").as_deref(),
            Some(&b"shared"[..]),
            "control: the public write on the same tick did travel"
        );
    }

    /// **(d2) A peer's private namespace is TAKEN by the rail and REFUSED by
    /// the projection.**
    ///
    /// (d) is the sender-side half: a private write on this node never enters
    /// the outbox, so it never travels. This is the receiver-side half, and it
    /// is the guard the deleted `POST /internal/app/state` route used to carry
    /// in its handler — mTLS proves a caller is in the mesh, not that it runs
    /// honest code, so the invariant has to survive a peer that puts a private
    /// namespace on the ring deliberately. Rung 2e deleted route and handler
    /// together; this is where the invariant lives now.
    ///
    /// The rail DOES accept the ops, and the first assertion pins that rather
    /// than hiding it: a namespace is a directory and `/internal/ring/sync`
    /// ingests without judging an author, by design (an op's signature is
    /// checked at the fold, not at the listener). So the line is on B's disk.
    /// What refuses is `MeshStore::apply_projection`, and the store is the only
    /// thing any reader reads. The control is a public op carried over the same
    /// route in the same test — without it, a B that ingested nothing at all
    /// would pass.
    ///
    /// Watched RED by pointing `PRIVATE` at a namespace that is NOT on
    /// `GOSSIP_EXCLUDED_APP_IDS` (`notes`): everything else about the test is
    /// unchanged, the ops travel the same route, and the store assertion goes
    /// red because the projection now takes them. That is the sabotage
    /// available from this crate — the guard itself is
    /// `apply_projection`'s and `commonwealth-state` watches it red by
    /// deleting the arm (`apply_projection_refuses_an_excluded_namespace`).
    #[tokio::test]
    async fn a_peers_private_namespace_is_taken_by_the_rail_and_refused_by_the_projection() {
        const PRIVATE: &str = "notes-private";
        assert!(
            commonwealth_state::GOSSIP_EXCLUDED_APP_IDS.contains(&PRIVATE),
            "this test is about an excluded namespace"
        );
        let (ka, kb) = (
            SigningKey::from_bytes(&[13u8; 32]),
            SigningKey::from_bytes(&[14u8; 32]),
        );
        let (a_id, b_id) = (NodeId::from_u128(51), NodeId::from_u128(52));
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mesh = kv_mesh(&ka, &kb, a_id, b_id);
        let (a_state, a_rail) = kv_node(da.path(), &ka, a_id, mesh.clone());
        let (b_state, b_rail) = kv_node(db.path(), &kb, b_id, mesh);
        let now = commonwealth_core::clock::unix_now_secs();

        // A is hostile: it writes the private namespace onto its own journal
        // directly, which is what a peer running patched code would do. Its own
        // store is never asked, so the outbox guard (d) pins is not in the way.
        let a_private = a_rail.journal(PRIVATE).unwrap();
        assert_eq!(
            a_private
                .ingest_all(&[kv_op(PRIVATE, &ka, 0, "secret", Some(b"mine"), now)])
                .unwrap(),
            1
        );
        let a_public = a_rail.journal(KV).unwrap();
        assert_eq!(
            a_public
                .ingest_all(&[kv_op(KV, &ka, 0, "public", Some(b"shared"), now)])
                .unwrap(),
            1
        );

        // Both namespaces go to B through the real route.
        let url = serve(internal_router(b_state.clone())).await;
        let client = reqwest::Client::new();
        for journal in [&a_private, &a_public] {
            let out = exchange(&client, &url, &a_rail, journal).await;
            assert!(out.stop.is_none(), "{:?}", out.stop);
        }

        // The rail took both — B holds the private line on disk. If this fails
        // the test below proves nothing, because nothing arrived.
        assert_eq!(
            b_rail.journal(PRIVATE).unwrap().read().unwrap().0.len(),
            1,
            "the ingest is author-blind and namespace-blind, and that is the design"
        );

        crate::rail_kv_pump::project_all_on_disk(&b_state).await;

        assert_eq!(
            value_at(&b_state, PRIVATE, "secret"),
            None,
            "a private namespace a peer pushed reached no reader"
        );
        assert_eq!(
            value_at(&b_state, KV, "public").as_deref(),
            Some(&b"shared"[..]),
            "control: an ordinary namespace over the same route in the same test did land"
        );
    }

    /// **(e) The ROUND is what projects, and it does so whether or not it
    /// pulled anything.**
    ///
    /// The wiring the four tests above reach past: they call
    /// `project_all_on_disk` by hand, which is the boot path. In production a
    /// namespace is folded once per round, after every peer — and the second
    /// half of this test is why it cannot instead hang off `exchange`'s pulled
    /// count. **Half the ops a node receives never pass through its own
    /// `exchange`**: a peer PUSHES on call 2 of its exchange, and those land
    /// through `/internal/ring/sync`, a route in another crate. A projection
    /// conditioned on our own pull would be blind to exactly the direction the
    /// pump's nudge creates.
    ///
    /// Watched RED by moving the projection inside `if ex.pulled > 0`: the
    /// first half still passes and the converged round below reports
    /// `namespaces_projected: 0`, which is the shape of the bug.
    #[tokio::test]
    async fn a_round_projects_the_namespace_even_when_it_pulled_nothing() {
        let (ka, kb) = (
            SigningKey::from_bytes(&[9u8; 32]),
            SigningKey::from_bytes(&[10u8; 32]),
        );
        let (a_id, b_id) = (NodeId::from_u128(41), NodeId::from_u128(42));
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let now = commonwealth_core::clock::unix_now_secs();

        let a_dir = da.path().to_path_buf();
        let mut mesh_for_b = kv_mesh(&ka, &kb, a_id, b_id);
        let (a_state, a_rail) = kv_node(&a_dir, &ka, a_id, kv_mesh(&ka, &kb, a_id, b_id));
        a_rail
            .journal(KV)
            .unwrap()
            .ingest_all(&[kv_op(KV, &ka, 0, "from-a", Some(b"a"), now)])
            .unwrap();
        let a_addr = serve_at(internal_router(a_state.clone())).await;

        // B's view of the mesh has A at a real address; everything else about
        // the two states is identical.
        if let Some(record) = mesh_for_b.members.get_mut(&a_id) {
            record.addresses = vec![a_addr];
        }
        let (b_state, b_rail) = kv_node(db.path(), &kb, b_id, mesh_for_b);
        b_rail
            .journal(KV)
            .unwrap()
            .ingest_all(&[kv_op(KV, &kb, 0, "from-b", Some(b"b"), now)])
            .unwrap();

        let round = run_one_round(&b_state).await;
        assert_eq!(round.peers_reached, 1, "{round:?}");
        assert_eq!((round.ops_pulled, round.ops_pushed), (1, 1), "{round:?}");
        assert_eq!(round.namespaces_projected, 1, "{round:?}");
        assert_eq!(
            value_at(&b_state, KV, "from-a").as_deref(),
            Some(&b"a"[..]),
            "the round folded what it pulled"
        );

        // Converged: nothing moves, and the namespace is projected anyway.
        let round = run_one_round(&b_state).await;
        assert_eq!((round.ops_pulled, round.ops_pushed), (0, 0), "{round:?}");
        assert_eq!(
            round.namespaces_projected, 1,
            "a round that pulled nothing still folds — ops a peer PUSHED \
             arrive by a route this loop never runs: {round:?}"
        );
    }

    /// **(f) Retention on a rail-backed namespace stays retained.**
    ///
    /// RED-FIRST, and the direction is the whole point: the store is a
    /// PROJECTION now, so a row a local sweep deletes has no incumbent and the
    /// next round's `merge_entry` puts it straight back from the journal. The
    /// sweep runs on a 60s-ish cadence and the fold runs on a 60s round, so a
    /// 30-day retention window on a meshed node is undone within a minute,
    /// every minute, forever.
    #[tokio::test]
    async fn a_retention_sweep_is_not_undone_by_the_next_projection() {
        const LEDGER: &str = commonwealth_state::CONTRIBUTIONS_APP_ID;
        let (ka, kb) = (
            SigningKey::from_bytes(&[15u8; 32]),
            SigningKey::from_bytes(&[16u8; 32]),
        );
        let (a_id, b_id) = (NodeId::from_u128(61), NodeId::from_u128(62));
        let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mesh = kv_mesh(&ka, &kb, a_id, b_id);
        let (_a_state, a_rail) = kv_node(da.path(), &ka, a_id, mesh.clone());
        let (b_state, _b_rail) = kv_node(db.path(), &kb, b_id, mesh);

        let now = commonwealth_core::clock::unix_now_secs();
        let window = u64::from(commonwealth_core::contributions::DEFAULT_WINDOW_DAYS) * 86_400;
        let floor = now - window;
        // Two ledger events A signed: one a day past the aggregation window,
        // one inside it. Days apart from the boundary, so no clock tick
        // between the plant and the sweep can move which side either is on.
        let a_journal = a_rail.journal(LEDGER).unwrap();
        assert_eq!(
            a_journal
                .ingest_all(&[
                    kv_op(LEDGER, &ka, 0, "old-event", Some(b"1"), floor - 86_400),
                    kv_op(LEDGER, &ka, 1, "fresh-event", Some(b"1"), now - 60),
                ])
                .unwrap(),
            2
        );

        let url = serve(internal_router(b_state.clone())).await;
        let out = exchange(&reqwest::Client::new(), &url, &a_rail, &a_journal).await;
        assert!(out.stop.is_none(), "the exchange failed: {:?}", out.stop);
        crate::rail_kv_pump::project_all_on_disk(&b_state).await;
        assert!(
            value_at(&b_state, LEDGER, "fresh-event").is_some(),
            "control: the ledger replicated at all"
        );

        // B's retention sweep, at the ONE cutoff the namespace's readers use.
        b_state
            .inner
            .mesh_store
            .gc_app_before(LEDGER, floor)
            .unwrap();
        assert_eq!(
            value_at(&b_state, LEDGER, "old-event"),
            None,
            "control: the sweep did delete the row"
        );

        // …and now one more round of the very thing that fills the store.
        crate::rail_kv_pump::project_all_on_disk(&b_state).await;
        assert_eq!(
            value_at(&b_state, LEDGER, "old-event"),
            None,
            "the projection put back a row retention had just taken"
        );
        assert!(
            value_at(&b_state, LEDGER, "fresh-event").is_some(),
            "and it took only the expired one"
        );
    }

    /// **(f2) The author's own sweep is not undone by the author's own
    /// journal, and it puts nothing on the rail.**
    ///
    /// The other half of (f). A's store leads its journal by an outbox drain,
    /// so `apply_projection` deliberately does not reconcile A's own rows
    /// against A's sealed live set — which means the ONLY thing that can keep
    /// A's expired rows out of A's store is the fold itself refusing them.
    ///
    /// The outbox assertion is the second claim: expiry emits no traffic. Every
    /// node derives the same floor from the same `t`, so retention needs no
    /// message — and a tombstone per retired row would grow the journal
    /// retention exists to bound.
    #[tokio::test]
    async fn an_authors_own_retention_sweep_is_not_undone_and_puts_nothing_on_the_rail() {
        const LEDGER: &str = commonwealth_state::CONTRIBUTIONS_APP_ID;
        let ka = SigningKey::from_bytes(&[17u8; 32]);
        let kb = SigningKey::from_bytes(&[18u8; 32]);
        let (a_id, b_id) = (NodeId::from_u128(71), NodeId::from_u128(72));
        let da = tempfile::tempdir().unwrap();
        let (a_state, a_rail) = kv_node(da.path(), &ka, a_id, kv_mesh(&ka, &kb, a_id, b_id));

        let now = commonwealth_core::clock::unix_now_secs();
        let window = u64::from(commonwealth_core::contributions::DEFAULT_WINDOW_DAYS) * 86_400;
        let floor = now - window;
        let a_journal = a_rail.journal(LEDGER).unwrap();
        assert_eq!(
            a_journal
                .ingest_all(&[
                    kv_op(LEDGER, &ka, 0, "old-event", Some(b"1"), floor - 86_400),
                    kv_op(LEDGER, &ka, 1, "fresh-event", Some(b"1"), now - 60),
                ])
                .unwrap(),
            2
        );
        crate::rail_kv_pump::project_all_on_disk(&a_state).await;
        assert!(
            value_at(&a_state, LEDGER, "fresh-event").is_some(),
            "control: A folded its own journal"
        );

        a_state
            .inner
            .mesh_store
            .gc_app_before(LEDGER, floor)
            .unwrap();
        assert_eq!(
            a_state.inner.mesh_store.outbox_len().unwrap(),
            0,
            "an expiry is not a delete: it publishes nothing, because every \
             node derives the same floor from the same `t`"
        );

        crate::rail_kv_pump::project_all_on_disk(&a_state).await;
        assert_eq!(
            value_at(&a_state, LEDGER, "old-event"),
            None,
            "A's own journal put back a row A's own retention had just taken"
        );
        assert!(
            value_at(&a_state, LEDGER, "fresh-event").is_some(),
            "and it took only the expired one"
        );
    }
}
