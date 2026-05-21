//! `RoleDossier` — the ambient context packet that threads between
//! role calls. Borrows the shape of `sovereign-core::ToolDossier`
//! per the plan: each role call sees a small, structured summary
//! of what the prior role did + what's been tried so far + the
//! sticky plan from the Planner.

use serde::{Deserialize, Serialize};

use crate::primitive::PrimitiveKind;
use crate::result::ToolResult;
use crate::role::Role;

/// Cap on `recent_outcomes` entries rendered into the prompt.
/// Older outcomes drop off — the next role needs the recent
/// texture, not the run's full history.
pub const MAX_DOSSIER_OUTCOMES: usize = 8;

/// Cap on a single summary line (rendered into recent_outcomes
/// list).
pub const MAX_SUMMARY_LEN: usize = 200;

/// Ambient state packet threaded into each role's system message.
/// Built incrementally by the native runner as role calls happen.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoleDossier {
    /// The role that just yielded. `None` at run start (before any
    /// role has acted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_role: Option<Role>,
    /// The plan emitted by the Planner. Set ONCE on
    /// Planner→Implementer transition; sticky across every
    /// subsequent role call. Every Implementer + Evaluator call
    /// reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// One-line summary of the prior role's last action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_action_summary: Option<String>,
    /// Free-form diagnosis from `handoff_to_implementer` (the
    /// Evaluator's read of the build/smoke output) or the
    /// Implementer's `what_you_changed` from
    /// `handoff_to_evaluator`. Capped at 1 KB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnosis: Option<String>,
    /// How many `write_file` calls have happened since the last
    /// successful `build` or `smoke`. 0 means we just verified.
    pub writes_since_last_verify: u32,
    /// Recent outcomes (most recent last), capped.
    pub recent_outcomes: Vec<RoleDossierOutcome>,
}

/// One row of `recent_outcomes` — a frozen one-line view of a
/// prior tool call's result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleDossierOutcome {
    pub role: Role,
    pub primitive: PrimitiveKind,
    pub summary: String,
}

impl RoleDossier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push an outcome onto the history, applying the cap. Caller
    /// formats the summary via `summarize`.
    pub fn push_outcome(&mut self, role: Role, primitive: PrimitiveKind, summary: String) {
        self.recent_outcomes.push(RoleDossierOutcome {
            role,
            primitive,
            summary: cap_str(&summary, MAX_SUMMARY_LEN),
        });
        if self.recent_outcomes.len() > MAX_DOSSIER_OUTCOMES {
            let drop = self.recent_outcomes.len() - MAX_DOSSIER_OUTCOMES;
            self.recent_outcomes.drain(0..drop);
        }
    }

    /// Record that the just-finished role yielded; update
    /// `from_role` and `last_action_summary`.
    pub fn note_yield(&mut self, role: Role, action_summary: String) {
        self.from_role = Some(role);
        self.last_action_summary = Some(cap_str(&action_summary, MAX_SUMMARY_LEN));
    }

    /// Set the plan (Planner has emitted `agent_plan`). Sticky.
    pub fn set_plan(&mut self, plan: String) {
        self.plan = Some(plan);
    }

    /// Set diagnosis text from a handoff primitive. Capped at 1 KB.
    pub fn set_diagnosis(&mut self, diagnosis: String) {
        self.diagnosis = Some(cap_str(&diagnosis, 1024));
    }

    pub fn clear_diagnosis(&mut self) {
        self.diagnosis = None;
    }

    /// Track verification staleness.
    pub fn on_write(&mut self) {
        self.writes_since_last_verify = self.writes_since_last_verify.saturating_add(1);
    }

    pub fn on_verify(&mut self) {
        self.writes_since_last_verify = 0;
    }

    /// Render the dossier as a context block for the next role's
    /// system message. Returns an empty string when the dossier is
    /// fresh (no prior activity).
    pub fn render(&self, for_role: Role) -> String {
        let mut out = String::new();
        out.push_str(&format!("[Role: {}]\n", for_role.id()));
        if let Some(plan) = self.plan.as_deref() {
            out.push_str(&format!("Plan (from Planner): {plan}\n"));
        }
        if let Some(diagnosis) = self.diagnosis.as_deref() {
            out.push_str(&format!("Diagnosis from {}: {diagnosis}\n",
                self.from_role.map(|r| r.id()).unwrap_or("?")));
        }
        if let Some(summary) = self.last_action_summary.as_deref() {
            out.push_str(&format!("Last action: {summary}\n"));
        }
        out.push_str(&format!(
            "Verification staleness: {} writes since last build/smoke.\n",
            self.writes_since_last_verify
        ));
        if !self.recent_outcomes.is_empty() {
            out.push_str("Recent actions:\n");
            for o in &self.recent_outcomes {
                out.push_str(&format!(
                    "  - {} ran {}: {}\n",
                    o.role.id(),
                    o.primitive.id(),
                    o.summary
                ));
            }
        }
        out
    }
}

/// One-line summary of a tool call's result, keyed to its
/// primitive. Used to feed `RoleDossier.push_outcome`. Operates on
/// the canonical `ToolResult.payload` shape so the summarizer is
/// language-agnostic.
pub fn summarize(primitive: PrimitiveKind, result: &ToolResult) -> String {
    match primitive {
        PrimitiveKind::InspectWorkdir => {
            let intent = result
                .payload
                .get("intent")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            match intent {
                "file" => {
                    let path = result
                        .payload
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let bytes = result
                        .payload
                        .get("bytes")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    format!("read {path} ({bytes} bytes)")
                }
                "dir" => {
                    let path = result
                        .payload
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let entries = result
                        .payload
                        .get("entries")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    format!("listed {path}/ ({entries} entries)")
                }
                "find_by_name" | "grep_contents" => {
                    let matches = result
                        .payload
                        .get("matches")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    format!("{intent}: {matches} matches")
                }
                _ => format!("inspect_workdir {intent}"),
            }
        }
        PrimitiveKind::WriteFile => {
            let path = result
                .payload
                .get("wrote")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let bytes = result
                .payload
                .get("bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            format!("wrote {path} ({bytes} bytes)")
        }
        PrimitiveKind::Build => {
            let ok = result.payload.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            if ok {
                "build ok".to_string()
            } else {
                let tail = result
                    .payload
                    .get("stdout_tail")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let first_err = first_error_line(tail);
                format!("build FAILED: {first_err}")
            }
        }
        PrimitiveKind::Smoke => {
            let ok = result.payload.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let passed = result.payload.get("passed").and_then(|v| v.as_u64()).unwrap_or(0);
            let failed = result.payload.get("failed").and_then(|v| v.as_u64()).unwrap_or(0);
            let total = result.payload.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
            if total > 0 {
                format!(
                    "smoke {}: {}/{} passed, {} failed",
                    if ok { "ok" } else { "FAILED" },
                    passed,
                    total,
                    failed
                )
            } else if ok {
                "smoke ok".to_string()
            } else {
                "smoke FAILED (no test count parsed)".to_string()
            }
        }
        PrimitiveKind::AgentDone => {
            let reason = result
                .payload
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("done: {reason}")
        }
        PrimitiveKind::AgentPlan => {
            let plan = result
                .payload
                .get("plan")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let snippet: String = plan.chars().take(80).collect();
            format!("planned: {snippet}")
        }
        PrimitiveKind::HandoffToEvaluator => {
            let what = result
                .payload
                .get("what_you_changed")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("handoff→evaluator: {what}")
        }
        PrimitiveKind::HandoffToImplementer => {
            let diag = result
                .payload
                .get("diagnosis")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("handoff→implementer: {diag}")
        }
    }
}

fn first_error_line(tail: &str) -> String {
    tail.lines()
        .find(|l| l.contains("error"))
        .map(|l| cap_str(l.trim(), 160))
        .unwrap_or_else(|| "see stdout_tail".into())
}

fn cap_str(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        s.to_string()
    } else {
        format!("{}…(+{} bytes)", &s[..limit], s.len() - limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dossier_push_caps_history() {
        let mut d = RoleDossier::new();
        for i in 0..(MAX_DOSSIER_OUTCOMES + 5) {
            d.push_outcome(
                Role::Implementer,
                PrimitiveKind::WriteFile,
                format!("write #{i}"),
            );
        }
        assert_eq!(d.recent_outcomes.len(), MAX_DOSSIER_OUTCOMES);
        // Oldest dropped, newest retained.
        assert!(d.recent_outcomes[0].summary.contains("#5"));
        assert!(d.recent_outcomes.last().unwrap().summary.contains(&format!(
            "#{}",
            MAX_DOSSIER_OUTCOMES + 4
        )));
    }

    #[test]
    fn dossier_render_includes_plan_after_set() {
        let mut d = RoleDossier::new();
        d.set_plan("Use HashMap. Iterate. Done.".into());
        let s = d.render(Role::Implementer);
        assert!(s.contains("Plan (from Planner): Use HashMap"));
        assert!(s.contains("[Role: implementer]"));
    }

    #[test]
    fn dossier_writes_since_verify_increments_and_resets() {
        let mut d = RoleDossier::new();
        d.on_write();
        d.on_write();
        assert_eq!(d.writes_since_last_verify, 2);
        d.on_verify();
        assert_eq!(d.writes_since_last_verify, 0);
    }

    #[test]
    fn summarize_write_file() {
        let r = ToolResult::ok(json!({"wrote": "src/lib.rs", "bytes": 137}));
        let s = summarize(PrimitiveKind::WriteFile, &r);
        assert!(s.contains("src/lib.rs"));
        assert!(s.contains("137"));
    }

    #[test]
    fn summarize_build_ok() {
        let r = ToolResult::ok(json!({"ok": true, "stdout_tail": ""}));
        let s = summarize(PrimitiveKind::Build, &r);
        assert_eq!(s, "build ok");
    }

    #[test]
    fn summarize_build_failed_extracts_first_error() {
        let r = ToolResult::ok(json!({
            "ok": false,
            "stdout_tail": "   Compiling x\nerror[E0277]: bad bound at src/lib.rs:15:8\n  full trace below"
        }));
        let s = summarize(PrimitiveKind::Build, &r);
        assert!(s.starts_with("build FAILED"));
        assert!(s.contains("error[E0277]"));
    }

    #[test]
    fn summarize_smoke_with_counts() {
        let r = ToolResult::ok(json!({
            "ok": false,
            "passed": 2,
            "failed": 1,
            "total": 3,
        }));
        let s = summarize(PrimitiveKind::Smoke, &r);
        assert!(s.contains("2/3"));
        assert!(s.contains("1 failed"));
    }

    #[test]
    fn diagnosis_is_capped() {
        let mut d = RoleDossier::new();
        d.set_diagnosis("x".repeat(2000));
        assert!(d.diagnosis.as_deref().unwrap().len() < 1100);
    }
}
