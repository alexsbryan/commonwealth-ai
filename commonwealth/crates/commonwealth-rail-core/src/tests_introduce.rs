// SPDX-License-Identifier: AGPL-3.0-or-later
//! Provenance: why a key is in the roster, and why a row can fail to say so.
//!
//! Its sibling `tests.rs` is about CONVERGENCE — what every node agrees the
//! journal says. This half is about the roster, which is a parameter of that
//! answer and never part of it, so the two do not share a fixture beyond
//! `tests_support`. They were one file until it passed the 1200-line ceiling
//! (ARCH §3.2).
//!
//! The bar these serve is a FRACTION, and its goodhart line says 1.0 over one
//! row proves nothing and that a warrant which merely CARRIES a string is not
//! resolved. So every negative here is a vouch that looks perfectly
//! well-formed on the roster and still does not resolve.

use crate::tests_support::*;
use crate::*;

/// The introduction act for `who`'s `key`, signed by `k`.
fn introduce(
    k: &SigningKey,
    ts: i64,
    seq: u64,
    who: &str,
    key_hex: &str,
    why: &str,
) -> Op<SignedOp> {
    let act = RailAct::Record {
        payload: Introduce::new(p(who), key_hex, why).payload().unwrap(),
    };
    signed(k, ts, seq, act)
}

/// A ring of one — alex, who added themself and has nobody to vouch for them.
fn founder() -> Roster {
    let mut r = Roster::default();
    r.bind_key(p("alex"), actor_of(&key(1)), None);
    r
}

/// **The demo, end to end.** Alex is in the ring because nobody vouched —
/// they were first. Dee is in it because alex wrote an introduction, signed
/// it, and the row names that op; the answer comes back out of the data with
/// a name, an op id and a date on it.
#[test]
fn a_roster_row_resolves_to_the_signed_op_that_introduced_it() {
    let dee = actor_of(&key(4));
    let intro = introduce(&key(1), 200, 0, "dee", &dee, "sold me the drill");

    let mut roster = founder();
    roster.bind_key(
        p("dee"),
        dee.clone(),
        Some(Vouch {
            op: intro.id.clone(),
            by: actor_of(&key(1)),
            at: 200,
        }),
    );

    let a = admit(&[intro.clone()], &[], &roster, NS, &Ed25519Verifier);
    assert!(a.is_complete(), "{:?}", a.gaps);

    let VouchStatus::Traced {
        op,
        by,
        by_actor,
        at,
        reason,
    } = trace(&roster, &a.ops, &p("dee"), &dee)
    else {
        panic!("{}", trace(&roster, &a.ops, &p("dee"), &dee));
    };
    assert_eq!(op, intro.id);
    assert_eq!(by, p("alex"));
    assert_eq!(by_actor, actor_of(&key(1)));
    assert_eq!(at, 200);
    assert_eq!(reason, "sold me the drill");

    // And the founder is UNKNOWN rather than traced. A row nobody vouched for
    // is not a failure of this rung; counting it as traced would be.
    assert_eq!(
        trace(&roster, &a.ops, &p("alex"), &actor_of(&key(1))),
        VouchStatus::Unknown
    );
}

/// **The op id must RESOLVE, not merely be present.** The goodhart this bar
/// names: a warrant field that is filled in and points at nothing. Here the
/// introduction was signed by a key the ring does not claim, so admission
/// refuses it — and the row is untraceable even though its `op`, `by` and
/// `at` all look right on the roster.
#[test]
fn a_warrant_naming_an_op_admission_refused_does_not_resolve() {
    let dee = actor_of(&key(4));
    let stranger = key(9);
    let intro = introduce(&stranger, 200, 0, "dee", &dee, "trust me");

    let mut roster = founder();
    roster.bind_key(
        p("dee"),
        dee.clone(),
        Some(Vouch {
            op: intro.id.clone(),
            by: actor_of(&stranger),
            at: 200,
        }),
    );

    let a = admit(&[intro.clone()], &[], &roster, NS, &Ed25519Verifier);
    assert!(
        a.gaps
            .iter()
            .any(|g| matches!(g, RailGap::UnknownSigner { .. })),
        "the fixture must actually be refused: {:?}",
        a.gaps
    );
    assert_eq!(
        trace(&roster, &a.ops, &p("dee"), &dee),
        VouchStatus::NotHeld { op: intro.id }
    );
}

/// **A warrant pointing at an act that is not an introduction.** The row
/// would render "introduced by alex" if the check trusted the roster, and the
/// op alex signed was about a drill.
#[test]
fn a_warrant_naming_an_ordinary_act_does_not_resolve() {
    let dee = actor_of(&key(4));
    let op = signed(&key(1), 200, 0, record("groceries"));
    let mut roster = founder();
    roster.bind_key(
        p("dee"),
        dee.clone(),
        Some(Vouch {
            op: op.id.clone(),
            by: actor_of(&key(1)),
            at: 200,
        }),
    );
    let a = admit(&[op.clone()], &[], &roster, NS, &Ed25519Verifier);
    assert_eq!(
        trace(&roster, &a.ops, &p("dee"), &dee),
        VouchStatus::NotAnIntroduction { op: op.id }
    );
}

/// **An introduction of somebody else is not this row's warrant.** Nothing
/// stops a roster from naming a real, verifying, admitted introduction that
/// is about a different key — and a check that stopped at "the op exists"
/// would call that traced.
#[test]
fn a_warrant_naming_an_introduction_of_another_key_does_not_resolve() {
    let (dee, eve) = (actor_of(&key(4)), actor_of(&key(5)));
    let intro = introduce(&key(1), 200, 0, "eve", &eve, "my sister");
    let mut roster = founder();
    roster.bind_key(
        p("dee"),
        dee.clone(),
        Some(Vouch {
            op: intro.id.clone(),
            by: actor_of(&key(1)),
            at: 200,
        }),
    );
    let a = admit(&[intro.clone()], &[], &roster, NS, &Ed25519Verifier);
    assert_eq!(
        trace(&roster, &a.ops, &p("dee"), &dee),
        VouchStatus::NamesAnother {
            op: intro.id,
            person: p("eve"),
            key: eve,
        }
    );
}

/// **A withdrawn vouch is not a vouch.** `Correct` voids permanently and
/// visibly, and an introduction is exactly the act a person regrets. The row
/// stays in the roster — removing it would be an op changing a roster — and
/// its warrant stops resolving.
#[test]
fn an_introduction_voided_by_a_correction_stops_resolving() {
    let dee = actor_of(&key(4));
    let intro = introduce(&key(1), 200, 0, "dee", &dee, "sold me the drill");
    let withdrawn = signed(
        &key(1),
        201,
        1,
        RailAct::Correct {
            corrects: intro.id.clone(),
            replacement: None,
        },
    );
    let mut roster = founder();
    roster.bind_key(
        p("dee"),
        dee.clone(),
        Some(Vouch {
            op: intro.id.clone(),
            by: actor_of(&key(1)),
            at: 200,
        }),
    );
    let a = admit(
        &[intro.clone(), withdrawn],
        &[],
        &roster,
        NS,
        &Ed25519Verifier,
    );
    assert_eq!(
        trace(&roster, &a.ops, &p("dee"), &dee),
        VouchStatus::Withdrawn { op: intro.id }
    );
}

/// **A key cannot be its own warrant.** The op was admissible only because
/// the roster already claimed that key, so reading it as the reason the key
/// is there is a circle. A SECOND laptop introduced by the first is a
/// different thing and still resolves — that is the `dee` row below.
#[test]
fn a_key_vouching_for_itself_does_not_resolve() {
    let dee = actor_of(&key(4));
    let alex2 = actor_of(&key(6));
    let circular = introduce(&key(4), 200, 0, "dee", &dee, "it's me");
    let second_laptop = introduce(&key(1), 201, 0, "alex", &alex2, "my other machine");

    let mut roster = founder();
    roster.bind_key(
        p("dee"),
        dee.clone(),
        Some(Vouch {
            op: circular.id.clone(),
            by: dee.clone(),
            at: 200,
        }),
    );
    roster.bind_key(
        p("alex"),
        alex2.clone(),
        Some(Vouch {
            op: second_laptop.id.clone(),
            by: actor_of(&key(1)),
            at: 201,
        }),
    );
    let a = admit(
        &[circular.clone(), second_laptop],
        &[],
        &roster,
        NS,
        &Ed25519Verifier,
    );
    assert_eq!(
        trace(&roster, &a.ops, &p("dee"), &dee),
        VouchStatus::SelfVouch { op: circular.id }
    );
    assert!(
        trace(&roster, &a.ops, &p("alex"), &alex2).is_traced(),
        "a second laptop vouched for by the first is not circular"
    );
}

/// **"Claimed AT THAT TIME" is the half a present-tense membership check
/// misses.** Dee vouches for eve, and dee's own introduction is dated after
/// the one they signed. Both rows are in the roster now, so a check asking
/// only "is the signer a member" passes; the chain is still backwards.
#[test]
fn an_introducer_admitted_after_the_op_they_signed_does_not_resolve() {
    let (dee, eve) = (actor_of(&key(4)), actor_of(&key(5)));
    let eve_intro = introduce(&key(4), 200, 1, "eve", &eve, "my friend");
    let dee_intro = introduce(&key(1), 300, 0, "dee", &dee, "sold me the drill");

    let mut roster = founder();
    roster.bind_key(
        p("dee"),
        dee.clone(),
        Some(Vouch {
            op: dee_intro.id.clone(),
            by: actor_of(&key(1)),
            at: 300,
        }),
    );
    roster.bind_key(
        p("eve"),
        eve.clone(),
        Some(Vouch {
            op: eve_intro.id.clone(),
            by: dee.clone(),
            at: 200,
        }),
    );
    let a = admit(
        &[eve_intro.clone(), dee_intro],
        &[],
        &roster,
        NS,
        &Ed25519Verifier,
    );
    assert!(
        a.ops.iter().any(|o| o.id == eve_intro.id),
        "the op itself is admitted — the roster claims dee NOW, which is why \
         a present-tense check passes: {:?}",
        a.gaps
    );
    assert_eq!(
        trace(&roster, &a.ops, &p("eve"), &eve),
        VouchStatus::IntroducerNotClaimedYet {
            op: eve_intro.id,
            by_actor: dee,
        }
    );
}

/// **The op is authoritative and the row is not quietly corrected to match
/// it.** `by` and `at` on a row are a legible copy; a row that disagrees with
/// the op it names is refused rather than rendered from whichever half the
/// renderer happened to read (ARCH §18.3).
#[test]
fn a_row_whose_stored_signer_disagrees_with_the_op_does_not_resolve() {
    let dee = actor_of(&key(4));
    let intro = introduce(&key(1), 200, 0, "dee", &dee, "sold me the drill");
    let mut roster = founder();
    roster.bind_key(p("bo"), actor_of(&key(2)), None);
    roster.bind_key(
        p("dee"),
        dee.clone(),
        Some(Vouch {
            op: intro.id.clone(),
            // bo did not sign this.
            by: actor_of(&key(2)),
            at: 200,
        }),
    );
    let a = admit(&[intro.clone()], &[], &roster, NS, &Ed25519Verifier);
    assert_eq!(
        trace(&roster, &a.ops, &p("dee"), &dee),
        VouchStatus::DisagreesWithTheOp { op: intro.id }
    );
}

/// **A roster written before this rung still reads.** The literal bytes of a
/// deployed `roster.json` — no `vouches` key anywhere — parse to the same
/// members they always did, and every row reads as warrant-unknown rather
/// than as a parse error. An existing file that failed to parse would be an
/// outage on every ring already on the mesh.
#[test]
fn a_roster_written_before_this_rung_reads_as_warrant_unknown() {
    let before = r#"{
  "members": {
    "alex": [
      "aaaa",
      "bbbb"
    ],
    "bo": [
      "cccc"
    ]
  }
}"#;
    let roster: Roster = serde_json::from_str(before).expect("a deployed roster.json must parse");
    assert_eq!(roster.members.len(), 2);
    assert_eq!(roster.members[&p("alex")], vec!["aaaa", "bbbb"]);
    assert!(roster.vouches.is_empty());
    for key_hex in ["aaaa", "bbbb", "cccc"] {
        assert_eq!(roster.vouch_for(key_hex), None);
    }
    assert_eq!(
        trace(&roster, &[], &p("alex"), "aaaa"),
        VouchStatus::Unknown,
        "no warrant is UNKNOWN, and unknown is not traced"
    );
    assert!(!VouchStatus::Unknown.is_traced());

    // And it round-trips to the same bytes: a roster with nothing to say
    // about provenance is written exactly as it was before the field existed,
    // so upgrading a node does not rewrite every ring's roster file.
    assert_eq!(serde_json::to_string_pretty(&roster).unwrap(), before);
}

/// **An introduction is an opaque payload and admission has no branch for
/// it.** The rail knows delivery, authenticity and convergence; if `admit`
/// ever learned what this act means, this test is where it would show — an
/// introduction folds through exactly like an expense, and `applies()` is
/// true because it is a `Record` and for no other reason.
#[test]
fn admission_carries_an_introduction_without_reading_it() {
    let dee = actor_of(&key(4));
    let intro = introduce(&key(1), 200, 0, "dee", &dee, "sold me the drill");
    let a = admit(&[intro.clone()], &[], &founder(), NS, &Ed25519Verifier);
    assert!(a.is_complete(), "{:?}", a.gaps);
    let op = &a.ops[0];
    assert!(
        op.applies(),
        "no special case: it is a Record like any other"
    );
    assert_eq!(
        Introduce::from_payload(op.payload.as_ref().unwrap()),
        Some(Introduce::new(p("dee"), dee, "sold me the drill")),
        "and the READER is what understands it"
    );
}

/// An app's own act is not claimed as an introduction because it shares a
/// word. The shape is the act.
#[test]
fn a_payload_that_is_not_the_introduce_shape_is_not_an_introduction() {
    for not_one in [
        serde_json::json!({ "kind": "thing", "what": "drill" }),
        serde_json::json!({ "kind": "introduce" }),
        serde_json::json!({ "kind": "introduce", "person": "dee", "key": "aaaa" }),
        serde_json::json!({ "kind": "introduce", "person": "dee", "key": 4, "reason": "x" }),
    ] {
        let payload = Payload::new(not_one.clone()).unwrap();
        assert_eq!(
            Introduce::from_payload(&payload),
            None,
            "{not_one} was read as an introduction"
        );
    }
}

/// The two renderings every surface that shows an op to a person uses. The
/// leap day is where a hand-rolled civil-from-days conversion goes wrong.
#[test]
fn a_stamp_reads_as_a_date_a_person_can_match_to_a_conversation() {
    assert_eq!(short_stamp(0), "1970-01-01 00:00");
    assert_eq!(short_stamp(1_788_048_000), "2026-08-30 00:00");
    assert_eq!(short_stamp(1_709_164_800), "2024-02-29 00:00");
    assert_eq!(short_id("ring-0123456789abcdef"), "ring-0123456…");
    assert_eq!(short_id("short"), "short");
}

/// **The bar, counted rather than asserted.** `ra-introduction-is-traceable`
/// is a FRACTION — rows written through the introduction path that resolve to
/// an existing, verifying, same-ring `Introduce` — and its goodhart line says
/// 1.0 over one row proves nothing. So this builds the chain a ring actually
/// grows by: alex founds it, alex brings bo, bo brings cy, cy brings dee.
/// Three rows carry a warrant, three resolve, and each one names the person
/// who vouched rather than a key.
#[test]
fn every_row_written_through_the_introduction_path_traces_to_its_signer() {
    let keys = [("alex", 1u8), ("bo", 2), ("cy", 3), ("dee", 4)];
    let mut roster = Roster::default();
    roster.bind_key(p("alex"), actor_of(&key(1)), None);

    // Each introduction is signed by the person admitted on the one before,
    // which is the chain SPKI/SDSI calls a linked local name.
    let mut ops = Vec::new();
    for (i, (who, seed)) in keys.iter().enumerate().skip(1) {
        let (by_name, by_seed) = keys[i - 1];
        let ts = 200 + i as i64;
        let op = introduce(
            &key(by_seed),
            ts,
            0,
            who,
            &actor_of(&key(*seed)),
            &format!("{by_name} knows {who}"),
        );
        roster.bind_key(
            p(who),
            actor_of(&key(*seed)),
            Some(Vouch {
                op: op.id.clone(),
                by: actor_of(&key(by_seed)),
                at: ts,
            }),
        );
        ops.push(op);
    }

    let a = admit(&ops, &[], &roster, NS, &Ed25519Verifier);
    assert!(a.is_complete(), "{:?}", a.gaps);

    let rows: Vec<(Person, String, VouchStatus)> = roster
        .members
        .iter()
        .flat_map(|(person, ks)| {
            ks.iter()
                .map(|k| (person.clone(), k.clone(), trace(&roster, &a.ops, person, k)))
        })
        .collect();
    let written_here: Vec<_> = rows
        .iter()
        .filter(|(_, k, _)| roster.vouch_for(k).is_some())
        .collect();
    let traced = written_here
        .iter()
        .filter(|(_, _, s)| s.is_traced())
        .count();

    assert_eq!(written_here.len(), 3, "three rows carry a warrant");
    assert_eq!(
        traced,
        written_here.len(),
        "ra-introduction-is-traceable = {traced}/{}: {:?}",
        written_here.len(),
        written_here
            .iter()
            .filter(|(_, _, s)| !s.is_traced())
            .map(|(who, _, s)| format!("{who}: {s}"))
            .collect::<Vec<_>>()
    );
    // And each one names a PERSON, which is the whole point of asking.
    for (who, k, status) in &written_here {
        let VouchStatus::Traced { by, .. } = status else {
            unreachable!()
        };
        let expected = keys[keys.iter().position(|(n, _)| n == &who.as_str()).unwrap() - 1].0;
        assert_eq!(by, &p(expected), "{who} ({k}) was brought in by {expected}");
    }
    // The founder is the denominator's honest exclusion: no warrant, not a
    // failure, and NOT counted as traced.
    assert_eq!(
        trace(&roster, &a.ops, &p("alex"), &actor_of(&key(1))),
        VouchStatus::Unknown
    );
}
