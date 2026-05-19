//! Fast-inference Judge — evaluates semantic assertions against
//! model output for the search-gym predicate scorer.
//!
//! Two principles drove this module:
//!
//!   1. **No string-list matching.** Predicates like "did the model
//!      acknowledge zero results" were hardcoded substring walks
//!      (`["no results", "couldn't find", …]`) in Phase 1 — brittle
//!      and gameable by phrasing. The Judge replaces that with a
//!      fast structured-output call to the primary model.
//!   2. **Trust requires calibration.** A judge that hasn't been
//!      proven to agree with humans is just another model
//!      hallucinating verdicts. The `CalibrationReceipt` marker
//!      gates construction so production code can only build a
//!      judge that's passed the calibration bank — see `judge_
//!      calibration.rs` (lands in 2b).
//!
//! The judge is intentionally single-purpose: one boolean per call,
//! plus a one-sentence rationale captured into transcripts for
//! operator inspection. Multi-criteria evaluation = multiple calls,
//! keeping each verdict cleanly auditable.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

/// The Judge contract. Every implementation answers: "Given this
/// `subject` (typically the model-under-test's response), does
/// `assertion` hold?" — and explains its reasoning in one sentence.
///
/// Implementations must be deterministic given a `(subject, assertion)`
/// pair to whatever extent the underlying model allows: temperature
/// is pinned at 0, thinking is disabled, and output is grammar-
/// constrained. Repeat calls with identical inputs should produce
/// identical verdicts in the steady state.
#[async_trait]
pub trait Judge: Send + Sync {
    async fn judge(&self, assertion: &str, subject: &str) -> Result<Verdict, String>;
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Verdict {
    pub passes: bool,
    /// One-sentence rationale from the judge. Stored in the
    /// per-replay transcript for operator inspection. Never used
    /// programmatically by the scorer — the scorer reads `passes`.
    pub rationale: String,
}

/// Proof type that the bearer has passed the calibration bank at
/// the required agreement threshold. The constructor of `Calibration
/// Receipt` is intentionally private (only `judge_calibration.rs`
/// can mint a real one) so production scorers can't accidentally
/// instantiate a judge that's never been validated against humans.
///
/// Until Phase 2b lands, an `untrusted()` escape hatch keeps
/// development unblocked — every call emits a `warn!` so the
/// uncalibrated state is impossible to forget about.
pub struct CalibrationReceipt {
    pub(crate) source: CalibrationSource,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum CalibrationSource {
    /// Phase 2b's calibration run minted this receipt after the
    /// judge agreed with the hand-labeled bank at ≥95% per-category.
    /// Reserved — not yet wired (2b).
    #[allow(dead_code)]
    PassedBank,
    /// Explicit opt-out. Logs a warning on each judge call.
    /// Removed once 2b's calibration bank is in the repo.
    Untrusted,
    /// Test-only. Constructible from #[cfg(test)] paths.
    #[cfg(test)]
    TestOnly,
}

impl CalibrationReceipt {
    /// Dev-mode escape hatch: build a judge that hasn't been
    /// calibrated. Each `judge()` call will log a `warn!` so the
    /// status is observable. Don't ship harness results that
    /// depend on an untrusted judge — they have unknown agreement
    /// with human ground truth.
    pub fn untrusted() -> Self {
        Self {
            source: CalibrationSource::Untrusted,
        }
    }

    /// Mint a trusted receipt from a passing calibration run. The
    /// only way to obtain a `CalibrationProof` is via
    /// `judge_calibration::calibrate(...)` returning Ok — so
    /// production scorers can never instantiate this path without
    /// an actual passing bank run.
    pub fn from_passing_proof(_proof: CalibrationProof) -> Self {
        Self {
            source: CalibrationSource::PassedBank,
        }
    }

    #[cfg(test)]
    pub fn test_only() -> Self {
        Self {
            source: CalibrationSource::TestOnly,
        }
    }
}

/// Zero-sized witness type that the bearer ran the calibration
/// bank and met the ≥95% per-category agreement threshold. The
/// constructor is `pub(super)` so only the sibling
/// `judge_calibration` module can mint it. External code is
/// structurally prevented from forging a passing-bank receipt.
#[derive(Debug)]
pub struct CalibrationProof {
    _private: (),
}

impl CalibrationProof {
    pub(super) fn new_from_passing_run() -> Self {
        Self { _private: () }
    }
}

pub struct FastInferenceJudgeCfg {
    pub base_url: String,
    pub model: String,
    pub timeout: Duration,
}

impl Default for FastInferenceJudgeCfg {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:9741".to_string(),
            // commonwealth/fast targets the dedicated fast slot
            // (Qwen3.5-9B-UD-MTP-Q6_K_XL as of 2026-05-18). Using
            // fast avoids contention with the primary slot — which
            // gym fixtures use for the test target and which long-
            // running enrichment pipelines also use. 9B class + MTP
            // gives ~3-5 s/case on classifier-style prompts on
            // Strix Halo Vulkan. Override with --judge-model.
            model: "commonwealth/fast".to_string(),
            timeout: Duration::from_secs(120),
        }
    }
}

pub struct FastInferenceJudge {
    client: reqwest::Client,
    cfg: FastInferenceJudgeCfg,
    calibration: CalibrationSource,
}

impl FastInferenceJudge {
    pub fn new(
        client: reqwest::Client,
        cfg: FastInferenceJudgeCfg,
        receipt: CalibrationReceipt,
    ) -> Self {
        Self {
            client,
            cfg,
            calibration: receipt.source,
        }
    }
}

// JSON schema the daemon enforces on the judge's output. Two fields,
// both required: a boolean verdict and a short rationale. The
// constraint enforcer guarantees the response parses cleanly.
fn verdict_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "passes":    { "type": "boolean" },
            "rationale": { "type": "string", "maxLength": 240 }
        },
        "required":             ["passes", "rationale"],
        "additionalProperties": false
    })
}

const JUDGE_SYSTEM_PROMPT: &str = "You evaluate whether a piece of text satisfies \
a given assertion. The text was written by a different system; you are not its \
author and not its respondent. Return a JSON object with `passes` (true/false) \
and `rationale` (one sentence naming the specific phrase or absence in the text \
that drove the decision). If the assertion is only partially satisfied or \
ambiguous, return false.

CRITICAL: Judge ONLY against the assertion's literal text. The text may \
reference entities, events, or data your training cut off before — or that \
came from a synthetic test fixture. Treat every factual claim in the text \
as ground truth; your task is solely to check whether the text's shape, \
framing, or structure matches what the assertion asks for. Do not use your \
own world knowledge to declare a claim hallucinated, impossible, or \
unverifiable — that lies outside what you are being asked to judge.";

#[async_trait]
impl Judge for FastInferenceJudge {
    async fn judge(&self, assertion: &str, subject: &str) -> Result<Verdict, String> {
        if matches!(self.calibration, CalibrationSource::Untrusted) {
            tracing::warn!(
                "search_gym: judge UNCALIBRATED — agreement with humans unverified. \
                 Phase 2b calibration bank not yet wired."
            );
        }

        // Simple wrapping — earlier iterations used emphatic
        // "BEGIN/END EVALUATED TEXT" markers and the model started
        // treating them as a jailbreak protocol to defend against,
        // generating defensive rationales instead of evaluating.
        // Plain "Assertion: …\n\nText: …" mirrors the production
        // distiller's classifier shape and keeps the model focused
        // on the task.
        let user_msg = format!(
            "Assertion: {assertion}\n\nText:\n{subject}"
        );

        // Classifier settings, restored to the deterministic profile
        // the Judge trait's contract promises ("temperature pinned at
        // 0, repeat calls should produce identical verdicts").
        //
        //   - `temperature: 0.0` is the headline change. The earlier
        //     workaround used T=0.7 via `sampling_mode: instruct`
        //     because T=0 + the old llguidance grammar got stuck on
        //     whitespace tokens (every legal continuation had identical
        //     logits at T=0). The in-house JSON constraint enforcer
        //     [[project_grammar_in_house_enforcer]] (2026-04) bypasses
        //     llama-grammar.cpp entirely with a mask-based sampler that
        //     allows T=0 cleanly. Validated 2026-05-19: search-gym
        //     fixture 07 was oscillating 2/5–5/5 across replays under
        //     T=0.7; T=0 + multi-judge consensus removes the noise.
        //   - `enable_thinking: false` + `think_budget: 0` force-
        //     suppress thinking even on instruct-trained families.
        //     Iteration 2 saw the model emit verbose discursive
        //     rationales when thinking wasn't fully off.
        let body = json!({
            "model": self.cfg.model,
            "messages": [
                { "role": "system", "content": JUDGE_SYSTEM_PROMPT },
                { "role": "user",   "content": user_msg }
            ],
            "temperature": 0.0,
            "sampling_mode": "instruct",
            "chat_template_kwargs": { "enable_thinking": false },
            "think_budget": 0,
            "max_tokens": 320,
            "stream": false,
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name":   "judge_verdict",
                    "schema": verdict_schema(),
                    "strict": true
                }
            }
        });

        let endpoint = format!(
            "{}/v1/chat/completions",
            self.cfg.base_url.trim_end_matches('/')
        );
        let resp = self
            .client
            .post(&endpoint)
            .json(&body)
            .timeout(self.cfg.timeout)
            .send()
            .await
            .map_err(|e| format!("judge http error: {e}"))?;

        let status = resp.status();
        let raw_body = resp
            .text()
            .await
            .map_err(|e| format!("judge http: read body: {e}"))?;

        if !status.is_success() {
            return Err(format!(
                "judge daemon returned {} ({})",
                status.as_u16(),
                raw_body.chars().take(400).collect::<String>()
            ));
        }

        let parsed: ChatResponse = serde_json::from_str(&raw_body)
            .map_err(|e| format!("judge response shell parse: {e}"))?;
        let content = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| "judge response missing choices[0].message.content".to_string())?;

        // The model may still emit a stripped `<think></think>` block
        // before the JSON object (depends on chat template), so look
        // for the JSON object explicitly rather than parsing whole
        // content. Cheap state-machine that just finds the balanced
        // outer braces.
        let json_slice = extract_json_object(&content)
            .ok_or_else(|| format!("judge: no balanced JSON object in content={content:?}"))?;

        let verdict: Verdict = serde_json::from_str(json_slice)
            .map_err(|e| format!("judge: parse verdict {json_slice:?}: {e}"))?;

        tracing::debug!(
            passes = verdict.passes,
            assertion = %assertion.chars().take(60).collect::<String>(),
            "search_gym: judge verdict"
        );

        Ok(verdict)
    }
}

// ─── Internal helpers ────────────────────────────────────────────

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

// Parse Verdict directly via the same Deserialize the schema enforces.
// The `passes` field is required, the `rationale` field is required.
impl<'de> Deserialize<'de> for Verdict {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            passes: bool,
            rationale: String,
        }
        let w = Wire::deserialize(d)?;
        Ok(Verdict {
            passes: w.passes,
            rationale: w.rationale,
        })
    }
}

/// Walk the content, find the first balanced `{ … }` substring.
/// Tolerates leading thinking blocks / whitespace / stray text.
/// Doesn't account for braces inside strings — fine here because
/// the judge's output is a small flat object emitted by a grammar-
/// constrained sampler; a stray string-quoted `{` in `rationale`
/// is the only edge case and is rare enough to surface as a
/// MalformedVerdict (which logs the offending content).
fn extract_json_object(content: &str) -> Option<&str> {
    let bytes = content.as_bytes();
    let start = content.find('{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for i in start..bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&content[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_object_finds_balanced_braces() {
        let s = r#"<think></think>
{"passes": true, "rationale": "The text says 'no results found' explicitly."}
trailing junk"#;
        let obj = extract_json_object(s).unwrap();
        let v: Verdict = serde_json::from_str(obj).unwrap();
        assert!(v.passes);
        assert!(v.rationale.contains("no results"));
    }

    #[test]
    fn extract_json_object_handles_nested() {
        let s = r#"{"passes": false, "rationale": "Says {curly} but doesn't match."}"#;
        let obj = extract_json_object(s).unwrap();
        let v: Verdict = serde_json::from_str(obj).unwrap();
        assert!(!v.passes);
    }

    #[test]
    fn extract_json_object_handles_quoted_braces() {
        let s = r#"junk {"passes": true, "rationale": "Contains a literal \"}\" character"} after"#;
        let obj = extract_json_object(s).unwrap();
        assert!(obj.starts_with('{') && obj.ends_with('}'));
        let v: Verdict = serde_json::from_str(obj).unwrap();
        assert!(v.passes);
    }

    #[test]
    fn extract_json_object_returns_none_on_unbalanced() {
        assert!(extract_json_object("no braces here").is_none());
        assert!(extract_json_object("{unclosed").is_none());
    }

    #[test]
    fn verdict_rejects_unknown_fields() {
        // The Wire struct uses deny_unknown_fields, so a judge that
        // accidentally produces extra keys should fail loudly rather
        // than silently dropping them.
        let s = r#"{"passes": true, "rationale": "ok", "extra": 1}"#;
        let err = serde_json::from_str::<Verdict>(s).unwrap_err();
        assert!(format!("{err}").contains("extra"), "err={err}");
    }

    #[test]
    fn untrusted_receipt_constructs() {
        let _r = CalibrationReceipt::untrusted();
        // Compile-only check: the marker is a real type the
        // constructor accepts. (Runtime warn! emission is exercised
        // in higher-level integration tests where we capture logs.)
    }

    #[test]
    fn default_cfg_targets_fast_slot() {
        let cfg = FastInferenceJudgeCfg::default();
        assert_eq!(cfg.model, "commonwealth/fast");
        assert!(cfg.base_url.contains("9741"));
    }

    // A simple mock Judge for testing the scorer integration without
    // hitting the daemon. The scorer's tests use this directly.
    pub struct FixedVerdictJudge {
        pub verdict: Verdict,
    }

    #[async_trait]
    impl Judge for FixedVerdictJudge {
        async fn judge(
            &self,
            _assertion: &str,
            _subject: &str,
        ) -> Result<Verdict, String> {
            Ok(self.verdict.clone())
        }
    }

    pub struct ScriptedJudge {
        // Returns (passes, rationale) keyed by a substring of the
        // assertion, in order of first match.
        pub script: Vec<(String, bool, String)>,
    }

    #[async_trait]
    impl Judge for ScriptedJudge {
        async fn judge(
            &self,
            assertion: &str,
            _subject: &str,
        ) -> Result<Verdict, String> {
            for (key, passes, rationale) in &self.script {
                if assertion.contains(key) {
                    return Ok(Verdict {
                        passes: *passes,
                        rationale: rationale.clone(),
                    });
                }
            }
            Err(format!(
                "scripted judge: no script entry for assertion={assertion}"
            ))
        }
    }
}

#[cfg(test)]
pub use tests::{FixedVerdictJudge, ScriptedJudge};
