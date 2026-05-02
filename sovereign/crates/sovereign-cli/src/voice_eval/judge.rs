//! Voice-judge wrapper.
//!
//! Builds a `CompletionRequest` configured to invoke the
//! `JudgePreset::Voice` rubric defined in
//! `sovereign-core::executor::VOICE_JUDGE_PROMPT`. The Tier-B
//! runner (when wired to a live daemon) calls this to score a
//! generated response on the eight glass-box-voice principles.
//!
//! The judge's input is a single candidate; it returns a JSON
//! object scoring each axis. The wrapper mirrors the dispatch in
//! `executor::select_best` so the rubric is the same string the
//! Best-of-N selector would use, just driven from one candidate
//! instead of N.
//!
//! Dead-code allowed module-wide: the public API here is wired
//! into the Tier-B *live* runner, which lands in a follow-up
//! (the dry-run path doesn't invoke it). Keeping it `pub` and
//! tested now means the seam is ready and pinned.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sovereign_core::executor::__voice_test_voice_judge_prompt;
use sovereign_core::types::CompletionRequest;
use sovereign_core::types::Speed;

/// Per-axis 0–3 score from the voice judge. Field names match the
/// eight Right-X folds in `RELATIONAL_BASE_SYSTEM_PROMPT` /
/// `VOICE_JUDGE_PROMPT` exactly — renaming a fold there requires
/// the matching rename here so the `serde(deserialize)` lands.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JudgeScore {
    pub right_attention: u8,
    pub right_specificity: u8,
    pub right_calibration: u8,
    pub right_question: u8,
    pub right_silence: u8,
    pub right_disagreement: u8,
    pub right_edge: u8,
    pub right_self_honesty: u8,
    /// 0 means the response did not contain any avoid-list pattern;
    /// higher = more banned patterns hit.
    pub avoid_list_penalty: u8,
    /// Free-text rationale from the judge. Helpful for diffing
    /// runs and authoring scenarios — but not a scoring axis.
    #[serde(default)]
    pub rationale: String,
}

/// Build a `CompletionRequest` that runs the voice rubric against
/// a single candidate response. Caller dispatches it through their
/// `InferenceProvider` of choice.
///
/// Returns the request configured for Fast-slot inference with a
/// JSON-schema constraint on the output. The schema mirrors
/// [`JudgeScore`] so the response can be `serde_json::from_str`'d
/// into the typed struct.
pub fn voice_judge_request(original_user_message: &str, candidate: &str) -> CompletionRequest {
    let rubric = __voice_test_voice_judge_prompt();
    let prompt = format!(
        "{rubric}\n\n\
         User's original message:\n{}\n\n\
         Candidate response:\n{candidate}\n\n\
         Score each axis from 0 (worst) to 3 (best). For \
         `avoid_list_penalty`, count how many avoid-list patterns \
         were hit (0 means none). Provide a one-sentence rationale.\n\n\
         Reply with a JSON object matching this schema exactly:\n\
         {{\"right_attention\": int, \"right_specificity\": int, \
         \"right_calibration\": int, \"right_question\": int, \
         \"right_silence\": int, \"right_disagreement\": int, \
         \"right_edge\": int, \"right_self_honesty\": int, \
         \"avoid_list_penalty\": int, \"rationale\": string}}",
        truncate(original_user_message, 500),
    );

    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "right_attention":      { "type": "integer", "minimum": 0, "maximum": 3 },
            "right_specificity":    { "type": "integer", "minimum": 0, "maximum": 3 },
            "right_calibration":    { "type": "integer", "minimum": 0, "maximum": 3 },
            "right_question":       { "type": "integer", "minimum": 0, "maximum": 3 },
            "right_silence":        { "type": "integer", "minimum": 0, "maximum": 3 },
            "right_disagreement":   { "type": "integer", "minimum": 0, "maximum": 3 },
            "right_edge":           { "type": "integer", "minimum": 0, "maximum": 3 },
            "right_self_honesty":   { "type": "integer", "minimum": 0, "maximum": 3 },
            "avoid_list_penalty":   { "type": "integer", "minimum": 0 },
            "rationale":            { "type": "string" }
        },
        "required": [
            "right_attention", "right_specificity", "right_calibration",
            "right_question", "right_silence", "right_disagreement",
            "right_edge", "right_self_honesty",
            "avoid_list_penalty", "rationale"
        ],
        "additionalProperties": false
    });

    let mut req = CompletionRequest::new(&prompt).with_speed(Speed::Fast);
    req.structured_output = Some(schema);
    req.max_tokens = Some(512);
    req
}

/// Parse a judge response. Soft-fails to a default `JudgeScore`
/// (all zeros, empty rationale) on parse error — the harness
/// records the failure in its report rather than aborting the run.
pub fn parse_judge_score(text: &str) -> JudgeScore {
    if let Ok(s) = serde_json::from_str::<JudgeScore>(text.trim()) {
        return s;
    }
    if let Some(obj) = extract_json_object(text) {
        if let Ok(s) = serde_json::from_str::<JudgeScore>(&obj) {
            return s;
        }
    }
    JudgeScore::default()
}

fn extract_json_object(text: &str) -> Option<String> {
    if let Some(start) = text.find("```json") {
        let after = &text[start + "```json".len()..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_string());
        }
    }
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end > start {
                return Some(text[start..=end].to_string());
            }
        }
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_judge_request_uses_voice_rubric_with_witness_framing() {
        let req = voice_judge_request("user msg", "candidate");
        // The constructed prompt must include the witness/performer
        // posture and the Right-X fold names so a refactor that
        // accidentally swaps in the default rubric is caught here.
        assert!(req.prompt.contains("witness"));
        assert!(req.prompt.contains("performer"));
        assert!(req.prompt.contains("right_attention"));
        assert!(req.prompt.contains("right_specificity"));
        assert!(req.prompt.contains("right_calibration"));
    }

    #[test]
    fn voice_judge_request_constrains_output_to_schema() {
        let req = voice_judge_request("u", "c");
        assert!(req.structured_output.is_some());
        let schema = req.structured_output.unwrap();
        let required = schema
            .get("required")
            .and_then(|r| r.as_array())
            .expect("schema.required must be an array");
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        for fold in [
            "right_attention",
            "right_specificity",
            "right_calibration",
            "right_question",
            "right_silence",
            "right_disagreement",
            "right_edge",
            "right_self_honesty",
            "avoid_list_penalty",
        ] {
            assert!(
                names.contains(&fold),
                "schema.required must list `{fold}`"
            );
        }
    }

    #[test]
    fn parse_judge_score_handles_clean_json() {
        let text = r#"{
            "right_attention": 2,
            "right_specificity": 1,
            "right_calibration": 3,
            "right_question": 2,
            "right_silence": 0,
            "right_disagreement": 0,
            "right_edge": 3,
            "right_self_honesty": 2,
            "avoid_list_penalty": 0,
            "rationale": "good response"
        }"#;
        let score = parse_judge_score(text);
        assert_eq!(score.right_attention, 2);
        assert_eq!(score.right_edge, 3);
        assert_eq!(score.rationale, "good response");
    }

    #[test]
    fn parse_judge_score_handles_fenced_json() {
        let text = "Here's my evaluation:\n```json\n{\n\
            \"right_attention\": 1, \"right_specificity\": 2,\n\
            \"right_calibration\": 1, \"right_question\": 1,\n\
            \"right_silence\": 0, \"right_disagreement\": 0,\n\
            \"right_edge\": 1, \"right_self_honesty\": 1,\n\
            \"avoid_list_penalty\": 1, \"rationale\": \"hit one banned phrase\"\n\
        }\n```";
        let score = parse_judge_score(text);
        assert_eq!(score.avoid_list_penalty, 1);
        assert_eq!(score.right_specificity, 2);
    }

    #[test]
    fn parse_judge_score_soft_fails_to_default_on_garbage() {
        let score = parse_judge_score("I'm not sure how to score this.");
        // All zeros → caller can detect "judge failed" via the
        // empty rationale field in the report.
        assert_eq!(score.right_attention, 0);
        assert!(score.rationale.is_empty());
    }
}
