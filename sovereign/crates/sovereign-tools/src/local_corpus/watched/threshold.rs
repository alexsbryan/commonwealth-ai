//! Pure deletion-threshold guard.
//!
//! Evaluates a sweep's diff against the corpus's `DeletionGuardConfig`
//! and returns whether the deletion phase should proceed. Adds and
//! updates always proceed; only deletions can trip the guard.
//!
//! Both thresholds (absolute count + fraction of live docs) compose as
//! OR — a 50-doc folder losing 30 docs trips on percentage but not
//! absolute, a 200,000-doc folder losing 5,000 docs trips on absolute
//! but not percentage. Both failure modes are real.

use super::status::TrippedRule;
use crate::local_corpus::config::DeletionGuardConfig;

/// Evaluator's verdict.
#[derive(Debug, Clone, PartialEq)]
pub enum GuardDecision {
    /// Proceed with the deletion phase.
    Allow,
    /// Skip the deletion phase and pause the corpus into
    /// `WatchedFolderStatus::PausedAwaitingConfirmation`.
    Pause(TrippedRule),
}

pub struct DeletionGuard;

impl DeletionGuard {
    /// `removed_count` is the number of docs the diff would delete in
    /// this sweep. `live_before` is the count of live docs in the
    /// prior manifest. The guard short-circuits to `Allow` when
    /// either: the guard is disabled in config; nothing would be
    /// deleted; or `live_before == 0` (initial sweep — there's nothing
    /// to lose).
    pub fn evaluate(
        removed_count: usize,
        live_before: usize,
        cfg: &DeletionGuardConfig,
    ) -> GuardDecision {
        if !cfg.enabled || removed_count == 0 || live_before == 0 {
            return GuardDecision::Allow;
        }

        if removed_count >= cfg.absolute_threshold {
            return GuardDecision::Pause(TrippedRule::Absolute {
                threshold: cfg.absolute_threshold,
                observed: removed_count,
            });
        }

        let fraction = removed_count as f32 / live_before as f32;
        if fraction >= cfg.fractional_threshold {
            return GuardDecision::Pause(TrippedRule::Fractional {
                threshold: cfg.fractional_threshold,
                observed: fraction,
            });
        }

        GuardDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(abs: usize, frac: f32) -> DeletionGuardConfig {
        DeletionGuardConfig {
            absolute_threshold: abs,
            fractional_threshold: frac,
            enabled: true,
        }
    }

    #[test]
    fn allow_when_no_deletions() {
        assert_eq!(
            DeletionGuard::evaluate(0, 100, &cfg(10, 0.05)),
            GuardDecision::Allow
        );
    }

    #[test]
    fn allow_when_initial_sweep() {
        // live_before=0 means the corpus had no docs before this
        // sweep — nothing for the guard to protect.
        assert_eq!(
            DeletionGuard::evaluate(0, 0, &cfg(10, 0.05)),
            GuardDecision::Allow
        );
    }

    #[test]
    fn allow_when_disabled() {
        let mut c = cfg(10, 0.05);
        c.enabled = false;
        // Even a wipe-the-corpus deletion is allowed when the guard
        // is off.
        assert_eq!(
            DeletionGuard::evaluate(1_000, 1_000, &c),
            GuardDecision::Allow
        );
    }

    #[test]
    fn trip_on_absolute_threshold() {
        // 30 deletions, abs=25, frac=0.50 (would NOT trip on
        // fraction): 30/100 = 0.30 < 0.50. Verifies absolute fires
        // independently of fractional.
        let decision = DeletionGuard::evaluate(30, 100, &cfg(25, 0.50));
        match decision {
            GuardDecision::Pause(TrippedRule::Absolute {
                threshold,
                observed,
            }) => {
                assert_eq!(threshold, 25);
                assert_eq!(observed, 30);
            }
            other => panic!("expected Absolute pause, got {other:?}"),
        }
    }

    #[test]
    fn trip_on_fractional_threshold() {
        // 5 of 10 deletions, abs=100 (would NOT trip on absolute),
        // frac=0.10. Verifies fractional fires independently.
        let decision = DeletionGuard::evaluate(5, 10, &cfg(100, 0.10));
        match decision {
            GuardDecision::Pause(TrippedRule::Fractional {
                threshold,
                observed,
            }) => {
                assert!((threshold - 0.10).abs() < f32::EPSILON);
                assert!((observed - 0.50).abs() < f32::EPSILON);
            }
            other => panic!("expected Fractional pause, got {other:?}"),
        }
    }

    #[test]
    fn absolute_takes_precedence_when_both_trip() {
        // Both rules trip; absolute is checked first so the rule
        // surfaced is `Absolute`. This is documentation of order, not
        // a correctness invariant — the user-facing message just
        // names whichever fires first.
        let decision = DeletionGuard::evaluate(50, 100, &cfg(25, 0.10));
        assert!(matches!(
            decision,
            GuardDecision::Pause(TrippedRule::Absolute { .. })
        ));
    }
}
