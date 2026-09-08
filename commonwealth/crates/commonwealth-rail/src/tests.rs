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

    let f = journal.admit(&r, &Ed25519Verifier).unwrap();
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

    let f = journal.admit(&r, &Ed25519Verifier).unwrap();
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
    assert_eq!(
        journal.admit(&ring(), &Ed25519Verifier).unwrap().ops.len(),
        1
    );
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
        a.admit(&r, &Ed25519Verifier).unwrap(),
        b.admit(&r, &Ed25519Verifier).unwrap(),
        "the fixture must actually be partitioned"
    );

    // Heal, both directions, one exchange each way.
    let for_a = b.ops_missing_from(&a.digest().unwrap()).unwrap();
    let for_b = a.ops_missing_from(&b.digest().unwrap()).unwrap();
    assert_eq!(a.ingest_all(&for_a).unwrap(), 1);
    assert_eq!(b.ingest_all(&for_b).unwrap(), 1);

    let (fa, fb) = (
        a.admit(&r, &Ed25519Verifier).unwrap(),
        b.admit(&r, &Ed25519Verifier).unwrap(),
    );
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
    assert_eq!(a.admit(&r, &Ed25519Verifier).unwrap(), fa);
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

    let fa = a.admit(&r, &Ed25519Verifier).unwrap();
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
    let healed = a.admit(&r, &Ed25519Verifier).unwrap();
    assert!(healed.is_complete(), "{:?}", healed.gaps);
    assert_eq!(healed, b.admit(&r, &Ed25519Verifier).unwrap());
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

    let f = journal.admit(&r, &Ed25519Verifier).unwrap();
    assert_eq!(applied(&f), vec!["whole"], "the whole line still counts");
    assert!(
        f.gaps
            .iter()
            .any(|g| matches!(g, RailGap::MalformedLine { line: 2, .. })),
        "{:?}",
        f.gaps
    );
}

// ── retention: the seal authorises, `compact` executes ───────

/// The lines a naive truncation would keep: everything at or above `from`.
/// Used ONLY by the negative control below — the real path is
/// [`RingJournal::compact`], and a second implementation of "what to keep"
/// exists here precisely so it can be shown to be wrong.
fn truncate_by_hand(journal: &RingJournal, from: u64) {
    let path = journal.dir().join("ring_oplog.jsonl");
    let kept: String = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|l| {
            serde_json::from_str::<Op<SignedOp>>(l)
                .map(|o| o.kind.seq >= from)
                .unwrap_or(true)
        })
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&path, kept).unwrap();
}

/// **The negative control, and the measurement that made retention a phase.**
/// Deleting a prefix on a local rule — a truncation setting, a size cap, an
/// age policy — is what a journal usually does, and on this rail it reports
/// one `SequenceHole` per deleted op, permanently, to every housemate, while
/// the node is in perfect health. The ceiling run produced ten thousand of
/// them this way.
///
/// It is here so the tests below cannot pass vacuously: the identical deletion
/// with a seal behind it is complete, and this is the proof that "complete" was
/// something to earn.
#[test]
fn deleting_a_prefix_with_no_seal_behind_it_makes_holes() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let r = ring();
    for what in ["one", "two", "three", "after"] {
        journal.append(record(what), &key(1), &r).unwrap();
    }
    truncate_by_hand(&journal, 3);

    let f = journal.admit(&r, &Ed25519Verifier).unwrap();
    assert!(!f.is_complete());
    assert_eq!(
        f.gaps.len(),
        3,
        "one hole per deleted op, and they never close: {:?}",
        f.gaps
    );
}

/// **The rail can now delete, and it deletes through a verb.** This is the
/// whole point of the sealed floor, exercised the only way that proves it:
/// lines are physically removed from a journal on disk, and then the node has
/// to still be right and still be quiet.
///
/// Before the floor, each of the three numbered assertions below failed in a
/// different way — the compacted node reported three `SequenceHole`s (the
/// control above), advertised nothing at all for the actor, and had its whole
/// holding pushed back at it on the next sixty-second round. Compaction
/// amplified traffic and then undid itself, which is why "the rail cannot
/// delete anything at any granularity" was true.
#[test]
fn a_compacted_journal_stays_complete_and_stops_the_prefix_coming_back() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, b) = (open(dir_a.path()), open(dir_b.path()));
    let r = ring();

    // Alex writes three acts and then seals them — through the ordinary door,
    // under the ordinary key, taking the next ordinary seq. There is no
    // second authoring path and no setting.
    for what in ["one", "two", "three"] {
        a.append(record(what), &key(1), &r).unwrap();
    }
    let seal = a.append(RailAct::Seal, &key(1), &r).unwrap();
    assert_eq!(seal.kind.seq, 3, "a seal is just the actor's next op");
    a.append(record("after"), &key(1), &r).unwrap();

    // B is a peer that received everything and has NOT compacted.
    b.ingest_all(&a.read().unwrap().0).unwrap();

    // A retires what the seal covers.
    let done = a.compact(&r, &Ed25519Verifier).unwrap();
    assert_eq!(done.removed, 3, "seqs 0..=2 are below the floor");
    assert_eq!(done.kept, 2, "the seal itself and the act above it");
    assert_eq!(done.gaps_cleared, 0);
    assert_eq!(done.floors, Floors::from([(actor_of(&key(1)), 3)]));
    assert_eq!(a.read().unwrap().0.len(), 2, "the prefix is really gone");
    assert!(
        !a.dir().join("ring_oplog.compacting").exists(),
        "the rewrite leaves no temp file behind"
    );

    // 1. A compacted node is not a broken one.
    let fa = a.admit(&r, &Ed25519Verifier).unwrap();
    assert!(fa.is_complete(), "{:?}", fa.gaps);
    assert_eq!(applied(&fa), vec!["after"]);

    // 2. It can still say what it needs, from the floor rather than from zero.
    assert_eq!(
        a.digest().unwrap(),
        Digest::from([(actor_of(&key(1)), 4)]),
        "a compacted actor must make a claim, not fall silent"
    );

    // 3. So the peer that still holds the retired prefix sends none of it.
    assert!(
        b.ops_missing_from(&a.digest().unwrap()).unwrap().is_empty(),
        "the retired prefix must not come back every sixty seconds"
    );
    assert_eq!(a.ingest_all(&[]).unwrap(), 0);
    assert_eq!(a.read().unwrap().0.len(), 2, "and it stays deleted");

    // Compacting again is a no-op rather than an error or a second bite.
    let again = a.compact(&r, &Ed25519Verifier).unwrap();
    assert_eq!((again.removed, again.kept), (0, 2));
}

/// **A node that has never sealed deletes nothing.** The floor is the only
/// authority `compact` has, so with no seal on the journal there is no
/// authority and the call is a successful no-op — not an error, because a ring
/// that has never sealed is a normal ring.
#[test]
fn compacting_a_journal_with_no_seal_removes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let r = ring();
    for what in ["one", "two", "three"] {
        journal.append(record(what), &key(1), &r).unwrap();
    }
    let done = journal.compact(&r, &Ed25519Verifier).unwrap();
    assert_eq!((done.removed, done.kept), (0, 3));
    assert!(done.floors.is_empty());
    assert_eq!(journal.read().unwrap().0.len(), 3);
}

/// **A seal the rail refused deletes nothing from disk (ARCH §18.3).**
///
/// The sibling of `a_seal_the_rail_refused_retires_nothing` in the core suite,
/// and the one that matters more: that test proves a forged seal does not
/// suppress a gap, this one proves it does not erase a file. `/internal/ring/
/// sync` ingests peer ops exactly as signed, so this line — a seal claiming to
/// be alex's, signed with cy's key — is what a hostile peer actually pushes. If
/// `compact` derived its floor from the seals a node HOLDS rather than the ones
/// admission AUTHENTICATED, one pushed line would destroy a member's history on
/// every node that received it, irreversibly and with nothing red anywhere.
#[test]
fn a_forged_seal_deletes_nothing_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let r = ring();
    for what in ["one", "two", "three"] {
        journal.append(record(what), &key(1), &r).unwrap();
    }
    let act = RailAct::Seal;
    let body = serde_json::to_string(&act).unwrap();
    let forged = Op::new(
        SignedOp {
            seq: 3,
            sig: sign_ring_op(&key(3), NS, 103, 3, &body),
            act,
        },
        103,
        actor_of(&key(1)),
    );
    journal.ingest(&forged).unwrap();

    let done = journal.compact(&r, &Ed25519Verifier).unwrap();
    assert_eq!(done.removed, 0, "a refused seal is not an authority");
    assert!(done.floors.is_empty());
    assert_eq!(journal.read().unwrap().0.len(), 4, "nothing was deleted");
}

/// **A peer prunes what somebody else retired.** Author-only pruning would
/// bound the writer's disk and nobody else's — every housemate would still keep
/// a full copy of everyone's history forever, which is the growth this phase
/// exists to stop. A seal binds whoever admits it.
#[test]
fn compaction_prunes_a_peers_retired_prefix_too() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, b) = (open(dir_a.path()), open(dir_b.path()));
    let r = ring();
    for what in ["one", "two"] {
        a.append(record(what), &key(1), &r).unwrap();
    }
    a.append(RailAct::Seal, &key(1), &r).unwrap();
    // Bo has written too, and has NOT sealed — bo's history is untouched.
    b.append(record("bo-one"), &key(2), &r).unwrap();
    b.ingest_all(&a.read().unwrap().0).unwrap();

    let done = b.compact(&r, &Ed25519Verifier).unwrap();
    assert_eq!(done.removed, 2, "alex's retired prefix, on bo's disk");
    assert_eq!(done.kept, 2, "alex's seal and bo's own unsealed act");
    let f = b.admit(&r, &Ed25519Verifier).unwrap();
    assert!(f.is_complete(), "{:?}", f.gaps);
    assert_eq!(applied(&f), vec!["bo-one"]);
}

/// **An op a correction still names outlives the floor above it.** "Retired"
/// means nobody will ask for this again, and a surviving
/// [`RailAct::Correct`] pointing at it says otherwise. Deleting it would turn
/// the correction into a permanent `DanglingCorrection` — which the guard would
/// then catch, refusing every future compaction of this ring and quietly
/// restoring the unbounded growth the verb exists to stop.
#[test]
fn an_op_a_correction_names_survives_a_seal_above_it() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let r = ring();
    let first = journal.append(record("one"), &key(1), &r).unwrap();
    journal.append(record("two"), &key(1), &r).unwrap();
    journal
        .append(
            RailAct::Correct {
                corrects: first.id.clone(),
                replacement: None,
            },
            &key(2),
            &r,
        )
        .unwrap();
    journal.append(RailAct::Seal, &key(1), &r).unwrap();

    let done = journal.compact(&r, &Ed25519Verifier).unwrap();
    assert_eq!(
        done.removed, 1,
        "only `two` is both retired and unreferenced"
    );
    let held = journal.read().unwrap().0;
    assert!(
        held.iter().any(|o| o.id == first.id),
        "the corrected op is still on disk"
    );
    let f = journal.admit(&r, &Ed25519Verifier).unwrap();
    assert!(
        f.is_complete(),
        "no correction was left dangling: {:?}",
        f.gaps
    );
}

/// **A journal with lines this build cannot read is not rewritten at all.**
/// `SkippedLine` carries a line number and a parse error, never the bytes, so
/// an unreadable line cannot survive a rewrite. The dangerous case is not the
/// corrupt line but the one from a NEWER format version: compacting there would
/// make an old build the one that deletes the future.
#[test]
fn compaction_refuses_a_journal_it_cannot_fully_read() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let r = ring();
    journal.append(record("one"), &key(1), &r).unwrap();
    journal.append(RailAct::Seal, &key(1), &r).unwrap();
    let path = journal.dir().join("ring_oplog.jsonl");
    let mut raw = std::fs::read_to_string(&path).unwrap();
    raw.push_str("{not json\n");
    std::fs::write(&path, &raw).unwrap();

    let refusal = journal.compact(&r, &Ed25519Verifier);
    assert!(
        matches!(&refusal, Err(RailError::Rejected(why)) if why.contains("cannot read")),
        "{refusal:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        raw,
        "the file is untouched, byte for byte"
    );
}

/// **The guard: compaction refuses rather than raise a gap.** The `referenced`
/// clause keeps a corrected op alive by matching the id on the line, and a line
/// whose id was rewritten does not match — so this is the case that gets past
/// it. The invariant "a node may delete only what a floor covers" is encoded as
/// a check on the result rather than trusted to the filter, because a filter is
/// a rule the next caller has to remember and a refusal is not (ARCH §7).
///
/// It fires on a journal that is already compromised, which is the point: the
/// outcome is a refusal naming the gap, not a destructive best effort.
#[test]
fn compaction_refuses_when_it_would_raise_a_gap() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let r = ring();

    // The op a correction will name, ingested with its id rewritten. Admission
    // still counts it — identity comes from content (ARCH §7.5) — so nothing
    // dangles yet.
    let real = signed(&key(1), 100, 0, record("one"));
    let mut tampered = real.clone();
    tampered.id = OpId::from_raw("ring-ffffffffffffffff");
    journal.ingest(&tampered).unwrap();
    journal
        .ingest(&signed(
            &key(2),
            101,
            0,
            RailAct::Correct {
                corrects: real.id.clone(),
                replacement: None,
            },
        ))
        .unwrap();
    journal
        .ingest(&signed(&key(1), 102, 1, RailAct::Seal))
        .unwrap();

    let before = journal.admit(&r, &Ed25519Verifier).unwrap();
    assert!(
        !before
            .gaps
            .iter()
            .any(|g| matches!(g, RailGap::DanglingCorrection { .. })),
        "control: the correction resolves before compaction: {:?}",
        before.gaps
    );

    let refusal = journal.compact(&r, &Ed25519Verifier);
    assert!(
        matches!(&refusal, Err(RailError::Rejected(why)) if why.contains("would raise")),
        "{refusal:?}"
    );
    assert_eq!(journal.read().unwrap().0.len(), 3, "nothing was deleted");
}

/// **A gap may DISAPPEAR, and the count says so.** A line that failed the
/// signature check, sitting below a floor its claimed author authenticated, is
/// one nobody will ever ask for again — and keeping it would let anyone grow a
/// peer's journal without bound by pushing junk under an old seal. So it goes,
/// and its gap goes with it.
///
/// That is a change to what this node claims completeness over, which is
/// exactly the kind of change a destructive path may not make silently
/// (ARCH §18.3) — hence [`Compaction::gaps_cleared`] rather than a quieter
/// journal and no account of why.
#[test]
fn a_refused_line_under_a_real_floor_goes_and_is_counted() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let r = ring();
    for what in ["one", "two"] {
        journal.append(record(what), &key(1), &r).unwrap();
    }
    // Cy's signature over a line claiming to be alex's: refused by admission,
    // still on the disk, still counted in `held`.
    let act = record("forged");
    let body = serde_json::to_string(&act).unwrap();
    journal
        .ingest(&Op::new(
            SignedOp {
                seq: 2,
                sig: sign_ring_op(&key(3), NS, 102, 2, &body),
                act,
            },
            102,
            actor_of(&key(1)),
        ))
        .unwrap();
    let before = journal.admit(&r, &Ed25519Verifier).unwrap();
    assert!(
        matches!(before.gaps.as_slice(), [RailGap::BadSignature { .. }]),
        "{:?}",
        before.gaps
    );

    // Alex seals above all three. The seal is real, so the floor is real.
    journal.append(RailAct::Seal, &key(1), &r).unwrap();
    let done = journal.compact(&r, &Ed25519Verifier).unwrap();
    assert_eq!(done.removed, 3, "two real acts and the forgery under them");
    assert_eq!(done.gaps_cleared, 1, "the BadSignature went with them");
    assert!(journal.admit(&r, &Ed25519Verifier).unwrap().is_complete());
}
