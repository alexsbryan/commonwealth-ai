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
//! Every applied level is glassboxed: a warn-level trace (module
//! target, so standard `sovereign_core=info` filters surface it) plus
//! a human-readable note the call sites thread into message
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

/// One turn's REAL assembled sizes, measured pre-trim at the enforce
/// site. Phase 2 of the sensor redesign: these land in the Runtime's
/// per-conversation `assembly_memo`, giving `estimate_compaction_pressure`
/// a floor based on what assembly actually demanded last turn (the
/// component estimate alone sees only history + memories + preamble —
/// roughly a third of the prompt), and giving the Phase-3 allocator its
/// demand signal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MeasuredAssembly {
    pub(crate) system_tokens: u32,
    pub(crate) prompt_tokens: u32,
    pub(crate) reserved: u32,
    pub(crate) ctx: u32,
}

impl MeasuredAssembly {
    pub(crate) fn input_tokens(&self) -> u32 {
        self.system_tokens.saturating_add(self.prompt_tokens)
    }
}

/// History budget for the NEXT assembly, derived from the previous
/// turn's measured demand (Phase 3). The knowledge bundle is governed
/// separately by the ctx-aware retrieval ceilings (which now read the
/// memo's REAL `system_tokens` instead of a static cushion); this
/// allocator owns the remaining flexible component — the rendered
/// history's per-message age caps. A coarse proportional controller:
/// shrink by last turn's overshoot ratio so assembly fits by
/// construction and the Phase-1 trim ladder becomes the rare backstop.
/// Self-correcting: the next memo reflects the shrunken render.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Allocation {
    /// Multiplier for the history age-caps (`chars_for_message_age`).
    /// 1.0 = defaults; floor 0.3 — recent-turn coreference is the
    /// last thing to sacrifice before the trim ladder takes over.
    pub(crate) history_scale: f32,
}

impl Allocation {
    pub(crate) fn identity() -> Self {
        Self { history_scale: 1.0 }
    }
}

/// Derive the next turn's allocation from the previous measured
/// demand. `None` memo (first turn of a conversation, or fresh
/// process) → identity: defaults are correct until proven otherwise.
pub(crate) fn allocate(prev: Option<&MeasuredAssembly>) -> Allocation {
    let Some(prev) = prev else {
        return Allocation::identity();
    };
    let available = prev
        .ctx
        .saturating_sub(prev.reserved)
        .saturating_sub(TEMPLATE_MARGIN_TOKENS);
    let demand = prev.input_tokens();
    if demand <= available || available == 0 {
        return Allocation::identity();
    }
    let ratio = (available as f32 / demand as f32).clamp(0.0, 1.0);
    let history_scale = ratio.max(0.3);
    tracing::info!(
        prev_demand = demand,
        available,
        history_scale,
        "allocation: scaling history render to last turn's overshoot"
    );
    Allocation { history_scale }
}

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

/// Enforce the budget contract on `request` in place, returning both
/// the outcome and the PRE-trim measurement (the demand signal the
/// memo records — what assembly wanted, not what survived). `count` is
/// the provider's tokenizer (`InferenceProvider::count_tokens`) and
/// `ctx` its `effective_context_size()`; both are threaded as plain
/// values so this module stays unit-testable without a provider.
pub(crate) fn enforce(
    request: &mut CompletionRequest,
    count: &dyn Fn(&str) -> u32,
    ctx: u32,
) -> (BudgetOutcome, MeasuredAssembly) {
    let reserved = request.max_tokens.unwrap_or(0) as u32;
    let budget_for_input = ctx
        .saturating_sub(reserved)
        .saturating_sub(TEMPLATE_MARGIN_TOKENS);

    let input_tokens = |req: &CompletionRequest| -> u32 {
        count(req.system_message.as_deref().unwrap_or("")).saturating_add(count(&req.prompt))
    };

    let measured = MeasuredAssembly {
        system_tokens: count(request.system_message.as_deref().unwrap_or("")),
        prompt_tokens: count(&request.prompt),
        reserved,
        ctx,
    };
    if measured.input_tokens() <= budget_for_input {
        return (BudgetOutcome::Fits, measured);
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
        return (trimmed_outcome(applied), measured);
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
        return (trimmed_outcome(applied), measured);
    }

    // ── Level 3: shrink the response reservation ──
    let after_trims = input_tokens(request);
    let available_for_response = ctx
        .saturating_sub(after_trims)
        .saturating_sub(TEMPLATE_MARGIN_TOKENS) as usize;
    if available_for_response >= MIN_RESPONSE_TOKENS {
        if request
            .max_tokens
            .is_some_and(|m| m > available_for_response)
        {
            applied.push(format!(
                "response reservation: {} → {available_for_response} tokens",
                request.max_tokens.unwrap_or(0),
            ));
            request.max_tokens = Some(available_for_response);
        }
        return (trimmed_outcome(applied), measured);
    }

    tracing::warn!(
        input_tokens = after_trims,
        ctx,
        reserved,
        "prompt budget: cannot fit even after maximal degradation — engine backstop will reject"
    );
    (
        BudgetOutcome::CannotFit {
            input_tokens: after_trims,
            ctx,
        },
        measured,
    )
}

fn trimmed_outcome(applied: Vec<String>) -> BudgetOutcome {
    let note = format!(
        "prompt trimmed to fit context window ({})",
        applied.join("; ")
    );
    tracing::warn!(%note, "prompt budget enforced");
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

    /// enforce returns the PRE-trim measurement — the demand signal,
    /// not the post-trim survivor.
    #[test]
    fn enforce_measures_pre_trim_demand() {
        let prompt = "evidence ".repeat(2000);
        let mut r = req("tiny system", &prompt, 256);
        let pre_prompt_tokens = est(&r.prompt);
        let (out, measured) = enforce(&mut r, &est, 2048);
        assert!(matches!(out, BudgetOutcome::Trimmed { .. }));
        assert_eq!(measured.prompt_tokens, pre_prompt_tokens);
        assert_eq!(measured.reserved, 256);
        assert_eq!(measured.ctx, 2048);
        // The request itself WAS trimmed below the measurement.
        assert!(est(&r.prompt) < measured.prompt_tokens);
    }

    #[test]
    fn allocate_is_identity_without_memo_or_when_fitting() {
        assert_eq!(allocate(None), Allocation::identity());
        let fits = MeasuredAssembly {
            system_tokens: 1000,
            prompt_tokens: 2000,
            reserved: 1024,
            ctx: 8192,
        };
        assert_eq!(allocate(Some(&fits)), Allocation::identity());
    }

    #[test]
    fn allocate_scales_history_by_overshoot_with_floor() {
        let over = MeasuredAssembly {
            system_tokens: 6000,
            prompt_tokens: 4000,
            reserved: 1024,
            ctx: 8192,
        };
        let a = allocate(Some(&over));
        assert!(a.history_scale < 1.0, "must shrink: {a:?}");
        assert!(a.history_scale >= 0.3, "floor: {a:?}");

        // Pathological demand pins to the floor, never below.
        let extreme = MeasuredAssembly {
            system_tokens: 100_000,
            prompt_tokens: 100_000,
            reserved: 1024,
            ctx: 8192,
        };
        assert_eq!(allocate(Some(&extreme)).history_scale, 0.3);
    }

    #[test]
    fn fitting_request_is_untouched() {
        let mut r = req("system", "prompt", 512);
        let before_prompt = r.prompt.clone();
        assert_eq!(enforce(&mut r, &est, 8192).0, BudgetOutcome::Fits);
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
        let (out, _) = enforce(&mut r, &est, 1600);
        let sys = r.system_message.as_deref().unwrap();
        assert!(matches!(out, BudgetOutcome::Trimmed { .. }), "got {out:?}");
        assert!(sys.contains("Base instructions."));
        assert!(sys.contains("Trailing block stays."));
        assert!(sys.contains(TRIM_MARKER));
        assert!(
            sys.contains("ASSISTANT: newest reply"),
            "newest line must survive"
        );
        assert!(
            !sys.contains("old message number 0"),
            "oldest line must drop"
        );
    }

    #[test]
    fn evidence_front_trim_preserves_tail_question() {
        let evidence = "evidence ".repeat(2000);
        let prompt = format!("{evidence}\nQUESTION: what is the answer?");
        let mut r = req("tiny system", &prompt, 256);
        let (out, _) = enforce(&mut r, &est, 2048);
        assert!(matches!(out, BudgetOutcome::Trimmed { .. }), "got {out:?}");
        assert!(r.prompt.starts_with(TRIM_MARKER));
        assert!(r.prompt.ends_with("QUESTION: what is the answer?"));
        let total = est(r.system_message.as_deref().unwrap()) + est(&r.prompt);
        assert!(
            total + 256 + TEMPLATE_MARGIN_TOKENS <= 2048,
            "total {total} too big"
        );
    }

    #[test]
    fn response_reservation_shrinks_before_giving_up() {
        // Input that fits the window but not alongside a 4096 reservation.
        let prompt = "evidence ".repeat(1200); // ~2700 tokens est
        let mut r = req("sys", &prompt, 4096);
        let (out, _) = enforce(&mut r, &est, 4096);
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
        let (out, _) = enforce(&mut r, &est, 300);
        assert!(
            matches!(out, BudgetOutcome::CannotFit { .. }),
            "got {out:?}"
        );
    }
}
