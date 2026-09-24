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
    let body = body_json(&act, None, None);
    let forged = Op::new(
        SignedOp {
            seq: 3,
            // cy's key, over a line that claims to be alex's.
            sig: sign_ring_op(&key(3), NS, 103, 3, &body),
            act,
            on_behalf_of: None,
            view: None,
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

/// **A seal the log itself calls wrong retires nothing.** `Correct` voids
/// without erasure and the void set is commutative — but the floors were
/// computed before the void set existed, so a voided seal kept suppressing
/// its author's holes in `admit`, `digest` and `compact` alike. Seq 1 is
/// missing here; it is a hole precisely because the seal that claimed to
/// retire it is voided.
#[test]
fn a_corrected_seal_retires_nothing() {
    let seal = signed(&key(1), 102, 2, RailAct::Seal);
    let correct = signed(
        &key(1),
        103,
        3,
        RailAct::Correct {
            corrects: seal.id.clone(),
            replacement: None,
        },
    );
    let f = admitted(&[signed(&key(1), 100, 0, record("kept")), seal, correct]);

    assert!(
        !f.floors.contains_key(&actor_of(&key(1))),
        "a voided seal raises no floor: {:?}",
        f.floors
    );
    assert!(
        f.gaps.contains(&RailGap::SequenceHole {
            actor: actor_of(&key(1)),
            missing: 1,
        }),
        "seq 1 is a hole again — the retirement was itself corrected: {:?}",
        f.gaps
    );
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
        body_json(&act, None, None),
        serde_json::to_string(&act).unwrap(),
        "an absent name must not reach the signed bytes"
    );
    assert!(
        body_json(&act, Some("dee"), None).ends_with(r#","on_behalf_of":"dee"}"#),
        "a stated name goes last: {}",
        body_json(&act, Some("dee"), None)
    );
}

// ── the view an act commits to ──────────────────────────────

fn claimed_view(head_fill: char) -> crate::Digest {
    crate::Digest::from(std::collections::BTreeMap::from([(
        String::from("c0ffee"),
        crate::View {
            from: 0,
            mark: 3,
            head: head_fill.to_string().repeat(64),
        },
    )]))
}

/// **The signed body carries the view, last.** The two with-view vectors are
/// the wire contract (ROOT_CAUSE_FIXES A3): frozen by hand from the layout
/// rule — act, then `on_behalf_of`, then `view`, each skipped when absent —
/// and watched failing against the old bytes before the field reached them.
#[test]
fn the_signed_body_golden_vectors() {
    let act = record("milk");
    let view = claimed_view('a');
    let view_json = format!(
        r#"{{"v":2,"entries":{{"c0ffee":{{"from":0,"mark":3,"head":"{}"}}}}}}"#,
        "a".repeat(64)
    );

    // The bytes every pre-view op on every replica signed.
    assert_eq!(
        body_json(&act, None, None),
        serde_json::to_string(&act).unwrap()
    );
    // The view goes last and is skipped when absent.
    assert_eq!(
        body_json(&act, None, Some(&view)),
        format!(
            r#"{{"op":"record","payload":{{"kind":"thing","what":"milk"}},"view":{view_json}}}"#
        ),
        "an act with a view signs it, last"
    );
    assert_eq!(
        body_json(&act, Some("dee"), Some(&view)),
        format!(
            r#"{{"op":"record","payload":{{"kind":"thing","what":"milk"}},"on_behalf_of":"dee","view":{view_json}}}"#
        ),
        "name before view, both after the act"
    );
}

/// **The view is inside the signature.** A stamp a peer could rewrite in
/// flight would make every act claim whichever history suited the carrier
/// (ARCH §18.1 — the `on_behalf_of` rule one field later).
#[test]
fn a_view_rewritten_after_signing_is_refused() {
    let act = record("milk");
    let view = claimed_view('a');
    let body = body_json(&act, None, Some(&view));
    let op = Op::new(
        SignedOp {
            seq: 0,
            sig: sign_ring_op(&key(1), NS, 100, 0, &body),
            act,
            on_behalf_of: None,
            view: Some(view),
        },
        100,
        actor_of(&key(1)),
    );
    assert!(
        admitted(std::slice::from_ref(&op)).gaps.is_empty(),
        "the honest op verifies"
    );

    let mut rewritten = op;
    rewritten.kind.view = Some(claimed_view('b'));
    let f = admitted(std::slice::from_ref(&rewritten));
    assert!(
        f.gaps
            .iter()
            .any(|g| matches!(g, RailGap::BadSignature { .. })),
        "a rewritten view must break the signature: {:?}",
        f.gaps
    );
}

/// **An equivocation pair is its own evidence pack.** Two acts at one seq,
/// each line carrying the view its author claimed when writing it: each act
/// alone proves its own claimed history (it is signed over), and the pair
/// shows the divergence with nothing but the two lines. Admission's half —
/// both branches excluded — is already pinned where forks are audited.
#[test]
fn an_equivocation_carries_two_claimed_views() {
    let (view_a, view_b) = (claimed_view('a'), claimed_view('b'));
    let build = |act: RailAct, view: &crate::Digest| {
        let body = body_json(&act, None, Some(view));
        Op::new(
            SignedOp {
                seq: 2,
                sig: sign_ring_op(&key(1), NS, 102, 2, &body),
                act,
                on_behalf_of: None,
                view: Some(view.clone()),
            },
            102,
            actor_of(&key(1)),
        )
    };
    let left = build(record("left"), &view_a);
    let right = build(record("right"), &view_b);

    // The evidence rides the wire form — parse it back and the claimed views
    // are still there, still signed.
    for (op, expected) in [(&left, &view_a), (&right, &view_b)] {
        let wire = serde_json::to_string(op).unwrap();
        let back: Op<SignedOp> = serde_json::from_str(&wire).unwrap();
        assert_eq!(
            back.kind.view.as_ref(),
            Some(expected),
            "the claim survives the wire"
        );
    }
    assert_ne!(left.kind.view, right.kind.view, "two claimed histories");

    let f = admitted(&[left, right]);
    assert!(
        f.gaps
            .iter()
            .any(|g| matches!(g, RailGap::SequenceFork { .. })),
        "both branches are excluded by name: {:?}",
        f.gaps
    );
}

/// Leg 5's stamp half: the membership acts ship at the bumped line version
/// and the original three keep the bytes they always had — so an un-upgraded
/// node names exactly the acts it cannot read, and nothing else changes
/// version.
#[test]
fn the_membership_acts_ship_at_a_bumped_line_version() {
    let wrote = signed(&key(1), 100, 0, record("x"));
    let sealed = signed(&key(1), 101, 1, RailAct::Seal);
    let corrected = signed(
        &key(1),
        102,
        2,
        RailAct::Correct {
            corrects: wrote.id.clone(),
            replacement: None,
        },
    );
    let admitted_someone = signed(
        &key(1),
        103,
        3,
        RailAct::Admit {
            person: Person::from("bo"),
            key: crate::actor_of(&key(2)),
        },
    );
    let cut = signed(
        &key(1),
        104,
        4,
        RailAct::Remove {
            key: crate::actor_of(&key(2)),
            through_seq: 0,
        },
    );
    assert_eq!(
        (wrote.v, sealed.v, corrected.v),
        (1, 1, 1),
        "Record, Correct and Seal keep the version they always had"
    );
    assert_eq!(
        (admitted_someone.v, cut.v),
        (MEMBERSHIP_LINE_VERSION, MEMBERSHIP_LINE_VERSION),
        "the membership acts ship at the bump"
    );
}

/// The back-compat stop condition of the membership amendment, pinned
/// byte-for-byte: a ring with no `Admit` in its journal must admit exactly
/// as it did before membership existed. Frozen from this fixture's first
/// post-landing fold (RFC style — generated once, then pinned); every field
/// of the Admission is in the string, so any drift in the no-act path is a
/// diff here rather than an invisible behaviour change.
#[test]
fn a_ring_with_no_membership_acts_admits_exactly_as_before() {
    let ops: Vec<Op<SignedOp>> = include_str!("fixtures/rail_ops_before_on_behalf_of.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("the fixture is a journal line"))
        .collect();
    let f = admitted(&ops);
    assert_eq!(
        serde_json::to_string(&(&f.ops, &f.gaps, &f.floors)).unwrap(),
        r#"[[{"id":"ring-10b53bd03defce30","actor":"8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c","person":"alex","seq":0,"ts_unix":1700000000,"voided":true,"payload":{"kind":"thing","what":"milk"}},{"id":"ring-ced6c95b3f4dac0f","actor":"8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394","person":"bo","seq":0,"ts_unix":1700000001,"corrects":"ring-10b53bd03defce30","voided":false},{"id":"ring-75e6702c797ca7e8","actor":"ed4928c628d1c2c6eae90338905995612959273a5c63f93636c14614ac8737d1","person":"cy","seq":0,"ts_unix":1700000002,"voided":false}],[],{"ed4928c628d1c2c6eae90338905995612959273a5c63f93636c14614ac8737d1":0}]"#,
        "a ring with no membership act folds byte-identically to the build before"
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
