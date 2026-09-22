use super::*;

/// A caller at position 1 is not queued behind other callers — it is
/// waiting out the turn already running. Charging it a WHOLE turn is what
/// turned a wait bound into a ban on slow turns.
#[test]
fn an_almost_finished_turn_is_not_charged_as_a_whole_one() {
    let eta = EtaEwma::new(31_000);
    // Nothing elapsed: a full turn, as before.
    assert_eq!(eta.predict_wait_ms(1, 1, 0), 31_000);
    // 30s in, the honest wait is 1s — the caller is SERVED where it used
    // to be refused.
    assert_eq!(eta.predict_wait_ms(1, 1, 30_000), 1_000);
    // A turn that has outrun the average predicts 0, never underflows.
    assert_eq!(eta.predict_wait_ms(1, 1, 90_000), 0);
    // Real queueing still predicts real waits.
    assert_eq!(eta.predict_wait_ms(3, 1, 1_000), 3 * 31_000 - 1_000);
    // Parallel drain is still honest.
    assert_eq!(eta.predict_wait_ms(4, 2, 0), 2 * 31_000);
}

/// Named failing input (ARCH §18.1), taken from production rather than
/// invented: `avg_turn_ms = 30_690` against the 30_000 default bound, at
/// position 1 with an EMPTY queue. That is the median of 625 sheds
/// observed on a 27B host on 2026-08-26, 624 of them at position 1 — the
/// host refused all concurrency because its own turns are naturally
/// slower than the bound (note `bf432b4d`).
#[test]
fn the_bound_is_a_wait_bound_not_a_ban_on_slow_turns() {
    const BOUND: u64 = 30_000;
    let eta = EtaEwma::new(30_690);
    // The defect, preserved: with nothing elapsed the prediction clears
    // the bound and the caller is shed.
    assert!(eta.predict_wait_ms(1, 1, 0) > BOUND);
    // The fix: one second into the in-flight turn the honest remaining
    // wait is under the bound, and the caller is served.
    assert!(eta.predict_wait_ms(1, 1, 1_000) < BOUND);
    // And genuine congestion still sheds — two callers already parked.
    assert!(eta.predict_wait_ms(3, 1, 1_000) > BOUND);
}

// String keys keep the policy tests domain-free.
fn core(slots: usize, depth: usize) -> SchedCore<&'static str> {
    SchedCore::new(slots, depth)
}

#[test]
fn grants_until_slots_exhausted_then_queues() {
    let mut c = core(2, 32);
    assert_eq!(c.admit("a", 1.0, 1), AdmitOutcome::Granted);
    assert_eq!(c.admit("b", 1.0, 1), AdmitOutcome::Granted);
    assert_eq!(c.available(), 0);
    match c.admit("c", 1.0, 1) {
        AdmitOutcome::Enqueued { seq, status } => {
            assert_eq!(seq, 0);
            assert_eq!(status.position, 1);
        }
        other => panic!("expected Enqueued, got {other:?}"),
    }
}

#[test]
fn per_user_cap_blocks_even_with_free_slots() {
    // 4 slots but a cap of 1: one origin can't take a second slot even
    // though three sit free.
    let mut c = core(4, 32);
    assert_eq!(c.admit("a", 1.0, 1), AdmitOutcome::Granted);
    match c.admit("a", 1.0, 1) {
        AdmitOutcome::Enqueued { status, .. } => assert_eq!(status.position, 1),
        other => panic!("same origin should be capped, got {other:?}"),
    }
    assert_eq!(c.available(), 3, "free slots stay reserved for others");
    // A different origin still gets a free slot immediately.
    assert_eq!(c.admit("b", 1.0, 1), AdmitOutcome::Granted);
}

#[test]
fn reciprocity_cap_lets_a_contributor_hold_more() {
    // Same origin, but a cap of 2 (a contributor's higher allowance) lets
    // it hold two concurrent slots where a cap-1 consumer could not.
    let mut c = core(4, 32);
    assert_eq!(c.admit("contrib", 2.0, 2), AdmitOutcome::Granted);
    assert_eq!(
        c.admit("contrib", 2.0, 2),
        AdmitOutcome::Granted,
        "cap 2 → a second concurrent slot is allowed"
    );
    match c.admit("contrib", 2.0, 2) {
        AdmitOutcome::Enqueued { .. } => {}
        other => panic!("third should queue at cap 2, got {other:?}"),
    }
}

#[test]
fn weighted_dequeue_order_highest_first_fifo_tiebreak() {
    let mut c = core(1, 32);
    assert_eq!(c.admit("holder", 1.0, 1), AdmitOutcome::Granted);

    // seq0 low(1.0), seq1 high1(2.0), seq2 high2(2.0 — same as high1).
    let _ = c.admit("low", 1.0, 1); // seq0
    let _ = c.admit("high1", 2.0, 1); // seq1
    let _ = c.admit("high2", 2.0, 1); // seq2

    assert_eq!(c.position_of(1), 1, "highest weight, enqueued first");
    assert_eq!(c.position_of(2), 2, "equal weight, later seq");
    assert_eq!(c.position_of(0), 3, "lowest weight last");

    // Drain: release the holder, claim whoever's promoted, repeat.
    let keys: HashMap<u64, &'static str> = [(0, "low"), (1, "high1"), (2, "high2")]
        .into_iter()
        .collect();
    let mut served = Vec::new();
    let mut holder = "holder";
    loop {
        let woken = c.release(&holder);
        let Some(seq) = woken.first().copied() else {
            break;
        };
        assert_eq!(c.claim_or_status(seq), ClaimOrStatus::Claimed);
        served.push(seq);
        holder = keys[&seq];
        if c.waiting_len() == 0 {
            break;
        }
    }
    assert_eq!(served, vec![1, 2, 0], "served high-weight FIFO then low");
}

#[test]
fn sheds_at_depth_cap() {
    let mut c = core(1, 2); // 1 slot, depth 2
    assert_eq!(c.admit("h", 1.0, 4), AdmitOutcome::Granted);
    assert!(matches!(
        c.admit("w1", 1.0, 4),
        AdmitOutcome::Enqueued { .. }
    ));
    assert!(matches!(
        c.admit("w2", 1.0, 4),
        AdmitOutcome::Enqueued { .. }
    ));
    assert_eq!(
        c.admit("w3", 1.0, 4),
        AdmitOutcome::Shed {
            would_be_position: 3
        },
        "queue full at depth 2 → shed"
    );
}

#[test]
fn try_grant_never_queues_and_hints_position() {
    let mut c = core(1, 32);
    assert_eq!(c.try_grant("h", 1.0, 4), TryGrant::Granted);
    assert_eq!(
        c.try_grant("r", 1.0, 4),
        TryGrant::WouldQueue { position: 1 }
    );
    assert_eq!(c.waiting_len(), 0, "REST attempt left no waiter behind");
}

#[test]
fn position_is_monotonic_as_queue_drains() {
    let mut c = core(1, 32);
    let _ = c.admit("h", 1.0, 4);
    let _ = c.admit("w1", 1.0, 4); // seq0
    let _ = c.admit("w2", 1.0, 4); // seq1
    let _ = c.admit("w3", 1.0, 4); // seq2
    assert_eq!(c.position_of(2), 3);

    let woken = c.release(&"h");
    assert_eq!(woken, vec![0], "FIFO: seq0 promoted first");
    assert_eq!(c.claim_or_status(0), ClaimOrStatus::Claimed);
    assert_eq!(
        c.position_of(2),
        2,
        "position only decreases as line drains"
    );
}

#[test]
fn cancel_of_granted_waiter_returns_the_slot() {
    let mut c = core(1, 32);
    let _ = c.admit("h", 1.0, 4);
    let _ = c.admit("w1", 1.0, 4); // seq0
    let _ = c.admit("w2", 1.0, 4); // seq1

    let woken = c.release(&"h");
    assert_eq!(woken, vec![0]);
    assert_eq!(c.available(), 0, "slot reserved for seq0");

    // seq0's client disconnects before claiming → cancel hands the slot
    // to seq1, not stranding it.
    let re_woken = c.cancel(0);
    assert_eq!(re_woken, vec![1], "cancel re-promoted the next waiter");
    assert_eq!(c.claim_or_status(1), ClaimOrStatus::Claimed);
}

#[test]
fn cancel_of_waiting_waiter_frees_no_slot() {
    let mut c = core(1, 32);
    let _ = c.admit("h", 1.0, 4);
    let _ = c.admit("w1", 1.0, 4); // seq0, waiting (not granted)
    assert_eq!(c.cancel(0), Vec::<u64>::new(), "no slot to return");
    assert_eq!(c.available(), 0, "holder still holds the only slot");
    assert_eq!(c.waiting_len(), 0, "waiter removed cleanly");
}

#[test]
fn record_turn_updates_ewma() {
    let mut c = core(1, 32);
    let before = c.eta.avg_turn_ms();
    c.record_turn(0);
    assert!(c.eta.avg_turn_ms() < before, "EWMA moved toward the sample");
    assert_eq!(c.eta.avg_turn_ms(), 2250, "(0 + 3·3000)/4");
}

#[test]
fn eta_accounts_for_parallel_slots() {
    let mut c = core(4, 32);
    c.eta = EtaEwma::new(1000);
    assert_eq!(
        c.status(4).estimated_wait_ms,
        1000,
        "4 in 4 slots = 1 batch"
    );
    assert_eq!(c.status(5).estimated_wait_ms, 2000, "5 needs a 2nd batch");
}

#[test]
fn zero_slots_admits_nothing() {
    // 0 is a valid budget now — the peer gate's "reject all" ceiling. No
    // free slot, so `admit` queues and `try_grant` refuses (would-queue).
    let mut c = core(0, 32);
    assert!(
        matches!(c.admit("a", 1.0, 1), AdmitOutcome::Enqueued { .. }),
        "no slot → queues, never grants"
    );
    let mut c2 = core(0, 32);
    assert!(
        matches!(c2.try_grant("a", 1.0, 1), TryGrant::WouldQueue { .. }),
        "no slot → not granted"
    );
}

#[test]
fn reciprocity_weight_normalizes_against_max() {
    assert_eq!(
        reciprocity_weight(100.0, 100.0, 0.5),
        1.5,
        "top contributor → 1+k"
    );
    assert_eq!(reciprocity_weight(50.0, 100.0, 0.5), 1.25, "half → 1+k·0.5");
    assert_eq!(
        reciprocity_weight(0.0, 100.0, 0.5),
        1.0,
        "no contribution → neutral"
    );
    assert_eq!(
        reciprocity_weight(100.0, 0.0, 0.5),
        1.0,
        "no fleet signal → neutral"
    );
    assert_eq!(
        reciprocity_weight(100.0, 100.0, 0.0),
        1.0,
        "k=0 → reciprocity off"
    );
}

// ── Equal-share cap (order serve50-identity) ────────────────────

/// covers: FE-102
#[test]
fn fair_share_cap_is_uncapped_for_a_lone_principal() {
    // The single-principal no-regression arm, as an assertion rather
    // than as prose: alone on the host, a caller's allowance is the
    // same "not rationing" sentinel it had before this rule existed.
    assert_eq!(fair_share_cap(16, 0), u32::MAX);
    assert_eq!(fair_share_cap(16, 1), u32::MAX);
}

#[test]
fn fair_share_cap_divides_the_budget_equally() {
    assert_eq!(fair_share_cap(16, 2), 8);
    assert_eq!(fair_share_cap(16, 4), 4);
    // The §9.3 population: 1 greedy + 9 polite.
    assert_eq!(fair_share_cap(16, 10), 1);
}

#[test]
fn fair_share_cap_never_reaches_zero() {
    // A cap of 0 would refuse EVERY request and make this rule a shed
    // decider. The shed decider is the inference slot queue; this cap
    // must only ever ration, never refuse outright.
    assert_eq!(fair_share_cap(16, 100), 1);
    assert_eq!(fair_share_cap(0, 2), 1);
    assert_eq!(fair_share_cap(1, 50), 1);
    assert_eq!(fair_share_cap(16, usize::MAX), 1);
}

#[test]
fn fair_share_cap_is_identical_for_every_principal() {
    // "Deficit, never weight": the rule takes no argument that could
    // rank one caller above another. Equal share is structural — there
    // is no parameter to pass a contribution weight through.
    let caps: Vec<u32> = (0..8).map(|_| fair_share_cap(24, 6)).collect();
    assert!(caps.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(caps[0], 4);
}

#[test]
fn active_keys_counts_distinct_origins_and_prunes_on_release() {
    let mut c = core(8, 32);
    assert_eq!(c.active_keys(), 0);
    let _ = c.admit("a", 1.0, 4);
    let _ = c.admit("a", 1.0, 4);
    assert_eq!(c.active_keys(), 1, "two turns from one origin is ONE key");
    let _ = c.admit("b", 1.0, 4);
    assert_eq!(c.active_keys(), 2);
    c.release(&"b");
    assert_eq!(c.active_keys(), 1, "an idle origin stops taking a share");
    c.release(&"a");
    c.release(&"a");
    assert_eq!(c.active_keys(), 0);
}

#[test]
fn active_keys_including_counts_a_newcomer_before_it_holds_anything() {
    let mut c = core(8, 32);
    let _ = c.admit("a", 1.0, 4);
    assert_eq!(c.active_keys(), 1);
    assert_eq!(
        c.active_keys_including(&"b"),
        2,
        "a newcomer must widen the denominator BEFORE it is admitted, \
             or the cap engages one turn late"
    );
    assert_eq!(
        c.active_keys_including(&"a"),
        1,
        "an already-active origin must not be double-counted"
    );
}

#[test]
fn equal_share_cap_bounds_a_greedy_origin_against_polite_ones() {
    // The §9.3 shape in miniature, at the policy level: one origin
    // offering far more load than the others must not end up holding
    // more than its equal share of the slots.
    let budget = 16;
    let mut c = core(usize::MAX, 32); // no global ceiling: the cap is the only rule
    let principals = [
        "greedy", "p1", "p2", "p3", "p4", "p5", "p6", "p7", "p8", "p9",
    ];
    // Everyone takes one turn, so all ten are active.
    for who in principals {
        let cap = fair_share_cap(budget, c.active_keys_including(&who));
        assert_eq!(c.try_grant(who, 1.0, cap), TryGrant::Granted);
    }
    assert_eq!(c.active_keys(), 10);
    // Now the greedy origin keeps firing. Every further attempt is
    // refused while the other nine hold their single turn each.
    for _ in 0..32 {
        let cap = fair_share_cap(budget, c.active_keys_including(&"greedy"));
        assert_eq!(cap, 1, "ten active principals over a budget of 16");
        assert!(
            !matches!(c.try_grant("greedy", 1.0, cap), TryGrant::Granted),
            "a greedy origin must not exceed its equal share"
        );
    }
    assert_eq!(
        c.inflight_of(&"greedy"),
        1,
        "greedy holds exactly its share, not 32"
    );
    assert_eq!(c.in_flight(), 10, "ten principals, ten turns");
}

#[test]
fn a_lone_greedy_origin_is_not_throttled() {
    // The other half of the same rule, and the no-regression arm: with
    // nobody else on the host, the same greedy origin takes everything
    // it asks for. A cap that fired here would be a pure regression.
    let budget = 16;
    let mut c = core(usize::MAX, 32);
    for _ in 0..32 {
        let cap = fair_share_cap(budget, c.active_keys_including(&"solo"));
        assert_eq!(cap, u32::MAX);
        assert_eq!(c.try_grant("solo", 1.0, cap), TryGrant::Granted);
    }
    assert_eq!(c.inflight_of(&"solo"), 32);
}

#[test]
fn set_slots_grow_promotes_waiters() {
    let mut c = core(1, 32);
    assert_eq!(c.admit("a", 1.0, 4), AdmitOutcome::Granted);
    let _ = c.admit("b", 1.0, 4); // seq0, queued (the 1 slot is full)
    assert_eq!(c.available(), 0);
    // Grow to 2 slots → the queued waiter is promoted at once.
    assert_eq!(c.set_slots(2), vec![0], "grow promotes the queued waiter");
    assert_eq!(c.claim_or_status(0), ClaimOrStatus::Claimed);
    assert_eq!(c.in_flight(), 2);
}

#[test]
fn set_slots_shrink_below_inflight_evicts_no_one() {
    let mut c = core(3, 32);
    for who in ["a", "b", "d"] {
        assert_eq!(c.admit(who, 1.0, 9), AdmitOutcome::Granted);
    }
    assert_eq!(c.in_flight(), 3);
    // Shrink to 1: the 3 in flight keep running; no free slots appear
    // until usage falls back under the new ceiling.
    c.set_slots(1);
    assert_eq!(c.available(), 0, "over-subscribed → no free slots");
    assert_eq!(c.in_flight(), 3, "nobody evicted");
    c.release(&"a");
    assert_eq!(c.available(), 0, "held 2 still ≥ 1");
    c.release(&"b");
    assert_eq!(c.available(), 0, "held 1 == ceiling");
    c.release(&"d");
    assert_eq!(c.available(), 1, "held 0 → one free slot");
}
