// SPDX-License-Identifier: AGPL-3.0-or-later
//! The verifier seam, seals, and acting on behalf of a guest.
//!
//! Split out of `tests.rs`, which the ring-rail work pushed into the
//! 800-1200 approach band (ARCH §3.1). Same crate, same fixtures.

use crate::*;

use crate::tests_support::*;

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

// ── the sealed floor ─────────────────────────────────────────

/// A `Seal` whose author signed it, at `seq`, plus one act above it. The
/// holding a node has after it compacts everything the seal retired.
fn sealed_then(seq: u64, what: &str) -> Vec<Op<SignedOp>> {
    vec![
        signed(&key(1), 100 + seq as i64, seq, RailAct::Seal),
        signed(&key(1), 101 + seq as i64, seq + 1, record(what)),
    ]
}

/// **Compaction must not look like breakage.** Before the floor existed,
/// `admit` walked `0..=highest` and a node that had retired a sealed prefix
/// reported one `SequenceHole` per retired op — permanently, and to every
/// housemate, while being in perfect health.
///
/// The first assertion is the negative control: the same two ops WITHOUT the
/// seal are three holes, so the second cannot pass on a holding that was
/// never missing anything.
#[test]
fn a_sealed_prefix_is_retired_rather_than_reported_as_a_hole() {
    let unsealed = vec![signed(&key(1), 104, 4, record("after"))];
    assert_eq!(
        admitted(&unsealed).gaps.len(),
        4,
        "control: with nothing sealed, seqs 0..=3 are four holes"
    );

    let f = admitted(&sealed_then(3, "after"));
    assert!(
        f.is_complete(),
        "a compacted node is not a broken one: {:?}",
        f.gaps
    );
    assert_eq!(applied(&f), vec!["after"]);
}

/// **A refused seal retires nothing (ARCH §18.3).** A seal is the one act that
/// makes the rail stop asking for history, so a seal the rail could not
/// authenticate must be a refusal and never an absence. This one names `alex`
/// as its actor and is signed with a key that is not alex's — the exact op a
/// hostile peer would push at `/internal/ring/sync`, which ingests as-signed.
///
/// If the floor came from the seal ops a node HOLDS rather than the ones it
/// ADMITS, one forged line would retire a member's whole history on every node
/// that received it.
#[test]
fn a_seal_the_rail_refused_retires_nothing() {
    let act = RailAct::Seal;
    let body = body_json(&act, None);
    let forged = Op::new(
        SignedOp {
            seq: 3,
            // cy's key, over a line that claims to be alex's.
            sig: sign_ring_op(&key(3), NS, 103, 3, &body),
            act,
            on_behalf_of: None,
        },
        103,
        actor_of(&key(1)),
    );
    let f = admitted(&[forged, signed(&key(1), 104, 4, record("after"))]);

    assert!(
        f.gaps.iter().any(
            |g| matches!(g, RailGap::BadSignature { actor, .. } if *actor == actor_of(&key(1)))
        ),
        "the forged seal is refused, not silently ignored: {:?}",
        f.gaps
    );
    for missing in 0..=3 {
        assert!(
            f.gaps.contains(&RailGap::SequenceHole {
                actor: actor_of(&key(1)),
                missing,
            }),
            "seq {missing} is still missing — a refused seal did not retire it: {:?}",
            f.gaps
        );
    }
    assert!(!f.is_complete());
}

/// **A seal is bounded by the key that signed it.** `RailAct::Seal` carries no
/// actor, so sealing somebody else's history is unwritable rather than
/// refused — but the floor derivation still has to key on the signer and not,
/// say, apply the highest seal in the journal to everyone.
#[test]
fn a_seal_retires_only_the_history_of_the_key_that_signed_it() {
    let mut ops = sealed_then(3, "after");
    // bo holds only their seq 2 — seqs 0 and 1 have not arrived, and alex's
    // seal says nothing about that.
    ops.push(signed(&key(2), 110, 2, record("bo-late")));
    let f = admitted(&ops);
    assert_eq!(
        f.gaps,
        vec![
            RailGap::SequenceHole {
                actor: actor_of(&key(2)),
                missing: 0
            },
            RailGap::SequenceHole {
                actor: actor_of(&key(2)),
                missing: 1
            },
        ],
        "alex is sealed, bo is not"
    );
}

// ── whose words an act was ───────────────────────────────────

/// The field is LAST and skipped when absent, so an act that names nobody
/// signs the bytes it signed before the field existed. Stated here rather
/// than trusted to serde's declaration order, because every op on every
/// replica rests on it.
#[test]
fn an_act_naming_nobody_signs_the_bytes_it_always_did() {
    let act = record("milk");
    assert_eq!(
        body_json(&act, None),
        serde_json::to_string(&act).unwrap(),
        "an absent name must not reach the signed bytes"
    );
    assert!(
        body_json(&act, Some("dee")).ends_with(r#","on_behalf_of":"dee"}"#),
        "a stated name goes last: {}",
        body_json(&act, Some("dee"))
    );
}

/// Ops written before `on_behalf_of` existed, captured from this crate at
/// f51b66112 and committed verbatim. Every replica holds lines like these;
/// if adding the field moved a byte, they would all become `BadSignature`
/// the day a node upgraded, and no test that constructs its ops with the
/// NEW code could ever notice.
#[test]
fn rail_ops_written_before_on_behalf_of_still_verify() {
    let ops: Vec<Op<SignedOp>> = include_str!("fixtures/rail_ops_before_on_behalf_of.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("the fixture is a journal line"))
        .collect();
    assert_eq!(ops.len(), 3, "record, correct-without-replacement, seal");
    assert!(
        ops.iter().all(|o| o.kind.on_behalf_of.is_none()),
        "the fixture predates the field"
    );

    let f = admitted(&ops);
    assert!(
        f.gaps.is_empty(),
        "an op written before the field must still verify: {:?}",
        f.gaps
    );
    assert!(
        f.ops.iter().all(|o| o.on_behalf_of.is_none()),
        "nothing invents a name"
    );
}

/// The name is inside the signature, so rewriting it in flight — the only
/// way a peer could reattribute somebody's words — is a refusal and not a
/// silent correction. The id is re-derived from the tampered body, so the
/// ONLY thing wrong with this op is the signature.
#[test]
fn a_name_rewritten_after_signing_is_refused() {
    let honest = signed_for(NS, &key(1), 100, 0, record("milk"), Some("dee"));
    let f = admitted(&[honest.clone()]);
    assert!(f.gaps.is_empty(), "{:?}", f.gaps);
    assert_eq!(
        f.ops[0].on_behalf_of.as_deref(),
        Some("dee"),
        "admission carries the name through"
    );

    let mut tampered = honest.kind.clone();
    tampered.on_behalf_of = Some("eve".to_string());
    let reattributed = Op::new(tampered, honest.ts_unix, honest.actor.clone());
    let f = admitted(&[reattributed]);
    assert!(
        matches!(f.gaps.as_slice(), [RailGap::BadSignature { .. }]),
        "rewriting the name must not verify: {:?}",
        f.gaps
    );
    assert!(f.ops.is_empty(), "nothing tampered reaches an app");
}

/// Every act kind carries it — including a correction that states no
/// replacement, which has no payload a name could have ridden in.
#[test]
fn a_correction_with_no_replacement_still_names_the_guest() {
    let first = signed_for(NS, &key(1), 100, 0, record("milk"), Some("dee"));
    let retraction = signed_for(
        NS,
        &key(1),
        101,
        1,
        RailAct::Correct {
            corrects: first.id.clone(),
            replacement: None,
        },
        Some("dee"),
    );
    let f = admitted(&[first, retraction]);
    assert!(f.gaps.is_empty(), "{:?}", f.gaps);
    assert!(
        f.ops
            .iter()
            .all(|o| o.on_behalf_of.as_deref() == Some("dee")),
        "a payload-less act names its guest too"
    );
    assert!(applied(&f).is_empty(), "the retraction voided the record");
}
