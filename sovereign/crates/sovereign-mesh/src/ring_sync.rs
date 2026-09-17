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
//! `RING_SYNC_OPS_BUDGET_BYTES`,
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

use commonwealth_core::mesh::NodeStatus;
use commonwealth_transport::{peer_contact, TrafficClass};
use sovereign_api::routes_internal::{
    RingSyncRequest, RingSyncResponse, RING_SYNC_OPS_BUDGET_BYTES,
};
use sovereign_api::state::AppState;
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

    let self_id = app_state.inner.fabric.identity.current();
    let peers: Vec<commonwealth_transport::PeerContact> = {
        let mesh = app_state.inner.fabric.mesh.read().await;
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
mod projection_tests;
#[cfg(test)]
mod snapshot_tests;
#[cfg(test)]
mod tests;
