// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gap identification — the bridge between "best-effort answer from
//! local corpus" and "structured information request to the user".
//!
//! After the agent has done its best with available evidence, this module
//! asks a Fast-slot model: *is there a single nameable external thing
//! that would materially improve the answer?* If yes, it returns a
//! populated [`InformationRequest`] that the planner can wrap in a
//! `StepKind::AwaitUserInfo` step. If no, it returns `None` and the
//! synthesis proceeds normally.
//!
//! The function is **conservative on parse failure** — when the model's
//! output doesn't deserialize, we treat it as "no gap" rather than as
//! "ambiguous, surface anyway." False positives here interrupt the user
//! with low-value requests, which is worse than silently proceeding
//! with the corpus-only answer.

use crate::error::Result;
use crate::slot_policy::Workload;
use crate::title::strip_think_blocks;
use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, InformationRequest, InformationRequestKind};

/// Hard caps on the inputs we feed the gap-assessment prompt. The
/// gap-checker doesn't need the full answer or evidence — it just
/// needs enough to spot a missing-evidence shape. Halved from the
/// original 3000+3000 because the prompt-fill on a Fast-slot 9B
/// running grammar-constrained decoding turned the post-answer
/// audit into a 55s wait that the user noticed.
///
/// The budget is spent as a HEAD+TAIL window, not a head-only cut
/// (2026-07-15, the Einstein four-papers false positive): a head-only
/// cut on an enumerated answer eats the LAST item — the judge saw a
/// four-papers question, three papers of answer, and dutifully filed
/// an information request for the fourth paper the user could already
/// read on screen (answer 2,884 bytes; "Mass–energy equivalence"
/// started at byte 1,942, past the old 1,500 cut). Gaps live at tails
/// — missing items, trailing hedges — so the tail is the
/// highest-signal region a head-only cut throws away. Same total
/// budget, zero added decode time.
const ANSWER_HEAD_CHARS: usize = 900;
const ANSWER_TAIL_CHARS: usize = 600;
const EVIDENCE_HEAD_CHARS: usize = 900;
const EVIDENCE_TAIL_CHARS: usize = 600;
const MAX_QUESTION_CHARS: usize = 600;

/// Seam marker for the head+tail window. Tells the judge the middle
/// was elided FOR LENGTH — without it, the cut itself reads as the
/// answer trailing off, which is exactly the shape that invites a
/// false "incomplete answer" gap.
const ELISION_MARKER: &str = "\n[… middle elided for length — the text continues without a gap …]\n";

/// Token budget for the gap response. With the schema relaxed to
/// require only `has_gap` + `gap`, a useful response fits in ~120
/// tokens — current_understanding / relevance / satisfying_source /
/// search_hints are all optional now (UI gracefully omits empties).
/// Was 320 when the schema forced all six fields; that drove
/// generation past 30s on grammar-constrained decoding.
const GAP_MAX_TOKENS: usize = 192;

/// Skip the gap check entirely when the answer is already long
/// AND the corpus delivered substantial evidence. Heuristic guard
/// — the gap check is most valuable when retrieval was thin or
/// the answer is short enough to leave room for elaboration. On a
/// 4000+ char answer grounded in 5000+ chars of evidence, the
/// expected information-gain is low and the 15-20s wait isn't
/// earning its keep.
const ANSWER_SATURATION_CHARS: usize = 4_000;
const EVIDENCE_SATURATION_CHARS: usize = 5_000;

/// Identify whether a meaningful external information gap remains after
/// the agent's first-pass answer.
///
/// Returns:
/// - `Ok(None)` when the model reports no gap, the response can't be parsed,
///   or no fields are populated. Conservative — we'd rather silently proceed
///   than surface a bad request.
/// - `Ok(Some(req))` when a gap was identified. `task_id` and `step_id` are
///   left empty here; the executor stamps them before emitting.
pub async fn identify_gap(
    inference: &dyn InferenceProvider,
    question: &str,
    answer_so_far: &str,
    retrieved_evidence: &str,
) -> Result<Option<InformationRequest>> {
    // Cheap pre-check: if the answer is already saturated by the
    // corpus, don't pay 15-20s on a Fast-slot grammar-constrained
    // call to almost certainly return "no gap." The saturation
    // thresholds are deliberately conservative so we only skip
    // when there's high confidence the call would be a no-op.
    if answer_so_far.len() >= ANSWER_SATURATION_CHARS
        && retrieved_evidence.len() >= EVIDENCE_SATURATION_CHARS
    {
        tracing::info!(
            answer_chars = answer_so_far.len(),
            evidence_chars = retrieved_evidence.len(),
            "gap_check: skipped — answer is saturated by corpus evidence"
        );
        return Ok(None);
    }

    let q = truncate_to_char_boundary(question, MAX_QUESTION_CHARS);
    let a = window_head_tail(answer_so_far, ANSWER_HEAD_CHARS, ANSWER_TAIL_CHARS);
    let e = window_head_tail(retrieved_evidence, EVIDENCE_HEAD_CHARS, EVIDENCE_TAIL_CHARS);

    // Terse prompt: the model audits the answer for a missing
    // piece of external evidence, returns a short JSON object.
    // Optional fields are *allowed* but not requested — the model
    // gravitates toward terse output when the prompt doesn't ask
    // for elaboration, which cuts generation tokens (and grammar-
    // constrained decoding wall time) substantially.
    let prompt = format!(
        "Audit this answer for the single most valuable missing external evidence.\n\n\
         Question: {q}\n\n\
         Answer:\n{a}\n\n\
         Local corpus evidence:\n{e}\n\n\
         If the evidence is already strong enough, respond exactly: {{\"has_gap\": false}}\n\n\
         Otherwise respond with: {{\"has_gap\": true, \"gap\": \"<a precise question to verify, specific enough to act on>\"}}\n\
         You MAY add \"relevance\", \"satisfying_source\", or \"search_hints\" if useful — keep each terse.\n\n\
         Output the JSON object only — no preface."
    );

    // Schema requires only has_gap; the rest are optional. The
    // UI's `{#if non-empty}` guards on `current_understanding`,
    // `satisfying_source`, and `search_hints` mean omitting them
    // produces a clean card with just the gap question + (if
    // present) relevance — which is the load-bearing UX. Optional
    // structure also gives llguidance fewer required-token paths
    // to enforce, which speeds generation.
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "has_gap":              { "type": "boolean" },
            "current_understanding":{ "type": "string" },
            "gap":                  { "type": "string" },
            "relevance":            { "type": "string" },
            "satisfying_source":    { "type": "string" },
            "search_hints":         { "type": "array", "items": { "type": "string" } }
        },
        "required": ["has_gap"]
    });

    // SLOT_POLICY §3 Housekeep: gap-audit of a research answer.
    let mut request = CompletionRequest::for_workload(Workload::Housekeep, prompt)
        .with_system(
            "You audit research answers for missing evidence. Output only \
             the requested JSON object — no thinking, no preface.",
        )
        .with_output_budget(GAP_MAX_TOKENS as u32);
    request.temperature = Some(0.0);
    request.structured_output = Some(schema);
    let response = inference.complete(&request).await?;

    Ok(parse_gap_response(&response.text))
}

/// Parse the model's response into an Option<InformationRequest>.
/// Conservative — returns None on any failure.
fn parse_gap_response(raw: &str) -> Option<InformationRequest> {
    let cleaned = strip_think_blocks(raw);
    let trimmed = cleaned.trim();

    // Allow markdown-fenced JSON (```json ... ```), bare JSON, or JSON
    // preceded by some text the model insisted on adding.
    let candidate = extract_json_object(trimmed)?;

    let val: serde_json::Value = serde_json::from_str(candidate).ok()?;

    let has_gap = val
        .get("has_gap")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !has_gap {
        return None;
    }

    let gap = val
        .get("gap")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if gap.is_empty() {
        // Model said "yes there's a gap" but didn't specify it. Treat as
        // no gap rather than surfacing an empty card.
        return None;
    }

    let current_understanding = val
        .get("current_understanding")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let relevance = val
        .get("relevance")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let satisfying_source = val
        .get("satisfying_source")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let search_hints = val
        .get("search_hints")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|h| h.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(InformationRequest {
        current_understanding,
        gap,
        relevance,
        satisfying_source,
        search_hints,
        task_id: String::new(),
        step_id: 0,
        // Gap-checker only produces post-answer refinement cards;
        // planned-step cards come from `StepKind::AwaitUserInfo` and
        // are stamped by the executor instead.
        kind: InformationRequestKind::Refinement,
        task_title: String::new(),
    })
}

/// Find the JSON object inside `s` — handles bare `{...}`, `\`\`\`json ... \`\`\``
/// fences, and prefixes the model added against instruction.
fn extract_json_object(s: &str) -> Option<&str> {
    // Strip a markdown code fence if present.
    let s = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```"))
        .map(|t| t.trim_start_matches('\n'))
        .map(|t| t.trim_end_matches("```").trim_end_matches('\n'))
        .unwrap_or(s);

    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&s[start..=end])
}

/// Head+tail window over `s`: the first `head_max` and last `tail_max`
/// bytes (char-boundary-safe) joined by [`ELISION_MARKER`]. Returns the
/// input untouched when it already fits the combined budget — the
/// marker only ever appears when something was actually elided.
fn window_head_tail(s: &str, head_max: usize, tail_max: usize) -> std::borrow::Cow<'_, str> {
    if s.len() <= head_max + tail_max + ELISION_MARKER.len() {
        return std::borrow::Cow::Borrowed(s);
    }
    let head = truncate_to_char_boundary(s, head_max);
    // Tail: nearest char boundary at or AFTER len - tail_max, so the
    // tail never exceeds its budget and never splits a code point.
    let mut start = s.len() - tail_max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    std::borrow::Cow::Owned(format!("{head}{ELISION_MARKER}{}", &s[start..]))
}

/// Walk `s` back to the nearest valid UTF-8 char boundary at or before
/// `max` bytes. Avoids panicking on multi-byte characters when truncating.
fn truncate_to_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_no_gap_returns_none() {
        assert!(parse_gap_response(r#"{"has_gap": false}"#).is_none());
    }

    #[test]
    fn parse_no_gap_with_extras_returns_none() {
        // Model emitted has_gap=false but also filled some fields by accident.
        // Still treat as no-gap.
        let raw = r#"{"has_gap": false, "gap": "stale stuff"}"#;
        assert!(parse_gap_response(raw).is_none());
    }

    #[test]
    fn parse_full_gap_populates_all_fields() {
        let raw = r#"{
            "has_gap": true,
            "current_understanding": "The agent has mapped the theoretical debate.",
            "gap": "Empirical magnitude of the R&D investment effect post-IRA.",
            "relevance": "Determines whether the innovation argument is large or small.",
            "satisfying_source": "A 2023-2024 NEJM or Health Affairs study.",
            "search_hints": ["NEJM 2024 IRA pharmaceutical", "CBO pipeline analysis 2024"]
        }"#;
        let req = parse_gap_response(raw).expect("expected Some");
        assert!(req.gap.starts_with("Empirical magnitude"));
        assert_eq!(req.search_hints.len(), 2);
        assert!(req.satisfying_source.contains("NEJM"));
        // Gap-checker is the post-answer refinement producer; UI
        // contract requires the kind to be stamped accordingly so the
        // card renders the "sharpen answer" chrome.
        assert_eq!(req.kind, InformationRequestKind::Refinement);
        assert!(
            req.task_title.is_empty(),
            "Refinement cards have no task — task_title must be empty"
        );
    }

    #[test]
    fn parse_strips_think_blocks_before_json() {
        let raw = "<think>let me consider</think>\n```json\n{\"has_gap\": false}\n```";
        assert!(parse_gap_response(raw).is_none());
    }

    #[test]
    fn parse_handles_markdown_fenced_full_gap() {
        let raw = "```json\n{\"has_gap\": true, \"gap\": \"What is X?\"}\n```";
        let req = parse_gap_response(raw).expect("Some");
        assert_eq!(req.gap, "What is X?");
    }

    #[test]
    fn parse_garbage_returns_none() {
        assert!(parse_gap_response("not json at all").is_none());
        assert!(parse_gap_response("").is_none());
        assert!(parse_gap_response("{").is_none());
    }

    #[test]
    fn parse_has_gap_true_but_empty_gap_returns_none() {
        // Model said yes but didn't fill the gap field — surfacing an
        // empty card to the user is worse than silently proceeding.
        assert!(parse_gap_response(r#"{"has_gap": true, "gap": ""}"#).is_none());
        assert!(parse_gap_response(r#"{"has_gap": true}"#).is_none());
    }

    #[test]
    fn truncate_walks_back_to_char_boundary() {
        let s = "Schrödinger";
        let t = truncate_to_char_boundary(s, 7);
        assert!(s.starts_with(t));
        // Valid UTF-8: doesn't panic on .chars().
        let _ = t.chars().count();
    }

    #[test]
    fn window_passes_short_input_through_unmarked() {
        let s = "A short answer that fits the whole budget.";
        let w = window_head_tail(s, 900, 600);
        assert_eq!(w.as_ref(), s);
        assert!(!w.contains("elided"), "no elision marker on a passthrough");
    }

    /// The Einstein four-papers regression (2026-07-15): an enumerated
    /// answer longer than the head budget must keep its TAIL — the old
    /// head-only cut fed the judge papers 1–3 of a four-paper answer
    /// and produced an information request for the fourth paper the
    /// user could already read on screen.
    #[test]
    fn window_keeps_the_enumerations_last_item() {
        let filler = "The paper reshaped the field in ways contemporaries took years to absorb. "
            .repeat(9);
        let answer = format!(
            "Einstein's 1905 papers were four groundbreaking works.\n\n\
             **1. Photoelectric Effect**\n{filler}\n\
             **2. Brownian Motion**\n{filler}\n\
             **3. Special Relativity**\n{filler}\n\
             **4. Mass–Energy Equivalence (E = mc²)**\nThe fourth paper established that \
             mass and energy are interchangeable."
        );
        // The regression shape: past the head budget, under saturation.
        assert!(answer.len() > ANSWER_HEAD_CHARS + ANSWER_TAIL_CHARS);
        assert!(answer.len() < ANSWER_SATURATION_CHARS);
        let w = window_head_tail(&answer, ANSWER_HEAD_CHARS, ANSWER_TAIL_CHARS);
        assert!(
            w.contains("Mass–Energy Equivalence"),
            "the fourth paper must survive the window: {w}"
        );
        assert!(
            w.contains("Photoelectric Effect"),
            "the head must survive too"
        );
        assert!(
            w.contains("elided for length"),
            "the seam must be labeled so the cut can't read as the answer trailing off"
        );
    }

    #[test]
    fn window_is_char_boundary_safe_on_multibyte_seams() {
        // Multibyte chars positioned to straddle both the head cut and
        // the tail start. Must not panic and must stay valid UTF-8.
        let s = "é".repeat(2_000);
        let w = window_head_tail(&s, 899, 601); // odd budgets land mid-char
        let _ = w.chars().count();
        assert!(w.contains("elided for length"));
    }
}
