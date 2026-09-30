// SPDX-License-Identifier: AGPL-3.0-or-later
//! The door's model call, dialed to serve on this host (pb-meshapp-rest;
//! phase-b-23: serve mounts `/v1/completions` and `/v1/chat/completions`
//! over its own adapter, and the editor door dials them). The edit slot is
//! read from serve's self-report through the one reader svrn uses too
//! (`sovereign_turn_client::serve_self`). Every failure is traced and
//! returned by name; the lane turns it into `unavailable` or `error`.

use sovereign_contracts::oicp::openai_types::{ChatCompletionRequest, ChatCompletionResponse};
use sovereign_contracts::oicp::FimCompletionRequest;
use sovereign_contracts::types::EditSlotInfo;

/// serve's edit slot, or `None` when serve is not reachable at `base` or
/// holds no editing model.
pub(super) async fn edit_slot(base: &str) -> Option<EditSlotInfo> {
    match sovereign_turn_client::serve_self::read_served_self(base).await {
        Ok(served) => {
            if served.edit_slot.is_none() {
                tracing::debug!(target: "next_edit", serve = base, "model lane: serve holds no edit slot");
            }
            served.edit_slot
        }
        Err(e) => {
            tracing::debug!(target: "next_edit", serve = base, error = %e, "model lane: serve's edit slot unread");
            None
        }
    }
}

/// A raw (completion-style) prompt through serve's `/v1/completions`:
/// `(content, finish_reason)`, or why not.
pub(super) async fn raw_completion(
    http: &reqwest::Client,
    base: &str,
    request: &FimCompletionRequest,
) -> Result<(String, Option<String>), String> {
    let body = serde_json::json!({
        "raw_prompt": request.raw_prompt,
        "path": request.path,
        "language": request.language,
        "max_tokens": request.max_tokens,
        "temperature": request.temperature,
        "stop": request.stop,
        "stream": false,
    });
    let answer = post(http, base, "/v1/completions", &body).await?;
    let choice = &answer["choices"][0];
    let text = choice["text"]
        .as_str()
        .ok_or_else(|| format!("serve at {base} answered a completion with no text: {answer}"))?;
    Ok((
        text.to_string(),
        choice["finish_reason"].as_str().map(str::to_string),
    ))
}

/// A chat prompt through serve's `/v1/chat/completions`:
/// `(content, finish_reason)`, or why not.
pub(super) async fn chat_completion(
    http: &reqwest::Client,
    base: &str,
    request: &ChatCompletionRequest,
) -> Result<(String, Option<String>), String> {
    let answer = post(http, base, "/v1/chat/completions", request).await?;
    let resp: ChatCompletionResponse = serde_json::from_value(answer)
        .map_err(|e| format!("serve at {base} answered an unreadable chat completion: {e}"))?;
    let choice = resp.choices.into_iter().next();
    let finish = choice.as_ref().and_then(|c| c.finish_reason.clone());
    Ok((
        choice.map(|c| c.message.content).unwrap_or_default(),
        finish,
    ))
}

async fn post(
    http: &reqwest::Client,
    base: &str,
    path: &str,
    body: &impl serde::Serialize,
) -> Result<serde_json::Value, String> {
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let resp = http
        .post(&url)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("serve at {base} is not reachable: {e}"))?;
    let status = resp.status();
    let answer: serde_json::Value = resp.json().await.map_err(|e| {
        format!("serve at {url} answered HTTP {status} with an unreadable body: {e}")
    })?;
    if !status.is_success() {
        return Err(format!("serve at {url} refused: HTTP {status}: {answer}"));
    }
    Ok(answer)
}
