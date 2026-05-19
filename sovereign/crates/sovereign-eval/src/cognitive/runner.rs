//! Calls the daemon's `/v1/chat/completions` endpoint once per item.
//!
//! Mirrors the transport shape from `crate::judge::call_chat_completions`:
//! reqwest blocking, 600s timeout, structured JSON body. Unlike the
//! judge, the cognitive runner does NOT retry on parse failure — a
//! malformed response is itself a signal worth surfacing in the
//! report, not a transient error to paper over.
//!
//! Pinned-by-default hyperparameters (temperature 0.0, seed) make the
//! suite reproducible. Operators iterating on item shape can override
//! via the CLI.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::cognitive::item::{Item, Scoring, render};

/// Same temperature/seed convention as `judge.rs:JUDGE_TEMPERATURE` /
/// `JUDGE_SEED`. The cognitive bank is meant to be reproducible.
pub const DEFAULT_TEMPERATURE: f32 = 0.0;
pub const DEFAULT_SEED: u64 = 0xC067;
pub const DEFAULT_MAX_TOKENS: u32 = 1024;
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

const FALLBACK_SYSTEM_PROMPT: &str = "You output ONE valid JSON object exactly matching the schema implied by the user's question. No prose around it. No markdown fences. No code blocks.";

#[derive(Debug, Clone)]
pub struct RunOpts<'a> {
    pub daemon_url: &'a str,
    pub model: &'a str,
    pub temperature: f32,
    pub seed: u64,
    pub max_tokens: u32,
    pub workspace_root: &'a Path,
}

/// One per-item result; the report aggregates these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemResult {
    pub item_id: String,
    pub category: String,
    pub title: String,
    pub source_path: String,
    pub model: String,
    pub elapsed_ms: u64,
    /// Raw assistant content (no trimming, no fence stripping). The
    /// scorer is responsible for whatever parsing it needs.
    pub response_raw: String,
    /// True iff the model returned a 2xx and a non-empty `content`
    /// string. Transport failures land in `error`.
    pub transport_ok: bool,
    pub error: Option<String>,
    /// Prompt-token count from the daemon's `usage` envelope, when
    /// present. `None` for transport failures or providers that
    /// don't report usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    /// Completion-token count from the daemon's `usage` envelope.
    /// Throughput is derived as `completion_tokens / (elapsed_ms / 1000)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
}

pub fn run_item(item: &Item, opts: &RunOpts<'_>) -> Result<ItemResult> {
    let rendered = render(item, opts.workspace_root)
        .with_context(|| format!("rendering prompt for `{}`", item.item.id))?;
    let system = rendered
        .system
        .as_deref()
        .unwrap_or(FALLBACK_SYSTEM_PROMPT);
    let response_format = response_format_for(&item.scoring);
    let started = Instant::now();
    let outcome = call_chat_completions(opts, system, &rendered.user, response_format.as_ref());
    let elapsed = started.elapsed();
    let (transport_ok, response_raw, prompt_tokens, completion_tokens, error) = match outcome {
        Ok(call) => (true, call.content, call.prompt_tokens, call.completion_tokens, None),
        Err(e) => (false, String::new(), None, None, Some(e.to_string())),
    };
    Ok(ItemResult {
        item_id: item.item.id.clone(),
        category: item.item.category.as_str().to_string(),
        title: item.item.title.clone(),
        source_path: item.source_path.display().to_string(),
        model: opts.model.to_string(),
        elapsed_ms: elapsed.as_millis() as u64,
        response_raw,
        transport_ok,
        error,
        prompt_tokens,
        completion_tokens,
    })
}

/// Single call's parsed outputs. Lets `run_item` return the content
/// alongside the daemon's `usage` envelope (tokens for throughput).
struct CallOutcome {
    content: String,
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

/// Build an OpenAI `response_format` JSON-schema for items whose
/// scoring kind expects a structured JSON object. The shape mirrors
/// what the scorer reads (choice_field / confidence_field) so the
/// model is constrained to produce parseable output regardless of
/// its instruction-following bias.
///
/// `ExactMatch` and `ToolUse` items skip the constraint — exact-match
/// expects free text; tool-use uses the tool_calls envelope (a
/// separate grammar surface).
///
/// This is a generic eval-runner feature: any model the bank runs
/// against benefits. Models that already comply with the system-prompt
/// JSON instruction lose nothing; models that don't are pulled into
/// the schema. Same fairness contract as installing a grammar in any
/// other OpenAI-compatible runner.
pub(crate) fn response_format_for(scoring: &Scoring) -> Option<serde_json::Value> {
    match scoring {
        Scoring::MultiChoice {
            choice_field, ..
        } => Some(serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "choice_with_rationale",
                "schema": {
                    "type": "object",
                    "properties": {
                        // Multi-choice prompts in this bank use
                        // single-letter labels (A/B/C/D). Constraining
                        // to that alphabet rejects prose tokens like
                        // "Pick" or "Approach B" before they corrupt
                        // the choice field. Same fairness contract as
                        // the wider response_format wrapper — any
                        // backend that complies with the prompt's
                        // declared shape (also single-letter) is
                        // unaffected.
                        choice_field: {
                            "type": "string",
                            "enum": ["A", "B", "C", "D", "E"]
                        },
                        "rationale": { "type": "string", "maxLength": 500 }
                    },
                    "required": [choice_field, "rationale"],
                    "additionalProperties": false
                }
            }
        })),
        Scoring::Calibration {
            confidence_field, ..
        } => Some(serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "claim_with_confidence",
                "schema": {
                    "type": "object",
                    "properties": {
                        confidence_field: { "type": "integer" },
                        "rationale": { "type": "string", "maxLength": 500 }
                    },
                    "required": [confidence_field, "rationale"],
                    "additionalProperties": false
                }
            }
        })),
        Scoring::ExactMatch { .. } | Scoring::ToolUse { .. } => None,
    }
}

fn call_chat_completions(
    opts: &RunOpts<'_>,
    system: &str,
    user: &str,
    response_format: Option<&serde_json::Value>,
) -> Result<CallOutcome> {
    let url = format!(
        "{}/v1/chat/completions",
        opts.daemon_url.trim_end_matches('/')
    );
    let mut body = serde_json::json!({
        "model": opts.model,
        "temperature": opts.temperature,
        "top_p": 1.0,
        "max_tokens": opts.max_tokens,
        "seed": opts.seed,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
    });
    if let Some(rf) = response_format {
        body["response_format"] = rf.clone();
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .build()
        .context("building reqwest client")?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .context("POST /v1/chat/completions")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        let truncated = if text.len() > 1024 { &text[..1024] } else { &text[..] };
        bail!("daemon returned {status}: {truncated}");
    }
    let v: serde_json::Value = resp.json().context("parsing daemon response")?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    if content.is_empty() {
        bail!("daemon returned empty content");
    }
    // Throughput accounting: pull `usage.prompt_tokens` and
    // `usage.completion_tokens` from the response envelope.
    // OpenAI-compatible providers all emit this; missing values
    // simply degrade the throughput aggregate to None.
    let prompt_tokens = v
        .pointer("/usage/prompt_tokens")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as u32);
    let completion_tokens = v
        .pointer("/usage/completion_tokens")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as u32);
    Ok(CallOutcome {
        content,
        prompt_tokens,
        completion_tokens,
    })
}
