// SPDX-License-Identifier: AGPL-3.0-or-later
//! `projection`'s tests. A sibling file only so `projection.rs` stays
//! under ARCH §3.1's 1200-line ceiling — moved verbatim, nothing renamed.
use super::tests_fixture::*;
use super::*;
use crate::act::{Revocation, Submission, WorkActKind};
use commonwealth_rail_core::{Op, SignedOp};

// -------------------------------------------------------------
// The gate: the order-independence property
// -------------------------------------------------------------

/// **The property this module exists to hold, and the one watched red.**
///
/// Two halves, and each has its own failing input:
///
/// 1. *Arrival order.* The same acts handed to `admit` in a different
///    sequence — here, reversed — must produce the identical projection.
///    The failing input is a fold that sorted by anything of its own:
///    `measurements_rail::read` sorts by `measured_at`, and copying that
///    line would make the winner of a contested lease depend on a
///    timestamp the payload carries rather than on the one admission
///    verified.
/// 2. *Void handling.* A retracted `Lease` must contribute nothing —
///    including when its correction sits BEFORE it in the total order,
///    which is what `bo`'s back-dated correction below arranges (the
///    lease is at t=200, the correction at t=150). The failing input is a
///    fold that walks `Admission::ops` instead of `Admission::applied()`:
///    `bo`'s dead lease then wins, `cy`'s real lease becomes a
///    `lost_leases` row, and `cy`'s `Complete` becomes `unreadable` — a
///    unit that ran and reported reads as a unit nobody holds.
///
/// The equivalence is stated against a THIRD admission that never carried
/// the retracted act at all, because "the same as itself" is what a
/// vacuous property test asserts.
#[test]
fn the_projection_is_a_function_of_the_surviving_acts_not_of_the_walk() {
    let (submit, a, _b) = submission();
    let bo_lease = op(2, 200, 0, &lease(&a));

    let with_retraction = vec![
        op(1, 100, 0, &submit),
        bo_lease.clone(),
        correct(2, 150, 1, &bo_lease),
        op(3, 300, 0, &lease(&a)),
        op(3, 400, 1, &completion(&a)),
    ];
    let mut reversed = with_retraction.clone();
    reversed.reverse();
    // Never retracted, never written: the act set the one above reduces to.
    let without = vec![
        op(1, 100, 0, &submit),
        op(3, 300, 0, &lease(&a)),
        op(3, 400, 1, &completion(&a)),
    ];

    let forwards = fold(&with_retraction);
    let backwards = fold(&reversed);
    let clean = fold(&without);

    // (1) arrival order is not an input.
    assert_eq!(
        forwards, backwards,
        "the fold must read admission's total order and add none of its own"
    );
    // (2) the retracted lease contributed nothing, and its correction sat
    //     before it in the total order.
    assert_eq!(
        forwards, clean,
        "a voided act must leave no trace — not a lease, not a lost_leases row, \
             not an unreadable count"
    );

    // And the surviving answer is the one a reader would expect, so the
    // equality above cannot be satisfied by three identically-empty folds.
    assert_eq!(forwards.lost_leases, vec![]);
    assert_eq!(forwards.unreadable, 0);
    assert!(matches!(
        status(&forwards, &a, 400_000),
        WorkUnitStatus::Complete { ref lessee, .. } if *lessee == who(3)
    ));
}

// -------------------------------------------------------------
// The named tests
// -------------------------------------------------------------

/// **Named test.** Failing input: two `Lease` acts for one unit at the
/// SAME second, by two actors, offered to `admit` in both orders. The
/// total order breaks the tie by actor key, so both nodes agree; a fold
/// that took "whoever I saw first" would give the two nodes different
/// winners and two donors would run the same unit.
#[test]
fn two_actors_leasing_one_unit_in_either_order_give_one_winner_and_one_lost_lease() {
    let (submit, a, _b) = submission();
    let forwards = vec![
        op(1, 100, 0, &submit),
        op(2, 200, 0, &lease(&a)),
        op(3, 200, 0, &lease(&a)),
    ];
    let mut backwards = forwards.clone();
    backwards.reverse();

    let one = fold(&forwards);
    let two = fold(&backwards);
    assert_eq!(one, two, "the winner is admission's, not arrival's");

    // bo's key sorts below cy's, so bo holds it and cy is the loser.
    let (bo, cy) = (who(2), who(3));
    assert!(bo.as_str() < cy.as_str(), "the fixture's tie-break premise");
    assert!(matches!(
        status(&one, &a, 200_000),
        WorkUnitStatus::Leased { ref lessee, attempts: 1, .. } if *lessee == bo
    ));
    assert_eq!(
        one.lost_leases,
        vec![LostLease {
            handoff: handoff(),
            unit_hash: a.unit_hash.clone(),
            winner: bo,
            loser: cy,
            at_ms: 200_000,
        }],
        "the loser is reported so it can cancel, never dropped"
    );
    assert_eq!(one.unreadable, 0, "a lost lease is not an unreadable line");
}

/// **Named test.** Failing input: a `Complete` signed by `cy` for a unit
/// `bo` holds. Applying it would let any ring member write a verdict for
/// work somebody else is doing.
#[test]
fn a_complete_from_a_non_lessee_is_unreadable() {
    let (submit, a, _b) = submission();
    let proj = fold(&[
        op(1, 100, 0, &submit),
        op(2, 200, 0, &lease(&a)),
        op(3, 300, 0, &completion(&a)),
    ]);

    assert_eq!(proj.unreadable, 1);
    assert!(
        matches!(status(&proj, &a, 300_000), WorkUnitStatus::Leased { .. }),
        "the unit is still bo's, and still running"
    );
}

/// **Named test.** Failing input: a lease taken at t=200 and never
/// renewed, read at `200_000 + LEASE_MS`. The deadline millisecond
/// belongs to nobody — `is_live_at`'s exclusive boundary — so at exactly
/// the deadline the unit is queued again, with the attempt carried.
#[test]
fn a_lease_with_no_renew_past_its_deadline_is_requeued_with_attempts_plus_one() {
    let (submit, a, _b) = submission();
    let proj = fold(&[op(1, 100, 0, &submit), op(2, 200, 0, &lease(&a))]);
    let deadline = 200_000 + LEASE_MS;

    assert!(matches!(
        status(&proj, &a, deadline - 1),
        WorkUnitStatus::Leased { attempts: 1, .. }
    ));
    assert_eq!(
        status(&proj, &a, deadline),
        WorkUnitStatus::Queued { prior_attempts: 1 },
        "the attempt is carried forward, so MAX_UNIT_ATTEMPTS counts total attempts"
    );
    assert_eq!(
        expired(&proj, deadline),
        ReapStats {
            requeued: 1,
            terminal_failed: 0,
            phase_transitions: 0,
        },
        "the sweep is counted, never silent"
    );
    // Renewed in time, the same lease survives the same instant.
    let renewed = fold(&[
        op(1, 100, 0, &submit),
        op(2, 200, 0, &lease(&a)),
        op(2, 400, 1, &renew(&a)),
    ]);
    assert!(matches!(
        status(&renewed, &a, deadline),
        WorkUnitStatus::Leased { attempts: 1, .. }
    ));

    // Failing input for the other half: a renew signed AFTER the deadline.
    // Every other node has already re-offered the unit, so extending it
    // here would be this node disagreeing with the ring — counted, not
    // applied.
    let late = fold(&[
        op(1, 100, 0, &submit),
        op(2, 200, 0, &lease(&a)),
        op(2, (deadline / 1_000) as i64 + 1, 1, &renew(&a)),
    ]);
    assert_eq!(late.unreadable, 1);
    assert_eq!(
        status(&late, &a, deadline),
        WorkUnitStatus::Queued { prior_attempts: 1 },
        "the late renew did not push the deadline out"
    );

    // And a renew from an actor that never held the lease is the same
    // discipline: reported, never applied.
    let stranger = fold(&[
        op(1, 100, 0, &submit),
        op(2, 200, 0, &lease(&a)),
        op(3, 300, 0, &renew(&a)),
    ]);
    assert_eq!(stranger.unreadable, 1);
}

/// **Named test.** Failing input: a third lease that also lapses.
/// `MAX_UNIT_ATTEMPTS` is 3, so the unit is terminal after it — and a
/// fourth `Lease` act finds nothing to take.
#[test]
fn the_third_lapsed_attempt_is_terminal() {
    let (submit, a, _b) = submission();
    let mut ops = vec![op(1, 100, 0, &submit)];
    // Three leases, each taken after the previous one has lapsed.
    let mut at = 200i64;
    for (n, seed) in [(0u64, 2u8), (0, 3), (1, 2)] {
        ops.push(op(seed, at, n, &lease(&a)));
        at += (LEASE_MS as i64) / 1_000 + 1;
    }
    let proj = fold(&ops);
    let after_all = ms_of(at);

    assert_eq!(MAX_UNIT_ATTEMPTS, 3, "the constant this test is about");
    assert!(
        matches!(
            status(&proj, &a, after_all),
            WorkUnitStatus::Failed {
                attempts: 3,
                outcome: None,
                ..
            }
        ),
        "terminal, and `outcome: None` says nobody ever reported — the \
             Expired-vs-Free distinction",
    );
    assert_eq!(
        expired(&proj, after_all),
        ReapStats {
            requeued: 0,
            terminal_failed: 1,
            // The handoff still has unit `b` queued, so it stays Open.
            phase_transitions: 0,
        }
    );

    // A fourth lease has nothing to take, and says so.
    let mut fourth = ops.clone();
    fourth.push(op(3, at, 1, &lease(&a)));
    assert_eq!(fold(&fourth).unreadable, 1);
}

/// A reported `Fail` re-queues while attempts remain — `worklist.rs`'s
/// `ack_failure` — and is terminal once they are spent, carrying the
/// judgement the donor reported.
#[test]
fn a_reported_failure_requeues_until_the_attempts_are_spent() {
    let (submit, a, _b) = submission();
    let one = fold(&[
        op(1, 100, 0, &submit),
        op(2, 200, 0, &lease(&a)),
        op(2, 210, 1, &failure(&a)),
    ]);
    assert_eq!(
        status(&one, &a, 210_000),
        WorkUnitStatus::Queued { prior_attempts: 1 }
    );

    let mut ops = vec![op(1, 100, 0, &submit)];
    let mut ts = 200i64;
    for (seq, seed) in [(0u64, 2u8), (2, 2), (4, 2)] {
        ops.push(op(seed, ts, seq, &lease(&a)));
        ops.push(op(seed, ts + 1, seq + 1, &failure(&a)));
        ts += 10;
    }
    let spent = fold(&ops);
    assert!(
        matches!(
            status(&spent, &a, ms_of(ts)),
            WorkUnitStatus::Failed {
                attempts: 3,
                outcome: Some(_),
                ..
            }
        ),
        "reported: the outcome is carried, unlike a lapse"
    );
}

/// A `Revoke` from the submitter closes the handoff; one from anybody else
/// is `unreadable`. Failing input: `cy` revoking `alex`'s handoff.
#[test]
fn only_the_submitter_may_revoke_a_handoff() {
    let (submit, _a, _b) = submission();
    let by_stranger = fold(&[
        op(1, 100, 0, &submit),
        op(
            3,
            200,
            0,
            &WorkAct::Revoke(Revocation { handoff: handoff() }),
        ),
    ]);
    assert_eq!(by_stranger.unreadable, 1);
    assert_eq!(by_stranger.handoffs[&handoff()].revoked, None);
    assert_eq!(
        by_stranger.handoffs[&handoff()].phase_at(200_000),
        HandoffPhase::Open
    );

    let by_submitter = fold(&[
        op(1, 100, 0, &submit),
        op(
            1,
            200,
            1,
            &WorkAct::Revoke(Revocation { handoff: handoff() }),
        ),
    ]);
    assert_eq!(by_submitter.unreadable, 0);
    assert!(matches!(
        by_submitter.handoffs[&handoff()].phase_at(200_000),
        HandoffPhase::Failed { .. }
    ));
    assert!(
        by_submitter.takeable_at(200_000).is_empty(),
        "a revoked handoff stops being offered"
    );
}

/// The fairness rule: `ORDER BY attempts ASC, key ASC`. Failing input: a
/// unit that has already burnt an attempt sitting ahead of a fresh one —
/// which is how one poisonous unit monopolises a cohort.
#[test]
fn the_queue_offers_the_fewest_attempts_first() {
    let (submit, a, b) = submission();
    // `a` is leased and lapses; `b` is untouched.
    let proj = fold(&[op(1, 100, 0, &submit), op(2, 200, 0, &lease(&a))]);
    let after = 200_000 + LEASE_MS;

    let queue = proj.takeable_at(after);
    assert_eq!(queue.len(), 2);
    assert_eq!(
        queue[0].unit_hash, b.unit_hash,
        "0 attempts before 1 attempt, whatever the hashes sort like"
    );
    assert_eq!(queue[1].unit_hash, a.unit_hash);

    // With no lapse both are fresh and the tie breaks on the stable key,
    // so two nodes offer the same unit next.
    let fresh = fold(&[op(1, 100, 0, &submit)]);
    let mut by_hash = vec![a.unit_hash.clone(), b.unit_hash.clone()];
    by_hash.sort();
    assert_eq!(
        fresh
            .takeable_at(100_000)
            .into_iter()
            .map(|r| r.unit_hash)
            .collect::<Vec<_>>(),
        by_hash
    );
}

/// The latest `Offer` per actor wins, and "latest" is the total order's
/// last — not a timestamp read off the payload.
#[test]
fn the_last_offer_in_the_total_order_is_the_live_one() {
    let proj = fold(&[
        op(2, 100, 0, &WorkAct::Offer(offer(&["process:v1"], None))),
        op(2, 200, 1, &WorkAct::Offer(offer(&["ingest:v1"], None))),
        op(3, 150, 0, &WorkAct::Offer(offer(&["process:v1"], None))),
    ]);
    assert_eq!(proj.offers.len(), 2);
    assert_eq!(proj.offers[&who(2)].kinds, vec![kind("ingest:v1")]);
    assert_eq!(proj.offers[&who(3)].kinds, vec![kind("process:v1")]);
}

/// Acts about a handoff nobody submitted are counted, not dropped, and
/// they are the same `unreadable` counter `act::read`'s undecodable lines
/// land in. Failing input: a `Lease` for a handoff id this journal never
/// carried.
#[test]
fn acts_for_an_unknown_handoff_are_counted_as_unreadable() {
    let (_submit, a, _b) = submission();
    let proj = fold(&[op(2, 200, 0, &lease(&a)), op(2, 300, 1, &completion(&a))]);
    assert_eq!(proj.unreadable, 2);
    assert!(proj.handoffs.is_empty());
    assert_eq!(proj.gaps, 0, "nothing was missing — the acts were unusable");
}

/// At-least-once delivery: a repeated report is normal and is counted, not
/// treated as a non-lessee write.
#[test]
fn a_repeated_complete_is_counted_as_a_double_delivery() {
    let (submit, a, _b) = submission();
    let proj = fold(&[
        op(1, 100, 0, &submit),
        op(2, 200, 0, &lease(&a)),
        op(2, 300, 1, &completion(&a)),
        op(2, 400, 2, &completion(&a)),
    ]);
    assert_eq!(proj.double_deliveries, 1);
    assert_eq!(proj.unreadable, 0);
}

/// The phase is derived from the units, and `Merging` is never one of the
/// answers — see [`WorkHandoff::phase_at`].
#[test]
fn the_handoff_phase_walks_open_to_draining_to_complete_and_never_merges() {
    let (submit, a, b) = submission();
    let ops = |extra: Vec<Op<SignedOp>>| {
        let mut v = vec![op(1, 100, 0, &submit)];
        v.extend(extra);
        fold(&v)
    };
    let phase = |p: &WorkProjection, now| p.handoffs[&handoff()].phase_at(now);

    assert_eq!(phase(&ops(vec![]), 100_000), HandoffPhase::Open);
    let both_leased = ops(vec![op(2, 200, 0, &lease(&a)), op(2, 210, 1, &lease(&b))]);
    assert_eq!(phase(&both_leased, 210_000), HandoffPhase::Draining);
    let done = ops(vec![
        op(2, 200, 0, &lease(&a)),
        op(2, 210, 1, &lease(&b)),
        op(2, 220, 2, &completion(&a)),
        op(2, 230, 3, &completion(&b)),
    ]);
    assert_eq!(phase(&done, 230_000), HandoffPhase::Complete);

    for now in [100_000u64, 210_000, 230_000, 10_000_000] {
        for p in [&ops(vec![]), &both_leased, &done] {
            assert_ne!(
                phase(p, now),
                HandoffPhase::Merging,
                "Merging is an ingest step and this plane has nothing to merge"
            );
        }
    }
}

/// A handoff past its `ttl_secs` stops being offered and says why. Failing
/// input: reading the queue one millisecond after the TTL elapses.
#[test]
fn a_handoff_past_its_ttl_stops_being_offered() {
    let (_s, a, b) = submission();
    let submit = WorkAct::Submit(Submission::new(
        handoff(),
        kind("process:v1"),
        vec![a, b],
        None,
        Some(60),
    ));
    let proj = fold(&[op(1, 100, 0, &submit)]);
    let ttl_end = 100_000 + 60_000;

    assert_eq!(proj.takeable_at(ttl_end - 1).len(), 2);
    assert!(proj.takeable_at(ttl_end).is_empty());
    assert!(matches!(
        proj.handoffs[&handoff()].phase_at(ttl_end),
        HandoffPhase::Failed { .. }
    ));
}

/// Every act kind reaches the fold — a fold that silently ignored one
/// would pass every test above that does not use it.
#[test]
fn every_act_kind_is_folded() {
    let (submit, a, _b) = submission();
    let proj = fold(&[
        op(1, 100, 0, &submit),
        op(2, 110, 0, &WorkAct::Offer(offer(&["process:v1"], None))),
        op(2, 200, 1, &lease(&a)),
        op(2, 210, 2, &renew(&a)),
        op(2, 220, 3, &failure(&a)),
        op(3, 300, 0, &lease(&a)),
        op(3, 310, 1, &completion(&a)),
        op(
            1,
            400,
            1,
            &WorkAct::Revoke(Revocation { handoff: handoff() }),
        ),
    ]);
    assert_eq!(WorkActKind::ALL.len(), 7);
    assert_eq!(proj.unreadable, 0, "every one of the seven applied");
    assert_eq!(proj.offers.len(), 1);
    assert!(matches!(
        status(&proj, &a, 400_000),
        WorkUnitStatus::Complete { .. }
    ));
    assert!(proj.handoffs[&handoff()].revoked.is_some());
}

/// `gaps` is admission's own count and is carried through untouched: an
/// empty projection beside a non-zero `gaps` is not an empty ring.
#[test]
fn admission_gaps_are_carried_onto_the_projection() {
    let (submit, _a, _b) = submission();
    // seq 0 is missing for alex, so the run from the floor has a hole.
    let proj = fold(&[op(1, 100, 3, &submit)]);
    assert_eq!(proj.gaps, 3, "seq 0, 1 and 2 are missing");
    assert_eq!(proj.handoffs.len(), 1, "the act itself still applied");
}

// -------------------------------------------------------------
// The lease decider — three states, and the third is the point
// -------------------------------------------------------------

/// **The decider a second donor got wrong, and it had no test until now.**
///
/// `lease_state` lived in `sovereign-mesh::work_donor` where a package
/// consumer could not reach it, so cw-lift 5f's lifted peer re-derived it —
/// as a BOOL — and cancelled a running unit whenever the journal merely
/// failed to read. The fix was to move the decider here; this is the test
/// that should have existed when it was written.
///
/// Failing input for each arm is named in the assertion. The `Unknown` arm is
/// deliberately absent: this function is handed a projection, so by
/// construction it has one, and `Unknown` is the CALLER's verdict for a fold
/// it could not obtain. That split is asserted separately below.
#[test]
fn the_lease_decider_answers_held_for_the_holder_and_lost_for_everyone_else() {
    let (submit, a, _b) = submission();
    let proj = fold(&[op(1, 100, 0, &submit), op(2, 200, 0, &lease(&a))]);
    let r = unit_ref(&a);
    let (holder, other) = (who(2), who(3));

    // Held: the fold names this actor, inside the lease window.
    assert_eq!(
        lease_state(&proj, &r, &holder, 200_000),
        LeaseState::Held,
        "the actor the fold names as lessee holds it"
    );

    // Lost, to a named other. Failing input: a decider comparing anything but
    // the lessee — a donor that renewed here would be writing over a lease it
    // does not hold.
    assert!(
        matches!(lease_state(&proj, &r, &other, 200_000), LeaseState::Lost(ref why) if why.contains(holder.as_str())),
        "a non-lessee is Lost, and the reason NAMES who holds it"
    );

    // Lost, by lapse. Failing input: a decider reading `Leased` without
    // consulting `status_at` — the lease would look live forever and two
    // donors would both believe they hold it.
    let lapsed = 200_000 + commonwealth_core::knowledge::LEASE_MS + 1;
    assert!(
        matches!(lease_state(&proj, &r, &holder, lapsed), LeaseState::Lost(_)),
        "past expires_at_ms the holder has lost it too — expiry is derived, \
         not published, so every node agrees without an act"
    );

    // Lost, because the unit is not in this fold at all. Failing input: an
    // `unwrap` or a `false`; the first panics a donor loop and the second
    // silently renews a lease on a unit nobody is tracking.
    let empty = fold(&[]);
    assert!(
        matches!(
            lease_state(&empty, &r, &holder, 200_000),
            LeaseState::Lost(_)
        ),
        "a unit absent from the fold is Lost, never Held"
    );
}

/// A reported unit is not still held.
///
/// Failing input: a decider that treats "not Leased" as "still mine". After a
/// `Complete` is admitted the heartbeat must stop renewing, or the donor
/// appends `Renew` acts for work it already reported — which the fold counts
/// as `unreadable`.
#[test]
fn a_reported_unit_is_lost_to_its_own_reporter() {
    let (submit, a, _b) = submission();
    let proj = fold(&[
        op(1, 100, 0, &submit),
        op(2, 200, 0, &lease(&a)),
        op(2, 300, 1, &completion(&a)),
    ]);
    assert!(
        matches!(
            lease_state(&proj, &unit_ref(&a), &who(2), 300_000),
            LeaseState::Lost(_)
        ),
        "the actor that reported it no longer holds it"
    );
}

/// `Unknown` is the caller's verdict and never this function's.
///
/// The distinction is the whole reason the type has three variants rather
/// than two, so it is pinned rather than left to the doc comment: an
/// unreadable journal must never be expressible as `Lost`, because a caller
/// matching on `Lost` cancels.
#[test]
fn unknown_is_never_produced_by_the_pure_decider() {
    let (submit, a, _b) = submission();
    let proj = fold(&[op(1, 100, 0, &submit), op(2, 200, 0, &lease(&a))]);
    for (key, now) in [(who(2), 200_000u64), (who(3), 200_000), (who(2), u64::MAX)] {
        assert_ne!(
            lease_state(&proj, &unit_ref(&a), &key, now),
            LeaseState::Unknown,
            "given a fold, the answer is Held or Lost — never could-not-read"
        );
    }
}
