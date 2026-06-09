// SPDX-License-Identifier: AGPL-3.0-or-later
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

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::cognitive::item::{render, Item, Scoring};

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
    /// When `true`, omit `temperature` and `top_p` from the request
    /// body so the daemon's `ModelQuirks` per-family defaults apply
    /// (Qwen 3.5 = T=0.7/top_p=0.95/top_k=20, Gemma 4 = T=1.0/top_p=0.95/top_k=64,
    /// etc.). Use this for cross-family benchmarks where forcing a
    /// single T across all models penalises ones whose distribution
    /// is mis-tuned at T=0.
    pub family_defaults: bool,
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
    let system = rendered.system.as_deref().unwrap_or(FALLBACK_SYSTEM_PROMPT);
    let response_format = response_format_for(&item.scoring);
    let started = Instant::now();
    let outcome = call_chat_completions(opts, system, &rendered.user, response_format.as_ref());
    let elapsed = started.elapsed();
    let (transport_ok, response_raw, prompt_tokens, completion_tokens, error) = match outcome {
        Ok(call) => (
            true,
            call.content,
            call.prompt_tokens,
            call.completion_tokens,
            None,
        ),
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
        Scoring::MultiChoice { choice_field, .. } => Some(serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "rationale_then_choice",
                "schema": {
                    "type": "object",
                    // Field declaration order is load-bearing: the
                    // JsonConstraint walker accepts required props
                    // in declaration order. Forcing `rationale`
                    // first means the model writes its reasoning
                    // before the choice token — so the choice
                    // sampler conditions on the model's own
                    // argument rather than on a positional prior.
                    //
                    // Observed 2026-05-19 on Gemma 4 26B-A4B-it
                    // (Q6_K_XL): with `choice` first, the model
                    // emitted "A" on 7/7 failed hard-DQ items at
                    // T=0.0 AND T=1.0 with on-topic but
                    // wrong-conclusion rationale. The MoE quant has
                    // a positional bias at the first letter-choice
                    // token that argument-shaped context overrides.
                    "properties": {
                        "rationale": { "type": "string", "maxLength": 500 },
                        choice_field: {
                            "type": "string",
                            "enum": ["A", "B", "C", "D", "E"]
                        }
                    },
                    "required": ["rationale", choice_field],
                    "additionalProperties": false
                }
            }
        })),
        Scoring::Calibration {
            confidence_field, ..
        } => Some(serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "rationale_claim_is_true_confidence",
                "schema": {
                    "type": "object",
                    // Three-field shape, declaration order is load-bearing.
                    //
                    // 1. `rationale` — the model's argument. Forces the
                    //    decode to commit to a chain of reasoning before
                    //    naming a position (cf. MultiChoice).
                    // 2. `claim_is_true` — boolean. The judgment carrier.
                    //    Field name spells out the semantics so the
                    //    model can't interpret it as "I assert my
                    //    rationale" instead of "is the user's claim
                    //    true". Observed 2026-05-20 on Qwen3.6-35B-A3B:
                    //    a `verdict: bool` field flipped inverted on
                    //    items where the rationale correctly named the
                    //    claim as false — the model treated `verdict`
                    //    as a self-endorsement signal. Renaming +
                    //    description makes the field's referent
                    //    unambiguous across model families.
                    // 3. `confidence` — 1-5 enum (constrained, not bare
                    //    integer). Auxiliary calibration signal, not
                    //    the pass/fail axis.
                    "properties": {
                        "rationale": {
                            "type": "string",
                            "maxLength": 500,
                            "description": "One sentence explaining whether the claim from the user is true or false and why."
                        },
                        "claim_is_true": {
                            "type": "boolean",
                            "description": "Set this to true if the claim in the user message is true, and false if it is false. This is the answer to the user's question, not a self-endorsement of your rationale."
                        },
                        confidence_field: {
                            "type": "integer",
                            "enum": [1, 2, 3, 4, 5],
                            "description": "How confident you are in the claim_is_true value, on a 1-5 scale (1 = very unsure, 5 = very sure). This is auxiliary calibration data; it does not determine the answer."
                        }
                    },
                    "required": ["rationale", "claim_is_true", confidence_field],
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
    let mut body = if opts.family_defaults {
        // Omit temperature + top_p so the daemon falls back to the
        // model's `ModelQuirks` defaults. Keep seed for reproducibility.
        serde_json::json!({
            "model": opts.model,
            "max_tokens": opts.max_tokens,
            "seed": opts.seed,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        })
    } else {
        serde_json::json!({
            "model": opts.model,
            "temperature": opts.temperature,
            "top_p": 1.0,
            "max_tokens": opts.max_tokens,
            "seed": opts.seed,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        })
    };
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
        let truncated = if text.len() > 1024 {
            &text[..1024]
        } else {
            &text[..]
        };
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
