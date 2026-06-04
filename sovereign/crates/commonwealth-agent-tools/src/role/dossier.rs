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

/// Cap on the full verification (build/smoke) stdout_tail carried
/// from Evaluator to Implementer across role flips. The 200-char
/// summary in `recent_outcomes` loses line numbers, source-line
/// context, and compiler caret pointers — Implementer cannot fix
/// what it cannot see. 4 KiB carries a typical rustc / cargo test
/// failure with full caret + secondary spans without bloating
/// prompts.
pub const MAX_VERIFICATION_OUTPUT_LEN: usize = 4 * 1024;

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
    /// Concrete change list (pseudocode) emitted alongside the
    /// high-level plan. Each entry is a numbered one-line
    /// description of one distinct edit. Pinned in every
    /// Implementer turn so each patch is informed by the full set
    /// of pending changes, not just the most recent diagnosis.
    /// Closes the "patch reactively, break a sibling change" class
    /// observed on 4.2 2026-05-23.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pseudocode: Option<Vec<String>>,
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
    /// Full stdout_tail from the most recent build/smoke. Set on
    /// every Evaluator verify so the Implementer who picks up after
    /// a `handoff_to_implementer` sees the compiler's actual line +
    /// caret + source context — not just the 200-char "build
    /// FAILED" summary. Capped at `MAX_VERIFICATION_OUTPUT_LEN`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verification_output: Option<String>,
    /// Which primitive produced `last_verification_output` (Build or
    /// Smoke). Used to label the rendered block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verification_kind: Option<PrimitiveKind>,
    /// Whether `last_verification_output` came from a passing run.
    /// Drives `smoke_just_passed()` and through it the §B
    /// grammar-termination move: when the most recent verification
    /// was a passing smoke AND no writes have happened since, the
    /// Evaluator's effective tool subset shrinks to `[agent_done,
    /// handoff_to_implementer]`. Build/smoke literally cannot be
    /// re-emitted — closes the "Evaluator can't decide done" loop
    /// class structurally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verification_ok: Option<bool>,
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

    /// Set the pseudocode change list emitted by the Planner.
    /// Sticky across the run. Rendered prominently in every
    /// Implementer turn so each patch is informed by the full set.
    pub fn set_pseudocode(&mut self, pseudocode: Vec<String>) {
        if pseudocode.is_empty() {
            self.pseudocode = None;
        } else {
            self.pseudocode = Some(pseudocode);
        }
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

    /// Capture the full stdout_tail of a build/smoke call so the
    /// next Implementer turn (after `handoff_to_implementer`) sees
    /// the compiler's line + caret + source context. The `ok` flag
    /// drives `smoke_just_passed()` and the §B grammar-termination
    /// path. Capped.
    pub fn record_verification(&mut self, primitive: PrimitiveKind, ok: bool, output: &str) {
        // Only meaningful for the verifier primitives.
        debug_assert!(matches!(
            primitive,
            PrimitiveKind::Build | PrimitiveKind::Smoke
        ));
        self.last_verification_output = Some(cap_str(output, MAX_VERIFICATION_OUTPUT_LEN));
        self.last_verification_kind = Some(primitive);
        self.last_verification_ok = Some(ok);
    }

    /// True iff the most recent verification was a PASSING smoke
    /// AND no `write_file` has happened since (`writes_since_last_verify
    /// == 0`). When true, the Evaluator's next request is structurally
    /// restricted: the only legal next tools are `agent_done` and
    /// `handoff_to_implementer`. Build/smoke are excluded from the
    /// `tools` array entirely, so the OpenAI schema validator rejects
    /// any attempt — the build-loop-after-pass class is closed by
    /// construction, not by prompt or detector.
    ///
    /// Build ok alone does NOT trigger this: the model rightly
    /// proceeds to smoke after a green build. Only a smoke that has
    /// observed the binary execute cleanly counts as "we're done."
    pub fn smoke_just_passed(&self) -> bool {
        self.writes_since_last_verify == 0
            && self.last_verification_ok == Some(true)
            && self.last_verification_kind == Some(PrimitiveKind::Smoke)
    }

    /// True iff the most recent verification (build OR smoke) FAILED and
    /// no write has happened since (`writes_since_last_verify == 0`). When
    /// true, the Evaluator's next request is structurally restricted to
    /// `[handoff_to_implementer]`: re-running build/smoke on an unchanged
    /// workdir is deterministic waste (the dead-loop pathology), and
    /// `agent_done` is illegal with red tests. Forces the loop forward to
    /// an Implementer fix. Symmetric to `smoke_just_passed()`; closes the
    /// "Evaluator dead-loops on a failing verify" class (5.1-minilang
    /// trial-2 / v2, 2026-06-03) that previously only ended via a
    /// sticky/no-progress KILL — a 0/24 bomb — instead of a productive
    /// handoff.
    pub fn verification_just_failed(&self) -> bool {
        self.writes_since_last_verify == 0 && self.last_verification_ok == Some(false)
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
        // Pseudocode change list — sink-anchored for Implementer and
        // Evaluator so each turn sees the full set of pending edits
        // and can execute one-at-a-time without forgetting what
        // sibling changes are still due. Skipped for Planner because
        // it just emitted the list.
        if !matches!(for_role, Role::Planner) {
            if let Some(items) = self.pseudocode.as_deref() {
                if !items.is_empty() {
                    out.push_str("\nChange list (from Planner — execute these in order, do NOT discard any):\n");
                    for (i, item) in items.iter().enumerate() {
                        out.push_str(&format!("  {}. {}\n", i + 1, item));
                    }
                    out.push('\n');
                }
            }
        }
        if let Some(diagnosis) = self.diagnosis.as_deref() {
            out.push_str(&format!(
                "Diagnosis from {}: {diagnosis}\n",
                self.from_role.map(|r| r.id()).unwrap_or("?")
            ));
        }
        if let Some(summary) = self.last_action_summary.as_deref() {
            out.push_str(&format!("Last action: {summary}\n"));
        }
        out.push_str(&format!(
            "Verification staleness: {} writes since last build/smoke.\n",
            self.writes_since_last_verify
        ));
        // Implementer-only: full stdout_tail of the last build/smoke
        // so the model receives the compiler's structured output
        // (line numbers, caret, source spans), not just the
        // Evaluator's prose paraphrase via `diagnosis`. Other roles
        // don't need this: Planner doesn't fix code; Evaluator just
        // ran the verifier and has the result in chat history.
        if matches!(for_role, Role::Implementer) {
            if let (Some(out_text), Some(kind)) = (
                self.last_verification_output.as_deref(),
                self.last_verification_kind,
            ) {
                out.push_str(&format!(
                    "\nLast {} output (verbatim, capped {} bytes):\n```\n{}\n```\n",
                    kind.id(),
                    MAX_VERIFICATION_OUTPUT_LEN,
                    out_text
                ));
            }
        }
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
        PrimitiveKind::ReplaceFunction => {
            let name = result
                .payload
                .get("replaced")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let path = result
                .payload
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let replaced = result
                .payload
                .get("lines_replaced")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let inserted = result
                .payload
                .get("lines_inserted")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            format!("replaced fn {name} in {path} (-{replaced}/+{inserted} lines)")
        }
        PrimitiveKind::PatchFile => {
            let path = result
                .payload
                .get("patched")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let replaced = result
                .payload
                .get("lines_replaced")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let inserted = result
                .payload
                .get("lines_inserted")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            format!("patched {path} (-{replaced}/+{inserted} lines)")
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
            let ok = result
                .payload
                .get("ok")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
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
            let ok = result
                .payload
                .get("ok")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let passed = result
                .payload
                .get("passed")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let failed = result
                .payload
                .get("failed")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let total = result
                .payload
                .get("total")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
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
        return s.to_string();
    }
    // UTF-8 safe: walk back from `limit` to the previous char
    // boundary so we never slice mid-codepoint. Em-dashes and
    // other multibyte chars in Evaluator diagnoses (observed
    // 2026-05-23) used to panic the runner.
    let mut cut = limit;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…(+{} bytes)", &s[..cut], s.len() - cut)
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
        assert!(d
            .recent_outcomes
            .last()
            .unwrap()
            .summary
            .contains(&format!("#{}", MAX_DOSSIER_OUTCOMES + 4)));
    }

    #[test]
    fn dossier_render_includes_pseudocode_for_implementer() {
        // Closes class: "Implementer patches reactively because the
        // full change list isn't anchored in attention." If a
        // future PR drops the pseudocode rendering, multi-change
        // problems regress to one-cycle-per-bug iteration.
        let mut d = RoleDossier::new();
        d.set_plan("Fix four bugs in config_applier".into());
        d.set_pseudocode(vec![
            "1. deep_merge: lists replace, don't concat".into(),
            "2. expand_env: recurse to fixpoint".into(),
            "3. validate_schema: check missing required keys".into(),
        ]);
        let s = d.render(Role::Implementer);
        assert!(s.contains("Change list (from Planner"));
        assert!(s.contains("1. deep_merge"));
        assert!(s.contains("2. expand_env"));
        assert!(s.contains("3. validate_schema"));
    }

    #[test]
    fn dossier_render_skips_pseudocode_for_planner() {
        // Planner just emitted the list; rendering it back wastes
        // tokens. Skipped for that role only.
        let mut d = RoleDossier::new();
        d.set_pseudocode(vec!["1. do thing".into()]);
        let s = d.render(Role::Planner);
        assert!(!s.contains("Change list"));
    }

    #[test]
    fn dossier_pseudocode_empty_means_none() {
        let mut d = RoleDossier::new();
        d.set_pseudocode(vec!["1. one".into()]);
        assert!(d.pseudocode.is_some());
        d.set_pseudocode(Vec::new());
        assert!(d.pseudocode.is_none());
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
    fn cap_str_handles_multibyte_chars_at_boundary() {
        // Regression: 2026-05-23 panic when an em-dash (3 bytes)
        // straddled the cap. cap_str must walk back to the prev
        // char boundary rather than slicing mid-codepoint.
        let s = "x".repeat(198) + "—" + "y";
        // limit=200 lands inside the em-dash bytes.
        let out = cap_str(&s, 200);
        // Just assert it didn't panic and produced something.
        assert!(out.contains("…"));
        assert!(!out.contains("—") || out.len() < s.len());
    }

    #[test]
    fn diagnosis_is_capped() {
        let mut d = RoleDossier::new();
        d.set_diagnosis("x".repeat(2000));
        assert!(d.diagnosis.as_deref().unwrap().len() < 1100);
    }

    #[test]
    fn verification_output_renders_for_implementer() {
        // Closes class: "Implementer cannot fix compiler errors
        // because structured stdout_tail is lost on role flip."
        let mut d = RoleDossier::new();
        d.record_verification(
            PrimitiveKind::Build,
            false,
            "error: unexpected closing delimiter: `}`\n  --> src/lib.rs:14:1",
        );
        let s = d.render(Role::Implementer);
        assert!(s.contains("Last build output"));
        assert!(s.contains("error: unexpected closing delimiter"));
        assert!(s.contains("--> src/lib.rs:14:1"));
    }

    #[test]
    fn verification_output_does_not_render_for_evaluator_or_planner() {
        // Evaluator just ran the verifier — sees it in chat history.
        // Planner doesn't fix code. Rendering the block for them
        // would waste tokens.
        let mut d = RoleDossier::new();
        d.record_verification(PrimitiveKind::Build, false, "error: ...");
        assert!(!d.render(Role::Evaluator).contains("Last build output"));
        assert!(!d.render(Role::Planner).contains("Last build output"));
    }

    #[test]
    fn verification_output_is_capped() {
        let mut d = RoleDossier::new();
        let huge = "X".repeat(10_000);
        d.record_verification(PrimitiveKind::Smoke, true, &huge);
        assert!(
            d.last_verification_output.as_deref().unwrap().len()
                <= MAX_VERIFICATION_OUTPUT_LEN + 80
        );
    }

    // ── §B smoke_just_passed truth table ─────────────────────────
    //
    // The four-way truth table any future refactor must preserve:
    //
    //   primitive | ok    | writes_since | smoke_just_passed()
    //   ─────────────────────────────────────────────────────
    //   Smoke     | true  | 0            | true   ← only this triggers
    //   Smoke     | true  | >0           | false  (stale, code changed)
    //   Smoke     | false | 0            | false  (tests failed)
    //   Build     | true  | 0            | false  (need smoke too)
    //   (none)    | —     | —            | false  (fresh dossier)
    //
    // The runner conditions Evaluator's tool subset on this. Any
    // softening (returning true in a non-truth-table case) re-opens
    // the build-loop-after-pass class.

    #[test]
    fn smoke_just_passed_true_only_for_smoke_ok_with_no_writes_since() {
        let mut d = RoleDossier::new();
        d.record_verification(PrimitiveKind::Smoke, true, "ok");
        assert!(d.smoke_just_passed());
    }

    #[test]
    fn smoke_just_passed_false_for_failing_smoke() {
        let mut d = RoleDossier::new();
        d.record_verification(PrimitiveKind::Smoke, false, "1 test failed");
        assert!(!d.smoke_just_passed());
    }

    #[test]
    fn verification_just_failed_true_for_failing_smoke_no_writes() {
        let mut d = RoleDossier::new();
        d.record_verification(PrimitiveKind::Smoke, false, "1 test failed");
        assert!(d.verification_just_failed());
    }

    #[test]
    fn verification_just_failed_false_for_passing_smoke() {
        let mut d = RoleDossier::new();
        d.record_verification(PrimitiveKind::Smoke, true, "ok");
        assert!(!d.verification_just_failed());
    }

    #[test]
    fn verification_just_failed_false_after_write_lets_evaluator_reverify() {
        // Implementer edited after the failing smoke → the Evaluator must
        // be allowed to re-verify, not be force-handed-off on stale state.
        let mut d = RoleDossier::new();
        d.record_verification(PrimitiveKind::Smoke, false, "fail");
        d.on_write();
        assert!(!d.verification_just_failed());
    }

    #[test]
    fn smoke_just_passed_false_for_passing_build_alone() {
        // Build ok ≠ "we're done." Model should proceed to smoke.
        // If this returns true, the Evaluator would be locked out of
        // calling smoke after a green build — measurement bug.
        let mut d = RoleDossier::new();
        d.record_verification(PrimitiveKind::Build, true, "");
        assert!(!d.smoke_just_passed());
    }

    #[test]
    fn smoke_just_passed_false_after_write_invalidates() {
        // Implementer wrote after the passing smoke → result is now
        // stale; Evaluator must re-verify before terminating.
        let mut d = RoleDossier::new();
        d.record_verification(PrimitiveKind::Smoke, true, "ok");
        assert!(d.smoke_just_passed());
        d.on_write();
        assert!(!d.smoke_just_passed());
    }

    #[test]
    fn smoke_just_passed_false_on_fresh_dossier() {
        let d = RoleDossier::new();
        assert!(!d.smoke_just_passed());
    }

    #[test]
    fn smoke_just_passed_true_again_after_reverify() {
        // Implementer wrote, Evaluator re-verified, smoke passed:
        // the gate should re-engage. Pins that `on_verify` (which
        // resets writes_since_last_verify) cooperates with the
        // truth table — without it, post-write re-verification
        // would never re-arm the gate.
        let mut d = RoleDossier::new();
        d.record_verification(PrimitiveKind::Smoke, true, "ok");
        d.on_write();
        assert!(!d.smoke_just_passed());
        d.on_verify();
        d.record_verification(PrimitiveKind::Smoke, true, "ok");
        assert!(d.smoke_just_passed());
    }
}
