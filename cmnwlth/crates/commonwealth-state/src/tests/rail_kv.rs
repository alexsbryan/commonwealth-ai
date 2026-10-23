use super::*;
use commonwealth_rail_core::tests_support::{admitted, key, signed};
use commonwealth_rail_core::{OpId, RailAct};

/// One store write as the act a node would have signed. `ts` is the
/// JOURNAL line's time and `t` is the WRITE's — they are deliberately
/// different in these tests, because a fold that confuses them passes
/// every test where they agree.
fn write(k: &str, v: Option<&[u8]>, t: u64) -> RailAct {
    RailAct::Record {
        payload: to_payload(k, v, t).unwrap(),
    }
}

/// The act that closes a snapshot, as its author would have signed it.
fn mark(floor: u64) -> RailAct {
    RailAct::Record {
        payload: snapshot_mark(floor).unwrap(),
    }
}

/// What the fold says `actor` still asserts, or `None` when it makes no
/// claim at all — the two are different facts and this keeps them so.
fn live_set(p: &Projection, k: &commonwealth_rail_core::SigningKey) -> Option<Vec<String>> {
    p.sealed_actors
        .get(&commonwealth_rail_core::actor_of(k))
        .map(|keys| keys.iter().cloned().collect())
}

fn value_of(p: &Projection, key: &str) -> Option<Bytes> {
    p.rows
        .iter()
        .find(|r| r.key == key)
        .and_then(|r| r.value.clone())
}

#[test]
fn a_value_round_trips_through_the_payload() {
    // Non-UTF-8 on purpose: a store value is `Bytes` and JSON has no byte
    // string, which is the whole reason for base64.
    let raw = [0u8, 159, 146, 150, b'a'];
    let p = to_payload("shard:7", Some(&raw), 1_700_000_000).unwrap();
    let back = from_payload(&p).unwrap();
    assert_eq!(back.key, "shard:7");
    assert_eq!(back.value.as_deref(), Some(&raw[..]));
    assert_eq!(back.t, 1_700_000_000);
}

#[test]
fn a_tombstone_carries_no_value() {
    let p = to_payload("gone", None, 42).unwrap();
    assert_eq!(p.as_value().get(F_VALUE), None, "a tombstone has no `v`");
    assert_eq!(p.as_value().get(F_DELETED), Some(&Value::Bool(true)));
    let back = from_payload(&p).unwrap();
    assert_eq!(back.value, None);
    assert_eq!(back.t, 42);
}

/// An EMPTY value is not a tombstone. Both spellings exist on the wire
/// (`"v": ""` with `"d": false`, versus no `v` with `"d": true`) and
/// collapsing them would make `set(k, b"")` delete the key on every peer.
#[test]
fn an_empty_value_is_not_a_tombstone() {
    let p = to_payload("k", Some(b""), 9).unwrap();
    let back = from_payload(&p).unwrap();
    assert_eq!(back.value, Some(Bytes::new()));
    assert_ne!(back.value, None);
}

/// The named failing inputs. Each is a line this build must call
/// unreadable rather than guess at.
#[test]
fn a_line_this_build_cannot_read_is_refused_rather_than_guessed() {
    let cases: Vec<(&str, Value)> = vec![
        (
            "another app's act",
            json!({"kind": "mesh-measurement", "wire": "{}"}),
        ),
        ("no key", json!({"t": 1, "d": false, "v": "aGk="})),
        ("no time", json!({"k": "a", "d": false, "v": "aGk="})),
        ("no deleted flag", json!({"k": "a", "t": 1, "v": "aGk="})),
        // Contradictory: says it is a delete AND carries a value.
        (
            "tombstone with a value",
            json!({"k": "a", "t": 1, "d": true, "v": "aGk="}),
        ),
        // Contradictory the other way: not a delete and no value.
        (
            "a live act with no value",
            json!({"k": "a", "t": 1, "d": false}),
        ),
        (
            "not base64",
            json!({"k": "a", "t": 1, "d": false, "v": "!!!!"}),
        ),
        (
            "a negative time",
            json!({"k": "a", "t": -1, "d": false, "v": "aGk="}),
        ),
    ];
    for (why, v) in cases {
        let p = Payload::new(v).unwrap();
        assert!(
            from_payload(&p).is_none(),
            "{why} was read as a store write"
        );
    }
}

/// `Payload` refuses fractional numbers, so a `t` that is not a whole
/// number cannot even be constructed. Stated here because the module's
/// choice of unix SECONDS rests on it.
#[test]
fn a_fractional_time_cannot_be_a_payload_at_all() {
    assert!(Payload::new(json!({"k": "a", "t": 1.5, "d": false, "v": "aGk="})).is_err());
}

/// **LWW, and the control.** The rail's total order is
/// `(ts_unix, actor, seq, id)` — when a line was written DOWN. That is not the
/// order last-write-wins needs, and the difference only shows when the two
/// disagree: here the second act is later on the journal and older as a
/// write.
#[test]
fn a_lower_t_arriving_later_does_not_overwrite() {
    let alex = key(1);
    let a = admitted(&[
        signed(&alex, 100, 0, write("k", Some(b"first"), 500)),
        // Later line, OLDER write. Must lose.
        signed(&alex, 200, 1, write("k", Some(b"stale"), 400)),
    ]);
    let p = project(&a);
    assert_eq!(p.rows.len(), 1);
    assert_eq!(value_of(&p, "k").as_deref(), Some(&b"first"[..]));
    assert_eq!(p.rows[0].t, 500);

    // The control: a genuinely newer write DOES take it, so the assertion
    // above is about `t` and not about "the fold never updates".
    let a = admitted(&[
        signed(&alex, 100, 0, write("k", Some(b"first"), 500)),
        signed(&alex, 200, 1, write("k", Some(b"stale"), 400)),
        signed(&alex, 300, 2, write("k", Some(b"newest"), 600)),
    ]);
    assert_eq!(value_of(&project(&a), "k").as_deref(), Some(&b"newest"[..]));
}

/// **A snapshot re-append keeps its position.** After a seal the writer
/// re-appends its live rows so the floor can move without the live set
/// evaporating. Those lines are NEW — a much later `ts_unix`, a higher
/// `seq` — and the writes they carry are not. A fold ordering on the line
/// would hand every key back to whoever snapshotted most recently.
#[test]
fn a_snapshot_re_append_keeps_its_lww_position() {
    let (alex, bo) = (key(1), key(2));
    let a = admitted(&[
        signed(&alex, 100, 0, write("k", Some(b"alex-old"), 100)),
        // bo overwrote it.
        signed(&bo, 110, 0, write("k", Some(b"bo-new"), 200)),
        // alex seals and snapshots MUCH later — same write, original `t`.
        signed(&alex, 9_000, 1, write("k", Some(b"alex-old"), 100)),
    ]);
    let p = project(&a);
    assert_eq!(
        value_of(&p, "k").as_deref(),
        Some(&b"bo-new"[..]),
        "a snapshot re-append must not resurrect the write it carries"
    );
    assert_eq!(p.rows[0].t, 200);
}

/// A tombstone is an act like any other and loses to a newer write.
#[test]
fn a_tombstone_is_ordered_by_t_like_every_other_act() {
    let alex = key(1);
    let a = admitted(&[
        signed(&alex, 100, 0, write("k", Some(b"v"), 100)),
        signed(&alex, 200, 1, write("k", None, 200)),
    ]);
    let p = project(&a);
    assert_eq!(p.rows.len(), 1, "the tombstone is a row, not an absence");
    assert_eq!(p.rows[0].value, None);

    // Written again after the delete, and it comes back.
    let a = admitted(&[
        signed(&alex, 100, 0, write("k", Some(b"v"), 100)),
        signed(&alex, 200, 1, write("k", None, 200)),
        signed(&alex, 300, 2, write("k", Some(b"again"), 300)),
    ]);
    assert_eq!(value_of(&project(&a), "k").as_deref(), Some(&b"again"[..]));
}

/// **A foreign act costs one `unreadable`, not a key.** `mesh-measurements`
/// rides the same rail with its own vocabulary, and a peer on a shape this
/// build does not know is the same case. Both are COUNTED (ARCH §18.3) —
/// an empty projection beside a non-zero `unreadable` is a very different
/// fact from an empty projection beside a quiet ring.
#[test]
fn an_act_this_build_cannot_read_is_counted_not_dropped() {
    let alex = key(1);
    let foreign = RailAct::Record {
        payload: Payload::new(json!({"kind": "mesh-measurement", "wire": "{}"})).unwrap(),
    };
    let a = admitted(&[
        signed(&alex, 100, 0, write("k", Some(b"v"), 100)),
        signed(&alex, 200, 1, foreign),
    ]);
    let p = project(&a);
    assert_eq!(p.rows.len(), 1, "the readable key still projects");
    assert_eq!(p.unreadable, 1, "the foreign act is reported");
}

/// **A tie is broken the same way on every node.** Two nodes writing one
/// key in one second used to leave the mesh DIVERGED — `upsert_if_newer`
/// keeps the incumbent, so each node kept whatever it saw first. On the
/// rail there is no arrival order: the tie-break is `(actor, id)`, both of
/// which every node reads off the same signed line.
#[test]
fn a_tie_on_t_is_broken_the_same_way_on_every_node() {
    let (alex, bo) = (key(1), key(2));
    let one = signed(&alex, 100, 0, write("k", Some(b"alex"), 777));
    let two = signed(&bo, 100, 0, write("k", Some(b"bo"), 777));

    let forwards = project(&admitted(&[one.clone(), two.clone()]));
    let backwards = project(&admitted(&[two, one]));
    assert_eq!(
        forwards, backwards,
        "the fold must not depend on the order lines arrived in"
    );
    assert_eq!(forwards.rows.len(), 1);
    assert!(forwards.rows[0].value.is_some());
}

/// One actor, one key, two writes in one second: the later `seq` wins on
/// every node. The old last term was the op id, a content hash — so the
/// pair of values below is CHOSEN so that the earlier write's id sorts
/// higher, which is exactly the case the id rule got backwards.
#[test]
fn two_writes_by_one_actor_in_one_second_fold_in_program_order() {
    let alex = key(1);
    let mut picked = None;
    for n in 0..64u8 {
        let first = signed(&alex, 100, 0, write("k", Some(&[b'a', n]), 777));
        let second = signed(&alex, 100, 1, write("k", Some(&[b'b', n]), 777));
        if first.id.as_str() > second.id.as_str() {
            picked = Some((first, second, vec![b'b', n]));
            break;
        }
    }
    let (first, second, expected) = picked.expect("a value pair whose ids sort against seq");
    let p = project(&admitted(&[first.clone(), second.clone()]));
    assert_eq!(p.rows.len(), 1);
    assert_eq!(
        p.rows[0].value.as_deref(),
        Some(&expected[..]),
        "the second write in program order must win: {:?}",
        p.rows[0]
    );
    let reversed = project(&admitted(&[second, first]));
    assert_eq!(p, reversed, "arrival order must not matter");
}

/// A correction voids what it names, and `applied()` is the ONE definition
/// of surviving — this fold never re-derives it.
#[test]
fn a_voided_act_cannot_win_a_key() {
    let alex = key(1);
    let first = signed(&alex, 100, 0, write("k", Some(b"wrong"), 100));
    let void = signed(
        &alex,
        200,
        1,
        RailAct::Correct {
            corrects: OpId::from_raw(first.id.as_str()),
            replacement: None,
        },
    );
    let p = project(&admitted(&[first, void]));
    assert!(
        p.rows.is_empty(),
        "a voided write is not a tombstone and not a value; it never happened"
    );
    assert_eq!(p.unreadable, 0, "a void carries no payload to fail to read");
}

/// **The two shapes on this vocabulary do not overlap.** The mark is read
/// by field set exactly as a store write is, so the two parsers must
/// refuse each other's acts — otherwise a key called `snap` or an extra
/// field could turn a row into a claim about a whole live set.
#[test]
fn a_mark_is_not_a_store_write_and_a_store_write_is_not_a_mark() {
    let m = snapshot_mark(2_005).unwrap();
    assert_eq!(read_snapshot_mark(&m), Some(2_005));
    assert!(
        from_payload(&m).is_none(),
        "a mark must never project as a store row"
    );

    let w = to_payload("snap", Some(b"v"), 9).unwrap();
    assert!(
        read_snapshot_mark(&w).is_none(),
        "a key named `snap` is a key"
    );
    assert!(
        read_snapshot_mark(&Payload::new(json!({"snap": 1, "k": "a"})).unwrap()).is_none(),
        "a mark carries ONE field; anything else is another act"
    );
    assert!(read_snapshot_mark(&Payload::new(json!({"snap": -1})).unwrap()).is_none());
}

/// **A closed snapshot IS the actor's live set, and a retired key is not
/// in it.** `gone` was written before the seal and never re-appended —
/// which is what a tombstone this node published looks like once the seal
/// that retired it lands. It is still a ROW (the retired op is on this
/// disk until a prune takes it), and it is not in the live set. That gap
/// between the two is the whole reconciliation.
#[test]
fn a_closed_snapshot_reports_the_actors_whole_live_set() {
    let alex = key(1);
    let a = admitted(&[
        signed(&alex, 100, 0, write("gone", Some(b"stale"), 100)),
        signed(&alex, 101, 1, write("kept", Some(b"v"), 101)),
        signed(&alex, 900, 2, RailAct::Seal),
        signed(&alex, 901, 3, write("kept", Some(b"v"), 101)),
        signed(&alex, 902, 4, mark(2)),
    ]);
    let p = project(&a);
    assert_eq!(
        live_set(&p, &alex).as_deref(),
        Some(&["kept".to_string()][..]),
        "the live set is what the snapshot re-appended, and nothing else"
    );
    assert!(
        !p.rows.iter().any(|r| r.key == "gone"),
        "an op below its author's floor is retired, whether or not this node \
         has pruned it off disk yet"
    );
    // …and that is why the live set has to be reported separately: the fold
    // no longer mentions `gone` at all, so nothing in `rows` would ever take
    // the store row this node wrote before the seal.
    assert_eq!(p.unreadable, 0, "the mark is this build's own vocabulary");
}

/// **A tombstone above the floor is not live either**, and this is the
/// case where the two answers must agree: the row says delete, the live
/// set says the author no longer asserts it.
#[test]
fn a_post_seal_tombstone_is_out_of_the_live_set() {
    let alex = key(1);
    let a = admitted(&[
        signed(&alex, 900, 0, RailAct::Seal),
        signed(&alex, 901, 1, write("k", Some(b"v"), 500)),
        signed(&alex, 902, 2, mark(0)),
        signed(&alex, 903, 3, write("k", None, 600)),
    ]);
    let p = project(&a);
    assert_eq!(live_set(&p, &alex).as_deref(), Some(&[][..]));
    assert_eq!(p.rows.len(), 1);
    assert_eq!(p.rows[0].value, None, "the row is the tombstone");
}

/// **A half-arrived snapshot claims NOTHING — and the cheap gates would
/// both have claimed something.** The ring's pull is chunked, so this is
/// the state a node is really in between two chunks. Note the second
/// assertion: `is_complete()` is TRUE here, because the hole audit runs
/// from the floor and the seal IS the floor. A completeness gate would
/// have retired every row this actor owns (ARCH §18.2).
#[test]
fn a_snapshot_still_arriving_makes_no_claim_about_the_live_set() {
    let alex = key(1);
    let before = [
        signed(&alex, 100, 0, write("a", Some(b"1"), 100)),
        signed(&alex, 101, 1, write("b", Some(b"2"), 101)),
    ];
    // Chunk 1: the seal, and nothing above it yet.
    let a = admitted(&[
        before[0].clone(),
        before[1].clone(),
        signed(&alex, 900, 2, RailAct::Seal),
    ]);
    let p = project(&a);
    assert_eq!(live_set(&p, &alex), None, "no mark, no claim");
    assert!(
        a.is_complete(),
        "and `is_complete()` says yes — which is why it is not the gate"
    );

    // Chunk 2, still partial: one row of the snapshot, no mark.
    let a = admitted(&[
        before[0].clone(),
        before[1].clone(),
        signed(&alex, 900, 2, RailAct::Seal),
        signed(&alex, 901, 3, write("a", Some(b"1"), 100)),
    ]);
    assert_eq!(
        live_set(&project(&a), &alex),
        None,
        "one row above the floor is not a snapshot; the other row would              have been retired on the strength of it"
    );

    // The rest of it, and the claim is made.
    let a = admitted(&[
        before[0].clone(),
        before[1].clone(),
        signed(&alex, 900, 2, RailAct::Seal),
        signed(&alex, 901, 3, write("a", Some(b"1"), 100)),
        signed(&alex, 902, 4, write("b", Some(b"2"), 101)),
        signed(&alex, 903, 5, mark(2)),
    ]);
    assert_eq!(
        live_set(&project(&a), &alex),
        Some(vec!["a".to_string(), "b".to_string()])
    );
}

/// **A hole in the run below the mark withdraws the claim.** The mark
/// alone would be believed by a node that ingested it out of order, or
/// that could not parse one line of the snapshot — and the missing line is
/// exactly the row that would then be retired.
#[test]
fn a_hole_under_the_mark_withdraws_the_claim() {
    let alex = key(1);
    let held = [
        signed(&alex, 900, 0, RailAct::Seal),
        signed(&alex, 901, 1, write("a", Some(b"1"), 100)),
        // seq 2 — the row for "b" — never arrived.
        signed(&alex, 903, 3, mark(0)),
    ];
    let a = admitted(&held);
    assert!(!a.is_complete(), "the hole is reported");
    assert_eq!(live_set(&project(&a), &alex), None);
}

/// **A mark for an OLDER seal closes nothing.** A node that has not
/// compacted still holds the previous snapshot and its mark; reading that
/// one as current would claim a live set two seals out of date.
#[test]
fn a_mark_from_an_earlier_seal_does_not_close_the_current_one() {
    let alex = key(1);
    let a = admitted(&[
        signed(&alex, 900, 0, RailAct::Seal),
        signed(&alex, 901, 1, write("a", Some(b"1"), 100)),
        signed(&alex, 902, 2, mark(0)),
        // The second seal. Its snapshot has not arrived.
        signed(&alex, 903, 3, RailAct::Seal),
    ]);
    assert_eq!(live_set(&project(&a), &alex), None);
}

/// **An actor that never sealed is ABSENT, not empty** (ARCH §18.3). The
/// two mean opposite things to a reconciliation: absent is "I know nothing
/// about their whole set", empty is "they assert nothing".
#[test]
fn an_actor_that_never_sealed_is_absent_rather_than_empty() {
    let (alex, bo) = (key(1), key(2));
    let a = admitted(&[
        signed(&alex, 100, 0, write("a", Some(b"1"), 100)),
        signed(&bo, 900, 0, RailAct::Seal),
        signed(&bo, 901, 1, mark(0)),
    ]);
    let p = project(&a);
    assert_eq!(live_set(&p, &alex), None, "alex has not sealed");
    assert_eq!(
        live_set(&p, &bo),
        Some(vec![]),
        "bo sealed with nothing live and says so — the K7 case, and the              one a `some op above the floor` gate cannot see"
    );
}

/// **One actor's seal says nothing about another's rows.** The per-actor
/// fold is over that actor's OWN ops, so a key another node won is not in
/// this one's live set — and the store matches on origin, so it is not
/// this one's row either.
#[test]
fn a_sealed_actors_live_set_is_only_its_own_writes() {
    let (alex, bo) = (key(1), key(2));
    let a = admitted(&[
        signed(&bo, 100, 0, write("shared", Some(b"bo"), 500)),
        signed(&alex, 900, 0, RailAct::Seal),
        signed(&alex, 901, 1, write("mine", Some(b"alex"), 100)),
        signed(&alex, 902, 2, mark(0)),
    ]);
    let p = project(&a);
    assert_eq!(
        live_set(&p, &alex),
        Some(vec!["mine".to_string()]),
        "bo's key is not alex's to retire"
    );
    assert_eq!(value_of(&p, "shared").as_deref(), Some(&b"bo"[..]));
}
