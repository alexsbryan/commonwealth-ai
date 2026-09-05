// SPDX-License-Identifier: AGPL-3.0-or-later
use commonwealth_core::ids::NodeId;
use commonwealth_rail::{Ed25519Verifier, Payload, Person, RingJournal, RingSigner};
use sovereign_core::mesh_measurements as mm;

use super::{publish, read, republish, to_payload};
use crate::ring_roster::tests::{key, member, mesh_of, pubkey_of};
use crate::ring_roster::MeshRoster;

/// The crate's ONE measurement fixture. `mesh_http`'s endpoint tests read the
/// same record this module journals, or neither proves anything about the
/// same thing (ARCH §10.6).
pub(crate) fn a_measurement(tok_s: f64, at: u64) -> mm::MeasurementRecord {
    let host = mm::HostIdentity::from_live_mesh(Some(0xf0f)).expect("a fingerprint is a host");
    mm::MeasurementRecord {
        key: mm::MeasurementKey::for_plan(
            host,
            "mf1:deadbeef".into(),
            "pd2:cafef00d".into(),
            32768,
            mm::LinkClass::Direct,
        ),
        decode_tok_s: tok_s,
        decode_tok_s_min: tok_s - 0.1,
        decode_tok_s_max: tok_s + 0.1,
        ttft_ms: 2203.0,
        itl_p50_ms: 90.0,
        itl_p95_ms: 98.0,
        prefill_tok_s: None,
        cold_load_s: None,
        trials: 3,
        content_frames: 256,
        model_name: "Qwen3.5-122B".into(),
        placement_human: "36 local + 12 @beefymac".into(),
        nodes: 2,
        hops: 1,
        measured_at: at,
        build: "0.10.0".into(),
        backend: Some("vulkan".into()),
        link_rtt_ms: None,
        verdict: mm::Verdict::Valid,
        witness: None,
        conditions: None,
    }
}

/// A journal in its own directory, with the namespace this module owns.
fn journal(dir: &std::path::Path) -> RingJournal {
    RingJournal::open(dir, mm::MEASUREMENTS_APP_ID).expect("the app id is a legal namespace")
}

/// **Why the payload wraps rather than embeds.** `Payload` refuses any
/// fractional number, and a `MeasurementRecord` is nine `f64`s. Without this
/// the wrapper looks like ceremony a later cleanup would remove; with it, the
/// removal fails here.
#[test]
fn a_measurement_record_cannot_be_a_rail_payload_directly() {
    let record = a_measurement(17.35, 1_700_000_000);
    let raw = serde_json::to_value(&record).expect("a record serializes");
    let err = Payload::new(raw).expect_err("a rate is a fraction and the rail refuses fractions");
    assert!(
        err.to_string()
            .contains("may not contain the fractional number"),
        "unexpected refusal: {err}"
    );
    // And the wrapper is accepted, carrying the record unchanged.
    let payload = to_payload(&record).expect("wrapped, it travels");
    assert_eq!(
        super::from_payload(&payload)
            .expect("it reads back")
            .decode_tok_s,
        17.35
    );
}

/// **The gate: A publishes, B reads it off the rail.**
///
/// Nothing here is a mock. `publish` is the endpoint's own writer, the
/// transfer is the rail's own anti-entropy primitives — the pair
/// `ring_sync::exchange` calls — and B admits under a roster derived from its
/// own membership view, which is the only kind of roster this namespace has.
#[test]
fn a_measurement_published_on_a_is_readable_on_b_through_the_rail() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (key_a, key_b) = (key(21), key(22));
    let (id_a, id_b) = (NodeId::from_u128(1), NodeId::from_u128(2));

    // Both nodes hold the same membership: the mesh is what the roster is.
    let mesh = mesh_of(vec![
        member(id_a, "halo", Some(pubkey_of(&key_a))),
        member(id_b, "beefy", Some(pubkey_of(&key_b))),
    ]);
    let on_a = MeshRoster::derive(&mesh, id_a, Some(pubkey_of(&key_a)));
    let on_b = MeshRoster::derive(&mesh, id_b, Some(pubkey_of(&key_b)));

    let a = journal(dir_a.path());
    let b = journal(dir_b.path());
    let record = a_measurement(17.35, 1_700_000_000);
    publish(&a, &key_a, on_a.roster(), &record).expect("A can author on its own journal");

    // One anti-entropy exchange, exactly as `ring_sync` performs it.
    let theirs = b.digest().unwrap();
    let ops = a.ops_missing_from(&theirs).unwrap();
    assert_eq!(ops.len(), 1, "A offers the one op B lacks");
    assert_eq!(b.ingest_all(&ops).unwrap(), 1);

    let seen = read(
        &b.admit(on_b.roster(), &Ed25519Verifier).unwrap(),
        Some(&RingSigner::actor(&key_b)),
    );
    assert_eq!(seen.gaps, 0, "a complete answer, not a subset");
    assert_eq!(seen.unreadable, 0);
    assert_eq!(seen.found.len(), 1, "B reads A's measurement off the rail");
    assert_eq!(seen.found[0].record.decode_tok_s, 17.35);
    assert_eq!(seen.found[0].person, Person::from("halo"));
    assert_eq!(seen.found[0].actor, RingSigner::actor(&key_a));
    assert_eq!(
        on_b.node_id_of(&seen.found[0].actor),
        Some(id_a),
        "the publisher is named from the roster, never from the payload"
    );

    // A's own view of the same journal excludes what A wrote: the local file
    // is authoritative, and echoing it back would show the operator their own
    // run wearing their own node name.
    let mine = read(
        &a.admit(on_a.roster(), &Ed25519Verifier).unwrap(),
        Some(&RingSigner::actor(&key_a)),
    );
    assert!(mine.found.is_empty());
}

/// **The `Option<NodePubkey>` hazard, and the decision.**
///
/// A member on a pre-identity build has `node_pubkey: None`, so no roster can
/// claim it and every op it signs is an `UnknownSigner` gap. In an
/// append-only journal that is normally permanent.
///
/// It is not permanent here, and this test is the reason the roster has no
/// writer. The roster is a PARAMETER of the read, derived from membership as
/// it is at that moment; nothing is dropped when a signer cannot be placed,
/// so the same journal admits the same ops the instant the key arrives —
/// under the same actor, because a node's signing key is `load_or_generate`
/// on disk and does not change when its advertisement does.
#[test]
fn an_op_from_an_unidentified_peer_is_a_gap_that_heals_when_its_key_arrives() {
    let dir = tempfile::tempdir().unwrap();
    let (me, peer) = (key(31), key(32));
    let (id_me, id_peer) = (NodeId::from_u128(1), NodeId::from_u128(2));

    // The peer wrote to its own journal and we ingested the op; on our side
    // its member row carries no key yet.
    let theirs = tempfile::tempdir().unwrap();
    let their_journal = journal(theirs.path());
    let their_roster = MeshRoster::derive(
        &mesh_of(vec![member(id_peer, "beefy", Some(pubkey_of(&peer)))]),
        id_peer,
        Some(pubkey_of(&peer)),
    );
    publish(
        &their_journal,
        &peer,
        their_roster.roster(),
        &a_measurement(11.08, 1_700_000_100),
    )
    .unwrap();

    let ours = journal(dir.path());
    let (ops, _) = their_journal.read().unwrap();
    assert_eq!(ours.ingest_all(&ops).unwrap(), 1, "the line is on our disk");

    let before = MeshRoster::derive(
        &mesh_of(vec![
            member(id_me, "halo", Some(pubkey_of(&me))),
            member(id_peer, "beefy", None),
        ]),
        id_me,
        Some(pubkey_of(&me)),
    );
    let admission = ours.admit(before.roster(), &Ed25519Verifier).unwrap();
    let seen = read(&admission, Some(&RingSigner::actor(&me)));
    assert!(
        seen.found.is_empty(),
        "an op nobody claims is never admitted — self-certifying is not membership"
    );
    assert_eq!(seen.gaps, 1, "and the refusal is REPORTED, not swallowed");
    assert!(
        matches!(
            admission.gaps.first(),
            Some(commonwealth_rail::RailGap::UnknownSigner { actor, .. })
                if *actor == RingSigner::actor(&peer)
        ),
        "the gap names the key it could not place: {:?}",
        admission.gaps
    );
    assert_eq!(
        admission.held, 1,
        "nothing was dropped — the line is still there"
    );

    // The peer upgrades and its next gossip round stamps its pubkey. Same
    // journal, same bytes, nothing re-sent.
    let after = MeshRoster::derive(
        &mesh_of(vec![
            member(id_me, "halo", Some(pubkey_of(&me))),
            member(id_peer, "beefy", Some(pubkey_of(&peer))),
        ]),
        id_me,
        Some(pubkey_of(&me)),
    );
    let healed = read(
        &ours.admit(after.roster(), &Ed25519Verifier).unwrap(),
        Some(&RingSigner::actor(&me)),
    );
    assert_eq!(
        healed.gaps, 0,
        "the gap heals — a derived roster is not a frozen one"
    );
    assert_eq!(healed.found.len(), 1);
    assert_eq!(healed.found[0].record.decode_tok_s, 11.08);
    assert_eq!(healed.found[0].person, Person::from("beefy"));
}

/// The other half of the same decision: a node that cannot place its OWN key
/// does not write ops the whole ring would report as unplaceable. It is
/// refused at the door, with a sentence, and nothing is lost — the run is
/// already in `mesh-measurements.json`, and `republish` carries it the moment
/// an identity exists.
#[test]
fn a_node_that_cannot_place_its_own_key_refuses_to_publish() {
    let dir = tempfile::tempdir().unwrap();
    let me = key(41);
    let id = NodeId::from_u128(1);
    // Our own row has no stamp AND nothing was installed — a daemon with no
    // identity at all, which is what a pre-identity build looks like.
    let roster = MeshRoster::derive(&mesh_of(vec![member(id, "halo", None)]), id, None);
    assert!(roster.is_empty());

    let err = publish(
        &journal(dir.path()),
        &me,
        roster.roster(),
        &a_measurement(9.0, 1),
    )
    .expect_err("authoring under an unclaimed key is refused");
    assert!(
        err.contains("nobody in the") && err.contains("roster claims that key"),
        "the refusal must be a sentence a person can act on: {err}"
    );
    let (ops, _) = journal(dir.path()).read().unwrap();
    assert!(ops.is_empty(), "and nothing was written");
}

/// `to_wire` refuses an invalid run, and the rail inherits that refusal
/// rather than restating it. A failed run is glassbox material for the
/// operator who caused it and noise to everyone else.
#[test]
fn an_invalid_run_never_reaches_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let me = key(51);
    let id = NodeId::from_u128(1);
    let roster = MeshRoster::derive(
        &mesh_of(vec![member(id, "halo", Some(pubkey_of(&me)))]),
        id,
        Some(pubkey_of(&me)),
    );
    let mut bad = a_measurement(0.0, 1_700_000_000);
    bad.verdict = mm::Verdict::Invalid {
        problems: vec!["no content frames".into()],
    };
    let err = publish(&journal(dir.path()), &me, roster.roster(), &bad).expect_err("refused");
    assert_eq!(err, "an invalid run does not travel");
    assert!(journal(dir.path()).read().unwrap().0.is_empty());
}

/// The boot reconcile appends what the journal lacks and, run again, appends
/// nothing. Idempotence is by CONTENT (`wire_key`) and not by op id: an op id
/// folds in `seq` and a timestamp, so an id-keyed check would mint a fresh
/// line on every start and the journal would grow without bound.
#[test]
fn a_republish_appends_once_and_never_again() {
    let dir = tempfile::tempdir().unwrap();
    let me = key(61);
    let id = NodeId::from_u128(1);
    let roster = MeshRoster::derive(
        &mesh_of(vec![member(id, "halo", Some(pubkey_of(&me)))]),
        id,
        Some(pubkey_of(&me)),
    );
    let mut invalid = a_measurement(1.0, 1_700_000_300);
    invalid.verdict = mm::Verdict::Invalid {
        problems: vec!["no content frames".into()],
    };
    let local = vec![
        a_measurement(17.35, 1_700_000_000),
        a_measurement(11.08, 1_700_000_100),
        invalid,
    ];

    let j = journal(dir.path());
    let first = republish(&j, &me, roster.roster(), &local);
    assert_eq!(first.appended, 2);
    assert_eq!(first.withheld, 1, "the invalid run stays home");
    assert_eq!(first.already_held, 0);

    let second = republish(&j, &me, roster.roster(), &local);
    assert_eq!(second.appended, 0, "a second boot appends nothing");
    assert_eq!(second.already_held, 2);
    assert_eq!(j.read().unwrap().0.len(), 2, "and the journal did not grow");
}

/// A reader scanning a list wants the run they just took at the top. The
/// journal's own order is `(ts_unix, actor, id)` — the total order every node
/// agrees on, which is a delivery property and not a reading one — so the
/// recency sort is this module's and is pinned here.
#[test]
fn measurements_are_returned_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let me = key(71);
    let id = NodeId::from_u128(1);
    let roster = MeshRoster::derive(
        &mesh_of(vec![member(id, "halo", Some(pubkey_of(&me)))]),
        id,
        Some(pubkey_of(&me)),
    );
    let j = journal(dir.path());
    for at in [1_700_000_100u64, 1_700_000_900, 1_700_000_500] {
        publish(&j, &me, roster.roster(), &a_measurement(7.0, at)).unwrap();
    }
    // `None` excludes nothing — the diagnostic path, and the only way to see
    // what this node has actually put on the ring.
    let seen = read(&j.admit(roster.roster(), &Ed25519Verifier).unwrap(), None);
    let times: Vec<u64> = seen.found.iter().map(|m| m.record.measured_at).collect();
    assert_eq!(times, vec![1_700_000_900, 1_700_000_500, 1_700_000_100]);
    assert_eq!(seen.found[0].actor, RingSigner::actor(&me));
}

/// An admitted line this build cannot read as a measurement — a peer on a
/// newer schema, or a second act somebody adds to this namespace later — is
/// COUNTED. "Nobody has measured this" and "somebody has, in a dialect we do
/// not speak" send an operator to different places (ARCH §18.3).
#[test]
fn an_admitted_line_this_build_cannot_read_is_counted_not_swallowed() {
    let dir = tempfile::tempdir().unwrap();
    let me = key(81);
    let id = NodeId::from_u128(1);
    let roster = MeshRoster::derive(
        &mesh_of(vec![member(id, "halo", Some(pubkey_of(&me)))]),
        id,
        Some(pubkey_of(&me)),
    );
    let j = journal(dir.path());
    publish(
        &j,
        &me,
        roster.roster(),
        &a_measurement(11.08, 1_700_000_000),
    )
    .unwrap();
    // A well-formed rail act that is not one of ours.
    j.append(
        commonwealth_rail::RailAct::Record {
            payload: Payload::new(serde_json::json!({ "kind": "something-else" })).unwrap(),
        },
        &me,
        roster.roster(),
    )
    .unwrap();

    let seen = read(&j.admit(roster.roster(), &Ed25519Verifier).unwrap(), None);
    assert_eq!(seen.found.len(), 1);
    assert_eq!(seen.unreadable, 1);
    assert_eq!(
        seen.gaps, 0,
        "an act we cannot read is not a gap — it arrived"
    );
}

/// **A journal forgets nothing, and the file forgets on purpose.** The local
/// file keeps `MAX_RUNS_PER_KEY` runs per configuration so variance stays
/// visible without unbounded growth; the rail has no such cap. Applying the
/// publisher's own depth on the read side is what stops a reader seeing more
/// history than `mesh bench --history` shows the person who took it — and
/// stops that difference widening every time somebody re-benches.
///
/// Per CONFIGURATION, not per publisher: a second config measured once is
/// still there.
#[test]
fn a_publishers_history_is_capped_at_the_depth_their_own_file_keeps() {
    let dir = tempfile::tempdir().unwrap();
    let me = key(91);
    let id = NodeId::from_u128(1);
    let roster = MeshRoster::derive(
        &mesh_of(vec![member(id, "halo", Some(pubkey_of(&me)))]),
        id,
        Some(pubkey_of(&me)),
    );
    let j = journal(dir.path());

    let over = mm::MAX_RUNS_PER_KEY + 3;
    for i in 0..over {
        publish(
            &j,
            &me,
            roster.roster(),
            &a_measurement(10.0, 1_700_000_000 + i as u64),
        )
        .unwrap();
    }
    // A different configuration — same machine, a different context length.
    let mut other = a_measurement(10.0, 1_700_000_000);
    other.key.n_ctx = 8192;
    publish(&j, &me, roster.roster(), &other).unwrap();

    let seen = read(&j.admit(roster.roster(), &Ed25519Verifier).unwrap(), None);
    assert_eq!(
        j.read().unwrap().0.len(),
        over + 1,
        "the journal kept every line"
    );
    assert_eq!(
        seen.found.len(),
        mm::MAX_RUNS_PER_KEY + 1,
        "eight of the repeated configuration, plus the one run of the other"
    );
    assert_eq!(
        seen.unreadable, 0,
        "retention is not an unreadable line — the answer is complete"
    );
    let newest: Vec<u64> = seen
        .found
        .iter()
        .filter(|m| m.record.key.n_ctx == 32768)
        .map(|m| m.record.measured_at)
        .collect();
    assert_eq!(newest.len(), mm::MAX_RUNS_PER_KEY);
    assert_eq!(
        newest[0],
        1_700_000_000 + (over - 1) as u64,
        "the runs kept are the NEWEST, as the file's FIFO keeps them"
    );
}
