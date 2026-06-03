//! Confidence grade returned by `work_in_flight`.
//!
//! Phase 1 emits only [`ConfidenceGrade::Declared`]. The other three
//! variants exist on the enum so wire forms are stable across the
//! Phase 2 cut-over (when CodeWatcher-driven Observations arrive).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfidenceGrade {
    /// A live Claim covering the scope. Phase 1 emits only this.
    Declared,
    /// CodeWatcher edit Observation within the active window. Phase 2.
    Active,
    /// CodeWatcher edit Observation within the recent window. Phase 2.
    Recent,
    /// Tool-call inspection Observation with no edits in the same
    /// session. Phase 2.
    Exploring,
}

impl ConfidenceGrade {
    pub const fn id(&self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Active => "active",
            Self::Recent => "recent",
            Self::Exploring => "exploring",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "declared" => Some(Self::Declared),
            "active" => Some(Self::Active),
            "recent" => Some(Self::Recent),
            "exploring" => Some(Self::Exploring),
            _ => None,
        }
    }
}

/// Spec §8 thresholds. Read at query time so the same Observation
/// gracefully degrades Active → Recent → (dropped) as time passes,
/// without the writer having to refresh records.
pub const ACTIVE_WINDOW_SECS: u64 = 300; // 5 min
pub const RECENT_WINDOW_SECS: u64 = 1_800; // 30 min

/// Map `(now, last_observed_at, source)` → grade per spec §8.
/// Returns `None` when the observation is too old to qualify even
/// as Recent (the caller drops it from the result set).
pub fn observation_grade(
    now_secs: u64,
    last_observed_at: u64,
    source: crate::model::ObservationSource,
) -> Option<ConfidenceGrade> {
    use crate::model::ObservationSource as S;
    let age = now_secs.saturating_sub(last_observed_at);
    match source {
        S::CodeWatcherEdit if age <= ACTIVE_WINDOW_SECS => Some(ConfidenceGrade::Active),
        S::CodeWatcherEdit if age <= RECENT_WINDOW_SECS => Some(ConfidenceGrade::Recent),
        S::CodeWatcherEdit => None,
        // Tool-call inspection — Phase 2b. Reported as Exploring
        // when fresh; ages out at the Recent window so the read
        // surface doesn't accumulate stale "someone looked at this
        // yesterday" noise.
        S::ToolCallInspect if age <= RECENT_WINDOW_SECS => Some(ConfidenceGrade::Exploring),
        S::ToolCallInspect => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_roundtrip_all_variants() {
        for v in [
            ConfidenceGrade::Declared,
            ConfidenceGrade::Active,
            ConfidenceGrade::Recent,
            ConfidenceGrade::Exploring,
        ] {
            assert_eq!(ConfidenceGrade::from_id(v.id()), Some(v));
        }
    }
}
