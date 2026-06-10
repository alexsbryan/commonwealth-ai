// SPDX-License-Identifier: AGPL-3.0-or-later
//! Prompt-budget guard — Phase 1 of the budget-sensor redesign.
//!
//! Enforces the contract no other layer enforces today:
//!
//! ```text
//! tokens(system) + tokens(prompt) + max_tokens(reserved) ≤ effective ctx
//! ```
//!
//! Before this guard, the only enforcement was the engine's
//! `clamp_max_tokens` **hard error** ("Prompt too long … Shorten the
//! conversation"), which surfaces to the user as a terminal error loop
//! with no recovery affordance — the failure class behind the
//! cancel-storm conversation-bricking finding (harness note 2cd9227e).
//! The compaction pressure sensor can't catch it because it measures
//! only history + memories + preamble; the dominant overflow
//! components (retrieval bundle, system base, response reservation)
//! are invisible to it. Converging the sensor on real assembled sizes
//! is Phase 2; this module just guarantees assembly never exceeds the
//! window.
//!
//! Applied at the two post-consolidation request-construction sites
//! (`prepare_knowledge_query_plan` → `KnowledgeQueryPlan.request`, and
//! the deep/simple path's request built from `KnowledgeContext`), i.e.
//! everything that carries a retrieval bundle. Tiny-prompt handlers
//! (conation, metalingual, …) can't realistically overflow and are out
//! of scope for Phase 1.
//!
//! Degradation order (stops at the first level that fits):
//!   1. trim the rendered history block inside the system message,
//!      oldest lines first (we own that render — see
//!      `text_utils::format_conversation_history`),
//!   2. front-trim the prompt's evidence body (instructions + question
//!      live at the tail on both pipelines; the oldest-ranked evidence
//!      is at the front),
//!   3. shrink the response reservation (`max_tokens`) down to a floor —
//!      a shorter answer beats a terminal error.
//!
//! Every applied level is glassboxed: a `runtime:prompt_budget` warn
//! plus a human-readable note the call sites thread into message
//! metadata, so the operator can see "this turn was trimmed to fit"
//! instead of silently degraded retrieval.

use crate::types::CompletionRequest;

/// Header line `format_conversation_history` renders — the anchor for
/// structural (line-wise) history trimming. Pinned by a unit test in
/// `text_utils` so a render change breaks loudly here.
const HISTORY_HEADER: &str = "Prior conversation (most recent last):";

/// Tokens reserved for chat-template framing (role markers, BOS/EOS,
/// think-tag scaffolding) that `count_tokens` on the raw strings can't
/// see, plus slack for the remote providers' chars/4 estimate.
const TEMPLATE_MARGIN_TOKENS: u32 = 128;

/// Never shrink the response reservation below this — an answer capped
/// here is terse but useful; below it the turn is better off failing
/// loudly so the operator sees a real configuration problem.
const MIN_RESPONSE_TOKENS: usize = 256;

/// Marker injected where content was removed, so the model (and any
/// operator reading a captured prompt) knows the elision happened.
const TRIM_MARKER: &str = "[earlier content trimmed to fit the context window]";

/// Outcome of [`enforce`], for glassbox surfaces.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BudgetOutcome {
    /// Request fit untouched — the overwhelmingly common case.
    Fits,
    /// Request was modified to fit. The note is operator-readable and
    /// belongs in message metadata.
    Trimmed { note: String },
    /// Even maximal degradation can't fit this request in the window
    /// (pathological: ctx smaller than the floor + margins). The
    /// engine error remains the backstop.
    CannotFit { input_tokens: u32, ctx: u32 },
}

/// Enforce the budget contract on `request` in place. `count` is the
/// provider's tokenizer (`InferenceProvider::count_tokens`) and `ctx`
/// its `effective_context_size()`; both are threaded as plain values
/// so this module stays unit-testable without a provider.
pub(crate) fn enforce(
    request: &mut CompletionRequest,
    count: &dyn Fn(&str) -> u32,
    ctx: u32,
) -> BudgetOutcome {
    let reserved = request.max_tokens.unwrap_or(0) as u32;
    let budget_for_input = ctx
        .saturating_sub(reserved)
        .saturating_sub(TEMPLATE_MARGIN_TOKENS);

    let input_tokens =
        |req: &CompletionRequest| -> u32 {
            count(req.system_message.as_deref().unwrap_or(""))
                .saturating_add(count(&req.prompt))
        };

    let initial = input_tokens(request);
    if initial <= budget_for_input {
        return BudgetOutcome::Fits;
    }

    let mut applied: Vec<String> = Vec::new();

    // ── Level 1: history block, oldest lines first ──
    if let Some(system) = request.system_message.as_deref() {
        if system.contains(HISTORY_HEADER) {
            let over = input_tokens(request).saturating_sub(budget_for_input);
            let trimmed = trim_history_block(system, over, count);
            if let Some((new_system, dropped)) = trimmed {
                request.system_message = Some(new_system);
                applied.push(format!("history: dropped {dropped} oldest line(s)"));
            }
        }
    }
    if input_tokens(request) <= budget_for_input {
        return trimmed_outcome(applied);
    }

    // ── Level 2: front-trim the prompt's evidence body ──
    {
        let over = input_tokens(request).saturating_sub(budget_for_input);
        // chars-per-token is provider-dependent; derive it from the
        // actual string being cut so the estimate self-corrects.
        let prompt_tokens = count(&request.prompt).max(1);
        let chars_per_token =
            (request.prompt.chars().count() as f32 / prompt_tokens as f32).max(1.0);
        // 10% slack so one pass usually suffices.
        let cut_chars = ((over as f32 * chars_per_token) * 1.1) as usize;
        if cut_chars > 0 && request.prompt.chars().count() > cut_chars {
            let keep_from = floor_char_boundary(&request.prompt, cut_chars);
            request.prompt = format!("{TRIM_MARKER}\n{}", &request.prompt[keep_from..]);
            applied.push(format!("evidence: front-trimmed ~{cut_chars} chars"));
        }
    }
    if input_tokens(request) <= budget_for_input {
        return trimmed_outcome(applied);
    }

    // ── Level 3: shrink the response reservation ──
    let after_trims = input_tokens(request);
    let available_for_response = ctx
        .saturating_sub(after_trims)
        .saturating_sub(TEMPLATE_MARGIN_TOKENS) as usize;
    if available_for_response >= MIN_RESPONSE_TOKENS {
        if request.max_tokens.is_some_and(|m| m > available_for_response) {
            applied.push(format!(
                "response reservation: {} → {available_for_response} tokens",
                request.max_tokens.unwrap_or(0),
            ));
            request.max_tokens = Some(available_for_response);
        }
        return trimmed_outcome(applied);
    }

    tracing::warn!(
        target: "runtime:prompt_budget",
        input_tokens = after_trims,
        ctx,
        reserved,
        "prompt budget: cannot fit even after maximal degradation — engine backstop will reject"
    );
    BudgetOutcome::CannotFit {
        input_tokens: after_trims,
        ctx,
    }
}

fn trimmed_outcome(applied: Vec<String>) -> BudgetOutcome {
    let note = format!("prompt trimmed to fit context window ({})", applied.join("; "));
    tracing::warn!(target: "runtime:prompt_budget", %note, "prompt budget enforced");
    BudgetOutcome::Trimmed { note }
}

/// Drop the oldest rendered lines from the history block until roughly
/// `over_tokens` are recovered (or the block is exhausted). Returns the
/// rewritten system message and the dropped-line count, or `None` when
/// nothing could be dropped. Operates ONLY between the history header
/// and the next blank line — the rest of the system message passes
/// through byte-identical.
fn trim_history_block(
    system: &str,
    over_tokens: u32,
    count: &dyn Fn(&str) -> u32,
) -> Option<(String, usize)> {
    let header_at = system.find(HISTORY_HEADER)?;
    let block_start = header_at + HISTORY_HEADER.len();
    let block_end = system[block_start..]
        .find("\n\n")
        .map(|i| block_start + i)
        .unwrap_or(system.len());
    let block = &system[block_start..block_end];

    let lines: Vec<&str> = block.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= 1 {
        return None; // keep at least the newest line for coreference
    }

    let mut recovered: u32 = 0;
    let mut drop_n = 0;
    // Oldest lines render first; always keep the newest.
    for line in &lines[..lines.len() - 1] {
        if recovered >= over_tokens {
            break;
        }
        recovered = recovered.saturating_add(count(line));
        drop_n += 1;
    }
    if drop_n == 0 {
        return None;
    }

    let kept = &lines[drop_n..];
    let mut new_block = String::new();
    new_block.push('\n');
    new_block.push_str(TRIM_MARKER);
    for line in kept {
        new_block.push('\n');
        new_block.push_str(line);
    }
    let mut out = String::with_capacity(system.len());
    out.push_str(&system[..block_start]);
    out.push_str(&new_block);
    out.push_str(&system[block_end..]);
    Some((out, drop_n))
}

/// Largest byte index ≤ the target char offset that is a char boundary.
fn floor_char_boundary(s: &str, char_offset: usize) -> usize {
    s.char_indices()
        .nth(char_offset)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// chars/4 — the trait's default estimator; good enough for tests.
    fn est(s: &str) -> u32 {
        (s.chars().count() / 4) as u32
    }

    fn req(system: &str, prompt: &str, max_tokens: usize) -> CompletionRequest {
        CompletionRequest {
            prompt: prompt.to_string(),
            system_message: Some(system.to_string()),
            max_tokens: Some(max_tokens),
            ..Default::default()
        }
    }

    #[test]
    fn fitting_request_is_untouched() {
        let mut r = req("system", "prompt", 512);
        let before_prompt = r.prompt.clone();
        assert_eq!(enforce(&mut r, &est, 8192), BudgetOutcome::Fits);
        assert_eq!(r.prompt, before_prompt);
        assert_eq!(r.max_tokens, Some(512));
    }

    #[test]
    fn history_block_trims_oldest_first_and_preserves_rest_of_system() {
        let old_lines: String = (0..40)
            .map(|i| format!("USER: old message number {i} {}\n", "pad ".repeat(30)))
            .collect();
        let system = format!(
            "Base instructions.\n\n{HISTORY_HEADER}\n{old_lines}ASSISTANT: newest reply\n\nTrailing block stays."
        );
        let prompt = "question ".repeat(50);
        let mut r = req(&system, &prompt, 512);
        // ctx chosen so history must shrink but levels 2-3 aren't needed.
        let out = enforce(&mut r, &est, 1600);
        let sys = r.system_message.as_deref().unwrap();
        assert!(matches!(out, BudgetOutcome::Trimmed { .. }), "got {out:?}");
        assert!(sys.contains("Base instructions."));
        assert!(sys.contains("Trailing block stays."));
        assert!(sys.contains(TRIM_MARKER));
        assert!(sys.contains("ASSISTANT: newest reply"), "newest line must survive");
        assert!(!sys.contains("old message number 0"), "oldest line must drop");
    }

    #[test]
    fn evidence_front_trim_preserves_tail_question() {
        let evidence = "evidence ".repeat(2000);
        let prompt = format!("{evidence}\nQUESTION: what is the answer?");
        let mut r = req("tiny system", &prompt, 256);
        let out = enforce(&mut r, &est, 2048);
        assert!(matches!(out, BudgetOutcome::Trimmed { .. }), "got {out:?}");
        assert!(r.prompt.starts_with(TRIM_MARKER));
        assert!(r.prompt.ends_with("QUESTION: what is the answer?"));
        let total = est(r.system_message.as_deref().unwrap()) + est(&r.prompt);
        assert!(total + 256 + TEMPLATE_MARGIN_TOKENS <= 2048, "total {total} too big");
    }

    #[test]
    fn response_reservation_shrinks_before_giving_up() {
        // Input that fits the window but not alongside a 4096 reservation.
        let prompt = "evidence ".repeat(1200); // ~2700 tokens est
        let mut r = req("sys", &prompt, 4096);
        let out = enforce(&mut r, &est, 4096);
        match out {
            BudgetOutcome::Trimmed { note } => {
                assert!(note.contains("response reservation"), "{note}");
                let m = r.max_tokens.unwrap();
                assert!((MIN_RESPONSE_TOKENS..4096).contains(&m), "max_tokens {m}");
            }
            other => panic!("expected Trimmed, got {other:?}"),
        }
    }

    #[test]
    fn pathological_window_reports_cannot_fit() {
        let prompt = "x ".repeat(4000);
        let mut r = req("sys", &prompt, 512);
        // Window smaller than even the floor + margin can absorb.
        let out = enforce(&mut r, &est, 300);
        assert!(matches!(out, BudgetOutcome::CannotFit { .. }), "got {out:?}");
    }
}
