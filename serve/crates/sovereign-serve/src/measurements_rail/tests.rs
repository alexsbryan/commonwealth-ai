// SPDX-License-Identifier: AGPL-3.0-or-later
//! The read side, over ops signed and admitted with rail-core's own fixtures:
//! what `read` makes of an admission is pure, so it is pinned without a
//! journal. The writers dial cw-rails and are pinned against a real one in
//! `tests/placement_rail_e2e.rs`.

use std::collections::BTreeMap;

use crate::mesh_measurements as mm;
use commonwealth_rail_core::tests_support::{key, signed_in};
use commonwealth_rail_core::{
    actor_of, admit, Admission, Ed25519Verifier, Payload, Person, RailAct, RailGap, Roster,
    SigningKey,
};

use super::{from_payload, read, to_payload};

/// The crate's ONE measurement fixture.
pub fn a_measurement(tok_s: f64, at: u64) -> mm::MeasurementRecord {
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

/// A roster naming each `(person, key)`.
fn roster(members: &[(&str, &SigningKey)]) -> Roster {
    let mut m = BTreeMap::new();
    for (name, k) in members {
        m.insert(Person::from(*name), vec![actor_of(k)]);
    }
    Roster::new(m)
}

/// `k`'s journal lines, one per record in order, as its node signs them.
fn lines(
    k: &SigningKey,
    records: &[mm::MeasurementRecord],
) -> Vec<commonwealth_rail_core::Op<commonwealth_rail_core::SignedOp>> {
    records
        .iter()
        .enumerate()
        .map(|(i, r)| {
            signed_in(
                mm::MEASUREMENTS_APP_ID,
                k,
                1_700_000_000 + i as i64,
                i as u64,
                RailAct::Record {
                    payload: to_payload(r).expect("a valid run travels"),
                },
            )
        })
        .collect()
}

fn admitted(
    ops: &[commonwealth_rail_core::Op<commonwealth_rail_core::SignedOp>],
    roster: &Roster,
) -> Admission {
    admit(ops, &[], roster, mm::MEASUREMENTS_APP_ID, &Ed25519Verifier)
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
        from_payload(&payload).expect("it reads back").decode_tok_s,
        17.35
    );
}

/// A peer's line is read, and its publisher is named by the roster the line
/// was admitted under — never by the payload. The reader's own lines are
/// dropped: the local file is authoritative, and echoing it back would show
/// the operator their own run wearing their own node name.
#[test]
fn a_peers_line_is_read_and_named_by_the_roster() {
    let (a, b) = (key(21), key(22));
    let ring = roster(&[("halo", &a), ("beefy", &b)]);
    let admission = admitted(&lines(&a, &[a_measurement(17.35, 1_700_000_000)]), &ring);

    let seen = read(&admission, Some(&actor_of(&b)));
    assert_eq!(seen.gaps, 0, "a complete answer, not a subset");
    assert_eq!(seen.unreadable, 0);
    assert_eq!(seen.found.len(), 1, "B reads A's measurement");
    assert_eq!(seen.found[0].record.decode_tok_s, 17.35);
    assert_eq!(seen.found[0].person, Person::from("halo"));
    assert_eq!(seen.found[0].actor, actor_of(&a));

    assert!(read(&admission, Some(&actor_of(&a))).found.is_empty());
}

/// **A derived roster is not a frozen one.** A line whose signer no roster
/// claims is an `UnknownSigner` gap — reported, never swallowed, and never
/// dropped — and the same line is admitted the moment the roster names it.
#[test]
fn an_op_from_an_unidentified_peer_is_a_gap_that_heals_when_its_key_arrives() {
    let (me, peer) = (key(31), key(32));
    let ops = lines(&peer, &[a_measurement(11.08, 1_700_000_100)]);

    let before = admitted(&ops, &roster(&[("halo", &me)]));
    let seen = read(&before, Some(&actor_of(&me)));
    assert!(
        seen.found.is_empty(),
        "an op nobody claims is never admitted — self-certifying is not membership"
    );
    assert_eq!(seen.gaps, 1, "and the refusal is REPORTED, not swallowed");
    assert!(
        matches!(
            before.gaps.first(),
            Some(RailGap::UnknownSigner { actor, .. }) if *actor == actor_of(&peer)
        ),
        "the gap names the key it could not place: {:?}",
        before.gaps
    );
    assert_eq!(
        before.held, 1,
        "nothing was dropped — the line is still there"
    );

    let healed = read(
        &admitted(&ops, &roster(&[("halo", &me), ("beefy", &peer)])),
        Some(&actor_of(&me)),
    );
    assert_eq!(healed.gaps, 0, "the gap heals");
    assert_eq!(healed.found.len(), 1);
    assert_eq!(healed.found[0].record.decode_tok_s, 11.08);
    assert_eq!(healed.found[0].person, Person::from("beefy"));
}

/// A reader scanning a list wants the run they just took at the top. The
/// journal's own order is `(ts_unix, actor, id)` — the total order every node
/// agrees on, which is a delivery property and not a reading one — so the
/// recency sort is this module's and is pinned here.
#[test]
fn measurements_are_returned_newest_first() {
    let me = key(71);
    let records: Vec<_> = [1_700_000_100u64, 1_700_000_900, 1_700_000_500]
        .into_iter()
        .map(|at| a_measurement(7.0, at))
        .collect();
    // `None` excludes nothing — the diagnostic path.
    let seen = read(
        &admitted(&lines(&me, &records), &roster(&[("halo", &me)])),
        None,
    );
    let times: Vec<u64> = seen.found.iter().map(|m| m.record.measured_at).collect();
    assert_eq!(times, vec![1_700_000_900, 1_700_000_500, 1_700_000_100]);
    assert_eq!(seen.found[0].actor, actor_of(&me));
}

/// An admitted line this build cannot read as a measurement — a peer on a
/// newer schema, or a second act somebody adds to this namespace later — is
/// COUNTED. "Nobody has measured this" and "somebody has, in a dialect we do
/// not speak" send an operator to different places (ARCH §18.3).
#[test]
fn an_admitted_line_this_build_cannot_read_is_counted_not_swallowed() {
    let me = key(81);
    let mut ops = lines(&me, &[a_measurement(11.08, 1_700_000_000)]);
    ops.push(signed_in(
        mm::MEASUREMENTS_APP_ID,
        &me,
        1_700_000_050,
        1,
        RailAct::Record {
            payload: Payload::new(serde_json::json!({ "kind": "something-else" })).unwrap(),
        },
    ));
    let seen = read(&admitted(&ops, &roster(&[("halo", &me)])), None);
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
/// history than `mesh bench --history` shows the person who took it.
///
/// Per CONFIGURATION, not per publisher: a second config measured once is
/// still there.
#[test]
fn a_publishers_history_is_capped_at_the_depth_their_own_file_keeps() {
    let me = key(91);
    let over = mm::MAX_RUNS_PER_KEY + 3;
    let mut records: Vec<_> = (0..over)
        .map(|i| a_measurement(10.0, 1_700_000_000 + i as u64))
        .collect();
    // A different configuration — same machine, a different context length.
    let mut other = a_measurement(10.0, 1_700_000_000);
    other.key.n_ctx = 8192;
    records.push(other);

    let admission = admitted(&lines(&me, &records), &roster(&[("halo", &me)]));
    assert_eq!(admission.ops.len(), over + 1, "the journal kept every line");
    let seen = read(&admission, None);
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
