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

use super::*;

use super::tests_support::*;

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
    let f = admit(&ops, &skipped, &ring(), NS);
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

// ── the journal on disk ──────────────────────────────────────

fn open(dir: &std::path::Path) -> RingJournal {
    RingJournal::open(dir, NS).unwrap()
}

#[test]
fn a_namespace_cannot_be_a_path() {
    let dir = tempfile::tempdir().unwrap();
    for bad in [
        "../../etc",
        "a/b",
        "",
        "Has-Caps",
        "with space",
        &"x".repeat(65),
    ] {
        assert!(
            matches!(
                RingJournal::open(dir.path(), bad),
                Err(RailError::BadNamespace(_))
            ),
            "{bad:?} was accepted"
        );
    }
    assert!(RingJournal::open(dir.path(), "house-expenses_2").is_ok());
}

#[test]
fn appending_assigns_contiguous_sequence_numbers_per_actor() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let r = ring();
    for i in 0..3 {
        let op = journal.append(record("x"), &key(1), &r).unwrap();
        assert_eq!(op.kind.seq, i);
    }
    // A second actor writing to the same journal keeps its OWN counter.
    let op = journal.append(record("y"), &key(2), &r).unwrap();
    assert_eq!(op.kind.seq, 0);

    let f = journal.admit(&r).unwrap();
    assert!(f.is_complete(), "{:?}", f.gaps);
    assert_eq!(f.ops.len(), 4);
}

/// A journal line is one flat JSON object — the envelope's fields and the
/// act's, side by side, the same shape the other three oplog tenants write.
#[test]
fn a_journal_line_is_one_flat_object_that_reads_back() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let r = ring();
    let written = journal.append(record("milk"), &key(1), &r).unwrap();

    let raw = std::fs::read_to_string(journal.dir().join("ring_oplog.jsonl")).unwrap();
    let v: serde_json::Value = serde_json::from_str(raw.trim()).unwrap();
    for k in ["id", "v", "ts_unix", "actor", "seq", "sig", "op", "payload"] {
        assert!(v.get(k).is_some(), "line is missing {k}: {raw}");
    }
    assert_eq!(v["op"], "record");
    assert_eq!(v["payload"]["what"], "milk");

    let (back, skipped) = journal.read().unwrap();
    assert!(skipped.is_empty());
    assert_eq!(back, vec![written]);
}

/// **The one thing the door still refuses, and why.** Authoring under a key
/// the ring's own roster does not carry produces ops that every node — this
/// one included — reports as `UnknownSigner` forever. Refusing here turns a
/// permanent silent gap into one sentence naming the command that fixes it.
#[test]
fn the_door_refuses_to_author_under_a_key_the_ring_does_not_know() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let stranger = journal.append(record("x"), &key(42), &ring());
    let Err(RailError::Rejected(why)) = stranger else {
        panic!("the door authored an op nobody in the ring can read");
    };
    assert!(
        why.contains("roster add"),
        "the refusal must name the fix: {why}"
    );
    assert_eq!(journal.read().unwrap().0.len(), 0, "nothing was written");

    // A member writes fine.
    assert!(journal.append(record("x"), &key(1), &ring()).is_ok());
}

/// The rail no longer judges what an act MEANS, and that is the trade the
/// opaque payload buys. An app's own refusals belong to the app, which owns
/// one validator its door and its reducer both call — the same shape this
/// module used to have, one layer up.
#[test]
fn the_door_has_no_opinion_about_what_an_act_says() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let nonsense = RailAct::Record {
        payload: Payload::new(serde_json::json!({
            "kind": "expense",
            "amount_cents": -1,
            "participants": [],
        }))
        .unwrap(),
    };
    assert!(journal.append(nonsense, &key(1), &ring()).is_ok());
}

/// A payload with no canonical form never becomes a journal line, and if one
/// arrives from a peer it reads back as a malformed line rather than as a
/// mysterious bad signature.
#[test]
fn a_payload_with_no_canonical_form_is_a_malformed_line() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let r = ring();
    journal.append(record("whole"), &key(1), &r).unwrap();
    let path = journal.dir().join("ring_oplog.jsonl");
    let mut raw = std::fs::read_to_string(&path).unwrap();
    raw.push_str(
        r#"{"id":"ring-abc","v":1,"ts_unix":1,"actor":"aa","seq":0,"sig":"x","op":"record","payload":{"amount":3.5}}"#,
    );
    raw.push('\n');
    std::fs::write(&path, raw).unwrap();

    let f = journal.admit(&r).unwrap();
    assert_eq!(applied(&f), vec!["whole"]);
    assert!(
        f.gaps
            .iter()
            .any(|g| matches!(g, RailGap::MalformedLine { line: 2, .. })),
        "{:?}",
        f.gaps
    );
}

/// A peer's op is written exactly as signed — not re-signed, not re-numbered —
/// and arriving twice is not an error.
#[test]
fn ingesting_a_peers_op_preserves_it_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let peer = signed(&key(2), 100, 0, record("x"));
    assert!(journal.ingest(&peer).unwrap());
    assert!(
        !journal.ingest(&peer).unwrap(),
        "second delivery is a no-op"
    );
    let (back, _) = journal.read().unwrap();
    assert_eq!(back, vec![peer]);
    assert_eq!(journal.admit(&ring()).unwrap().ops.len(), 1);
}

#[test]
fn a_missing_roster_is_an_empty_ring_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    assert_eq!(journal.roster().unwrap(), Roster::default());
    journal.set_roster(&ring()).unwrap();
    assert_eq!(journal.roster().unwrap(), ring());
}

// ── the two-node drill ───────────────────────────────────────

/// **Partition, write on both sides, heal — and agree.**
///
/// The whole design's reason for existing, exercised against two real
/// journals on disk rather than against `admit` in isolation.
#[test]
fn two_partitioned_nodes_converge_on_an_identical_admission() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, b) = (open(dir_a.path()), open(dir_b.path()));
    let r = ring();

    // Partitioned: neither node can see the other's write.
    a.append(record("groceries"), &key(1), &r).unwrap();
    b.append(record("beer"), &key(2), &r).unwrap();
    assert_ne!(
        a.admit(&r).unwrap(),
        b.admit(&r).unwrap(),
        "the fixture must actually be partitioned"
    );

    // Heal, both directions, one exchange each way.
    let for_a = b.ops_missing_from(&a.digest().unwrap()).unwrap();
    let for_b = a.ops_missing_from(&b.digest().unwrap()).unwrap();
    assert_eq!(a.ingest_all(&for_a).unwrap(), 1);
    assert_eq!(b.ingest_all(&for_b).unwrap(), 1);

    let (fa, fb) = (a.admit(&r).unwrap(), b.admit(&r).unwrap());
    assert_eq!(fa, fb, "two nodes, one answer");
    assert!(fa.is_complete(), "{:?}", fa.gaps);
    // Both acts survive, and the ORDER is content-derived rather than the
    // order either node happened to write in: these two land in the same
    // second, so the tie is broken by actor key and neither node's local
    // history wins. Sorted here for that reason — asserting the literal
    // sequence would be asserting a property of two fixture keypairs.
    let mut acts = applied(&fa);
    acts.sort();
    assert_eq!(acts, vec!["beer", "groceries"]);

    // And the exchange is idempotent: running it again moves nothing.
    let again = b.ops_missing_from(&a.digest().unwrap()).unwrap();
    assert!(again.is_empty());
    assert_eq!(a.ingest_all(&for_a).unwrap(), 0);
    assert_eq!(a.admit(&r).unwrap(), fa);
}

/// **A peer that dies mid-sync leaves a hole, and the hole is named.**
///
/// Half of B's ops reach A. A must not report a clean answer over what it
/// got: the acts are real and they are a subset, and only the gap says so.
#[test]
fn a_half_delivered_peer_is_a_named_hole_not_a_clean_answer() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, b) = (open(dir_a.path()), open(dir_b.path()));
    let r = ring();
    b.append(record("first"), &key(2), &r).unwrap();
    b.append(record("second"), &key(2), &r).unwrap();

    // Only the SECOND op lands — the connection dropped after one of them.
    let all = b.read().unwrap().0;
    let second = all.iter().find(|o| o.kind.seq == 1).unwrap().clone();
    a.ingest_all(&[second]).unwrap();

    let fa = a.admit(&r).unwrap();
    assert!(!fa.is_complete(), "A must not claim a complete answer");
    assert_eq!(
        fa.gaps,
        vec![RailGap::SequenceHole {
            actor: actor_of(&key(2)),
            missing: 0
        }]
    );

    // And the digest asks for the hole rather than claiming the high mark.
    assert!(
        !a.digest().unwrap().contains_key(&actor_of(&key(2))),
        "A holds nothing contiguous from B, so it must claim nothing"
    );
    let repair = b.ops_missing_from(&a.digest().unwrap()).unwrap();
    assert_eq!(
        repair.len(),
        2,
        "the hole and everything above it come back"
    );
    a.ingest_all(&repair).unwrap();
    let healed = a.admit(&r).unwrap();
    assert!(healed.is_complete(), "{:?}", healed.gaps);
    assert_eq!(healed, b.admit(&r).unwrap());
}

/// A torn last line — the daemon died between the write and the sync — is a
/// reported gap, not an invisible subtraction.
#[test]
fn a_torn_last_line_is_reported_rather_than_quietly_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let r = ring();
    journal.append(record("whole"), &key(1), &r).unwrap();
    let path = journal.dir().join("ring_oplog.jsonl");
    let mut raw = std::fs::read_to_string(&path).unwrap();
    raw.push_str("{\"id\":\"ring-abc\",\"v\":1,\"ts_un");
    std::fs::write(&path, raw).unwrap();

    let f = journal.admit(&r).unwrap();
    assert_eq!(applied(&f), vec!["whole"], "the whole line still counts");
    assert!(
        f.gaps
            .iter()
            .any(|g| matches!(g, RailGap::MalformedLine { line: 2, .. })),
        "{:?}",
        f.gaps
    );
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
