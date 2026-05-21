//! Failure-class derivation — turn a (agent.json, witness.json) pair
//! into a single closed-enum class so a sweep across many cells can
//! be reduced to a class histogram.
//!
//! Pure: takes the persisted JSON shapes and produces a class. No I/O,
//! no daemon round-trips. The aggregator owns the walk; this module
//! owns the rule. Per ARCH §2.1 the enum is closed.
//!
//! Rules (evaluated in priority order — first match wins):
//!
//! 1. **Solved** — witness.pass_fraction > 0.85.
//! 2. **Partial** — 0 < witness.pass_fraction ≤ 0.85.
//! 3. **Hung** — agent exit = `timeout`. Wall cap fired.
//! 4. **AgentCrash** — agent exit = `crashed`.
//! 5. **TokenBudget** — agent exit = `tokens_exceeded`.
//! 6. **LoopTrap** — agent exit = `no_progress`. The no-progress
//!    detector cut off a stuck tool-call loop.
//! 7. **ToolDenied** — agent exit = `tool_denied`. Allowlist enforcement.
//! 8. **ParseFailedEnvelope** — `tool_calls.len() == 0` AND
//!    `final_assistant_text` contains a `<tool_call>` marker. Indicates
//!    the model TRIED to emit a tool call and the daemon parser
//!    rejected it (malformed envelope shape, missing `name`, etc.).
//! 9. **DaemonTruncate** — `tool_calls.len() == 0` AND
//!    `tokens_output < 200` AND `final_assistant_text` is non-empty.
//!    Indicates the daemon stopped the model too early (the 2026-05-21
//!    brace-tracker bug was this shape).
//! 10. **ModelChatted** — `tool_calls.len() == 0` AND
//!     `tokens_output >= 200` AND `final_assistant_text` is non-empty.
//!     The model talked itself into "done" without ever invoking a
//!     tool — pi declares agent_end on the first text-only turn.
//! 11. **EmptyResponse** — `tool_calls.len() == 0` AND
//!     `final_assistant_text` is empty. Agent produced nothing.
//! 12. **ToolCallNoop** — `tool_calls.len() > 0` AND no tool in the
//!     call list mutates the workdir (read/find/grep/ls only). The
//!     agent looked but never acted.
//! 13. **AlgorithmicWrong** — `tool_calls.len() > 0` AND at least one
//!     mutating call (write/edit/bash) AND tests fail. The agent
//!     tried; the algorithm was wrong.
//! 14. **WriteThrash** — agent exit = `write_thrash`. Model emitted
//!     N consecutive `write` tool calls without an interleaving
//!     `bash` verify. Each rewrite overlays partial code on top of
//!     partial code and the final file is incoherent. Distinct from
//!     LoopTrap because the workdir IS changing on each tool call
//!     — the no-progress detector wouldn't catch this. Distinct from
//!     AlgorithmicWrong because the model never stabilized on a single
//!     algorithm to be wrong about.
//!
//! Class 13 is the desired bottom of the funnel: every class above is
//! a *system* failure to convert agent intent into bench outcome; class
//! 13 is an *agent* failure to solve the problem correctly. Sweep
//! reports use this distinction to project the impact of structural
//! fixes ("alternation grammar would close classes 6 + 8 + 10").

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Closed enum per ARCH §2.1. Each variant is a distinct failure mode
/// or success outcome that maps to one and only one rule in the
/// derivation table above.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum FailureClass {
    Solved,
    Partial,
    Hung,
    AgentCrash,
    TokenBudget,
    LoopTrap,
    ToolDenied,
    ParseFailedEnvelope,
    DaemonTruncate,
    ModelChatted,
    EmptyResponse,
    ToolCallNoop,
    AlgorithmicWrong,
    WriteThrash,
}

impl FailureClass {
    pub fn id(&self) -> &'static str {
        match self {
            FailureClass::Solved => "solved",
            FailureClass::Partial => "partial",
            FailureClass::Hung => "hung",
            FailureClass::AgentCrash => "agent_crash",
            FailureClass::TokenBudget => "token_budget",
            FailureClass::LoopTrap => "loop_trap",
            FailureClass::ToolDenied => "tool_denied",
            FailureClass::ParseFailedEnvelope => "parse_failed_envelope",
            FailureClass::DaemonTruncate => "daemon_truncate",
            FailureClass::ModelChatted => "model_chatted",
            FailureClass::EmptyResponse => "empty_response",
            FailureClass::ToolCallNoop => "tool_call_noop",
            FailureClass::AlgorithmicWrong => "algorithmic_wrong",
            FailureClass::WriteThrash => "write_thrash",
        }
    }

    /// One-line description for the histogram printer.
    pub fn description(&self) -> &'static str {
        match self {
            FailureClass::Solved => "tests pass at >85%",
            FailureClass::Partial => "tests pass at 1-85%",
            FailureClass::Hung => "wall-clock cap fired",
            FailureClass::AgentCrash => "agent subprocess crashed",
            FailureClass::TokenBudget => "output-token budget exceeded",
            FailureClass::LoopTrap => "no-progress detector cut off a tool loop",
            FailureClass::ToolDenied => "agent attempted disallowed tool",
            FailureClass::ParseFailedEnvelope => "model emitted tool call; daemon parser rejected it",
            FailureClass::DaemonTruncate => "daemon stopped model early (<200 tokens, no tool call)",
            FailureClass::ModelChatted => "model talked itself into 'done' without a tool call",
            FailureClass::EmptyResponse => "agent produced no output",
            FailureClass::ToolCallNoop => "agent only read/grep'd, never wrote",
            FailureClass::AlgorithmicWrong => "agent wrote code; tests failed",
            FailureClass::WriteThrash => "agent wrote >=5x without bash verify between",
        }
    }

    /// True when the class indicates a *system* gap (bench, daemon, or
    /// parser) rather than a *model* capability gap. The histogram
    /// printer surfaces this so the operator can see at a glance how
    /// much of the failure pile is fixable upstream of the model.
    pub fn is_system_failure(&self) -> bool {
        matches!(
            self,
            FailureClass::Hung
                | FailureClass::AgentCrash
                | FailureClass::LoopTrap
                | FailureClass::ToolDenied
                | FailureClass::ParseFailedEnvelope
                | FailureClass::DaemonTruncate
                | FailureClass::EmptyResponse
        )
    }
}

/// Just enough of the persisted `agent.json` shape for class
/// derivation. The aggregator deserializes the file into this; we
/// don't depend on the runtime `AgentSummary` struct so the file
/// schema can evolve without breaking the derivation.
#[derive(Debug, Clone, Deserialize)]
pub struct PersistedAgentRun {
    pub tokens_input: u64,
    pub tokens_output: u64,
    #[serde(default)]
    pub wall_ms: u64,
    #[serde(default)]
    pub exit_reason: serde_json::Value,
    #[serde(default)]
    pub tool_calls: Vec<PersistedToolCall>,
    #[serde(default)]
    pub final_assistant_text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PersistedToolCall {
    pub tool: String,
    #[serde(default)]
    pub args_preview: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PersistedWitness {
    pub pass_fraction: f64,
    pub passed: u32,
    pub failed: u32,
    pub total: u32,
    #[serde(default)]
    pub verify_exit_ok: bool,
}

/// Tools that mutate the workdir. Reading the prompt scaffold or
/// grep'ing for definitions is NOT mutation; only write/edit/bash
/// count. `bash` is included because the agent can execute commands
/// that write files (touch, cat > file, cargo init, …).
const MUTATING_TOOLS: &[&str] = &["write", "edit", "bash"];

/// Token-output threshold below which a zero-tool-call response is
/// classified as `DaemonTruncate` rather than `ModelChatted`. Chosen
/// at 200 because a coherent model-chatted-itself-to-done response
/// typically clears 250+ tokens; the 88-token truncation we saw on
/// 2026-05-21 cleared this floor cleanly.
const DAEMON_TRUNCATE_TOKEN_FLOOR: u64 = 200;

/// Witness pass-fraction threshold for `Solved` (matches the
/// canonical bucket in problem.toml score_buckets — the top bucket
/// starts at 0.85).
const SOLVED_THRESHOLD: f64 = 0.85;

/// Derive a class from the (agent run, witness) pair. See module-level
/// docs for the rule table.
pub fn classify(
    agent: &PersistedAgentRun,
    witness: Option<&PersistedWitness>,
) -> FailureClass {
    // Rule 1, 2: witness-driven (when witness ran). Witness runs even
    // on a crashed agent so these arms fire BEFORE the exit-reason
    // arms — a partial implementation that crashed late still gets
    // credit for the tests it passed.
    if let Some(w) = witness {
        if w.pass_fraction > SOLVED_THRESHOLD {
            return FailureClass::Solved;
        }
        if w.pass_fraction > 0.0 {
            return FailureClass::Partial;
        }
    }

    // Rules 3-7: exit-reason-driven.
    let exit_kind = agent
        .exit_reason
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("completed");
    match exit_kind {
        "timeout" => return FailureClass::Hung,
        "crashed" => return FailureClass::AgentCrash,
        "tokens_exceeded" => return FailureClass::TokenBudget,
        "no_progress" => return FailureClass::LoopTrap,
        "write_thrash" => return FailureClass::WriteThrash,
        "tool_denied" => return FailureClass::ToolDenied,
        _ => {}
    }

    // Rules 8-11: zero-tool-call branches.
    let tool_call_count = agent.tool_calls.len();
    let final_text_empty = agent.final_assistant_text.trim().is_empty();
    // The `<tool_call>` marker surviving into `final_assistant_text`
    // means at least one envelope was emitted that the daemon
    // parser did NOT extract — the strip pass only fires on parsed
    // envelopes, so an unparsed one stays visible in the text
    // stream pi accumulated. This is true regardless of how many
    // OTHER calls parsed successfully (a turn-1 read + a turn-2
    // failed-edit lands here even though tool_calls.len() == 1).
    let unparsed_envelope_visible =
        !final_text_empty && agent.final_assistant_text.contains("<tool_call>");
    if unparsed_envelope_visible {
        return FailureClass::ParseFailedEnvelope;
    }
    if tool_call_count == 0 {
        if final_text_empty {
            return FailureClass::EmptyResponse;
        }
        if agent.tokens_output < DAEMON_TRUNCATE_TOKEN_FLOOR {
            return FailureClass::DaemonTruncate;
        }
        return FailureClass::ModelChatted;
    }

    // Rules 12-13: tool calls happened, no unparsed envelopes left.
    let any_mutating = agent
        .tool_calls
        .iter()
        .any(|tc| MUTATING_TOOLS.iter().any(|m| tc.tool == *m));
    if !any_mutating {
        return FailureClass::ToolCallNoop;
    }
    FailureClass::AlgorithmicWrong
}

/// Convenience: load `agent.json` + `witness.json` (latter optional)
/// from a per-problem artifact dir and classify. Returns `None` when
/// `agent.json` is missing or unparseable — the aggregator skips
/// such cells rather than poisoning the histogram with a fake class.
pub fn classify_from_dir(problem_dir: &Path) -> Option<FailureClass> {
    let agent_path = problem_dir.join("agent.json");
    let agent_bytes = std::fs::read(&agent_path).ok()?;
    let agent: PersistedAgentRun = serde_json::from_slice(&agent_bytes).ok()?;

    let witness_path = problem_dir.join("witness.json");
    let witness: Option<PersistedWitness> = std::fs::read(&witness_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());

    Some(classify(&agent, witness.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(
        tool_calls: Vec<(&str, &str)>,
        tokens_output: u64,
        exit: serde_json::Value,
        final_text: &str,
    ) -> PersistedAgentRun {
        PersistedAgentRun {
            tokens_input: 0,
            tokens_output,
            wall_ms: 0,
            exit_reason: exit,
            tool_calls: tool_calls
                .into_iter()
                .map(|(t, a)| PersistedToolCall {
                    tool: t.to_string(),
                    args_preview: a.to_string(),
                })
                .collect(),
            final_assistant_text: final_text.to_string(),
        }
    }

    fn witness(pass_fraction: f64, total: u32) -> PersistedWitness {
        PersistedWitness {
            pass_fraction,
            passed: (pass_fraction * total as f64).round() as u32,
            failed: total - (pass_fraction * total as f64).round() as u32,
            total,
            verify_exit_ok: pass_fraction > 0.0,
        }
    }

    #[test]
    fn solved_wins_when_pass_fraction_clears_threshold() {
        let agent = run(vec![("write", "")], 500, json!({"kind": "completed"}), "");
        let w = witness(0.92, 12);
        assert_eq!(classify(&agent, Some(&w)), FailureClass::Solved);
    }

    #[test]
    fn partial_wins_when_some_tests_pass() {
        let agent = run(vec![("write", "")], 500, json!({"kind": "completed"}), "");
        let w = witness(0.42, 12);
        assert_eq!(classify(&agent, Some(&w)), FailureClass::Partial);
    }

    #[test]
    fn hung_when_exit_timeout() {
        let agent = run(
            vec![("read", "")],
            500,
            json!({"kind": "timeout", "detail": {"cap_seconds": 600}}),
            "",
        );
        let w = witness(0.0, 12);
        assert_eq!(classify(&agent, Some(&w)), FailureClass::Hung);
    }

    #[test]
    fn loop_trap_when_no_progress() {
        let agent = run(
            vec![("read", ""), ("read", ""), ("read", "")],
            500,
            json!({"kind": "no_progress", "detail": {"consecutive_tool_calls": 8, "threshold": 8}}),
            "",
        );
        let w = witness(0.0, 12);
        assert_eq!(classify(&agent, Some(&w)), FailureClass::LoopTrap);
    }

    #[test]
    fn parse_failed_when_zero_tools_but_envelope_marker_in_text() {
        let agent = run(
            vec![],
            500,
            json!({"kind": "completed"}),
            "I'll edit the file.\n<tool_call>{\"arguments\":\"…\"}</tool_call>",
        );
        let w = witness(0.0, 12);
        assert_eq!(
            classify(&agent, Some(&w)),
            FailureClass::ParseFailedEnvelope
        );
    }

    #[test]
    fn daemon_truncate_when_short_response_no_tool() {
        // The exact 2026-05-21 brace-tracker bug shape: 88 tokens,
        // mid-prose stop, no tool call.
        let agent = run(
            vec![],
            88,
            json!({"kind": "completed"}),
            "Let me set up the linear system:\n- Let x_{r,c}",
        );
        let w = witness(0.0, 12);
        assert_eq!(classify(&agent, Some(&w)), FailureClass::DaemonTruncate);
    }

    #[test]
    fn model_chatted_when_long_response_no_tool() {
        let agent = run(
            vec![],
            450,
            json!({"kind": "completed"}),
            "Here is my plan for solving the puzzle: (… 450 tokens of prose …)",
        );
        let w = witness(0.0, 12);
        assert_eq!(classify(&agent, Some(&w)), FailureClass::ModelChatted);
    }

    #[test]
    fn empty_response_when_no_tool_no_text() {
        let agent = run(vec![], 0, json!({"kind": "completed"}), "");
        let w = witness(0.0, 12);
        assert_eq!(classify(&agent, Some(&w)), FailureClass::EmptyResponse);
    }

    #[test]
    fn noop_when_only_read_tools() {
        let agent = run(
            vec![("read", ""), ("ls", ""), ("grep", "")],
            300,
            json!({"kind": "completed"}),
            "DONE",
        );
        let w = witness(0.0, 12);
        assert_eq!(classify(&agent, Some(&w)), FailureClass::ToolCallNoop);
    }

    #[test]
    fn algorithmic_wrong_when_write_but_tests_fail() {
        let agent = run(
            vec![("read", ""), ("write", "src/lib.rs"), ("bash", "cargo test")],
            800,
            json!({"kind": "completed"}),
            "DONE",
        );
        let w = witness(0.0, 12);
        assert_eq!(classify(&agent, Some(&w)), FailureClass::AlgorithmicWrong);
    }

    #[test]
    fn solved_beats_no_progress_when_witness_passes() {
        // Edge case: agent looped but enough writes landed that tests
        // pass. Witness signal wins — we credit the work.
        let agent = run(
            vec![("write", ""), ("read", "")],
            500,
            json!({"kind": "no_progress", "detail": {"consecutive_tool_calls": 8, "threshold": 8}}),
            "",
        );
        let w = witness(0.92, 12);
        assert_eq!(classify(&agent, Some(&w)), FailureClass::Solved);
    }

    #[test]
    fn loop_trap_beats_parse_failed_when_both_signals_present() {
        // Edge case: model emitted tool envelope text early then got
        // killed by no-progress. Exit signal wins.
        let agent = run(
            vec![("read", "")],
            500,
            json!({"kind": "no_progress", "detail": {"consecutive_tool_calls": 8, "threshold": 8}}),
            "<tool_call>{\"name\":\"read\"}</tool_call>",
        );
        let w = witness(0.0, 12);
        assert_eq!(classify(&agent, Some(&w)), FailureClass::LoopTrap);
    }

    #[test]
    fn parse_failed_envelope_beats_noop_when_unparsed_block_in_text() {
        // The 2026-05-21 sweep force0/1.1 shape: model emitted a turn-1
        // `read` (parsed OK) and a turn-2 `edit` envelope that the
        // daemon parser rejected (control-char escape doesn't recover
        // Qwen's nested-string emission). tool_calls.len() == 1 but
        // `<tool_call>` survives in final_assistant_text. The class
        // should attribute this to the system gap, not "agent only
        // read/grep'd."
        let agent = run(
            vec![("read", "src/lib.rs")],
            237,
            json!({"kind": "completed"}),
            "<think>plan</think>\n<tool_call>{\"arguments\":\"{...broken nested JSON...}\",\"name\":\"edit\"}</tool_call>",
        );
        let w = witness(0.0, 12);
        assert_eq!(
            classify(&agent, Some(&w)),
            FailureClass::ParseFailedEnvelope
        );
    }

    #[test]
    fn is_system_failure_flags_upstream_classes() {
        assert!(FailureClass::ParseFailedEnvelope.is_system_failure());
        assert!(FailureClass::DaemonTruncate.is_system_failure());
        assert!(FailureClass::LoopTrap.is_system_failure());
        assert!(!FailureClass::Solved.is_system_failure());
        assert!(!FailureClass::AlgorithmicWrong.is_system_failure());
        assert!(!FailureClass::ModelChatted.is_system_failure());
    }
}
