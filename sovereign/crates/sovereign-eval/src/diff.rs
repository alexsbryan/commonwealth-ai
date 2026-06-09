// SPDX-License-Identifier: AGPL-3.0-or-later
//! `score-diff` — diff two scored runs across all metrics.
//!
//! Without this, we have observations; with it, we have learning.
//! Every overnight that doesn't change exactly one operator-authored
//! input (tier 1 prompt, tier 2 spec, etc.) is uninterpretable —
//! score-diff makes the iteration discipline operationally visible.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::audit_trail::AuditReport;
use crate::judge::{Axes, JudgeReport};
use crate::manifest::Manifest;
use crate::mechanical::MechanicalReport;
use crate::regression::RegressionReport;
use crate::scope::ScopeReport;
use crate::tool_grader::ToolGradeReport;
use crate::workflow::WorkflowReport;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreBundle {
    pub manifest: Manifest,
    pub mechanical: Option<MechanicalReport>,
    pub judge: Option<JudgeReport>,
    pub reviewer_verdict: Option<ReviewerVerdict>,
    pub workflow: WorkflowReport,
    pub audit_trail: Option<AuditReport>,
    pub scope: Option<ScopeReport>,
    pub regression: Option<RegressionReport>,
    pub tool_grades: Option<ToolGradeReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewerVerdict {
    pub axes_adjusted: Axes,
    pub total_adjusted: u32,
    pub judge_calibration_note: String,
}

pub fn load_bundle(run_dir: &Path) -> Result<ScoreBundle> {
    let manifest_path = run_dir.join("manifest.json");
    let manifest: Manifest = serde_json::from_str(
        &std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?,
    )
    .context("parsing manifest.json")?;

    let mechanical = read_optional_json::<MechanicalReport>(&run_dir.join("mechanical.json"))?;
    let judge = read_optional_json::<JudgeReport>(&run_dir.join("judge_report.json"))?;
    let reviewer_verdict =
        read_optional_json::<ReviewerVerdict>(&run_dir.join("reviewer_verdict.json"))?;
    let audit_trail = read_optional_json::<AuditReport>(&run_dir.join("audit_trail.json"))?;
    let scope = read_optional_json::<ScopeReport>(&run_dir.join("scope.json"))?;
    let regression = read_optional_json::<RegressionReport>(&run_dir.join("regression.json"))?;
    let tool_grades = read_optional_json::<ToolGradeReport>(&run_dir.join("tool_grades.json"))?;
    let workflow = match read_optional_json::<WorkflowReport>(&run_dir.join("workflow.json"))? {
        Some(w) => w,
        None => crate::workflow::analyze(&manifest),
    };

    Ok(ScoreBundle {
        manifest,
        mechanical,
        judge,
        reviewer_verdict,
        workflow,
        audit_trail,
        scope,
        regression,
        tool_grades,
    })
}

fn read_optional_json<T: serde::de::DeserializeOwned>(p: &Path) -> Result<Option<T>> {
    if !p.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
    let v =
        serde_json::from_str(&text).with_context(|| format!("parsing JSON at {}", p.display()))?;
    Ok(Some(v))
}

pub fn render(a: &ScoreBundle, b: &ScoreBundle) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Run A: {}\nRun B: {}\n\n",
        a.manifest.run.run_id, b.manifest.run.run_id
    ));

    out.push_str("== Outcome ==\n");
    match (&a.mechanical, &b.mechanical) {
        (Some(am), Some(bm)) => {
            out.push_str(&format!(
                "  mechanical:  {}/{} → {}/{}  (Δ {:+})\n",
                am.tests_passed,
                am.tests_total,
                bm.tests_passed,
                bm.tests_total,
                bm.tests_passed as i32 - am.tests_passed as i32
            ));
        }
        _ => out.push_str("  mechanical: (one or both runs missing)\n"),
    }
    let total_a = effective_total(a);
    let total_b = effective_total(b);
    match (total_a, total_b) {
        (Some(ta), Some(tb)) => out.push_str(&format!(
            "  qualitative: {}/15 → {}/15  (Δ {:+})\n",
            ta,
            tb,
            tb as i32 - ta as i32
        )),
        _ => out.push_str("  qualitative: (one or both runs missing)\n"),
    }

    out.push_str("\n== Workflow ==\n");
    out.push_str(&format!(
        "  total tool calls:  {} → {}\n",
        a.workflow.total_tool_calls, b.workflow.total_tool_calls
    ));
    out.push_str(&format!(
        "  retries:           {} → {}\n",
        a.workflow.retry_calls, b.workflow.retry_calls
    ));
    out.push_str(&format!(
        "  empty-result rate: {:.1}% → {:.1}%\n",
        a.workflow.empty_result_rate * 100.0,
        b.workflow.empty_result_rate * 100.0
    ));
    out.push_str(&format!(
        "  elapsed (s):       {} → {}\n",
        a.workflow.elapsed_seconds, b.workflow.elapsed_seconds
    ));

    out.push_str("\n== Audit trail (run B reads run A's notes?) ==\n");
    match &b.audit_trail {
        Some(at) => out.push_str(&format!(
            "  coverage: {:.1}%   {} matched / {} substantive run-A notes; {} run-B `notes` queries\n",
            at.coverage * 100.0,
            at.matched_notes,
            at.run1_substantive_notes,
            at.run2_notes_queries
        )),
        None => out.push_str("  (audit_trail.json absent — only meaningful for the run-2-vs-run-1 pair)\n"),
    }

    out.push_str("\n== Scope compliance ==\n");
    let render_scope = |label: &str, sr: &Option<ScopeReport>, out: &mut String| match sr {
        Some(s) => out.push_str(&format!(
            "  {}: {:.1}%  ({} in-scope / {} total; {} OOS additions, {} OOS deletions)\n",
            label,
            s.scope_compliance * 100.0,
            s.in_scope_changes.len(),
            s.total_changes,
            s.additions_out_of_scope,
            s.deletions_out_of_scope
        )),
        None => out.push_str(&format!("  {}: (scope.json absent)\n", label)),
    };
    render_scope("Run A", &a.scope, &mut out);
    render_scope("Run B", &b.scope, &mut out);

    out.push_str("\n== Test regressions ==\n");
    let render_reg = |label: &str, rr: &Option<RegressionReport>, out: &mut String| match rr {
        Some(r) => out.push_str(&format!(
            "  {}: regressions={}, fixes={}, baseline {}/{} → current {}/{}\n",
            label,
            r.regression_count,
            r.fixes.len(),
            r.baseline_passed,
            r.baseline_total,
            r.current_passed,
            r.current_total
        )),
        None => out.push_str(&format!("  {}: (regression.json absent)\n", label)),
    };
    render_reg("Run A", &a.regression, &mut out);
    render_reg("Run B", &b.regression, &mut out);

    out.push_str("\n== Tool grades (replay vs. oracle) ==\n");
    let render_grades = |label: &str, tg: &Option<ToolGradeReport>, out: &mut String| match tg {
        Some(g) => {
            let acc = if g.graded_calls == 0 {
                0.0
            } else {
                let correct: u32 = g.per_tool_summary.values().map(|s| s.correct_count).sum();
                correct as f64 / g.graded_calls as f64
            };
            out.push_str(&format!(
                "  {}: graded {}/{} ({:.1}% accuracy); {} ungradeable; {} replay errors\n",
                label,
                g.graded_calls,
                g.total_calls,
                acc * 100.0,
                g.ungradeable_calls,
                g.replay_errors
            ));
        }
        None => out.push_str(&format!("  {}: (tool_grades.json absent)\n", label)),
    };
    render_grades("Run A", &a.tool_grades, &mut out);
    render_grades("Run B", &b.tool_grades, &mut out);

    out.push_str("\n== Operator artifacts ==\n");
    out.push_str(&format!(
        "  charter_sha:    {} → {}\n",
        short(a.manifest.experiment_repo.charter_sha256.as_deref()),
        short(b.manifest.experiment_repo.charter_sha256.as_deref())
    ));
    out.push_str(&format!(
        "  spec count:     {} → {}\n",
        a.manifest.experiment_repo.spec_shas.len(),
        b.manifest.experiment_repo.spec_shas.len()
    ));
    out.push_str(&format!(
        "  git_head:       {} → {}\n",
        short(a.manifest.experiment_repo.git_head.as_deref()),
        short(b.manifest.experiment_repo.git_head.as_deref())
    ));
    out.push_str(&format!(
        "  models:         {} → {}\n",
        a.manifest
            .models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>()
            .join(","),
        b.manifest
            .models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>()
            .join(",")
    ));

    out
}

fn effective_total(b: &ScoreBundle) -> Option<u32> {
    if let Some(rv) = &b.reviewer_verdict {
        return Some(rv.total_adjusted);
    }
    b.judge.as_ref().map(|j| j.total)
}

fn short(s: Option<&str>) -> String {
    match s {
        Some(s) if s.len() >= 8 => s[..8].to_string(),
        Some(s) => s.to_string(),
        None => "—".to_string(),
    }
}
