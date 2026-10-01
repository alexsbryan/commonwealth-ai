// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use sovereign_scheduler::decision_log::DecisionEvent;

fn outcomes(report: &RunReport) -> Vec<&RoutingOutcome> {
    report
        .records
        .iter()
        .filter_map(|e| match e {
            DecisionEvent::Outcome(o) => Some(&**o),
            _ => None,
        })
        .collect()
}

fn decisions(report: &RunReport) -> Vec<&sovereign_scheduler::decision_log::RoutingDecision> {
    report
        .records
        .iter()
        .filter_map(|e| match e {
            DecisionEvent::Decision(d) => Some(&**d),
            _ => None,
        })
        .collect()
}

#[test]
fn every_decision_gets_exactly_one_joined_outcome() {
    let s = scenario::household_evening_12(1);
    let r = run(&s, Arm::AsImplemented, 1);
    let ds = decisions(&r);
    let os = outcomes(&r);
    assert_eq!(ds.len(), s.arrivals.len());
    assert_eq!(os.len(), ds.len(), "every request must complete");
    let ids: std::collections::HashSet<_> = ds.iter().map(|d| d.decision_id.clone()).collect();
    for o in &os {
        assert!(ids.contains(&o.decision_id), "outcome with no decision");
    }
}

#[test]
fn a_run_is_reproducible_and_arms_share_one_world() {
    let s = scenario::household_evening_12(4);
    let a = run(&s, Arm::AsImplemented, 4);
    let b = run(&s, Arm::AsImplemented, 4);
    assert_eq!(a.truth.len(), b.truth.len());
    for (x, y) in a.truth.iter().zip(b.truth.iter()) {
        assert_eq!(x.total_ms, y.total_ms);
        assert_eq!(x.server, y.server);
    }
    // Switching the policy must not change the workload.
    let c = run(&s, Arm::TwoChoices, 4);
    assert_eq!(a.truth.len(), c.truth.len());
}

/// The hard invariants of §5, as assertions rather than scores.
#[test]
fn private_and_fast_requests_never_cross_the_wire() {
    let s = scenario::household_evening_12(9);
    for arm in ALL_ARMS {
        let r = run(&s, arm, 9);
        for fact in &r.truth {
            if matches!(fact.class, RequestClass::Private | RequestClass::Fast) {
                assert_eq!(
                    fact.origin,
                    fact.server,
                    "{}: a {:?} request was offloaded",
                    arm.label(),
                    fact.class
                );
            }
        }
    }
}

#[test]
fn the_oracle_is_at_least_as_fast_as_the_implementation() {
    let s = scenario::household_evening_12(5);
    let mean = |r: &RunReport| -> f64 {
        r.truth.iter().map(|f| f.total_ms as f64).sum::<f64>() / r.truth.len() as f64
    };
    let arm0 = mean(&run(&s, Arm::AsImplemented, 5));
    let oracle = mean(&run(&s, Arm::Oracle, 5));
    assert!(
        oracle <= arm0,
        "oracle {oracle:.0}ms should not lose to arm 0 {arm0:.0}ms"
    );
}

#[test]
fn gossip_makes_the_load_signal_stale_and_the_record_understates_it() {
    let s = scenario::household_evening_12(2);
    let r = run(&s, Arm::AsImplemented, 2);
    let aged: Vec<_> = r
        .truth
        .iter()
        .filter_map(|f| f.true_signal_age_ms.zip(f.recorded_signal_age_ms))
        .collect();
    assert!(
        !aged.is_empty(),
        "no peer-routed decision used a gossiped signal"
    );
    assert!(
        aged.iter().any(|(t, _)| *t > 10_000),
        "no decision saw a load signal older than one gossip round"
    );
    assert!(
        aged.iter().all(|(t, rec)| rec <= t),
        "the recorded age can never exceed the true age"
    );
}

/// Populating the OICP envelope's token counts switched on two hard
/// feasibility gates that had never bound in this sim. They must
/// still not bind: every simulated claim advertises 32768 context /
/// 4000 output, and no arrival exceeds either — so arm 0's candidate
/// set is unchanged and the F7 pricing recorded against it stays
/// comparable.
///
/// If an arrival distribution or a claim ever changes so that the
/// gates *do* bind, this fails loudly. That is the right outcome:
/// silently shrinking the candidate set would move every number on
/// the scoreboard for a reason nobody chose.
#[test]
fn no_arrival_is_gated_out_by_its_own_size() {
    use sovereign_scheduler::decision_log::ExclusionReason;
    let s = scenario::household_evening_12(13);
    let r = run(&s, Arm::AsImplemented, 13);
    for d in decisions(&r) {
        for ex in &d.excluded {
            assert!(
                !matches!(ex.reason, ExclusionReason::NoClaimMatch),
                "peer `{}` was excluded for claim mismatch — the context/output \
                     gates now bind, so the candidate set has moved and every number \
                     derived from it needs re-baselining",
                ex.name
            );
        }
    }
}

/// Wiring check for the §4.1 arm: it has to actually decide
/// differently somewhere. If [`Arm::PredictedTime`] silently fell
/// through to the product objective, the world is identical and the
/// two runs would agree exactly — and every table printed from the
/// arm would be arm 0 run twice under a different name.
#[test]
fn the_predicted_time_arm_decides_differently_from_arm_zero() {
    let s = scenario::household_evening_12(8);
    let offloads = |arm: Arm| -> usize {
        run(&s, arm, 8)
            .truth
            .iter()
            .filter(|f| f.origin != f.server)
            .count()
    };
    let base = offloads(Arm::AsImplemented);
    let predicted = offloads(Arm::PredictedTime);
    assert_ne!(
        base, predicted,
        "predicted-time took exactly as many offloads ({base}) as the product \
             objective — either the objective is not wired through RankInputs, or \
             the two genuinely coincide on this fleet and this check needs a sharper \
             discriminator"
    );
}

/// An arm may change *where* work runs and therefore the order it
/// finishes in — but never what work arrived. Compared as a
/// multiset of (arrival time, origin, class), because completion
/// order is exactly the thing an arm is allowed to move.
#[test]
fn an_arm_changes_where_work_runs_never_what_work_arrived() {
    let s = scenario::household_evening_12(6);
    let workload = |r: &RunReport| {
        let mut w: Vec<(u64, usize, RequestClass)> = r
            .truth
            .iter()
            .map(|f| (f.dispatched_at_ms, f.origin, f.class))
            .collect();
        w.sort();
        w
    };
    let baseline = workload(&run(&s, Arm::AsImplemented, 6));
    for arm in ALL_ARMS {
        assert_eq!(
            workload(&run(&s, arm, 6)),
            baseline,
            "{} saw a different workload",
            arm.label()
        );
    }
}
