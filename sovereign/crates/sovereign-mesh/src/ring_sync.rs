// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring journal's own replication loop — slower than gossip, and by
//! digest rather than by snapshot.
//!
//! # Why this is not a namespace on the gossip push
//!
//! `gossip.rs` Step 4 ships a **full mesh-store snapshot to every online peer
//! every ten seconds** — 8,640 rounds a day. A household writes on the order
//! of 3,500 journal ops a year, call it 1.5 MB, so riding that push would cost
//! roughly **246 GB/day of egress per node** and would tax every other
//! namespace on the same body forever. Bandwidth is the binding constraint
//! for this feature and it binds on day one.
//!
//! So: a sixty-second cadence (ample for money), and an exchange whose
//! request is a ~600-byte digest rather than the journal.
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

use std::time::{Duration, Instant};

use commonwealth_api::routes_internal::{
    RingSyncRequest, RingSyncResponse, RING_SYNC_OPS_BUDGET_BYTES,
};
use commonwealth_api::state::AppState;
use commonwealth_core::mesh::NodeStatus;
use commonwealth_transport::{peer_contact, TrafficClass};
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
}

/// Spawn the periodic ring-sync task. Call once per daemon start.
///
/// Runs one round **immediately** before entering the interval, because the
/// first thing a node that has been offline owes its ring is everything it
/// holds — waiting a full minute to boot-republish would leave a freshly
/// restarted peer confidently reporting a total over a subset for that whole
/// minute.
pub fn spawn_ring_sync_loop(app_state: AppState, interval: Duration) -> RingSyncHandle {
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
                    round_ms = started.elapsed().as_millis() as u64,
                    "ring sync: round"
                );
            }
            tokio::time::sleep(interval).await;
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
                let ex = exchange(http, &url, namespace, &journal).await;
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
    namespace: &str,
    journal: &commonwealth_rail::RingJournal,
) -> ExchangeOutcome {
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
        let ts = 1_700_000_000i64 + seq as i64;
        let sig = sign_ring_op(key, NS, ts, seq, &body_json(&act));
        Op::new(SignedOp { seq, sig, act }, ts, actor_of(key))
    }

    fn ops(key: &SigningKey, n: usize) -> Vec<Op<SignedOp>> {
        let payload = body_of_size(FIXTURE_BODY_BYTES);
        (0..n as u64)
            .map(|seq| {
                signed(
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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        format!("http://{addr}/internal/ring/sync")
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
        let (_sender, journal, _r1) = node(sender_dir.path(), &key, N);
        let (peer_state, peer_journal, _r2) =
            node(peer_dir.path(), &SigningKey::from_bytes(&[2u8; 32]), 0);

        let url = serve(internal_router(peer_state)).await;
        let out = exchange(&reqwest::Client::new(), &url, NS, &journal).await;

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

    /// The control for the test above: that journal really does need more
    /// than one exchange, so convergence there is evidence about the LOOP and
    /// not about a body that happened to fit.
    #[tokio::test]
    async fn ten_thousand_ops_do_not_fit_one_chunk() {
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let dir = tempfile::tempdir().unwrap();
        let (_state, journal, _r) = node(dir.path(), &key, 10_000);
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
        let (_state, journal, _r) = node(dir.path(), &key, 3);

        let out = exchange(&reqwest::Client::new(), &url, NS, &journal).await;
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
        let (_state, journal, _r) = node(dir.path(), &key, 3);

        let out = exchange(&reqwest::Client::new(), &url, NS, &journal).await;
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
            NS,
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
        let (_state, journal, _r) = node(dir.path(), &key, 3);

        let out = exchange(&reqwest::Client::new(), &url, NS, &journal).await;
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
}
