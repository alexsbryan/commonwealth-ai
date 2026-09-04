// SPDX-License-Identifier: AGPL-3.0-or-later
//! Convergence correctness, settled offline.
//!
//! Each of these is one of the failure modes the design was reviewed against.
//! The names say which interleaving loses an act, because a test called
//! `test_admit_2` teaches the next reader nothing about why the rule exists.
//!
//! **Nothing here knows what an expense is.** These tests used to be about
//! money, and half of them were really about delivery wearing a money
//! costume. The money half now lives in the reference app, in JS, where an
//! app author can read it. What is left is the part every ring app on this
//! rail inherits whether it wants to or not.

use crate::*;

use crate::tests_support::*;

// ── the property the whole design exists to have ─────────────

/// Every permutation of the same ops must produce the identical admission —
/// the act order AND the gap report. Nineteen laptops gossip in nineteen
/// orders; if this fails, two housemates read different answers off the same
/// journal and the plan says ship nothing.
///
/// Exhaustive over 6 ops (720 orderings) rather than randomised, so a failure
/// is reproducible and the coverage is a fact rather than a sample.
///
/// This used to be asserted over balances, which made the strongest property
/// in the system contingent on one tenant's arithmetic. Asserting it over the
/// admission is both stronger and true for every app that will ever sit on
/// this rail.
#[test]
fn the_admission_is_a_function_of_the_op_set_not_of_arrival_order() {
    let groceries = signed(&key(1), 100, 0, record("groceries"));
    let beer = signed(&key(2), 101, 0, record("beer"));
    let paid_back = signed(&key(3), 102, 0, record("paid-back"));
    // A correction whose timestamp is EARLIER than the op it corrects — a
    // clock-skew case that a positional walk would get wrong.
    let fix = signed(
        &key(1),
        99,
        1,
        RailAct::Correct {
            corrects: groceries.id.clone(),
            replacement: Some(payload("groceries-corrected")),
        },
    );
    // A correction pointing at an op nobody in this set holds.
    let dangling = signed(
        &key(2),
        103,
        1,
        RailAct::Correct {
            corrects: OpId::from_raw("ring-deadbeefdeadbeef"),
            replacement: None,
        },
    );
    // An op from a key the roster does not know.
    let stranger = signed(&key(9), 104, 0, record("stranger"));

    let ops = vec![groceries, beer, paid_back, fix, dangling, stranger];
    let expected = admitted(&ops);
    assert!(!expected.ops.is_empty());
    assert!(
        !expected.gaps.is_empty(),
        "this fixture must exercise gaps too"
    );

    let mut seen = 0usize;
    for order in permutations(ops.len()) {
        let shuffled: Vec<_> = order.iter().map(|i| ops[*i].clone()).collect();
        assert_eq!(admitted(&shuffled), expected, "order {order:?} disagreed");
        seen += 1;
    }
    assert_eq!(seen, 720, "6! orderings");
}

fn permutations(n: usize) -> Vec<Vec<usize>> {
    fn go(current: &mut Vec<usize>, rest: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if rest.is_empty() {
            out.push(current.clone());
            return;
        }
        for i in 0..rest.len() {
            let taken = rest.remove(i);
            current.push(taken);
            go(current, rest, out);
            current.pop();
            rest.insert(i, taken);
        }
    }
    let mut out = Vec::new();
    go(&mut Vec::new(), &mut (0..n).collect(), &mut out);
    out
}

/// Two byte-identical acts in the same second by the same actor must stay two
/// acts. [`Op::new`] hashes `(prefix, ts, actor, body)` and documents the
/// collision as by-design; `seq` is in the body, which is what separates them.
#[test]
fn two_identical_acts_in_the_same_second_are_two_acts() {
    let a = signed(&key(1), 100, 0, record("coffee"));
    let b = signed(&key(1), 100, 1, record("coffee"));
    assert_ne!(a.id, b.id, "seq must separate them");
    let f = admitted(&[a, b]);
    assert_eq!(applied(&f), vec!["coffee", "coffee"]);
    assert!(f.is_complete(), "{:?}", f.gaps);
}

// ── correction ───────────────────────────────────────────────

/// **The reason the void set is built from every correction at once.** A
/// correction that arrives before its target must still void it, or two nodes
/// disagree purely on delivery order.
#[test]
fn a_correction_that_arrives_first_still_voids_its_target() {
    let target = signed(&key(1), 100, 0, record("wrong"));
    let fix = signed(
        &key(1),
        101,
        1,
        RailAct::Correct {
            corrects: target.id.clone(),
            replacement: Some(payload("right")),
        },
    );
    let fix_first = admitted(&[fix.clone(), target.clone()]);
    let target_first = admitted(&[target, fix]);
    assert_eq!(fix_first, target_first);
    assert_eq!(applied(&fix_first), vec!["right"]);
}

/// A correction naming an op this node does not hold is not fatal — the void
/// is recorded and applies the moment the target arrives — but it means we
/// are missing an op, and that gets said.
#[test]
fn a_correction_pointing_at_an_op_we_do_not_hold_is_reported() {
    let orphan = signed(
        &key(1),
        100,
        0,
        RailAct::Correct {
            corrects: OpId::from_raw("ring-deadbeefdeadbeef"),
            replacement: None,
        },
    );
    let f = admitted(&[orphan.clone()]);
    assert_eq!(
        f.gaps,
        vec![RailGap::DanglingCorrection {
            by: orphan.id.clone(),
            missing: OpId::from_raw("ring-deadbeefdeadbeef"),
        }]
    );
    assert!(applied(&f).is_empty());

    // And when the target does arrive, the void applies with no repair step.
    let target = signed(&key(2), 99, 0, record("wrong"));
    let fix = signed(
        &key(1),
        100,
        0,
        RailAct::Correct {
            corrects: target.id.clone(),
            replacement: None,
        },
    );
    let healed = admitted(&[fix, target]);
    assert!(healed.is_complete(), "{:?}", healed.gaps);
    assert!(applied(&healed).is_empty(), "the target is voided");
}

/// The total order is `(ts_unix, actor, id)`, but the void set does not
/// consult it at all — so a correction stamped in the past still corrects.
#[test]
fn a_correction_stamped_in_the_past_still_corrects() {
    let target = signed(&key(1), 500, 0, record("wrong"));
    let fix = signed(
        &key(1),
        1,
        1,
        RailAct::Correct {
            corrects: target.id.clone(),
            replacement: Some(payload("right")),
        },
    );
    let f = admitted(&[target, fix]);
    assert_eq!(applied(&f), vec!["right"]);
}

/// **A correction never resurrects.** Correcting a correction cancels ITS
/// replacement and leaves the original voided; to bring an act back, write it
/// again. That is what keeps the void set one commutative scan instead of a
/// liveness walk whose answer depends on arrival order.
#[test]
fn correcting_a_correction_does_not_resurrect_the_original() {
    let original = signed(&key(1), 100, 0, record("original"));
    let first = signed(
        &key(1),
        101,
        1,
        RailAct::Correct {
            corrects: original.id.clone(),
            replacement: Some(payload("replacement")),
        },
    );
    let second = signed(
        &key(1),
        102,
        2,
        RailAct::Correct {
            corrects: first.id.clone(),
            replacement: None,
        },
    );
    let f = admitted(&[original, first, second]);
    assert!(
        applied(&f).is_empty(),
        "neither the original nor its replacement comes back: {:?}",
        applied(&f)
    );
    assert!(f.is_complete(), "{:?}", f.gaps);
}

/// A voided op stays in the list, marked, so an app can render what changed —
/// and `applied()` is the one definition of which ops a reducer sees.
#[test]
fn a_voided_op_is_still_visible_but_is_never_applied() {
    let target = signed(&key(1), 100, 0, record("wrong"));
    let fix = signed(
        &key(1),
        101,
        1,
        RailAct::Correct {
            corrects: target.id.clone(),
            replacement: Some(payload("right")),
        },
    );
    let f = admitted(&[target.clone(), fix.clone()]);
    assert_eq!(f.ops.len(), 2, "history is kept");
    let voided = f.ops.iter().find(|o| o.id == target.id).unwrap();
    assert!(voided.voided);
    assert!(!voided.applies());
    assert!(
        voided.payload.is_some(),
        "the app can still show what was corrected away"
    );
    let correction = f.ops.iter().find(|o| o.id == fix.id).unwrap();
    assert_eq!(correction.corrects.as_ref(), Some(&target.id));
    assert_eq!(applied(&f), vec!["right"]);
}

// ── lines this build cannot read ─────────────────────────────

/// **An un-upgraded node must say its answer covers a subset.** A
/// newer-format line it cannot read becomes a gap; admitting the rest and
/// reporting it bare is the §18.3 failure.
#[test]
fn a_line_from_a_newer_build_is_a_gap_not_an_invisible_omission() {
    let ops = vec![signed(&key(1), 100, 0, record("readable"))];
    let skipped = vec![
        SkippedLine::NewerVersion { line: 4, v: 2 },
        SkippedLine::Malformed {
            line: 7,
            error: "expected value".into(),
        },
    ];
    let f = admit(&ops, &skipped, &ring(), NS, &Ed25519Verifier);
    assert_eq!(
        applied(&f),
        vec!["readable"],
        "what it CAN read is still right"
    );
    assert!(f
        .gaps
        .contains(&RailGap::NewerVersionLine { line: 4, v: 2 }));
    assert!(f
        .gaps
        .iter()
        .any(|g| matches!(g, RailGap::MalformedLine { line: 7, .. })));
    assert!(!f.is_complete());
}

// ── admission ────────────────────────────────────────────────

/// The op admission checks is the one nobody else could have written. Flip a
/// byte of the body after signing and the signature is what catches it — not
/// `actor`, which the writer supplies.
#[test]
fn a_tampered_body_fails_its_signature_and_is_not_admitted() {
    let mut op = signed(&key(1), 100, 0, record("small"));
    op.kind.act = record("enormous");
    let f = admitted(&[op]);
    assert!(f.ops.is_empty(), "nothing admitted");
    assert!(
        f.gaps
            .iter()
            .any(|g| matches!(g, RailGap::BadSignature { .. })),
        "{:?}",
        f.gaps
    );
    // Rewriting the body also breaks the content-derived id, so both facts
    // are reported. Neither is enough on its own: the id mismatch says the
    // line was edited, the signature says it was not edited by its author.
    assert!(
        f.gaps
            .iter()
            .any(|g| matches!(g, RailGap::TamperedId { .. })),
        "{:?}",
        f.gaps
    );
}

/// Signing under your own fresh key proves only that you hold it. The roster
/// is what makes a signature mean membership.
#[test]
fn a_perfectly_signed_op_from_a_stranger_is_refused() {
    let op = signed(&key(42), 100, 0, record("x"));
    let f = admitted(&[op]);
    assert!(f.ops.is_empty());
    assert!(matches!(f.gaps.as_slice(), [RailGap::UnknownSigner { .. }]));
}

/// A signed op lifted out of one namespace and replayed into another must
/// fail. The namespace is bound into the signed message, so this is caught by
/// the signature rather than by a check somewhere downstream.
#[test]
fn an_op_signed_for_another_namespace_cannot_be_replayed_here() {
    let op = signed_in("tool-lending", &key(1), 100, 0, record("x"));
    let f = admitted(&[op]);
    assert!(f.ops.is_empty());
    assert!(matches!(f.gaps.as_slice(), [RailGap::BadSignature { .. }]));
}

/// The id on the line is advisory; admission uses the one the content
/// derives. A rewritten id therefore cannot make an op impersonate another
/// op's correction target — and it is still reported, because a peer doing it
/// is worth seeing.
#[test]
fn a_rewritten_id_changes_no_outcome_and_is_still_reported() {
    let mut op = signed(&key(1), 100, 0, record("x"));
    let real = op.id.clone();
    op.id = OpId::from_raw("ring-ffffffffffffffff");
    let f = admitted(&[op]);
    assert_eq!(applied(&f), vec!["x"], "the op still counts");
    assert_eq!(f.ops[0].id, real, "under the id its content derives");
    assert!(f.gaps.contains(&RailGap::TamperedId {
        claimed: OpId::from_raw("ring-ffffffffffffffff"),
        derived: real,
    }));
}

/// **The one thing that distinguishes "nothing happened" from "it never
/// reached me."** Alex wrote three ops and we hold two.
#[test]
fn a_hole_in_an_actors_sequence_is_reported() {
    let ops = vec![
        signed(&key(1), 100, 0, record("a")),
        signed(&key(1), 102, 2, record("c")),
    ];
    let f = admitted(&ops);
    assert_eq!(
        f.gaps,
        vec![RailGap::SequenceHole {
            actor: actor_of(&key(1)),
            missing: 1
        }]
    );
    assert_eq!(
        applied(&f),
        vec!["a", "c"],
        "what we hold is still admitted"
    );
}

/// One actor, one sequence number, two different ops: either equivocation or
/// a counter lost across a restart. Both are excluded — choosing one would be
/// inventing an answer.
#[test]
fn one_sequence_number_used_twice_excludes_both_ops() {
    let a = signed(&key(1), 100, 0, record("a"));
    let b = signed(&key(1), 101, 0, record("b"));
    let f = admitted(&[a.clone(), b.clone()]);
    assert!(f.ops.is_empty(), "{:?}", f.ops);
    let mut ids = vec![a.id, b.id];
    ids.sort();
    assert_eq!(
        f.gaps,
        vec![RailGap::SequenceFork {
            actor: actor_of(&key(1)),
            seq: 0,
            ids
        }]
    );
}

/// Gossip delivers the same op many times. That is the normal case, not an
/// anomaly, and it must not double-count.
#[test]
fn the_same_op_delivered_three_times_is_admitted_once() {
    let op = signed(&key(1), 100, 0, record("x"));
    let f = admitted(&[op.clone(), op.clone(), op]);
    assert_eq!(applied(&f), vec!["x"]);
    assert_eq!(f.held, 3, "and it says how many lines it read");
    assert!(f.is_complete(), "{:?}", f.gaps);
}

// ── what the rail refuses to know ────────────────────────────

/// The rail carries an act it cannot read. An app payload that is nonsense to
/// its own app is not the rail's business — the whole point of the cut is
/// that this file has no opinion about it.
#[test]
fn an_act_the_rail_cannot_interpret_is_still_admitted() {
    let weird = Payload::new(serde_json::json!({
        "kind": "something-this-build-has-never-heard-of",
        "nested": { "z": 1, "a": [1, 2, 3] },
    }))
    .unwrap();
    let op = signed(
        &key(1),
        100,
        0,
        RailAct::Record {
            payload: weird.clone(),
        },
    );
    let f = admitted(&[op]);
    assert!(f.is_complete(), "{:?}", f.gaps);
    assert_eq!(f.ops[0].payload.as_ref(), Some(&weird));
    assert_eq!(f.ops[0].person, p("alex"), "and it says who wrote it");
}

/// **The canonical-payload property, end to end.** Two spellings of one act
/// sign the same bytes and derive the same id, so a client library that
/// serialises its keys in a different order does not fork the ring.
#[test]
fn payload_key_order_does_not_change_the_id_or_the_signature() {
    let one: Payload = serde_json::from_str(r#"{"b":1,"a":2}"#).unwrap();
    let other: Payload = serde_json::from_str(r#"{"a":2,"b":1}"#).unwrap();
    let a = signed(&key(1), 100, 0, RailAct::Record { payload: one });
    let b = signed(&key(1), 100, 0, RailAct::Record { payload: other });
    assert_eq!(a.id, b.id);
    assert_eq!(a.kind.sig, b.kind.sig);
    assert!(admitted(&[a]).is_complete());
}

/// **A refusal reaching a housemate must be a sentence.** The append door
/// returned a raw serde dump at a person before the gap renderer existed, and
/// making the payload opaque puts a second serializer between the app and the
/// refusal — so the property gets a test rather than a promise.
#[test]
fn a_refused_act_comes_back_as_a_sentence_not_a_serde_dump() {
    let float = serde_json::json!({ "op": "record", "payload": { "amount": 3.5 } });
    let Err(RailError::Rejected(why)) = RailAct::from_json(float) else {
        panic!("a fractional payload was accepted");
    };
    assert!(why.contains("whole number"), "{why}");
    assert!(!why.contains('{'), "rendered as a dump: {why}");
    assert!(!why.contains("line 0"), "carries a phantom position: {why}");

    let scalar = serde_json::json!({ "op": "record", "payload": 42 });
    let Err(RailError::Rejected(why)) = RailAct::from_json(scalar) else {
        panic!("a bare scalar payload was accepted");
    };
    assert!(why.contains("JSON object"), "{why}");

    // And a well-formed act reads back through the same door.
    let good = serde_json::json!({ "op": "record", "payload": { "kind": "thing" } });
    assert!(RailAct::from_json(good).is_ok());
}

/// Every gap the rail can emit renders as a sentence. An unhandled variant
/// reaching a housemate as a serde dump is what this renderer exists to stop —
/// and the append door returned exactly that until it was written.
#[test]
fn every_gap_renders_as_a_sentence() {
    let id = OpId::from_raw("ring-0123456789abcdef");
    let actor = actor_of(&key(1));
    let all = vec![
        RailGap::MalformedLine {
            line: 3,
            error: "x".into(),
        },
        RailGap::NewerVersionLine { line: 4, v: 2 },
        RailGap::BadSignature {
            id: id.clone(),
            actor: actor.clone(),
        },
        RailGap::UnknownSigner {
            id: id.clone(),
            actor: actor.clone(),
        },
        RailGap::TamperedId {
            claimed: id.clone(),
            derived: id.clone(),
        },
        RailGap::SequenceHole {
            actor: actor.clone(),
            missing: 2,
        },
        RailGap::SequenceFork {
            actor,
            seq: 1,
            ids: vec![id.clone()],
        },
        RailGap::DanglingCorrection {
            by: id.clone(),
            missing: id,
        },
    ];
    for gap in &all {
        let sentence = gap.to_string();
        assert!(
            !sentence.contains('{'),
            "{gap:?} rendered as a dump: {sentence}"
        );
        assert!(
            sentence.len() > 15,
            "{gap:?} rendered as a fragment: {sentence}"
        );
        // Round-trips, because the CLI reads these back off the wire and
        // renders them through this same impl.
        let json = serde_json::to_value(gap).unwrap();
        assert_eq!(&serde_json::from_value::<RailGap>(json).unwrap(), gap);
    }
}

// ── the verifier seam ────────────────────────────────────────

/// A verifier whose answer is fixed, so what admission DOES with an answer can
/// be pinned without a keypair standing in the way. One stub for both answers:
/// two would be two spellings of the same nothing.
struct Says(bool);

impl RingVerifier for Says {
    fn name(&self) -> &'static str {
        "test-fixed-answer"
    }
    fn verify(&self, _: &str, _: &str, _: i64, _: u64, _: &str, _: &str) -> bool {
        self.0
    }
}

/// **The seam is real or it is decoration.** Ops the shipped verifier admits
/// must become gaps under a verifier that refuses them — which is false if
/// `admit` asks [`verify_ring_op`](crate::sig) directly and takes the
/// parameter for show. The first assertion is the negative control: without it
/// the second passes on a fixture that was never admissible.
#[test]
fn admission_asks_the_verifier_it_was_handed_and_not_a_hardcoded_one() {
    let ops = [
        signed(&key(1), 100, 0, record("a")),
        signed(&key(2), 101, 0, record("b")),
    ];
    assert_eq!(
        admitted(&ops).ops.len(),
        2,
        "control: both ops are admissible under the shipped verifier"
    );

    let f = admit(&ops, &[], &ring(), NS, &Says(false));
    assert!(f.ops.is_empty(), "a refused signature is never an act");
    assert!(
        f.gaps
            .iter()
            .all(|g| matches!(g, RailGap::BadSignature { .. })),
        "{:?}",
        f.gaps
    );
}

/// **A refusal is a refusal, never an absence (ARCH §18.3).** An op the rail
/// cannot authenticate must be counted in `held`, reported as a gap, and make
/// the answer say it covers a subset. Dropping it quietly is how an app states
/// a wrong total with complete confidence.
#[test]
fn a_signature_the_verifier_refuses_is_a_gap_and_never_a_silent_drop() {
    let f = admit(
        &[signed(&key(1), 100, 0, record("a"))],
        &[],
        &ring(),
        NS,
        &Says(false),
    );
    assert!(!f.is_complete(), "an unverifiable op makes this a subset");
    assert_eq!(f.held, 1, "the op is held and accounted for, not forgotten");
    assert!(matches!(f.gaps.as_slice(), [RailGap::BadSignature { .. }]));
}

/// **A verifier is not a roster.** The seam decides whether a key signed these
/// bytes; the roster decides whether the ring claims that key. Swapping in a
/// verifier that accepts everything must still leave a stranger out, or the
/// seam has handed membership to whoever installs a verifier.
#[test]
fn a_permissive_verifier_still_cannot_admit_a_stranger() {
    let f = admit(
        &[signed(&key(42), 100, 0, record("x"))],
        &[],
        &ring(),
        NS,
        &Says(true),
    );
    assert!(f.ops.is_empty());
    assert!(matches!(f.gaps.as_slice(), [RailGap::UnknownSigner { .. }]));
}
