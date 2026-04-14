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
use crate::title::strip_think_blocks;
use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, InformationRequest, Speed};

/// Hard caps on the inputs we feed the gap-assessment prompt. Keeps the
/// Fast-slot call cheap regardless of how long the answer or evidence are.
const MAX_ANSWER_CHARS: usize = 3000;
const MAX_EVIDENCE_CHARS: usize = 3000;
const MAX_QUESTION_CHARS: usize = 1000;

/// Token budget for the gap response. JSON with five short string fields
/// + a string array fits comfortably in 300 tokens.
const GAP_MAX_TOKENS: usize = 320;

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
    let q = truncate_to_char_boundary(question, MAX_QUESTION_CHARS);
    let a = truncate_to_char_boundary(answer_so_far, MAX_ANSWER_CHARS);
    let e = truncate_to_char_boundary(retrieved_evidence, MAX_EVIDENCE_CHARS);

    let prompt = format!(
        "You are auditing a research answer for the single most valuable \
         missing piece of external evidence.\n\n\
         The user asked:\n{q}\n\n\
         The agent's current answer (built from the local corpus):\n{a}\n\n\
         Evidence retrieved from the local corpus:\n{e}\n\n\
         Identify ONE specific external piece of information that, if obtained, \
         would materially sharpen this answer. Examples of \"sharpen\": \
         resolves a contested empirical claim, supplies a recent statistic, \
         names a primary source the agent should consult.\n\n\
         If the corpus evidence is already strong enough that no single \
         external item would meaningfully improve the answer, respond with \
         {{\"has_gap\": false}}.\n\n\
         Otherwise respond with this exact JSON shape:\n\
         {{\n\
           \"has_gap\": true,\n\
           \"current_understanding\": \"one short paragraph of what the agent now believes\",\n\
           \"gap\": \"a precise question or claim to verify, specific enough to act on\",\n\
           \"relevance\": \"how resolving this would change or sharpen the answer\",\n\
           \"satisfying_source\": \"what kind of source would satisfy the request (a paper, a stat, a primary doc)\",\n\
           \"search_hints\": [\"specific journal\", \"specific term\", \"specific database\"]\n\
         }}\n\n\
         Respond with the JSON object only — no preface, no thinking out loud."
    );

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

    let response = inference
        .complete(&CompletionRequest {
            prompt,
            system_message: Some(
                "You audit research answers for missing evidence. Output only \
                 the requested JSON object — no thinking, no preface."
                    .to_string(),
            ),
            preferred_speed: Speed::Fast,
            max_tokens: Some(GAP_MAX_TOKENS),
            temperature: Some(0.0),
            think_budget: Some(0),
            structured_output: Some(schema),
            top_k: None,
            top_p: None,
            oicp: None,
        })
        .await?;

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

    let has_gap = val.get("has_gap").and_then(|v| v.as_bool()).unwrap_or(false);
    if !has_gap {
        return None;
    }

    let gap = val.get("gap").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
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
}
