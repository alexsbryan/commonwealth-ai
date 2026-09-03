// SPDX-License-Identifier: AGPL-3.0-or-later
/// The ledger invariants, driven by inputs that trip them.
///
/// ARCH §18.1: a check with no failing input you can name is not a check.
/// Every arm of [`ledger_violations`] gets one here, and the SEP regression
/// gets its own by name.
mod ledger_tests {
    use sovereign_core::runtime::retrieval_ledger::{
        ledger_violations, DropReason, StepKind, StepLedger,
    };
    use sovereign_core::runtime::retrieval_pipeline::{deep_pipeline, kq_pipeline};

    fn led(considered: Option<usize>, drops: &[(DropReason, usize)]) -> StepLedger {
        let mut l = StepLedger {
            considered,
            accounted: Default::default(),
        };
        for (r, n) in drops {
            l = l.drop(*r, *n);
        }
        l
    }

    #[test]
    fn injector_that_removed_chunks_is_a_violation() {
        let v = ledger_violations(StepKind::Injector, -3, &led(Some(0), &[]));
        assert!(v.contains(&"injector REMOVED chunks"), "{v:?}");
    }

    #[test]
    fn filter_that_added_chunks_is_a_violation() {
        let v = ledger_violations(StepKind::Filter(DropReason::Duplicate), 2, &led(None, &[]));
        assert!(v.contains(&"filter ADDED chunks"), "{v:?}");
    }

    #[test]
    fn inert_step_that_changed_membership_is_a_violation() {
        let v = ledger_violations(StepKind::Inert, 1, &led(None, &[]));
        assert!(
            v.contains(&"non-mutating step changed pool membership"),
            "{v:?}"
        );
        let v = ledger_violations(StepKind::Reorder, -1, &led(None, &[]));
        assert!(
            v.contains(&"non-mutating step changed pool membership"),
            "{v:?}"
        );
    }

    #[test]
    fn injector_with_unaccounted_candidates_is_a_violation() {
        // 10 candidates, 2 admitted, 3 explained. Five vanished.
        let v = ledger_violations(
            StepKind::Injector,
            2,
            &led(Some(10), &[(DropReason::OutOfScope, 3)]),
        );
        assert!(
            v.contains(&"candidates unaccounted for (added + dropped != considered)"),
            "{v:?}"
        );
    }

    #[test]
    fn the_sep_shape_every_candidate_died_at_resolution() {
        // THE REGRESSION. Atlas grounding on SEP: 49 candidates, none
        // admitted, every one lost to a fetch scoped at a corpus that holds
        // no chunks. Under the old code this was `delta=0` and nothing else.
        let v = ledger_violations(
            StepKind::Injector,
            0,
            &led(Some(49), &[(DropReason::TitleMismatch, 49)]),
        );
        assert!(
            v.iter().any(|s| s.contains("died at resolution")),
            "the SEP shape must be reported, got {v:?}"
        );
    }

    #[test]
    fn every_candidate_vanished_with_no_reason_is_a_violation() {
        let v = ledger_violations(StepKind::Injector, 0, &led(Some(7), &[]));
        assert!(
            v.contains(&"every candidate vanished with no reason recorded"),
            "{v:?}"
        );
    }

    #[test]
    fn scope_drops_are_a_decision_not_a_failure() {
        // Zero admitted because every candidate was out of scope is a
        // LEGITIMATE zero — the step decided, it did not fail. Getting this
        // wrong would make the gate cry wolf on every scoped turn.
        let v = ledger_violations(
            StepKind::Injector,
            0,
            &led(Some(12), &[(DropReason::OutOfScope, 12)]),
        );
        assert!(v.is_empty(), "expected a clean legitimate zero, got {v:?}");
    }

    #[test]
    fn a_healthy_injector_and_a_true_zero_are_clean() {
        // 49 considered, 12 admitted, 37 explained.
        let v = ledger_violations(
            StepKind::Injector,
            12,
            &led(
                Some(49),
                &[
                    (DropReason::BudgetExhausted, 30),
                    (DropReason::Duplicate, 7),
                ],
            ),
        );
        assert!(v.is_empty(), "{v:?}");
        // Nothing to work with is not a violation.
        assert!(ledger_violations(StepKind::Injector, 0, &led(Some(0), &[])).is_empty());
    }

    #[test]
    fn a_filter_ledger_must_sum_to_what_it_removed() {
        let ok = ledger_violations(
            StepKind::Filter(DropReason::Duplicate),
            -4,
            &led(None, &[(DropReason::Duplicate, 4)]),
        );
        assert!(ok.is_empty(), "{ok:?}");
        let bad = ledger_violations(
            StepKind::Filter(DropReason::Duplicate),
            -4,
            &led(None, &[(DropReason::Duplicate, 1)]),
        );
        assert!(
            bad.contains(&"removals unaccounted for (removed != sum of reasons)"),
            "{bad:?}"
        );
    }

    #[test]
    fn an_unledgered_injector_is_reported_but_not_a_violation() {
        // The incremental state: kind is declared and checked, `considered`
        // is not wired yet. Must not cry wolf.
        assert!(ledger_violations(StepKind::Injector, 5, &led(None, &[])).is_empty());
    }

    #[test]
    fn resolution_failures_are_exactly_the_three_that_mean_a_defect() {
        for r in [
            DropReason::CorpusNotSearchable,
            DropReason::EvidenceUnresolvable,
            DropReason::TitleMismatch,
        ] {
            assert!(r.is_resolution_failure(), "{r:?} should be a failure");
        }
        for r in [
            DropReason::OutOfScope,
            DropReason::BudgetExhausted,
            DropReason::BelowThreshold,
            DropReason::Duplicate,
            DropReason::FeatureDisabled,
            DropReason::DeadLaw,
        ] {
            assert!(
                !r.is_resolution_failure(),
                "{r:?} is a decision, not a failure"
            );
        }
    }

    #[test]
    fn every_step_in_both_pipelines_declares_a_kind_and_the_set_is_total() {
        // The totality claim: all 27 steps are classified, and the two
        // pipelines between them cover injector / filter / reorder / inert.
        let mut kinds: Vec<std::mem::Discriminant<StepKind>> = Vec::new();
        let mut n = 0;
        for p in [kq_pipeline(), deep_pipeline(true)] {
            for s in &p.steps {
                n += 1;
                let d = std::mem::discriminant(&s.kind);
                if !kinds.contains(&d) {
                    kinds.push(d);
                }
            }
        }
        assert!(n >= 27, "expected at least 27 declared steps, saw {n}");
        assert!(
            kinds.len() >= 3,
            "the classification collapsed to {} kind(s)",
            kinds.len()
        );
    }
}
