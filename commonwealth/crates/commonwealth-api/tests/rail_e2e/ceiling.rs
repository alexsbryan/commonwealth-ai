// SPDX-License-Identifier: AGPL-3.0-or-later
//! The convergence ceiling, and the budget that ended it.
//!
//! Split out of `main.rs` when rung 2f took the suite past ARCH §3.2's 1200
//! lines. `use super::*` carries the shared fixtures — `sync_once`, `sync_raw`,
//! `node`, `bare_state` — which stay with the suite they were written for.

use super::*;

// ── the convergence ceiling, and the budget that ended it ────
//
// `ops_missing_from` returned everything a peer lacks in ONE body, and the
// receiver caps a request body at `MAX_REQUEST_BODY_BYTES`. Past ~9,599 ops
// of the fixture below those two met, and the meeting was silent: the 413 is
// answered at the extractor, so the handler never ran, the gauge it carries
// could not fire, and the sender filed a reachable peer as unreachable at
// DEBUG. The peer that had been refused the journal reported zero ops, zero
// gaps and a COMPLETE ring.
//
// `RING_SYNC_OPS_BUDGET_BYTES` stops the selection at a byte budget and the
// exchange repeats, so one body is no longer the unit of convergence.
// **Nothing on the wire changed shape** — the exchange was always idempotent
// — which is why every per-body figure below is unchanged and only what they
// MEAN has moved: they now size a chunk, not a ceiling.
//
// THE FIGURES ARE IN BYTES, NOT OPS, so every one names its fixture.
// Measured on this host with the production types (railread, release, and
// reproduced by `one_budgeted_chunk_carries_less_than_a_whole_journal`):
//
//   fixture body   B/op on the wire   ops in 8 MiB
//   594 B (order 2 §2's ledger)   873   9,608
//   ~332 B (an expense row)       609  13,774
//   ~274 B (a work-atlas obs.)    552  15,196
//
// The 594-byte fixture is the one carried here because it is the one order 2
// priced, and it puts the figure nearest the round number the order named.

/// A `Payload` whose serialised `RailAct::Record` body is at least `target`
/// bytes. Constructed rather than guessed: the envelope overhead is measured
/// by `body_json` on each pass instead of being hard-coded, so a change to
/// the act's wire shape moves the fixture with it.
fn body_of_size(target: usize) -> Payload {
    let mut filler = target.saturating_sub(40);
    loop {
        let payload = Payload::new(serde_json::json!({ "b": "x".repeat(filler) })).unwrap();
        let got = commonwealth_rail::body_json(&RailAct::Record {
            payload: payload.clone(),
        })
        .len();
        if got >= target {
            return payload;
        }
        filler += target - got;
    }
}

/// The fixture order 2 §2's ceiling table is quoted against.
const CEILING_FIXTURE_BODY_BYTES: usize = 594;

/// `n` real signed ops on disk, one `append_all`. Every op is signed for its
/// own `seq` and `ts`, so each carries a distinct content-derived `OpId` and
/// a signature that actually verifies — a fixture of clones would make the
/// convergence assertion below meaningless.
fn seed(journal: &RingJournal, key: &SigningKey, n: usize) -> usize {
    let payload = body_of_size(CEILING_FIXTURE_BODY_BYTES);
    let ops: Vec<_> = (0..n as u64)
        .map(|seq| {
            signed(
                key,
                journal.namespace(),
                seq,
                RailAct::Record {
                    payload: payload.clone(),
                },
            )
        })
        .collect();
    journal.ingest_all(&ops).unwrap()
}

/// One op, signed for its own `(namespace, ts, seq)` exactly as the door
/// signs it — so its `OpId` is distinct and its signature actually verifies.
fn signed(key: &SigningKey, namespace: &str, seq: u64, act: RailAct) -> Op<SignedOp> {
    let ts = 1_700_000_000i64 + seq as i64;
    let sig = commonwealth_rail::sign_ring_op(
        key,
        namespace,
        ts,
        seq,
        &commonwealth_rail::body_json(&act),
    );
    Op::new(SignedOp { seq, sig, act }, ts, key.actor())
}

fn solo_roster(key: &SigningKey) -> Roster {
    let mut m = std::collections::BTreeMap::new();
    m.insert(Person::from("alex"), vec![key.actor()]);
    Roster::new(m)
}

/// A daemon holding a ring under `dir`, signing as `key`. `n = 0` leaves it a
/// node that has never seen this ring at all — no `rings/<ns>` directory,
/// which is the state that matters below.
fn node(
    dir: &std::path::Path,
    key: &SigningKey,
    n: usize,
) -> (AppState, std::sync::Arc<RingJournal>) {
    let state = bare_state();
    let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
    let journal = rail.journal(NS).unwrap();
    if n > 0 {
        journal.set_roster(&solo_roster(key)).unwrap();
        assert_eq!(seed(&journal, key, n), n, "the fixture must land in full");
    }
    state.install_ring_rail(rail);
    (state, journal)
}

/// **The measurement, not arithmetic.** Pins the per-op wire cost of the
/// fixture the other tests use, and derives from it what one budgeted CHUNK
/// carries — so a change to the op envelope or to the budget moves the number
/// here, where it is one assertion, rather than silently un-chunking the
/// convergence tests below.
///
/// All three bounds are alarms rather than descriptions:
///
/// - **860-890 B/op** is the envelope. It is what every figure in the table
///   above is quoted against.
/// - **Under 10,000 ops in one body** is 2a's ceiling. It has NOT moved and
///   is not supposed to — the fix was to stop making one body the unit of
///   convergence, not to make the body bigger.
/// - **Under 10,000 ops in one chunk** is what keeps the tests below honest.
///   If a chunk ever carried a whole 10,000-op journal, every convergence
///   assertion here would pass without a second chunk ever being sent.
#[tokio::test]
async fn one_budgeted_chunk_carries_less_than_a_whole_journal() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    const N: usize = 500;
    let (_state, journal) = node(dir.path(), &key, N);

    let body = RingSyncRequest {
        namespace: NS.to_string(),
        digest: journal.digest().unwrap(),
        ops: journal.ops_missing_from(&Digest::new()).unwrap(),
    };
    assert_eq!(body.ops.len(), N, "an empty digest asks for everything");
    let per_op = serde_json::to_vec(&body).unwrap().len() / N;
    let one_body = commonwealth_api::server::MAX_REQUEST_BODY_BYTES / per_op;
    let per_chunk = RING_SYNC_OPS_BUDGET_BYTES / per_op;

    assert!(
        (860..=890).contains(&per_op),
        "the {CEILING_FIXTURE_BODY_BYTES}-byte fixture costs {per_op} B/op on \
         the wire, not the ~873 the ceiling table assumes"
    );
    assert!(
        one_body < 10_000,
        "one body now holds {one_body} ops — the envelope or the limit moved, \
         and every figure in the table above is stale"
    );
    assert!(
        (1..10_000).contains(&per_chunk),
        "one chunk carries {per_chunk} ops — the convergence tests below stop \
         exercising a second chunk"
    );
}

/// **The sender's loop, driven against the real route.**
///
/// The production spelling is `sovereign_mesh::ring_sync::exchange`, which
/// this harness cannot call: that one needs a `reqwest` client against a live
/// socket and this dispatches through `tower::oneshot`. (It is tested against
/// a real listener, in that crate, beside the loop it drives.) What is NOT
/// duplicated here is the DECISION — the chunk comes from the production
/// `ops_missing_from_within` at the production `RING_SYNC_OPS_BUDGET_BYTES`,
/// so a budget that stopped binding would fail these tests too.
///
/// Returns `(chunks, ops the peer reported as new)`.
async fn push_until_converged(peer: AppState, journal: &RingJournal) -> (usize, usize) {
    let mut chunks = 0usize;
    let mut pushed = 0usize;
    loop {
        // Call 1 — learn what they hold. A digest is ~600 bytes whatever the
        // journal weighs, which is why this half was never the problem.
        let theirs = sync_once(peer.clone(), NS, journal.digest().unwrap(), Vec::new()).await;
        let (ops, more) = journal
            .ops_missing_from_within(&theirs.digest, RING_SYNC_OPS_BUDGET_BYTES)
            .unwrap();
        if ops.is_empty() {
            assert!(!more, "nothing to send cannot also mean more remains");
            return (chunks, pushed);
        }
        let offered = ops.len();
        let push = RingSyncRequest {
            namespace: NS.to_string(),
            digest: journal.digest().unwrap(),
            ops,
        };
        let bytes = serde_json::to_vec(&push).unwrap().len();
        let (status, body) = sync_raw(peer.clone(), &push).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "chunk {chunks} was {bytes} B and was refused — the budget exists \
             to keep every chunk under the {} B limit",
            commonwealth_api::server::MAX_REQUEST_BODY_BYTES
        );
        let ingested = body.expect("a 200 carries a response").ingested;
        assert!(
            ingested > 0,
            "chunk {chunks} offered {offered} ops and moved none — a chunk \
             whose first op the peer already holds is the spin the contiguous \
             mark is supposed to make impossible"
        );
        pushed += ingested;
        chunks += 1;
        assert!(chunks < 64, "the push did not converge in 64 chunks");
    }
}

/// **The limit is still real; the budget is what keeps us under it.**
///
/// Nothing about the receiver got more permissive in 2f, and this is where
/// that is nailed down — otherwise "it converges now" could as easily mean
/// somebody raised `MAX_REQUEST_BODY_BYTES` and moved the same failure a
/// factor of two out.
///
/// - The whole selection in ONE body, the shape the loop sent until 2f, is
///   still refused at 10,000 ops.
/// - The 9,000-op arm is the negative control: a body under the limit is
///   served, so the 413 is evidence about SIZE and not about a route that
///   refuses everything.
/// - The same 10,000-op journal offered as a budgeted CHUNK — what the loop
///   sends now — is served.
#[tokio::test]
async fn the_budgeted_chunk_is_served_where_the_whole_journal_is_refused() {
    let key = SigningKey::from_bytes(&[1u8; 32]);

    for (n, want) in [
        (9_000usize, StatusCode::OK),
        (10_000, StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let sender_dir = tempfile::tempdir().unwrap();
        let receiver = tempfile::tempdir().unwrap();
        let (_sender, journal) = node(sender_dir.path(), &key, n);
        // A node that has never seen this ring: it holds nothing, so
        // `ops_missing_from(&empty)` is the sender's whole journal.
        let (peer, _) = node(receiver.path(), &SigningKey::from_bytes(&[2u8; 32]), 0);

        let push = RingSyncRequest {
            namespace: NS.to_string(),
            digest: journal.digest().unwrap(),
            ops: journal.ops_missing_from(&Digest::new()).unwrap(),
        };
        let bytes = serde_json::to_vec(&push).unwrap().len();
        let (status, _) = sync_raw(peer, &push).await;
        assert_eq!(
            status,
            want,
            "{n} unbudgeted ops = {bytes} B against a {} B limit",
            commonwealth_api::server::MAX_REQUEST_BODY_BYTES
        );
    }

    // And the same journal, budgeted the way the loop sends it.
    let sender_dir = tempfile::tempdir().unwrap();
    let receiver = tempfile::tempdir().unwrap();
    let (_sender, journal) = node(sender_dir.path(), &key, 10_000);
    let (peer, _) = node(receiver.path(), &SigningKey::from_bytes(&[2u8; 32]), 0);
    let (ops, more) = journal
        .ops_missing_from_within(&Digest::new(), RING_SYNC_OPS_BUDGET_BYTES)
        .unwrap();
    assert!(
        more,
        "the control: 10,000 ops must not fit one chunk, or the arm below \
         proves only that a small body is served"
    );
    let push = RingSyncRequest {
        namespace: NS.to_string(),
        digest: journal.digest().unwrap(),
        ops,
    };
    let bytes = serde_json::to_vec(&push).unwrap().len();
    let (status, _) = sync_raw(peer, &push).await;
    assert_eq!(status, StatusCode::OK, "the budgeted chunk was {bytes} B");
}

/// **The defect 2a named, flipped into the alarm.**
///
/// This test used to assert the defect: a peer refused the journal reported
/// zero ops, zero gaps and `is_complete()` TRUE. "I hold the whole journal
/// and there is nothing in it" and "the push that would have given me the
/// journal was refused" are different facts, and one 413 at the extractor
/// collapsed them into the plausible one (ARCH §18.3).
///
/// The same 10,000-op journal, over the same route, now converges — because
/// the exchange is budgeted and repeated rather than made bigger.
///
/// **The bootstrap case is the one that had to work.** A node that has never
/// seen this ring has no `rings/<ns>/` directory, so `run_one_round`
/// enumerates nothing and returns before dialling anybody
/// (`sovereign-mesh/src/ring_sync.rs:117`, `:124`): it can only ever be TOLD,
/// over the direction that has a body limit. This peer is that node, and the
/// assertion below is that being told now works at any journal size.
///
/// Watched RED against the unbudgeted shape (`RING_SYNC_OPS_BUDGET_BYTES`
/// raised past the body limit): the first chunk comes back 413 and the fold
/// reports the empty, complete ring the paragraph above describes.
#[tokio::test]
async fn a_journal_past_the_old_ceiling_converges_onto_a_node_that_has_never_seen_the_ring() {
    const N: usize = 10_000;
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let sender_dir = tempfile::tempdir().unwrap();
    let peer_dir = tempfile::tempdir().unwrap();
    let (_sender, journal) = node(sender_dir.path(), &key, N);
    let (peer_state, peer_journal) = node(peer_dir.path(), &SigningKey::from_bytes(&[2u8; 32]), 0);
    peer_journal.set_roster(&solo_roster(&key)).unwrap();

    let (chunks, pushed) = push_until_converged(peer_state, &journal).await;
    assert!(
        chunks > 1,
        "the control: {N} ops must take more than one chunk, or this passes \
         without ever exercising the repeat"
    );
    assert_eq!(pushed, N, "every op landed, in {chunks} chunks");

    let after = peer_journal
        .admit(&solo_roster(&key), &Ed25519Verifier)
        .unwrap();
    assert_eq!(after.ops.len(), N, "the whole journal arrived");
    assert!(
        after.is_complete(),
        "a chunked bootstrap is not a holed one: {:?}",
        after.gaps
    );
    assert_eq!(
        peer_journal.digest().unwrap(),
        journal.digest().unwrap(),
        "two nodes, one claim"
    );
}

/// **The response is budgeted too, and it had to be.**
///
/// `DefaultBodyLimit` bounds the REQUEST. The same journal used to come back
/// in a RESPONSE with no cap at all, which sounded like a free rescue and was
/// two problems: the direction that could not bootstrap a fresh peer was also
/// the one direction nothing bounded, on a route every peer that can route to
/// this host may call. So the budget applies to both, and the pull converges
/// by repeating exactly as the push does.
///
/// The bound is asserted every round rather than once, because a response
/// that fits on round one and not on round three is the failure that would
/// otherwise ship.
#[tokio::test]
async fn the_pull_direction_is_budgeted_and_converges_by_repeating() {
    const N: usize = 10_000;
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let holder_dir = tempfile::tempdir().unwrap();
    let (holder, journal) = node(holder_dir.path(), &key, N);

    let fresh_dir = tempfile::tempdir().unwrap();
    let (_fresh, fresh_journal) = node(fresh_dir.path(), &SigningKey::from_bytes(&[2u8; 32]), 0);
    fresh_journal.set_roster(&solo_roster(&key)).unwrap();

    let mut rounds = 0usize;
    loop {
        // A fresh peer's own digest, no ops offered — the pull direction.
        let body = sync_once(
            holder.clone(),
            NS,
            fresh_journal.digest().unwrap(),
            Vec::new(),
        )
        .await;
        let bytes = serde_json::to_vec(&body).unwrap().len();
        assert!(
            bytes <= commonwealth_api::server::MAX_REQUEST_BODY_BYTES,
            "round {rounds}: the response was {bytes} B against a {} B limit — \
             the pull direction is unbounded again",
            commonwealth_api::server::MAX_REQUEST_BODY_BYTES
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
    assert_eq!(
        fresh_journal.digest().unwrap(),
        journal.digest().unwrap(),
        "two nodes, one answer"
    );
}

/// **Does the sealed floor (`dc9010181`) reduce what goes on the wire? Only
/// the DELETE does, and nothing in the tree performs it.**
///
/// Two arms over the same journal, and the pair is the finding:
///
/// - **A seal alone buys nothing on the wire.** `ops_missing_from` is
///   author-blind over what this node HOLDS, and a peer with an empty digest
///   is missing all of it, so appending a `Seal` while the retired lines are
///   still on disk sends exactly the same ops — now over several chunks
///   instead of one refused body, which is cheaper in nothing but failure.
/// - **Deleting the retired lines is the whole mitigation.** The same push
///   after compaction carries the suffix only, in ONE chunk, and lands a
///   fresh peer on a ring `admit` calls COMPLETE — the floor travels as the
///   seal's own op, so what was retired is absent by agreement rather than a
///   hole.
///
/// The delete is done here, by this test, because there is no production
/// routine that does it: `RailAct::Seal` is reachable through `append`, and
/// `grep -rn "compact\|truncate"` over the rail crates, the API and
/// `ring_cmd` finds no writer that removes a retired prefix and no verb that
/// authors a seal. The chunked exchange makes an uncompacted journal
/// *converge*; the seal plus a delete is what makes it *small*, and those are
/// different wins.
#[tokio::test]
async fn a_seal_shortens_the_exchange_only_once_the_retired_lines_are_deleted() {
    const RETIRED: u64 = 10_000;
    const KEPT: u64 = 500;
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let dir = tempfile::tempdir().unwrap();
    let (_holder, journal) = node(dir.path(), &key, RETIRED as usize);

    // The seal takes the actor's next ordinary seq, then work continues.
    let payload = body_of_size(CEILING_FIXTURE_BODY_BYTES);
    let mut tail = vec![signed(&key, NS, RETIRED, RailAct::Seal)];
    tail.extend((RETIRED + 1..=RETIRED + KEPT).map(|seq| {
        signed(
            &key,
            NS,
            seq,
            RailAct::Record {
                payload: payload.clone(),
            },
        )
    }));
    assert_eq!(journal.ingest_all(&tail).unwrap(), tail.len());

    let push = |journal: &RingJournal| RingSyncRequest {
        namespace: NS.to_string(),
        digest: journal.digest().unwrap(),
        ops: journal.ops_missing_from(&Digest::new()).unwrap(),
    };

    // ARM 1 — sealed, not compacted. The selection is unchanged, so the
    // exchange still costs several chunks.
    let before = push(&journal);
    assert_eq!(before.ops.len(), (RETIRED + KEPT + 1) as usize);
    let peer_dir = tempfile::tempdir().unwrap();
    let (peer, peer_journal) = node(peer_dir.path(), &SigningKey::from_bytes(&[2u8; 32]), 0);
    peer_journal.set_roster(&solo_roster(&key)).unwrap();
    let (sealed_chunks, sealed_pushed) = push_until_converged(peer, &journal).await;
    assert_eq!(sealed_pushed, (RETIRED + KEPT + 1) as usize);
    assert!(
        sealed_chunks > 1,
        "a seal is an op, not a delete: {} ops still go out",
        before.ops.len()
    );

    // ARM 2 — the delete the rail has no routine for. Same filter shape as
    // `commonwealth-rail`'s own journal drill: keep the seal and everything
    // above it, keep anything unparseable rather than losing it.
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
    assert_eq!(
        journal.read().unwrap().0.len(),
        (KEPT + 1) as usize,
        "the prefix is really gone"
    );

    let after = push(&journal);
    let peer_dir = tempfile::tempdir().unwrap();
    let (peer, peer_journal) = node(peer_dir.path(), &SigningKey::from_bytes(&[2u8; 32]), 0);
    peer_journal.set_roster(&solo_roster(&key)).unwrap();
    assert_eq!(after.ops.len(), (KEPT + 1) as usize);
    let (compacted_chunks, compacted_pushed) = push_until_converged(peer, &journal).await;
    assert_eq!(
        compacted_pushed,
        (KEPT + 1) as usize,
        "the suffix fits and lands"
    );
    assert_eq!(
        compacted_chunks, 1,
        "and it lands in ONE chunk, against {sealed_chunks} before the delete"
    );

    // And the peer that has never seen this ring reads a COMPLETE journal:
    // the seal came with it, so `admit` counts holes from the floor.
    let admitted = peer_journal
        .admit(&solo_roster(&key), &Ed25519Verifier)
        .unwrap();
    assert!(
        admitted.is_complete(),
        "a compacted bootstrap is not a broken one: {:?}",
        admitted.gaps
    );
    assert_eq!(
        admitted.applied().count(),
        KEPT as usize,
        "the seal itself carries no payload"
    );
    assert_eq!(
        peer_journal.digest().unwrap(),
        journal.digest().unwrap(),
        "two nodes, one claim"
    );
}

// ── the rail listener, where the grant is the only way in ────

/// Drive the RAIL bind rather than the operator one.
async fn call_rail(state: AppState, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = commonwealth_api::server::client_router_for(
        state,
        commonwealth_api::server::ClientSurface::Rail,
    )
    .oneshot(req)
    .await
    .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// **Watched failing first, and it is the reason the rail is a separate bind.**
///
/// A ring app is a process on this machine, so it arrives on loopback — and on
/// the operator bind that alone admits it, before any bearer is read. Pointed
/// there, an app would arrive as an OPERATOR and its grant would be ignored:
/// the namespace scoping would be decorative, and a guard nobody can watch
/// fail is not a guard (§18.1). On the rail bind the token is the only way in.
#[tokio::test]
async fn on_the_rail_bind_a_loopback_caller_without_a_grant_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = with_guest(
        state_with_rail(dir.path(), &key),
        vec![Scope::Rails(NS.into())],
    );

    let (no_token, _) = call_rail(
        state.clone(),
        request("GET", "/v1/rail/log", LOOPBACK, None, None),
    )
    .await;
    assert_eq!(
        no_token,
        StatusCode::UNAUTHORIZED,
        "loopback alone must not admit on the rail bind"
    );

    // The same request WITH the grant is served, and served the right
    // namespace — which the caller never named.
    let (with_token, body) = call_rail(
        state,
        request("GET", "/v1/rail/log", LOOPBACK, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(with_token, StatusCode::OK, "{body}");
    assert_eq!(body["namespace"], NS);
}

/// The rail bind serves the rail and nothing else, proven from BOTH sides.
///
/// Two independent mechanisms refuse here and a test that only saw one could
/// be satisfied while the other silently broke:
///
/// - **A rail grant** gets 403 on everything outside `/v1/rail/*`, because
///   `Scope::paths()` is the allowlist and the auth layer checks it before
///   routing. That is what stops grants minting grants.
/// - **The daemon token** bypasses scope entirely — so a 404 for the same
///   paths is evidence about the ROUTER: those routes are not mounted on this
///   surface at all (§7.1). If they were, this arm would answer 200.
#[tokio::test]
async fn the_rail_bind_serves_nothing_but_the_rail() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let elsewhere = [
        "/internal/guest/grant",
        "/v1/models",
        "/v1/chat/completions",
        "/v1/mesh/status",
    ];
    for path in elsewhere {
        let state = with_guest(
            state_with_rail(dir.path(), &key),
            vec![Scope::Rails(NS.into())],
        );
        let (scoped, _) = call_rail(
            state.clone(),
            request("GET", path, LOOPBACK, Some(GUEST_TOKEN), None),
        )
        .await;
        assert_eq!(scoped, StatusCode::FORBIDDEN, "a rail grant reached {path}");

        // The same path under a credential that has no scope limit at all.
        let (unscoped, _) =
            call_rail(state, request("GET", path, LOOPBACK, Some(TOKEN), None)).await;
        assert_eq!(
            unscoped,
            StatusCode::NOT_FOUND,
            "{path} is MOUNTED on the rail surface — the route set, not the \
             grant, is what must exclude it"
        );
    }

    // `/status` is the documented odd one out and gets its own case.
    // `AUTH_EXEMPT_PATHS` lets it past the auth layer WITHOUT a scope check,
    // so it reaches the router — where the rail surface does not mount it. A
    // probe against a rail listener therefore sees 404, not 401, which reads
    // as "wrong node" rather than "wrong credential". That is a known cost of
    // the split, written down here so the next person to debug it does not
    // spend the afternoon on a credential that was never the problem.
    for bearer in [GUEST_TOKEN, TOKEN] {
        let state = with_guest(
            state_with_rail(dir.path(), &key),
            vec![Scope::Rails(NS.into())],
        );
        let (status, _) = call_rail(
            state,
            request("GET", "/status", LOOPBACK, Some(bearer), None),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "/status under {bearer}");
    }

    // And the rail itself is served on this bind, so the assertions above are
    // not passing because the whole router is empty.
    let state = with_guest(
        state_with_rail(dir.path(), &key),
        vec![Scope::Rails(NS.into())],
    );
    let (ok, _) = call_rail(
        state,
        request("GET", "/v1/rail/log", LOOPBACK, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(ok, StatusCode::OK);
}
