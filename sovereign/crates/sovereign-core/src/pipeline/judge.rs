//! Voice-judge wrapper — Phase 3.2 of the situated-team plan.
//!
//! Builds the `CompletionRequest` that runs the
//! `executor::VOICE_JUDGE_PROMPT` rubric against a single
//! candidate response, plus the [`JudgeScore`] type the result
//! deserialises into and a soft-failing parser.
//!
//! The runtime calls [`voice_judge_request`] inside a
//! `tokio::spawn` after the Presenter flushes to the user — the
//! score is fire-and-forget telemetry, recorded as a delayed
//! [`crate::types::NarrationPhase::PresentationComplete`] frame
//! and never gating the user-visible reply.
//!
//! This was originally implemented in
//! `sovereign-cli/src/voice_eval/judge.rs`; the runtime needs to
//! call it too, so the canonical home is here. The cli harness
//! re-exports from this module so there's one source of truth for
//! the request shape — renaming a fold or tightening a constraint
//! happens in one place.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::executor::__voice_test_voice_judge_prompt;
use crate::skills::SkillRegister;
use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Speed};

/// Per-axis 0–3 score from the voice judge. Field names match the
/// eight Right-X folds in
/// [`crate::executor::VOICE_JUDGE_PROMPT`] /
/// `RELATIONAL_BASE_SYSTEM_PROMPT` /
/// [`crate::pipeline::presenter::PRESENTER_RELATIONAL_SYSTEM`]
/// exactly — renaming a fold there requires the matching rename
/// here so the `serde(deserialize)` lands.
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
    /// runs and authoring scenarios — not a scoring axis.
    #[serde(default)]
    pub rationale: String,
}

impl JudgeScore {
    /// Sum the eight fold scores. Convenience for telemetry where
    /// a single number is more legible than nine fields.
    pub fn fold_total(&self) -> u8 {
        self.right_attention
            .saturating_add(self.right_specificity)
            .saturating_add(self.right_calibration)
            .saturating_add(self.right_question)
            .saturating_add(self.right_silence)
            .saturating_add(self.right_disagreement)
            .saturating_add(self.right_edge)
            .saturating_add(self.right_self_honesty)
    }
}

/// Build a `CompletionRequest` that runs the voice rubric against
/// a single candidate response. Caller dispatches it through their
/// [`crate::traits::InferenceProvider`] of choice (Fast slot
/// recommended; the rubric is a Fast-slot-shaped task).
///
/// Returns the request configured for Fast-slot inference with a
/// JSON-schema constraint on the output. The schema mirrors
/// [`JudgeScore`] so the response can be `serde_json::from_str`'d
/// into the typed struct.
pub fn voice_judge_request(
    original_user_message: &str,
    candidate: &str,
) -> CompletionRequest {
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
    // Judge is a structured-output task; suppress the Fast slot's
    // chain-of-thought so the JSON parses on first attempt.
    req.enable_thinking = Some(false);
    req
}

/// Should the judge run for this turn? Pure policy, exposed
/// separately from [`spawn_voice_judge`] so the runtime can short-
/// circuit before paying for the spawn (a cancelled `JoinHandle`
/// is cheap but a no-op judge call is cheaper still).
///
/// Skip on [`SkillRegister::Factual`] (no voice axes apply) or
/// when the operator has disabled the judge globally
/// (`pipeline.judge_enabled = false`).
pub fn should_judge(register: SkillRegister, judge_enabled: bool) -> bool {
    judge_enabled && matches!(register, SkillRegister::Relational)
}

/// Fire-and-forget voice judge. Spawns a tokio task that runs
/// [`voice_judge_request`] against the supplied candidate text and
/// returns a [`JoinHandle`] yielding the parsed [`JudgeScore`].
///
/// Phase 3.2: the runtime calls this AFTER the Presenter has
/// flushed to the user — so the judge never gates the user-visible
/// reply. The handle is awaited on a separate path that emits a
/// delayed
/// [`crate::types::NarrationPhase::PresentationComplete`] frame
/// when the score lands; if the runtime drops the handle the task
/// runs to completion and the score is logged but no narration
/// frame fires. Either path is acceptable — this is telemetry, not
/// behaviour.
///
/// On any provider error, returns a default [`JudgeScore`] (all
/// zeros, empty rationale) — the judge must never propagate
/// errors back into the turn handler. Soft-fail is the contract.
///
/// Caller is responsible for the [`should_judge`] gate; this
/// helper unconditionally spawns. Splitting policy from spawn
/// keeps the function signature uncluttered (no `Option` return).
pub fn spawn_voice_judge(
    provider: Arc<dyn InferenceProvider>,
    user_message: String,
    candidate: String,
) -> JoinHandle<JudgeScore> {
    tokio::spawn(async move {
        let request = voice_judge_request(&user_message, &candidate);
        match provider.complete(&request).await {
            Ok(response) => parse_judge_score(&response.text),
            Err(e) => {
                tracing::warn!(error = %e, "voice judge: provider error; recording default score");
                JudgeScore::default()
            }
        }
    })
}

/// Parse a judge response. Soft-fails to a default [`JudgeScore`]
/// (all zeros, empty rationale) on parse error — Phase 3.2 wants
/// the judge to be fire-and-forget telemetry, so a malformed
/// response should land as a "no-score" telemetry event rather
/// than aborting the turn.
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
        // Same assertions sovereign-cli's harness pinned, kept
        // here so the canonical implementation owns the contract.
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
        assert!(names.contains(&"right_attention"));
        assert!(names.contains(&"avoid_list_penalty"));
        assert!(names.contains(&"rationale"));
    }

    #[test]
    fn parse_judge_score_handles_bare_json() {
        let s = parse_judge_score(
            r#"{"right_attention":3,"right_specificity":2,"right_calibration":3,
                "right_question":2,"right_silence":3,"right_disagreement":2,
                "right_edge":3,"right_self_honesty":3,"avoid_list_penalty":0,
                "rationale":"clean witness reply"}"#,
        );
        assert_eq!(s.right_attention, 3);
        assert_eq!(s.avoid_list_penalty, 0);
        assert_eq!(s.fold_total(), 21);
    }

    #[test]
    fn parse_judge_score_handles_fenced_json() {
        let raw = "Here's the score:\n```json\n\
                   {\"right_attention\":1,\"right_specificity\":1,\
                   \"right_calibration\":1,\"right_question\":1,\
                   \"right_silence\":1,\"right_disagreement\":1,\
                   \"right_edge\":1,\"right_self_honesty\":1,\
                   \"avoid_list_penalty\":2,\"rationale\":\"x\"}\n```";
        let s = parse_judge_score(raw);
        assert_eq!(s.fold_total(), 8);
        assert_eq!(s.avoid_list_penalty, 2);
    }

    #[test]
    fn parse_judge_score_soft_fails_on_garbage() {
        let s = parse_judge_score("not json at all");
        assert_eq!(s.fold_total(), 0);
        assert_eq!(s.avoid_list_penalty, 0);
        assert!(s.rationale.is_empty());
    }

    #[test]
    fn should_judge_skips_factual_register() {
        assert!(!should_judge(SkillRegister::Factual, true));
    }

    #[test]
    fn should_judge_skips_when_disabled() {
        assert!(!should_judge(SkillRegister::Relational, false));
    }

    #[test]
    fn should_judge_runs_on_relational_when_enabled() {
        assert!(should_judge(SkillRegister::Relational, true));
    }

    #[tokio::test]
    async fn spawn_voice_judge_returns_default_score_on_provider_error() {
        use crate::error::Error;
        use crate::traits::InferenceProvider;
        use crate::types::{
            CompletionRequest, CompletionResponse, ProviderCapabilities,
        };
        use async_trait::async_trait;
        use futures::Stream;
        use std::pin::Pin;

        // Provider that always errors on `complete`. Mirrors the
        // shape of the real provider trait without pulling a heavy
        // mock crate; the judge is supposed to soft-fail under
        // exactly this kind of fault.
        struct ErringProvider;
        #[async_trait]
        impl InferenceProvider for ErringProvider {
            async fn complete(
                &self,
                _: &CompletionRequest,
            ) -> crate::error::Result<CompletionResponse> {
                Err(Error::Inference("boom".into()))
            }
            async fn complete_stream(
                &self,
                _: &CompletionRequest,
            ) -> crate::error::Result<
                Pin<Box<dyn Stream<Item = crate::error::Result<String>> + Send>>,
            > {
                unreachable!("judge does not call complete_stream")
            }
            async fn embed(&self, _: &str) -> crate::error::Result<Vec<f32>> {
                unreachable!("judge does not call embed")
            }
            fn capabilities(&self) -> ProviderCapabilities {
                ProviderCapabilities {
                    max_context_tokens: 4096,
                    supports_structured_output: true,
                    relative_speed: Speed::Fast,
                    relative_reasoning: crate::types::Depth::Shallow,
                }
            }
        }

        let provider: Arc<dyn InferenceProvider> = Arc::new(ErringProvider);
        let handle = spawn_voice_judge(
            provider,
            "user message".to_string(),
            "candidate text".to_string(),
        );
        let score = handle.await.expect("judge task should not panic");
        // Soft-fail contract: provider error → all-zero score, no
        // rationale, no propagation back to the turn handler.
        assert_eq!(score.fold_total(), 0);
        assert_eq!(score.avoid_list_penalty, 0);
        assert!(score.rationale.is_empty());
    }
}
