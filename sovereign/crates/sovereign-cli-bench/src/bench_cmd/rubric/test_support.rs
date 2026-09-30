// SPDX-License-Identifier: AGPL-3.0-or-later
//! Lane-shaped test doubles, shared by the rubric core's own tests.
//!
//! Kept in one place rather than copied into each test module: the scorer, the
//! aggregator and the paired test all need "a minimal item carrying criteria",
//! and three private copies of that would drift apart exactly when one of them
//! grows a field (ARCH_PRINCIPLES §10.6).

use super::judge::{Ballot, CriterionVerdict};
use super::score::{score_item, CriterionOutcome, RubricItem};

/// One judged criterion. `trials_*` are filled consistently with `verdict`
/// so unanimity accounting sees a coherent record.
pub fn outcome(id: &str, dim: &str, weight: i32, verdict: Option<Ballot>) -> CriterionOutcome {
    CriterionOutcome {
        criterion_id: id.into(),
        dimension: dim.into(),
        weight,
        verdict: CriterionVerdict {
            verdict,
            evidence: String::new(),
            trials_yes: matches!(verdict, Some(Ballot::Yes)) as u32,
            trials_no: matches!(verdict, Some(Ballot::No)) as u32,
            trials_failed: verdict.is_none() as u32,
        },
    }
}

/// Minimal lane-shaped item, standing in for a moral scenario or a situated
/// probe.
pub struct TestItem {
    pub id: String,
    pub group: String,
    pub criteria: Vec<CriterionOutcome>,
}

impl RubricItem for TestItem {
    fn id(&self) -> &str {
        &self.id
    }
    fn score(&self) -> Option<f64> {
        score_item(&self.criteria)
    }
    fn criteria(&self) -> &[CriterionOutcome] {
        &self.criteria
    }
    fn group(&self) -> Option<&str> {
        Some(&self.group)
    }
}

pub fn item(id: &str, group: &str, criteria: Vec<CriterionOutcome>) -> TestItem {
    TestItem {
        id: id.into(),
        group: group.into(),
        criteria,
    }
}
