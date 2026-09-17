//! The loop itself, against a live listener.
//!
//! `exchange` needs a `reqwest` client and a real socket, so these bind
//! the daemon's internal router on an ephemeral port — the
//! same shape `tests/main/gossip_integration.rs` uses for the gossip loop,
//! and the only way to drive the production loop rather than a second
//! spelling of it (ARCH §10.6).

use super::*;
use axum::response::IntoResponse;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_rail::{
    actor_of, body_json, sign_ring_op, Ed25519Verifier, Op, Payload, Person, RailAct, RingJournal,
    RingRail, Roster, SignedOp, SigningKey,
};
use sovereign_api::server::internal_router;
use std::collections::HashMap;
use std::sync::Arc;

pub(super) const NS: &str = "house-expenses";

/// The fixture 2a's ceiling table is quoted against: a 594-byte
/// serialised body, ~873 B/op on the wire.
pub(super) const FIXTURE_BODY_BYTES: usize = 594;

pub(super) fn bare_state_with_seed(seed: sovereign_api::state::FabricSeed) -> AppState {
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
    AppState::new_with_platform_and_engine_and_gauge_and_fabric(
        NodeId::from_u128(1),
        mesh,
        Arc::new(commonwealth_state::MeshStore::in_memory().unwrap()),
        Arc::new(sovereign_meshapp_registry::registry::AppRegistry::new()),
        None,
        None,
        seed,
    )
}

pub(super) fn body_of_size(target: usize) -> Payload {
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
pub(super) fn signed(key: &SigningKey, seq: u64, act: RailAct) -> Op<SignedOp> {
    signed_in(NS, key, seq, act)
}

/// A signature binds the namespace, so an op signed for `NS` is a gap on
/// any other ring — a fixture reused across namespaces would make every
/// test on the second ring pass or fail for that reason alone.
pub(super) fn signed_in(ns: &str, key: &SigningKey, seq: u64, act: RailAct) -> Op<SignedOp> {
    let ts = 1_700_000_000i64 + seq as i64;
    let sig = sign_ring_op(key, ns, ts, seq, &body_json(&act));
    Op::new(SignedOp { seq, sig, act }, ts, actor_of(key))
}

pub(super) fn ops(key: &SigningKey, n: usize) -> Vec<Op<SignedOp>> {
    ops_in(NS, key, n)
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

/// A node holding `n` signed ops. `n = 0` is a node that has never seen
/// this ring — the bootstrap case, which can only ever be TOLD.
pub(super) fn node(
    dir: &std::path::Path,
    key: &SigningKey,
    n: usize,
) -> (AppState, Arc<RingJournal>, Arc<RingRail>) {
    let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
    let journal = rail.journal(NS).unwrap();
    journal.set_roster(&solo_roster(key)).unwrap();
    if n > 0 {
        assert_eq!(journal.ingest_all(&ops(key, n)).unwrap(), n);
    }
    let state = bare_state_with_seed(sovereign_api::state::FabricSeed {
        ring_rail: Some(rail.clone()),
        ..Default::default()
    });
    (state, journal, rail)
}

pub(super) async fn serve(router: axum::Router) -> String {
    let addr = serve_at(router).await;
    format!("http://{addr}/internal/ring/sync")
}

/// [`serve`] for a caller that needs the ADDRESS rather than the route —
/// `run_one_round` dials a member's `addresses`, so a test that drives the
/// round has to put a real one in the mesh.
pub(super) async fn serve_at(router: axum::Router) -> std::net::SocketAddr {
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
        let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
        let journal = rail.journal(OWN).unwrap();
        assert_eq!(journal.ingest_all(&ops_in(OWN, &key, 3)).unwrap(), 3);
        let state = AppState::new_with_platform_and_engine_and_gauge_and_fabric(
            me,
            mesh_of(vec![member(me, "me", Some(pubkey_of(&key)))]),
            Arc::new(commonwealth_state::MeshStore::in_memory().unwrap()),
            Arc::new(sovereign_meshapp_registry::registry::AppRegistry::new()),
            None,
            None,
            sovereign_api::state::FabricSeed {
                ring_rail: Some(rail.clone()),
                ..Default::default()
            },
        );
        if with_source {
            MeshRosterSource::install(
                &rail,
                &state.inner.fabric.mesh,
                &state.inner.fabric.identity,
                state.self_node_pubkey(),
            )
            .unwrap();
        }
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
        serde_json::to_vec(&chunk).unwrap().len() <= sovereign_api::server::MAX_REQUEST_BODY_BYTES,
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
