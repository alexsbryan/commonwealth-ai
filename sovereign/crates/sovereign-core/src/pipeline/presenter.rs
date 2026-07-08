// SPDX-License-Identifier: AGPL-3.0-or-later
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
use crate::slot_policy::Workload;
use crate::types::{CompletionRequest, Speed};

/// What the Presenter returns. The text is the user-visible reply
/// the SSE bridge will stream; `register` is propagated so the
/// runtime can decide whether to invoke the async voice judge.
#[derive(Debug, Clone)]
pub struct PresentedOutput {
    pub text: String,
    pub register: SkillRegister,
}

/// Hard cap on Presenter output. iter10 — 1024 tokens (~3500
/// chars). Earlier caps (200/240/320) were bench-fitted to the
/// inner-work scenarios where witness replies are naturally short
/// (50-200 tokens). But real synthesis questions —
/// "is free will compatible with determinism?", "what's the
/// difference between objectivism and subjectivism?" — need
/// 800-1500 tokens to land well. The Presenter is the user-facing
/// streaming surface; it needs headroom. The bench may show length
/// blowouts on inner-work scenarios as a result, but those are a
/// SIGNAL that the Drafter's "be brief" discipline isn't holding,
/// not a runtime bug. Caller's `max_tokens` is honoured when
/// smaller.
pub const PRESENTER_MAX_TOKENS_CAP: u32 = 1024;

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
/// Remove `<tool_code>…</tool_code>` spans anywhere in `s`. A closed
/// span is removed inclusive of both tags; an unterminated open tag
/// (`<tool_code>` with no matching close — the model started a tool
/// call and stopped) drops everything from the open tag to the end.
/// Matches the lowercase tag the distilled models emit; all indices are
/// into `s` directly (no `to_lowercase()` remap, which could shift byte
/// offsets on non-ASCII text) and all other text is left intact.
fn strip_tool_code_blocks(s: &str) -> String {
    // BOTH envelope tags the distilled models reflex when no tool is wired:
    // `<tool_code>…</tool_code>` and `<tool_call>…</tool_call>` (closed, or
    // unterminated → drop from the open tag to end). A real tool call travels
    // as structured `tool_choice`/`tools`, never as prose — presentation-only.
    fn pass(s: &str, open: &str, close: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut cursor = 0usize;
        while let Some(rel) = s[cursor..].find(open) {
            let open_at = cursor + rel;
            out.push_str(&s[cursor..open_at]);
            match s[open_at..].find(close) {
                Some(rel_close) => {
                    cursor = open_at + rel_close + close.len();
                }
                None => {
                    cursor = s.len();
                    break;
                }
            }
        }
        out.push_str(&s[cursor..]);
        out
    }
    let s = pass(s, "<tool_code", "</tool_code>");
    let s = pass(&s, "<tool_call", "</tool_call>");
    strip_fenced_tool_blocks(&s)
}

/// Strip markdown-fenced tool-call blocks — the same reflex as the `<tool_call>`
/// envelope but wearing a code-fence, observed 2026-07-08 (8h chaos run, folder
/// corpus): the model emitted ```` ```tool_call:knowledge_lookup ```` as the
/// answer. A fence whose info-string begins `tool_call`/`tool_code` is never
/// legitimate answer content — it is a tool-invocation envelope. Remove the whole
/// fenced span; if the closing fence is missing (abandoned mid-emit), drop from
/// the open fence to end. Ordinary code fences (```rust, ```python, bare ```) are
/// untouched — only the tool-envelope info-strings match.
fn strip_fenced_tool_blocks(s: &str) -> String {
    fn is_tool_fence(line: &str) -> bool {
        let t = line.trim_start();
        let Some(info) = t.strip_prefix("```") else {
            return false;
        };
        let info = info.trim_start().to_lowercase();
        info.starts_with("tool_call") || info.starts_with("tool_code")
    }
    let mut out: Vec<&str> = Vec::new();
    let mut lines = s.lines().peekable();
    while let Some(line) = lines.next() {
        if is_tool_fence(line) {
            // Consume through the closing fence (a line that is just ```), or to
            // end if the block was never closed.
            for inner in lines.by_ref() {
                if inner.trim() == "```" {
                    break;
                }
            }
            continue;
        }
        out.push(line);
    }
    out.join("\n")
}

/// Strip BARE tool-invocation lines — the envelope-less reflex (observed
/// gen75-2026-07-02 on an unindexed folder corpus): the model narrates "Let me
/// search for information about this corpus:" and then emits
/// `knowledge_lookup(query="…")` as a plain prose line. No envelope, no colon
/// prefix, so neither `strip_tool_code_blocks` nor the marker list catches it,
/// and the raw call syntax ships to the user.
///
/// Two STRUCTURAL signals, no tool-name list (the reflex renames itself
/// per draw — `knowledge_lookup(query=…)` one run, `search(folder-corpus-…)`
/// the next — so name matching only ever fits the last observation):
///
/// 1. ANYWHERE: a whole unfenced line of `identifier(kwarg=…)` with a
///    search-ish keyword argument — that argument SHAPE is invocation syntax,
///    whatever the callee is called.
/// 2. TERMINAL: the answer's last unfenced content line is `identifier(…)`
///    (any name, any args) AND the line announcing it is FIRST-PERSON intent
///    ending in `:` ("Let me search …:", "I'll look up …:"). A model that
///    narrates its own action and then emits a call has handed off to a tool
///    that doesn't exist and stopped — nothing after the call can be the
///    answer. An instructional ending addressed to the USER ("Call it like
///    this:\n\nfoo(42)") is imperative, not first-person, and survives.
///
/// Fenced code blocks are skipped entirely; a dangling first-person intro is
/// dropped alongside what it announced.
fn strip_bare_tool_call_lines(s: &str) -> String {
    fn is_call_line(line: &str) -> bool {
        let t = line.trim();
        let Some(open) = t.find('(') else {
            return false;
        };
        if !t.ends_with(')') || open == 0 {
            return false;
        }
        let name = &t[..open];
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
            && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
    }
    fn has_search_kwarg(line: &str) -> bool {
        let t = line.trim();
        let Some(open) = t.find('(') else {
            return false;
        };
        let args = t[open + 1..].trim_start();
        const CALL_KWARGS: &[&str] = &["query=", "query =", "search=", "q=", "input=", "prompt="];
        CALL_KWARGS.iter().any(|k| args.starts_with(k))
    }
    /// First-person self-narrated intent ("Let me …:", "I'll …:") — the model
    /// announcing ITS OWN next action, as opposed to instructing the user.
    fn is_self_narrated_intent(line: &str) -> bool {
        let t = line.trim();
        if !t.ends_with(':') {
            return false;
        }
        let low = t.to_lowercase();
        [
            "let me ",
            "i'll ",
            "i will ",
            "i need to ",
            "i'm going to ",
            "i am going to ",
        ]
        .iter()
        .any(|p| low.starts_with(p))
    }

    // Pass 1: kwarg-shaped invocation lines, anywhere outside fences.
    let mut out: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let mut stripped_any = false;
    for line in s.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push(line);
            continue;
        }
        if !in_fence && is_call_line(line) && has_search_kwarg(line) {
            stripped_any = true;
            continue;
        }
        out.push(line);
    }
    // Pass 2: a TERMINAL self-narrated call, any name/args. `in_fence` now
    // tells us whether the tail of the answer sits inside an unclosed fence —
    // if so, leave it alone.
    if !in_fence {
        let last_content = out.iter().rposition(|l| !l.trim().is_empty());
        if let Some(li) = last_content {
            if is_call_line(out[li]) {
                let intro = out[..li].iter().rposition(|l| !l.trim().is_empty());
                if intro.is_some_and(|pi| is_self_narrated_intent(out[pi])) {
                    out.truncate(li);
                    stripped_any = true;
                }
            }
        }
    }
    if !stripped_any {
        return s.to_string();
    }
    // Drop a dangling trailing first-person intro ("Let me search …:") and
    // trailing blanks left behind by either pass.
    while let Some(last) = out.last() {
        let t = last.trim();
        if t.is_empty() || is_self_narrated_intent(last) {
            out.pop();
        } else {
            break;
        }
    }
    out.join("\n")
}

/// True when `text` is essentially a phantom tool call the chat CANNOT run — a
/// bare colon-call (`:code_search("X")`, `:document_operation(...)` — the format
/// Qwen3.6 reflexes for code/lookup questions even though chat wires no tools),
/// or a leftover envelope tag, with little real prose around it. The caller
/// replaces such an answer with an honest fallback rather than leak the raw call
/// to the user. Length-guarded so a genuine answer that merely MENTIONS a tool
/// name isn't caught. (The structured-tools path — Recipe Author — is unaffected:
/// real calls travel as `tool_choice`, never as this colon/tag prose.)
pub fn looks_like_phantom_tool_call(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    const MARKERS: &[&str] = &[
        ":code_search(",
        ":document_operation(",
        ":symbols(",
        ":callers(",
        ":callees(",
        ":blast(",
        ":recent_changes(",
        "<tool_call",
        "<tool_code",
    ];
    if t.len() < 400 && MARKERS.iter().any(|m| t.contains(m)) {
        return true;
    }
    // Whole-answer tool-PLANNING that leaked instead of an answer — observed
    // 2026-07-08 (8h chaos run, unindexed folder corpus / needs-rebuild KB): the
    // model narrated its intended retrieval ("I need to search … Let me use
    // `knowledge_lookup` …", "search corpus:<id>") rather than answering. THREE
    // signals must all hold, so a real answer is never caught:
    //   (a) FIRST-PERSON future intent — the model announcing its OWN next action
    //       ("I need to", "Let me", "I'll"), not describing HOW to a user;
    //   (b) it names a specific internal RETRIEVAL tool the chat cannot run;
    //   (c) it carries NO grounding citation (`[Source:]` / "Grounded in the
    //       source") — a real answer that merely mentions such an identifier in
    //       an instructional `code` block or cites a source is exempt.
    // Length-guarded on top of that.
    let low = t.to_lowercase();
    const PHANTOM_RETRIEVAL: &[&str] = &[
        "knowledge_lookup",
        "claim_search",
        "search corpus:",
        "lookup corpus:",
    ];
    const FIRST_PERSON_INTENT: &[&str] = &[
        "i need to ",
        "i'll ",
        "i will ",
        "let me ",
        "i'm going to ",
        "i am going to ",
    ];
    t.len() < 700
        && !low.contains("grounded in the source")
        && !low.contains("[source:")
        && PHANTOM_RETRIEVAL.iter().any(|m| low.contains(m))
        && FIRST_PERSON_INTENT.iter().any(|m| low.contains(m))
}

/// Present a model answer for the user: strip phantom tool-call envelopes the
/// chat reflexes (no executable tool is wired in chat), and if the WHOLE answer
/// was such a call, return an honest fallback rather than leak the raw call.
/// Shared by the desktop stream-complete and the runtime gate output so every
/// surface presents identically.
pub fn present_answer(raw: &str) -> String {
    let presented = strip_presenter_artifacts(raw);
    if looks_like_phantom_tool_call(&presented) {
        "I couldn't find that in the indexed material here.".to_string()
    } else {
        presented
    }
}

/// Idempotent: running over already-clean text returns the same
/// text. Benign on edge cases (empty / whitespace / just a label
/// → returns empty string after trim).
pub fn strip_presenter_artifacts(raw: &str) -> String {
    // Stage 1: think tags (delegated to existing helper).
    let mut text = strip_thinking_response(raw);

    // Stage 1b: phantom tool-call envelopes. Some distilled chat models
    // emit a `<tool_code>…</tool_code>` block ("Let me search for more
    // material…") even when no executable tool is wired on this host —
    // the server never parses or runs it, so it must not reach the user
    // as a raw tag, and an emitted-then-abandoned `<tool_code>` (no
    // closing tag) must not dump the open tag + trailing noise either.
    // Remove the whole span; if unterminated, drop from the open tag to
    // end. This is presentation-only — it does not suppress a real tool
    // call, which travels as structured `tool_choice`/`tools`, not prose.
    text = strip_tool_code_blocks(&text);
    text = strip_bare_tool_call_lines(&text);

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
    //
    // iter9: extended with analytical preambles ("Let me analyze",
    // "Looking at the", "Key considerations", "Key observations",
    // "First, let me", "To respond properly"). iter8 surfaced
    // these as the model's "thinking out loud" pattern when
    // `enable_thinking=false` is set — the model emits a Markdown-
    // style analysis without `<think>` tags. Treating them as
    // preambles strips the analysis and surfaces the actual reply
    // (when one follows) or returns empty (when the model never
    // got past the analysis, which is itself a useful failure
    // signal).
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
        "Let me analyze",
        "Let me work through",
        "Let me think",
        "Looking at the",
        "Looking at this",
        "First, let me",
        "To respond properly",
        "Key considerations",
        "Key observations",
        "Key insight",
    ];
    let next = {
        let trimmed = text.trim_start();
        if PREAMBLE_PREFIXES.iter().any(|p| trimmed.starts_with(p)) {
            if let Some(idx) = trimmed.find("\n\n") {
                Some(trimmed[idx + 2..].to_string())
            } else {
                trimmed.find('\n').map(|idx| trimmed[idx + 1..].to_string())
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
    // SLOT_POLICY §3 Synthesize: the Presenter is the user-visible
    // streaming surface. Bundle latency=Normal → shadow Speed::Slow
    // (Primary), unchanged from the prior explicit Slow.
    let mut req = CompletionRequest::for_workload(Workload::Synthesize, prompt);
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
        assert!(
            sys.len() < 400,
            "system prompt should be short ({} chars)",
            sys.len()
        );
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
            "Jordan",
            "Aleksei",
            "Devi",
            "Mark",
            "Sam",
            "therapist",
            "anxiety",
            "depression",
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
        let req = present_request("USER_MSG", "DRAFT_BODY", SkillRegister::Relational, 1024);
        assert!(req.prompt.contains("USER_MSG"));
        assert!(req.prompt.contains("DRAFT_BODY"));
    }

    #[test]
    fn strip_presenter_artifacts_removes_think_block() {
        let raw = "<think>planning out my reply</think>The actual reply.";
        assert_eq!(strip_presenter_artifacts(raw), "The actual reply.");
    }

    #[test]
    fn strip_presenter_artifacts_removes_closed_tool_code_block() {
        let raw = "Let me search for more material.\n<tool_code>\nsearch(\"lebanon\")\n</tool_code>\nHere it is.";
        let out = strip_presenter_artifacts(raw);
        assert!(!out.contains("tool_code"), "tool_code leaked: {out:?}");
        assert!(out.contains("Let me search for more material."));
        assert!(out.contains("Here it is."));
    }

    #[test]
    fn strip_presenter_artifacts_removes_unterminated_tool_code() {
        // The witnessed Lebanon failure: model opens the tag and stops.
        let raw = "I need to search for comprehensive information.\n\n<tool_code>\n";
        let out = strip_presenter_artifacts(raw);
        assert!(!out.contains("tool_code"), "dangling tag leaked: {out:?}");
        assert!(out.contains("I need to search for comprehensive information."));
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
        assert_eq!(strip_presenter_artifacts(raw), "You mentioned Jordan once.");
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
    fn strip_presenter_artifacts_removes_analytical_preambles_iter9() {
        // iter9 — model emits "Let me analyze this carefully:\n\n1. The user is..."
        // as visible meta-narration when enable_thinking=false. Strip
        // up to the first reply-shaped sentence after the analysis.
        let raw = "Let me analyze this carefully:\n\n\
                   The only mention of Jordan I have is from April 12.";
        assert_eq!(
            strip_presenter_artifacts(raw),
            "The only mention of Jordan I have is from April 12."
        );

        let raw2 = "Looking at the user's question and the drafter notes carefully:\n\n\
                    I don't have any record of that.";
        assert_eq!(
            strip_presenter_artifacts(raw2),
            "I don't have any record of that."
        );

        let raw3 = "Key considerations for this reply:\n\n\
                    From what I can see, you only mentioned this once.";
        assert_eq!(
            strip_presenter_artifacts(raw3),
            "From what I can see, you only mentioned this once."
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

    const TOOL_FALLBACK: &str = "I couldn't find that in the indexed material here.";

    #[test]
    fn present_answer_falls_back_on_bare_colon_tool_call() {
        // Qwen reflexes a bare `:code_search(...)` for code questions even
        // though chat wires no executable tool — show an honest fallback, not
        // the raw call.
        assert_eq!(
            present_answer("```\n:code_search(\"OICP_VERSION\")\n```"),
            TOOL_FALLBACK
        );
        assert_eq!(
            present_answer(":document_operation(document_id=\"x\")"),
            TOOL_FALLBACK
        );
    }

    #[test]
    fn present_answer_falls_back_when_tool_envelope_strips_to_empty() {
        // Both envelope tags strip to empty → fallback (not a blank message).
        assert_eq!(present_answer("<tool_code></tool_code>"), TOOL_FALLBACK);
        assert_eq!(
            present_answer("<tool_call>\n  symbols(\"X\")\n</tool_call>"),
            TOOL_FALLBACK
        );
    }

    #[test]
    fn present_answer_keeps_a_real_grounded_answer() {
        // A genuine answer that NAMES a symbol survives untouched — the trigger
        // is the colon-CALL form `:code_search(`, never a prose mention.
        let real = "The score field is `f32` [Source: \"pub score: f32,\"].";
        assert_eq!(present_answer(real), real);
        let mentions = "Use the code_search tool in the CLI to find it.";
        assert_eq!(present_answer(mentions), mentions);
    }

    #[test]
    fn fenced_tool_call_block_is_stripped() {
        // 2026-07-08 (8h chaos run): the model emitted a markdown-fenced
        // ```tool_call:knowledge_lookup``` block — the fence disguised the same
        // reflex as the <tool_call> envelope, so it slipped past both strippers.
        assert_eq!(
            strip_fenced_tool_blocks("Here:\n```tool_call:knowledge_lookup\nquery: x\n```\ndone"),
            "Here:\ndone"
        );
        // Unterminated fence → drop to end.
        assert_eq!(
            strip_fenced_tool_blocks("ok\n```tool_call\nknowledge_lookup(query=\"x\")"),
            "ok"
        );
        // Ordinary code fences are untouched.
        let rust = "See:\n```rust\nlet x = 1;\n```\nend";
        assert_eq!(strip_fenced_tool_blocks(rust), rust);
    }

    #[test]
    fn present_answer_falls_back_on_leaked_retrieval_plan() {
        // The whole answer is the model narrating its intended retrieval instead
        // of answering — no grounding, names a phantom tool the chat can't run.
        // Observed 2026-07-08 on an unindexed folder corpus (step 275/332/746).
        assert_eq!(
            present_answer(
                "search corpus:folder-corpus-2918e9ebc0b5 I don't have direct access to \
                 that folder. Let me search my knowledge corpus for this identifier."
            ),
            TOOL_FALLBACK
        );
        assert_eq!(
            present_answer(
                "To identify the most important material, I need to inspect its contents. \
                 I will use `knowledge_lookup` to find references to this corpus folder."
            ),
            TOOL_FALLBACK
        );
        assert_eq!(
            present_answer(
                "I need to search the local knowledge corpus. Let me use the claim_search \
                 tool first, and if that doesn't work I'll try a more general approach."
            ),
            TOOL_FALLBACK
        );
    }

    #[test]
    fn grounded_answer_naming_a_tool_survives_the_plan_guard() {
        // A REAL answer that cites a source is never a phantom, even if it mentions
        // a retrieval identifier — the no-citation guard protects it.
        let grounded = "The lookup path is `knowledge_lookup` [Source: retrieval.rs].";
        assert_eq!(present_answer(grounded), grounded);
        let grounded2 = "It searches the corpus first. Grounded in the source: \
                         \"knowledge_lookup runs before synthesis\".";
        assert_eq!(present_answer(grounded2), grounded2);
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

    // ── bare tool-call lines (gen75-2026-07-02, unindexed folder corpus) ──

    #[test]
    fn bare_tool_call_line_and_its_intro_are_stripped() {
        let raw = "I don't have direct access to the contents of \"folder-corpus-2918e9ebc0b5\" \
                   in my current context.\n\nLet me search for information about this corpus:\n\n\
                   knowledge_lookup(query=\"folder-corpus-2913e9ebc0bb\")";
        let out = present_answer(raw);
        assert!(!out.contains("knowledge_lookup"));
        assert!(!out.contains("Let me search"));
        assert!(out.starts_with("I don't have direct access"));
    }

    #[test]
    fn fenced_code_and_prose_calls_survive() {
        // A legitimate code answer: fenced call lines and in-prose mentions of
        // call syntax must not be touched.
        let raw = "The helper is invoked as `lookup(query=\"x\")` inside main:\n\
                   ```rust\nknowledge_lookup(query=\"x\")\n```\nDone.";
        assert_eq!(present_answer(raw), raw);
    }

    #[test]
    fn plain_function_call_without_search_kwarg_is_kept() {
        // Bare code lines that aren't search-tool-shaped stay (e.g. a snippet
        // answer showing a call with positional args).
        let raw = "Call it like this:\n\nfoo(42, start)";
        assert_eq!(present_answer(raw), raw);
    }

    #[test]
    fn positional_arg_search_verb_reflex_is_stripped() {
        // The reflex's second observed shape (alignfix replay): a SEARCH-verb
        // call with positional args instead of a kwarg.
        let raw = "I don't have visibility into that corpus.\n\n\
                   Let me try searching for references to this corpus:\n\n\
                   search(folder-corpus-2918)";
        let out = present_answer(raw);
        assert!(!out.contains("search(folder-corpus"));
        assert!(!out.contains("Let me try searching"));
        assert!(out.starts_with("I don't have visibility"));
    }
}
