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

use crate::cognitive::item::{Item, render};

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
}

pub fn run_item(item: &Item, opts: &RunOpts<'_>) -> Result<ItemResult> {
    let rendered = render(item, opts.workspace_root)
        .with_context(|| format!("rendering prompt for `{}`", item.item.id))?;
    let system = rendered
        .system
        .as_deref()
        .unwrap_or(FALLBACK_SYSTEM_PROMPT);
    let started = Instant::now();
    let (transport_ok, response_raw, error) =
        match call_chat_completions(opts, system, &rendered.user) {
            Ok(content) => (true, content, None),
            Err(e) => (false, String::new(), Some(e.to_string())),
        };
    let elapsed = started.elapsed();
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
    })
}

fn call_chat_completions(opts: &RunOpts<'_>, system: &str, user: &str) -> Result<String> {
    let url = format!(
        "{}/v1/chat/completions",
        opts.daemon_url.trim_end_matches('/')
    );
    let body = serde_json::json!({
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
    Ok(content)
}
