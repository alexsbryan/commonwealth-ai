//! Status DTOs for a watched-folder corpus.
//!
//! Both the CLI (`sovereign corpus watch-status`) and the internal HTTP
//! route (`GET /internal/corpus/watch/status/{id}`) serialise this
//! enum verbatim. One source of truth — never duplicate the shape.

use serde::{Deserialize, Serialize};

/// Top-level status for a watched-folder corpus. Drives the CLI status
/// renderer and (in Phase 2) the desktop banner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WatchedFolderStatus {
    /// Last sweep completed cleanly. `live_docs` reflects the
    /// post-sweep manifest; `tombstones` is the count of soft-deleted
    /// docs still inside the grace window.
    Idle {
        last_sweep_unix: u64,
        live_docs: usize,
        tombstones: usize,
    },
    /// A sweep is currently in progress. `current/total` tracks the
    /// active phase. `Walking` and `Diffing` use `total == 0` because
    /// they don't have a known denominator at start.
    Sweeping {
        phase: SweepPhase,
        current: usize,
        total: usize,
    },
    /// Threshold guard tripped on the most recent sweep. Adds + updates
    /// from that sweep already applied; deletions held until the user
    /// calls `confirm-deletion` (which clears the pause; the next sweep
    /// re-walks and applies whatever the current diff is).
    PausedAwaitingConfirmation {
        diff_summary: DiffSummary,
        tripped_rule: TrippedRule,
        sweep_started_unix: u64,
    },
    /// User-requested pause. The scheduler skips this corpus entirely
    /// until `resume` is called.
    PausedManual {
        since_unix: u64,
        reason: String,
    },
    /// A sweep errored (extraction panic that escaped the guard, IO
    /// failure mid-apply, etc.). The next scheduler tick will retry.
    Errored {
        message: String,
        errored_unix: u64,
    },
}

impl WatchedFolderStatus {
    pub fn is_paused(&self) -> bool {
        matches!(
            self,
            WatchedFolderStatus::PausedManual { .. }
                | WatchedFolderStatus::PausedAwaitingConfirmation { .. }
        )
    }

    /// Emit a stable, lowercase tag suitable for tracing fields and
    /// CLI status tables. Mirrors the serde `kind` tag without
    /// allocating.
    pub fn tag(&self) -> &'static str {
        match self {
            WatchedFolderStatus::Idle { .. } => "idle",
            WatchedFolderStatus::Sweeping { .. } => "sweeping",
            WatchedFolderStatus::PausedAwaitingConfirmation { .. } => {
                "paused_awaiting_confirmation"
            }
            WatchedFolderStatus::PausedManual { .. } => "paused_manual",
            WatchedFolderStatus::Errored { .. } => "errored",
        }
    }
}

/// Sweep phases — surfaced in `Sweeping` status and in
/// `WatchedFolderEvent::PhaseProgress`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SweepPhase {
    Walking,
    Diffing,
    Deleting,
    Updating,
    Adding,
    GcSoftDeletes,
}

/// Counts from a single sweep's diff. Carried in
/// `PausedAwaitingConfirmation` so the user sees what would happen if
/// they confirm. `live_before` is the count of live docs in the prior
/// manifest, used to compute the fractional threshold.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiffSummary {
    pub added: usize,
    pub modified: usize,
    pub removed: usize,
    pub live_before: usize,
}

/// Which threshold tripped the guard, with the values that triggered
/// it. The `observed` field carries enough context for the CLI to
/// print a useful message ("23 of 40 files would be deleted; threshold
/// is 25%") without re-reading state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "snake_case")]
pub enum TrippedRule {
    Absolute {
        threshold: usize,
        observed: usize,
    },
    Fractional {
        threshold: f32,
        observed: f32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_tag_round_trips_serde() {
        let s = WatchedFolderStatus::Idle {
            last_sweep_unix: 100,
            live_docs: 5,
            tombstones: 0,
        };
        assert_eq!(s.tag(), "idle");
        assert!(!s.is_paused());

        let s2 = WatchedFolderStatus::PausedManual {
            since_unix: 100,
            reason: "user".into(),
        };
        assert_eq!(s2.tag(), "paused_manual");
        assert!(s2.is_paused());
    }

    #[test]
    fn diff_summary_serializes_compactly() {
        let s = DiffSummary {
            added: 1,
            modified: 2,
            removed: 3,
            live_before: 10,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"added\":1"));
        let back: DiffSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn tripped_rule_round_trips() {
        let r = TrippedRule::Absolute { threshold: 100, observed: 200 };
        let j = serde_json::to_string(&r).unwrap();
        let back: TrippedRule = serde_json::from_str(&j).unwrap();
        assert_eq!(back, r);
    }
}
