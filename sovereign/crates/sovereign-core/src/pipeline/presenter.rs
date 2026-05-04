//! Presenter stage — voice-shaping pass on the Drafter's draft.
//!
//! The Drafter writes substantive prose against the curated
//! package; the Presenter rewrites that prose into the voice the
//! caller's [`SkillRegister`] requires. Two modes:
//!
//! - [`SkillRegister::Factual`] — citation cleanup + length cap.
//!   Minimal prompt; no voice judge; the goal is *don't mangle*
//!   the Drafter's substantive content while normalising
//!   formatting.
//! - [`SkillRegister::Relational`] — applies the eight Right-X
//!   folds from `RELATIONAL_BASE_SYSTEM_PROMPT`. Names the same
//!   folds so a presenter-stage failure surfaces the same
//!   axis-named issue the [`crate::pipeline::judge`] axes report.
//!
//! Always: [`crate::title::strip_think_blocks`] over the model
//! output before returning, so any chain-of-thought leak from the
//! Fast slot doesn't reach the user.
//!
//! Per the plan §3.2, an async voice-judge fires after the
//! Presenter on the Relational path; the Factual path skips it.
//! The judge runs as a `tokio::spawn` from the runtime — see
//! [`crate::pipeline::judge`] for the request builder + score
//! type.

use std::sync::Arc;

use crate::error::Result;
use crate::skills::SkillRegister;
use crate::title::strip_thinking_response;
use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Speed};

/// What the Presenter returns. The text is the user-visible reply
/// the SSE bridge will stream; `register` is propagated so the
/// runtime can decide whether to invoke the async voice judge.
#[derive(Debug, Clone)]
pub struct PresentedOutput {
    pub text: String,
    pub register: SkillRegister,
}

/// Hard cap on Presenter output. iter7 — 240 tokens (~840 chars).
/// iter5 at 200 truncated otherwise-clean responses mid-sentence
/// (scenario 01 emitted "I can't confirm whether Jordan" then was
/// chopped). 240 leaves room for a 2-paragraph witness reply
/// without licensing rambling. Caller's `max_tokens` is honoured
/// when smaller.
pub const PRESENTER_MAX_TOKENS_CAP: u32 = 240;

/// Deterministic post-processing on Presenter output. iter4 — all
/// the mechanical artifact stripping that iter1–iter3 had been
/// (badly) trying to teach the LLM lives here as code instead.
/// Loading "strip `---` separator lines" into the prompt was
/// causing Qwen 9B to *narrate the cleanup task* instead of
/// executing it (07-job iter3 emitted "Let me analyze the draft and
/// identify what needs to be cleaned up: 1. **Strip `---`...** —
/// None present...").
///
/// Patterns removed (in order):
/// 1. `<think>…</think>` blocks via [`strip_thinking_response`]
///    (handles unclosed openers + markdown-preamble shapes too).
/// 2. Leading `---` separator lines.
/// 3. Leading markdown labels: `**Rewritten:**`, `**Response:**`,
///    `**Polished Response:**`, `**Analysis:**`, `**Final:**`,
///    `**Output:**`, `**Reply:**` (with or without trailing colon
///    or following body).
/// 4. Leading "Sure, here's…", "Here's the polished…", "Here is…",
///    "I'll polish…", "Let me clean…" preambles up to the first
///    real sentence.
/// 5. Trailing meta lines: "Let me know if…", "I hope this
///    helps…", "Does that resonate?", "Hope this is what you were
///    looking for".
///
/// Idempotent: running over already-clean text returns the same
/// text. Benign on edge cases (empty / whitespace / just a label
/// → returns empty string after trim).
pub fn strip_presenter_artifacts(raw: &str) -> String {
    // Stage 1: think tags (delegated to existing helper).
    let mut text = strip_thinking_response(raw);

    // Stage 2: leading `---` separator lines (Markdown HR / draft
    // separator). Repeat to catch `---\n---\n`.
    loop {
        let next = {
            let trimmed = text.trim_start();
            if let Some(rest) = trimmed.strip_prefix("---") {
                let after = rest.trim_start_matches([' ', '\t']);
                if let Some(after_nl) = after.strip_prefix('\n') {
                    Some(after_nl.to_string())
                } else if after.is_empty() {
                    Some(String::new())
                } else {
                    None
                }
            } else {
                None
            }
        };
        match next {
            Some(s) => text = s,
            None => break,
        }
    }

    // Stage 3: leading markdown labels. Match `**Label:**` (with
    // optional trailing whitespace/newline) at the very start.
    const LABELS: &[&str] = &[
        "**Rewritten Response:**",
        "**Rewritten:**",
        "**Polished Response:**",
        "**Polished:**",
        "**Final Response:**",
        "**Final:**",
        "**Response:**",
        "**Analysis:**",
        "**Output:**",
        "**Reply:**",
        "**Edit:**",
    ];
    loop {
        let next = {
            let trimmed = text.trim_start();
            let mut found: Option<String> = None;
            for label in LABELS {
                if let Some(after) = trimmed.strip_prefix(label) {
                    found = Some(
                        after
                            .trim_start_matches([' ', '\t', '\n', '\r'])
                            .to_string(),
                    );
                    break;
                }
            }
            found
        };
        match next {
            Some(s) => text = s,
            None => break,
        }
    }

    // Stage 4: leading preamble sentences. Strip up to the first
    // double-newline (or single newline) when the FIRST sentence is
    // recognisable as a preamble.
    const PREAMBLE_PREFIXES: &[&str] = &[
        "Sure, here's ",
        "Sure, here is ",
        "Here's the polished ",
        "Here's a polished ",
        "Here is the polished ",
        "Here is a polished ",
        "Here's the rewritten ",
        "Here is the rewritten ",
        "Here's the cleaned ",
        "Here is the cleaned ",
        "I'll polish ",
        "I'll clean ",
        "Let me clean ",
        "Let me polish ",
        "Let me analyze the draft",
        "Let me work through",
    ];
    let next = {
        let trimmed = text.trim_start();
        if PREAMBLE_PREFIXES.iter().any(|p| trimmed.starts_with(p)) {
            if let Some(idx) = trimmed.find("\n\n") {
                Some(trimmed[idx + 2..].to_string())
            } else if let Some(idx) = trimmed.find('\n') {
                Some(trimmed[idx + 1..].to_string())
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some(s) = next {
        text = s;
    }

    // Stage 5: trailing meta lines. Strip from the LAST occurrence
    // of any matching opener to end of string.
    const TAIL_MARKERS: &[&str] = &[
        "\nLet me know if ",
        "\nI hope this helps",
        "\nHope this helps",
        "\nDoes that resonate",
        "\nDoes that make sense",
        "\nHope this is what you ",
        "\nFeel free to ",
        "\nLet me know how ",
    ];
    for marker in TAIL_MARKERS {
        if let Some(idx) = text.rfind(marker) {
            text.truncate(idx);
        }
    }

    text.trim().to_string()
}

/// Run the Presenter stage on a Drafter draft. Fast slot, single
/// completion. The mode-specific prompt is selected from `register`.
///
/// `max_tokens` is the cap for the Presenter's own emission — not
/// the original caller's budget. Sized smaller than the Drafter's
/// budget on the principle that voice-shaping rarely needs more
/// tokens than substantive drafting.
pub async fn present(
    provider: Arc<dyn InferenceProvider>,
    user_message: &str,
    draft: &str,
    register: SkillRegister,
    max_tokens: u32,
) -> Result<PresentedOutput> {
    let request = present_request(user_message, draft, register, max_tokens);
    let response = provider.complete(&request).await?;
    let text = strip_presenter_artifacts(&response.text);
    Ok(PresentedOutput { text, register })
}

/// Build the Presenter's `CompletionRequest`. Exposed `pub` so the
/// presenter-delta `voice_eval` harness mode can build the same
/// request the runtime would and test against frozen pre-presenter
/// drafts (see plan §Iteration loops).
pub fn present_request(
    user_message: &str,
    draft: &str,
    register: SkillRegister,
    max_tokens: u32,
) -> CompletionRequest {
    // iter6: short imperative system prompts (no "you are an
    // expert" framing), procedural checklist + one worked example
    // in the user prompt, co-located with the data. Applies the
    // small-model prompt-engineering principles: checklist beats
    // essay, few-shot beats explanation, decide locally not
    // globally, positive specs only (negatives leak as
    // in-context examples). The witness contract concepts are
    // demonstrated via the worked example, not declared via
    // listed rules.
    let system: &str = match register {
        SkillRegister::Factual => PRESENTER_FACTUAL_SYSTEM,
        SkillRegister::Relational => PRESENTER_RELATIONAL_SYSTEM_ITER6,
    };
    let prompt = match register {
        SkillRegister::Relational => format!(
            // iter7: pure few-shot. iter6's numbered procedure
            // section taught the model to emit numbered analysis
            // instead of executing — model output started with "Let
            // me analyze this carefully: 1. **What the user
            // asked**: ...". Removing the procedure removes the
            // shape the model was mirroring. Two examples (typical
            // case + edge case where the record has nothing) in
            // non-bench domains anchor the SHAPE of the witness
            // move without test leakage.
            "Two examples of the kind of reply we want, then your turn.\n\n\
             ---\n\n\
             User's message: Have I been making progress at the gym?\n\
             Drafter notes: one workout log entry from six months ago, \
             nothing since.\n\n\
             Reply: The only gym entry I have is from six months back — \
             nothing logged since then. One data point can't show a \
             trend; you'd need a couple more entries over time before \
             progress becomes visible.\n\n\
             ---\n\n\
             User's message: Did I ever finish that side project I was \
             excited about last summer?\n\
             Drafter notes: nothing in the record about a summer side \
             project.\n\n\
             Reply: I don't have anything in the record about a side \
             project from last summer. If you can name it I can check \
             again, but as it stands there's nothing for me to look at.\n\n\
             ---\n\n\
             User's message: {user_message}\n\
             Drafter notes: {draft}\n\n\
             Reply:"
        ),
        SkillRegister::Factual => {
            let _ = user_message;
            format!(
                "Polish the draft per your system message — preserve \
                 substance, normalise formatting.\n\n\
                 <draft>\n{draft}\n</draft>\n\n\
                 Begin with the first sentence of the response."
            )
        }
    };

    // iter3 (Primary slot for Presenter): the Presenter is what
    // the user *sees* — the streaming surface of the chat
    // interface. The legacy chat path used Primary for the
    // user-visible reply and that's the voice users associate with
    // the system. Putting the Presenter on the Fast slot in
    // iter0–iter2 swapped the streaming voice mid-turn, which both
    // made the surface feel less assured AND put the witness
    // contract on a smaller model that couldn't hold it. Slow here
    // means the same Primary slot the Drafter just used — already
    // warm, KV-cache hot.
    let mut req = CompletionRequest::new(&prompt).with_speed(Speed::Slow);
    req.system_message = Some(system.to_string());
    // iter2 hard cap retained: 320 tokens (~1000 chars) so a
    // Presenter failure can't run away. The cap is even more
    // important on Primary because runaway tokens cost more
    // wall-clock here than on Fast.
    req.max_tokens = Some(max_tokens.min(PRESENTER_MAX_TOKENS_CAP) as usize);
    // 0.3 — low enough to suppress paraphrase, high enough to
    // resolve into prose. Tested across iter0 (0.4) → iter1 (0.1,
    // locked into instruction-mirroring) → iter2 (0.3, current
    // sweet spot).
    req.temperature = Some(0.3);
    // Suppress the Fast slot's chain-of-thought — the Presenter is
    // a one-shot edit, not a planning step. enable_thinking: false
    // also reduces the stray-`</think>` rate the Phase 1.3 hygiene
    // fix has to repair.
    req.enable_thinking = Some(false);
    req
}

/// Minimal Factual-register system prompt: tighten formatting,
/// preserve citations, hold a length cap. No voice rules — the
/// Drafter's output is already factual; the Presenter's job here
/// is housekeeping, not rewriting. ~20 lines per the plan.
pub(crate) const PRESENTER_FACTUAL_SYSTEM: &str = "\
You are the Presenter for a factual response. Your job is to take \
the supplied draft and lightly normalise it for the user. Do not \
rewrite the substance; do not change citations; do not add or \
remove claims.\n\
\n\
What to do:\n\
- Strip any leftover meta-commentary (\"Here is the response...\", \
  \"Sure, I'll explain...\", trailing \"Let me know if...\").\n\
- Keep all citations exactly as they appear in the draft.\n\
- Preserve markdown structure (headings, lists, code blocks).\n\
- If the draft contains a `<think>` block or chain-of-thought \
  leak, drop it entirely.\n\
- If the draft is already clean and well-formatted, return it \
  unchanged.\n\
\n\
What NOT to do:\n\
- Do not add new content the draft doesn't contain.\n\
- Do not remove caveats or hedges the draft surfaced.\n\
- Do not rewrite for tone — this is a factual register.\n\
- Do not add a preamble or sign-off to your output.\n\
\n\
Reply with the cleaned response only.";

// iter4: PRESENTER_RELATIONAL_SYSTEM was removed; iter4–iter5
// used `runtime::epistemic_contract_for` (the legacy essay-style
// witness contract) directly.
//
// iter6: short imperative system prompt that names the role in
// one line, leaves the actual procedure to the user prompt
// (where it sits next to the data per the co-locate principle),
// and demonstrates the witness contract via a worked example
// rather than declaring it via listed rules. The legacy contract
// is still the design source — but it lives as a worked example
// in the user prompt now, where small models pattern-match on
// it harder than on a paragraph rubric.
pub(crate) const PRESENTER_RELATIONAL_SYSTEM_ITER6: &str = "\
Respond to a user. The Drafter wrote reference notes; you decide \
how to say it. Use the user's own words for names, dates, and \
counts. Match confidence to evidence. Be brief.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn present_request_factual_uses_factual_system() {
        let req = present_request("user q", "the draft", SkillRegister::Factual, 1024);
        let sys = req.system_message.as_deref().unwrap();
        assert!(sys.contains("Presenter for a factual response"));
        assert!(!sys.contains("witness"));
    }

    #[test]
    fn present_request_relational_iter6_short_imperative_system_prompt() {
        // iter6 — system prompt is one short imperative paragraph,
        // not the legacy essay-style witness contract. The contract
        // is demonstrated via a worked example in the USER prompt,
        // co-located with the data, per the small-model
        // prompt-engineering principles (checklist + few-shot beats
        // essay-style rule list).
        let req = present_request("user q", "the draft", SkillRegister::Relational, 1024);
        let sys = req.system_message.as_deref().unwrap();
        // Short imperative — no "RIGHT X" rule headers, no
        // "you are an expert" framing.
        assert!(sys.len() < 400, "system prompt should be short ({} chars)", sys.len());
        assert!(!sys.contains("RIGHT ATTENTION"));
        assert!(!sys.contains("witness, not a performer"));
        // But the load-bearing concepts surface compactly: confidence
        // calibration, brevity, user's own words.
        assert!(sys.to_lowercase().contains("confidence"));
        assert!(sys.to_lowercase().contains("brief"));
    }

    #[test]
    fn present_request_relational_iter7_pure_few_shot_no_procedure() {
        // iter7 — pure few-shot, no numbered procedure. iter6 had a
        // `## Procedure / 1. / 2. / 3.` section and the model
        // mirrored that structure as visible analysis ("Let me
        // analyze this carefully: 1. **What the user asked**: ...").
        // Dropping the procedure removes the shape the model was
        // copying.
        let req = present_request("USER_MSG", "DRAFT_BODY", SkillRegister::Relational, 1024);
        let p = &req.prompt;
        // No procedure header — the only structural elements are the
        // example separators (`---`) and `Reply:` cues.
        assert!(!p.contains("## Procedure"));
        assert!(!p.contains("Procedure:"));
        // Two examples present (typical case + edge case where
        // the record has nothing).
        let separator_count = p.matches("\n---\n").count();
        assert!(
            separator_count >= 3,
            "expect ≥3 `---` separators (after each example + before user turn), got {separator_count}"
        );
        // Bench scenario names MUST NOT appear in the examples.
        for forbidden in &[
            "Jordan", "Aleksei", "Devi", "Mark", "Sam",
            "therapist", "anxiety", "depression",
        ] {
            assert!(
                !p.contains(forbidden),
                "few-shot example must not crib from bench scenarios; found `{forbidden}`"
            );
        }
        // Data slots present.
        assert!(p.contains("USER_MSG"));
        assert!(p.contains("DRAFT_BODY"));
        // No "polish" / "rewrite" / "analyze" — those invite
        // instruction-mirroring on the small Fast slot.
        assert!(!p.to_lowercase().contains("polish"));
        assert!(!p.to_lowercase().contains("rewrite"));
        assert!(!p.to_lowercase().contains("analyze"));
    }

    #[test]
    fn present_request_relational_includes_user_message_and_draft() {
        // The Presenter prompt must include both the user message
        // and the Drafter notes — without the user message, iter4
        // showed the model defaults to writing about the draft.
        let req = present_request(
            "USER_MSG",
            "DRAFT_BODY",
            SkillRegister::Relational,
            1024,
        );
        assert!(req.prompt.contains("USER_MSG"));
        assert!(req.prompt.contains("DRAFT_BODY"));
    }

    #[test]
    fn strip_presenter_artifacts_removes_think_block() {
        let raw = "<think>planning out my reply</think>The actual reply.";
        assert_eq!(strip_presenter_artifacts(raw), "The actual reply.");
    }

    #[test]
    fn strip_presenter_artifacts_removes_leading_dash_line() {
        let raw = "---\n\nYou said you don't have enough.";
        assert_eq!(
            strip_presenter_artifacts(raw),
            "You said you don't have enough."
        );
    }

    #[test]
    fn strip_presenter_artifacts_removes_markdown_label() {
        let raw = "**Rewritten Response:**\n\nYou mentioned Jordan once.";
        assert_eq!(
            strip_presenter_artifacts(raw),
            "You mentioned Jordan once."
        );
    }

    #[test]
    fn strip_presenter_artifacts_handles_chained_artifacts() {
        // The 02-rich iter3 failure mode: `---\n\n**Response:**` as
        // the entire output. Both stages strip; result is empty.
        let raw = "---\n\n**Response:**";
        assert_eq!(strip_presenter_artifacts(raw), "");
    }

    #[test]
    fn strip_presenter_artifacts_removes_preamble() {
        let raw = "Sure, here's the polished version of your draft:\n\n\
                   You said only one mention of Jordan exists.";
        assert_eq!(
            strip_presenter_artifacts(raw),
            "You said only one mention of Jordan exists."
        );
    }

    #[test]
    fn strip_presenter_artifacts_removes_trailing_meta() {
        let raw = "You mentioned Aleksei in February.\n\nLet me know if you want more detail.";
        assert_eq!(
            strip_presenter_artifacts(raw),
            "You mentioned Aleksei in February."
        );
    }

    #[test]
    fn strip_presenter_artifacts_idempotent_on_clean_text() {
        let clean = "You told me on March 12 about your coworker Devi.";
        assert_eq!(strip_presenter_artifacts(clean), clean);
    }

    #[test]
    fn strip_presenter_artifacts_preserves_internal_dashes() {
        // Only LEADING `---` is a separator; internal use should
        // survive (e.g. citations, em dashes). Conservative test.
        let raw = "You said — once — that Jordan called.";
        assert_eq!(strip_presenter_artifacts(raw), raw);
    }

    #[test]
    fn present_request_caps_max_tokens_at_iter2_ceiling() {
        // iter2 — Presenter max_tokens hard-capped at
        // PRESENTER_MAX_TOKENS_CAP (320) regardless of caller input.
        // Closes the runaway-length failure where a meta-narrating
        // Presenter inherited the orchestrator's full 2048-token
        // budget and emitted ~600-token analysis blocks.
        let req = present_request("user q", "d", SkillRegister::Relational, 2048);
        assert_eq!(req.max_tokens, Some(PRESENTER_MAX_TOKENS_CAP as usize));
        // Smaller caller cap honoured.
        let req = present_request("user q", "d", SkillRegister::Relational, 128);
        assert_eq!(req.max_tokens, Some(128));
    }

    #[test]
    fn present_request_inlines_draft_in_user_prompt() {
        let req = present_request("user q", "DRAFT_BODY_MARKER", SkillRegister::Factual, 256);
        assert!(req.prompt.contains("DRAFT_BODY_MARKER"));
        assert!(req.prompt.contains("<draft>"));
        assert!(req.prompt.contains("</draft>"));
    }

    #[test]
    fn present_request_disables_thinking_to_avoid_stray_close_tags() {
        let req = present_request("user q", "d", SkillRegister::Relational, 256);
        assert_eq!(req.enable_thinking, Some(false));
    }

    #[test]
    fn present_request_uses_primary_slot() {
        // iter3: Presenter runs on the Primary (Slow) slot. It is
        // the user-facing streaming surface of the chat interface;
        // the legacy chat path used Primary, and putting the
        // Presenter on Fast in iter0-iter2 swapped the streaming
        // voice mid-turn AND put the witness contract on a smaller
        // model that couldn't hold it.
        let req = present_request("user q", "d", SkillRegister::Factual, 128);
        assert!(matches!(req.preferred_speed, Speed::Slow));
    }

    #[test]
    fn present_request_clamps_max_tokens_to_supplied_value() {
        // Caller cap below the iter2 hard ceiling (320) is honoured
        // verbatim — the cap is a max, not a min. A small caller
        // cap (e.g. a frozen presenter-delta harness scenario) must
        // still be respected.
        let req = present_request("user q", "d", SkillRegister::Factual, 200);
        assert_eq!(req.max_tokens, Some(200));
    }
}
