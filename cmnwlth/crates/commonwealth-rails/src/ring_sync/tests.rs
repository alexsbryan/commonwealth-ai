// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring round and its peer route, against a live listener
//! (pb-mesh-exit-transport; director phase-b-81 (5)).
//!
//! The successors of the daemon's ring tests (`ring_sync_loop_tests`,
//! `rail_e2e/ceiling`, `ring_sync_by_roster`, `ring_append_nudges_sync`,
//! `ring_return_syncs`), which drove this same loop when it ran in the
//! daemon: the round and the route are cw-rails' since pb-rails-parity, and
//! the daemon's copies retired with the flip. `exchange` needs a real socket,
//! so each responder is cw-rails' own `ring_routes` router on an ephemeral
//! port; the bodies are the old tests', moved onto `RingRail`.

use std::sync::Arc;

use axum::response::IntoResponse;
use commonwealth_rail::{
    actor_of, body_json, sign_ring_op, Digest, Ed25519Verifier, Op, Payload, Person, RailAct,
    RingJournal, RingRail, Roster, SignedOp, SigningKey,
};
use sovereign_peer_wire::{
    RingSyncRequest, RingSyncResponse, MAX_REQUEST_BODY_BYTES, RING_SYNC_OPS_BUDGET_BYTES,
};

use super::*;
use crate::ring_routes::{router, RingInbound};

mod round_tests;
use round_tests as round;

pub(super) const NS: &str = "house-expenses";

/// The fixture the ceiling table is quoted against: a 594-byte serialised
/// body, ~873 B/op on the wire.
const FIXTURE_BODY_BYTES: usize = 594;

pub(super) fn body_of_size(target: usize) -> Payload {
    let mut filler = target.saturating_sub(40);
    loop {
        let p = Payload::new(serde_json::json!({ "b": "x".repeat(filler) })).unwrap();
        let n = body_json(&RailAct::Record { payload: p.clone() }, None).len();
        if n >= target {
            return p;
        }
        filler += target - n;
    }
}

/// One op signed for its own `(namespace, ts, seq)`, so its id is distinct.
/// A signature binds the namespace, so an op signed for one ring is a gap on
/// any other.
pub(super) fn signed_in(ns: &str, key: &SigningKey, seq: u64, act: RailAct) -> Op<SignedOp> {
    let ts = 1_700_000_000i64 + seq as i64;
    let sig = sign_ring_op(key, ns, ts, seq, &body_json(&act, None));
    Op::new(
        SignedOp {
            seq,
            sig,
            act,
            on_behalf_of: None,
        },
        ts,
        actor_of(key),
    )
}

fn signed(key: &SigningKey, seq: u64, act: RailAct) -> Op<SignedOp> {
    signed_in(NS, key, seq, act)
}

pub(super) fn ops_in(ns: &str, key: &SigningKey, n: usize) -> Vec<Op<SignedOp>> {
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

pub(super) fn solo_roster(key: &SigningKey) -> Roster {
    let mut m = std::collections::BTreeMap::new();
    m.insert(Person::from("alex"), vec![actor_of(key)]);
    Roster::new(m)
}

/// A rail signing as `key` holding `n` signed ops on [`NS`], rostered to
/// `key`. `n = 0` is a node that has never seen this ring's ops — the
/// bootstrap case, which can only ever be TOLD.
fn node(dir: &std::path::Path, key: &SigningKey, n: usize) -> (Arc<RingRail>, Arc<RingJournal>) {
    let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
    let journal = rail.journal(NS).unwrap();
    journal.set_roster(&solo_roster(key)).unwrap();
    if n > 0 {
        assert_eq!(journal.ingest_all(&ops_in(NS, key, n)).unwrap(), n);
    }
    (rail, journal)
}

/// Serve `router` on an ephemeral loopback port, with the connect info the
/// internal listener carries. Returns the address.
pub(super) async fn serve_at(router: axum::Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
    });
    addr
}

/// cw-rails' ring routes over `rail`, as the internal listener mounts them.
pub(super) fn ring_router(rail: Arc<RingRail>) -> axum::Router {
    host_kit::shell::mount(vec![router(RingInbound {
        rail,
        live: Arc::new(crate::rail::LiveBuffer::default()),
        origins: commonwealth_media::origins::OriginRegistry::new(
            commonwealth_media::PublishedApps::default(),
        ),
    })])
}

/// `rail`'s sync route, served; the URL the round posts to.
async fn serve(rail: Arc<RingRail>) -> String {
    format!(
        "http://{}/internal/ring/sync",
        serve_at(ring_router(rail)).await
    )
}

/// One raw exchange call: the status, and the answer when it is a 200.
async fn sync_raw(url: &str, body: &RingSyncRequest) -> (u16, Option<RingSyncResponse>) {
    let resp = reqwest::Client::new()
        .post(url)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let answer = if status == 200 {
        Some(resp.json().await.unwrap())
    } else {
        None
    };
    (status, answer)
}

/// **The gate the ceiling measured.** A 10,000-op journal — past the
/// 9,599-op one-exchange ceiling — converges onto a peer that has never seen
/// this ring, through the real route and the real loop.
#[tokio::test]
async fn a_journal_past_the_one_exchange_ceiling_converges_onto_a_fresh_peer() {
    const N: usize = 10_000;
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let (sender_dir, peer_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (rail, journal) = node(sender_dir.path(), &key, N);
    let (peer_rail, peer_journal) = node(peer_dir.path(), &key, 0);

    let url = serve(peer_rail).await;
    let out = exchange(&reqwest::Client::new(), &url, rail.as_ref(), NS, None).await;

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

/// **A peer's seal shortens THIS node's disk, in the round it arrives.** The
/// assertion is on what is on disk afterwards: a prune that silently did
/// nothing would leave every count identical.
#[tokio::test]
async fn a_peers_seal_prunes_this_nodes_disk_in_the_round_it_arrives() {
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let (mine, theirs) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (rail, journal) = node(mine.path(), &key, 3);
    let (peer_rail, peer_journal) = node(theirs.path(), &key, 3);
    peer_journal
        .ingest(&signed(&key, 3, RailAct::Seal))
        .unwrap();
    assert_eq!(journal.read().unwrap().0.len(), 3, "control: we hold three");

    let url = serve(peer_rail).await;
    let out = exchange(&reqwest::Client::new(), &url, rail.as_ref(), NS, None).await;

    assert!(out.stop.is_none(), "the exchange failed: {:?}", out.stop);
    assert_eq!(out.pulled, 1, "one op came over, and it was the seal");
    let held = journal.read().unwrap().0;
    assert_eq!(held.len(), 1, "the retired prefix left our disk: {held:?}");
    assert!(matches!(held[0].kind.act, RailAct::Seal));
    let admitted = journal.admit(&solo_roster(&key), &Ed25519Verifier).unwrap();
    assert!(admitted.is_complete(), "gaps: {:?}", admitted.gaps);
    assert_eq!(
        journal.digest().unwrap(),
        peer_journal.digest().unwrap(),
        "two nodes, one claim"
    );
}

/// **A derived ring prunes too.** A ring with no `roster.json` is rostered
/// by membership (`MembershipRosterSource`); without the source the seal
/// arrives and retires nothing (the control), with it the prefix goes
/// exactly as on a hand-rostered ring.
#[tokio::test]
async fn a_peers_seal_prunes_a_ring_whose_roster_is_derived_from_membership() {
    const OWN: &str = "mesh-measurements";
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let me = commonwealth_core::ids::NodeId::from_u128(1);
    let pubkey = commonwealth_core::ids::NodePubkey(key.verifying_key().to_bytes());
    let mesh = Arc::new(tokio::sync::RwLock::new(round::mesh_of(vec![
        round::member(me, "me", &key, None),
    ])));
    let rail_on_own = |dir: &std::path::Path, with_source: bool| {
        let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
        let journal = rail.journal(OWN).unwrap();
        assert_eq!(journal.ingest_all(&ops_in(OWN, &key, 3)).unwrap(), 3);
        if with_source {
            crate::rail::MembershipRosterSource::install(&rail, &mesh, me, Some(pubkey));
        }
        (rail, journal)
    };
    let sealed_peer = |dir: &std::path::Path| {
        let (rail, journal) = rail_on_own(dir, true);
        journal
            .ingest(&signed_in(OWN, &key, 3, RailAct::Seal))
            .unwrap();
        rail
    };

    // Control: no source, so the roster is the empty file and nothing goes.
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (rail, journal) = rail_on_own(a.path(), false);
    let url = serve(sealed_peer(b.path())).await;
    let out = exchange(&reqwest::Client::new(), &url, rail.as_ref(), OWN, None).await;
    assert_eq!(out.pulled, 1, "{:?}", out.stop);
    assert_eq!(
        journal.read().unwrap().0.len(),
        4,
        "control: without the source the seal arrives and retires nothing"
    );

    // Membership answers, and the prefix goes.
    let (c, d) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (rail, journal) = rail_on_own(c.path(), true);
    let roster = RingRail::roster(&rail, &journal).await.unwrap();
    assert!(
        roster.person_for(&actor_of(&key)).is_some(),
        "the derived roster does not claim our key: {roster:?}"
    );
    let url = serve(sealed_peer(d.path())).await;
    let out = exchange(&reqwest::Client::new(), &url, rail.as_ref(), OWN, None).await;
    assert_eq!(out.pulled, 1, "{:?}", out.stop);
    let held = journal.read().unwrap().0;
    assert_eq!(held.len(), 1, "the retired prefix stayed on disk: {held:?}");
    assert!(matches!(held[0].kind.act, RailAct::Seal));
}

/// The control for the seal tests: the identical exchange with an ordinary
/// act pulls the same one op and deletes NOTHING.
#[tokio::test]
async fn an_ordinary_op_arriving_prunes_nothing() {
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let (mine, theirs) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (rail, journal) = node(mine.path(), &key, 3);
    let (peer_rail, peer_journal) = node(theirs.path(), &key, 3);
    peer_journal
        .ingest(&signed(
            &key,
            3,
            RailAct::Record {
                payload: body_of_size(FIXTURE_BODY_BYTES),
            },
        ))
        .unwrap();

    let url = serve(peer_rail).await;
    let out = exchange(&reqwest::Client::new(), &url, rail.as_ref(), NS, None).await;
    assert_eq!(out.pulled, 1);
    assert_eq!(journal.read().unwrap().0.len(), 4, "nothing was retired");
}

/// The budget binds: a 10,000-op journal does not fit one chunk, so the
/// convergence test above is evidence about the LOOP. Also pins the
/// per-op cost the ceiling table assumes, and that a chunk fits the limit it
/// was budgeted against.
#[tokio::test]
async fn ten_thousand_ops_do_not_fit_one_chunk() {
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let dir = tempfile::tempdir().unwrap();
    let (_rail, journal) = node(dir.path(), &key, 10_000);
    let (chunk, more) = journal
        .ops_missing_from_within(&Digest::new(), RING_SYNC_OPS_BUDGET_BYTES)
        .unwrap();
    assert!(more, "the budget must cut a 10,000-op journal short");
    assert!(
        chunk.len() < 10_000,
        "one chunk carried all {}",
        chunk.len()
    );
    assert!(
        serde_json::to_vec(&chunk).unwrap().len() <= MAX_REQUEST_BODY_BYTES,
        "a chunk must fit the limit it was budgeted against"
    );
    let all = journal.ops_missing_from(&Digest::new()).unwrap();
    let per_op = serde_json::to_vec(&all).unwrap().len() / all.len();
    assert!(
        (860..=890).contains(&per_op),
        "the {FIXTURE_BODY_BYTES}-byte fixture costs {per_op} B/op, not the ~873 the table assumes"
    );
}

/// The route's own limit: an unbudgeted push of 9,000 ops is served, 10,000
/// is refused 413, and the budgeted chunk of those 10,000 is served.
#[tokio::test]
async fn the_budgeted_chunk_is_served_where_the_whole_journal_is_refused() {
    let key = SigningKey::from_bytes(&[1u8; 32]);
    for (n, want) in [(9_000usize, 200u16), (10_000, 413)] {
        let (s, r) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (_rail, journal) = node(s.path(), &key, n);
        let (peer, _) = node(r.path(), &key, 0);
        let push = RingSyncRequest {
            namespace: NS.to_string(),
            digest: journal.digest().unwrap(),
            ops: journal.ops_missing_from(&Digest::new()).unwrap(),
        };
        let (status, _) = sync_raw(&serve(peer).await, &push).await;
        assert_eq!(status, want, "{n} unbudgeted ops");
    }
    let (s, r) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (_rail, journal) = node(s.path(), &key, 10_000);
    let (peer, _) = node(r.path(), &key, 0);
    let (ops, more) = journal
        .ops_missing_from_within(&Digest::new(), RING_SYNC_OPS_BUDGET_BYTES)
        .unwrap();
    assert!(more, "the control: 10,000 ops must not fit one chunk");
    let push = RingSyncRequest {
        namespace: NS.to_string(),
        digest: journal.digest().unwrap(),
        ops,
    };
    let (status, _) = sync_raw(&serve(peer).await, &push).await;
    assert_eq!(status, 200, "the budgeted chunk is served");
}

/// The pull direction is budgeted too, and converges by repeating: every
/// answer fits the limit, and each carries ops the puller lacked.
#[tokio::test]
async fn the_pull_direction_is_budgeted_and_converges_by_repeating() {
    const N: usize = 10_000;
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let (h, f) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (holder, journal) = node(h.path(), &key, N);
    let (_fresh, fresh_journal) = node(f.path(), &key, 0);
    let url = serve(holder).await;
    let mut rounds = 0usize;
    loop {
        let ask = RingSyncRequest {
            namespace: NS.to_string(),
            digest: fresh_journal.digest().unwrap(),
            ops: Vec::new(),
        };
        let (status, body) = sync_raw(&url, &ask).await;
        assert_eq!(status, 200);
        let body = body.unwrap();
        let bytes = serde_json::to_vec(&body).unwrap().len();
        assert!(
            bytes <= MAX_REQUEST_BODY_BYTES,
            "round {rounds}: {bytes} B — the pull direction is unbounded again"
        );
        if body.ops.is_empty() {
            break;
        }
        assert!(
            fresh_journal.ingest_all(&body.ops).unwrap() > 0,
            "round {rounds} carried ops the puller already held — the spin"
        );
        rounds += 1;
        assert!(rounds < 64, "the pull did not converge in 64 rounds");
    }
    assert!(
        rounds > 1,
        "the control: {N} ops must take more than one pull"
    );
    let admitted = fresh_journal
        .admit(&solo_roster(&key), &Ed25519Verifier)
        .unwrap();
    assert_eq!(admitted.ops.len(), N);
    assert!(admitted.is_complete(), "gaps: {:?}", admitted.gaps);
    assert_eq!(fresh_journal.digest().unwrap(), journal.digest().unwrap());
}

/// A seal is an op, not a delete: the exchange shortens only once the
/// retired lines are gone from disk, and the compacted suffix bootstraps a
/// fresh peer in one chunk, complete.
#[tokio::test]
async fn a_seal_shortens_the_exchange_only_once_the_retired_lines_are_deleted() {
    const RETIRED: u64 = 10_000;
    const KEPT: u64 = 500;
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let dir = tempfile::tempdir().unwrap();
    let (rail, journal) = node(dir.path(), &key, RETIRED as usize);
    let payload = body_of_size(FIXTURE_BODY_BYTES);
    let mut tail = vec![signed(&key, RETIRED, RailAct::Seal)];
    tail.extend((RETIRED + 1..=RETIRED + KEPT).map(|seq| {
        signed(
            &key,
            seq,
            RailAct::Record {
                payload: payload.clone(),
            },
        )
    }));
    assert_eq!(journal.ingest_all(&tail).unwrap(), tail.len());

    /// Bootstrap a fresh peer from `rail`: what it pushed, the peer's
    /// journal, and the dir that holds it.
    async fn bootstrap(
        rail: &RingRail,
        key: &SigningKey,
    ) -> (usize, Arc<RingJournal>, tempfile::TempDir) {
        let p = tempfile::tempdir().unwrap();
        let (peer, peer_journal) = node(p.path(), key, 0);
        let url = serve(peer).await;
        let out = exchange(&reqwest::Client::new(), &url, rail, NS, None).await;
        assert!(out.stop.is_none(), "{:?}", out.stop);
        (out.pushed, peer_journal, p)
    }
    let (sealed_pushed, _, _p1) = bootstrap(&rail, &key).await;
    assert_eq!(
        sealed_pushed,
        (RETIRED + KEPT + 1) as usize,
        "a seal is an op, not a delete: everything still goes out"
    );

    let path = journal.dir().join("ring_oplog.jsonl");
    let kept: String = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|l| {
            serde_json::from_str::<Op<SignedOp>>(l)
                .map(|o| o.kind.seq >= RETIRED)
                .unwrap_or(true)
        })
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&path, kept).unwrap();
    assert_eq!(journal.read().unwrap().0.len(), (KEPT + 1) as usize);

    let (ops, more) = journal
        .ops_missing_from_within(&Digest::new(), RING_SYNC_OPS_BUDGET_BYTES)
        .unwrap();
    assert!(!more, "the compacted suffix fits one chunk");
    assert_eq!(ops.len(), (KEPT + 1) as usize);
    let (compacted_pushed, peer_journal, _p2) = bootstrap(&rail, &key).await;
    assert_eq!(compacted_pushed, (KEPT + 1) as usize, "the suffix lands");
    let admitted = peer_journal
        .admit(&solo_roster(&key), &Ed25519Verifier)
        .unwrap();
    assert!(admitted.is_complete(), "gaps: {:?}", admitted.gaps);
    assert_eq!(admitted.applied().count(), KEPT as usize);
    assert_eq!(peer_journal.digest().unwrap(), journal.digest().unwrap());
}

/// A route answering `body` to every exchange, served; its URL.
async fn stand_in(
    answer: impl Fn(RingSyncRequest) -> axum::response::Response + Clone + Send + Sync + 'static,
) -> String {
    let app = axum::Router::new().route(
        "/internal/ring/sync",
        axum::routing::post(move |body: axum::body::Bytes| {
            let answer = answer.clone();
            async move { answer(serde_json::from_slice(&body).unwrap()) }
        }),
    );
    format!("http://{}/internal/ring/sync", serve_at(app).await)
}

/// A real pull is not discarded by a later failure: call 1's ops are on
/// disk and counted although call 2 is refused.
#[tokio::test]
async fn a_second_call_that_fails_still_reports_what_the_first_call_pulled() {
    let gift = ops_in(NS, &SigningKey::from_bytes(&[9u8; 32]), 5);
    let url = stand_in(move |req| {
        if !req.ops.is_empty() {
            return axum::http::StatusCode::PAYLOAD_TOO_LARGE.into_response();
        }
        axum::Json(RingSyncResponse {
            namespace: req.namespace,
            digest: Digest::new(),
            ops: gift.clone(),
            ingested: 0,
        })
        .into_response()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let (rail, journal) = node(dir.path(), &SigningKey::from_bytes(&[1u8; 32]), 3);

    let out = exchange(&reqwest::Client::new(), &url, rail.as_ref(), NS, None).await;
    assert!(
        matches!(out.stop, Some(ExchangeStop::Refused { .. })),
        "a 413 is a refusal: {:?}",
        out.stop
    );
    assert_eq!(out.pulled, 5, "call 1's five ops are counted");
    assert_eq!(journal.read().unwrap().0.len(), 8, "3 held + 5 pulled");
}

/// 403 and 413 are answers, not unreachability; a dead address is Failed.
#[tokio::test]
async fn a_peer_that_answers_403_or_413_is_refused_rather_than_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    let (rail, _journal) = node(dir.path(), &SigningKey::from_bytes(&[1u8; 32]), 3);
    for code in [403u16, 413] {
        let url = stand_in(move |_| {
            axum::http::StatusCode::from_u16(code)
                .unwrap()
                .into_response()
        })
        .await;
        let out = exchange(&reqwest::Client::new(), &url, rail.as_ref(), NS, None).await;
        match out.stop {
            Some(ExchangeStop::Refused { sent_bytes, status }) => {
                assert_eq!(status, code, "the status picks the sentence");
                assert!(
                    sent_bytes > 0,
                    "the refused size is what makes it actionable"
                );
            }
            other => panic!("{code}: expected Refused, got {other:?}"),
        }
    }
    let out = exchange(
        &reqwest::Client::new(),
        "http://127.0.0.1:1/internal/ring/sync",
        rail.as_ref(),
        NS,
        None,
    )
    .await;
    assert!(
        matches!(out.stop, Some(ExchangeStop::Failed(_))),
        "an unreachable address is not a refusal: {:?}",
        out.stop
    );
}

/// A peer that answers but never ingests cannot make the loop spin: the
/// stall check sees an unmoved digest and hands the round back.
#[tokio::test]
async fn a_peer_whose_digest_never_moves_stops_the_loop_instead_of_spinning() {
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = hits.clone();
    let url = stand_in(move |req| {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        axum::Json(RingSyncResponse {
            namespace: req.namespace,
            digest: Digest::new(),
            ops: Vec::new(),
            ingested: 0,
        })
        .into_response()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let (rail, _journal) = node(dir.path(), &SigningKey::from_bytes(&[1u8; 32]), 3);

    let out = exchange(&reqwest::Client::new(), &url, rail.as_ref(), NS, None).await;
    assert!(out.stop.is_none());
    assert_eq!((out.pulled, out.pushed), (0, 0));
    let calls = hits.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        calls <= 4,
        "the stall check must stop after the second chunk, not run all \
         {MAX_CHUNKS_PER_EXCHANGE} — {calls} calls"
    );
}
