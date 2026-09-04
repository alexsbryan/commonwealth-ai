// SPDX-License-Identifier: AGPL-3.0-or-later
//! The journal on disk, and the two-node drill.
//!
//! The half of the rail's suite that needs a filesystem. Its sibling —
//! convergence correctness over an op SET, which needs no disk — is
//! `commonwealth-rail-core`'s `tests.rs`, and the two were one file until the
//! crates split (2026-09-04).
//!
//! The fixtures come from `commonwealth_rail_core::tests_support` rather than
//! being restated here: the sync tests and these must be talking about the
//! same signed op or neither proves anything (ARCH §10.6).

use crate::*;

use commonwealth_rail_core::tests_support::*;

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
