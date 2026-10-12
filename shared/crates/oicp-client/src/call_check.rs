// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `required` call check (engine-swap decision 1). llama-server's own
//! `required` can run to max_tokens in prose before any call (battery case
//! 77a7138f2caa), and that prose reached the caller as if it were the call.
//! So a reply to a `required` request on such a host is checked for a call
//! to a tool it offered, asked once more on a miss, then failed with an
//! error naming the miss. It is never returned as prose.
//!
//! A Sovereign daemon is not checked: its engine enforces the call with a
//! grammar, and may answer it as `<tool_call>` text rather than `tool_calls`.

use serde_json::Value;
use sovereign_contracts::engine_config::GrammarSupport;
use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::types::CompletionRequest;

use crate::outbound::Admitted;
use crate::{response_wire, ChatCompletionResponse, RemoteApiProvider, StructuredOutputMode};

/// The tools a reply to `body` must call one of: `Some` when `body` requires
/// a call, offers tools, and goes to a host that does not enforce it.
pub(crate) fn required_tools(body: &Value, host: GrammarSupport) -> Option<Vec<String>> {
    if host == GrammarSupport::Sovereign || body.get("tool_choice")? != "required" {
        return None;
    }
    let names: Vec<String> = body
        .get("tools")?
        .as_array()?
        .iter()
        .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    (!names.is_empty()).then_some(names)
}

/// What `reply` did instead of calling one of `allowed`; `None` when it
/// called one.
pub(crate) fn miss(reply: &ChatCompletionResponse, allowed: &[String]) -> Option<String> {
    let Some(choice) = reply.choices.first() else {
        return Some("returned no choice".into());
    };
    let calls = &choice.message.tool_calls;
    if calls.iter().any(|c| allowed.contains(&c.function.name)) {
        return None;
    }
    if !calls.is_empty() {
        let called: Vec<&str> = calls.iter().map(|c| c.function.name.as_str()).collect();
        return Some(format!("called {called:?}, none of {allowed:?}"));
    }
    let prose = choice.message.content.as_deref().unwrap_or_default();
    let excerpt: String = prose.chars().take(160).collect();
    let finish = choice.finish_reason.as_deref().unwrap_or("none");
    Some(format!(
        "answered in prose with no call (finish_reason {finish}): {excerpt:?}"
    ))
}

impl RemoteApiProvider {
    /// `first`, the reply to `request` sent in `mode`, if it calls an allowed
    /// tool or the request requires no call; otherwise the reply to one more
    /// ask, if that one calls; otherwise an error naming both misses.
    pub(crate) async fn check_required_call(
        &self,
        admitted: &Admitted<'_>,
        url: &str,
        request: &CompletionRequest,
        mode: StructuredOutputMode,
        first: ChatCompletionResponse,
    ) -> Result<(ChatCompletionResponse, StructuredOutputMode)> {
        let body = self.build_request_in(request, mode)?;
        let Some(allowed) = required_tools(&body, self.grammar) else {
            return Ok((first, mode));
        };
        let Some(first_miss) = miss(&first, &allowed) else {
            tracing::debug!(target: "oicp_client", %url, ?allowed, "required call check: called");
            return Ok((first, mode));
        };
        tracing::warn!(
            target: "oicp_client",
            %url,
            ?allowed,
            miss = %first_miss,
            "required call check: missed; asking once more"
        );
        let (response, mode) = self.send_chat(admitted, url, request).await?;
        let second = response_wire::read_chat(response).await?;
        let Some(second_miss) = miss(&second, &allowed) else {
            tracing::info!(target: "oicp_client", %url, ?allowed, "required call check: called on the retry");
            return Ok((second, mode));
        };
        tracing::warn!(
            target: "oicp_client",
            %url,
            ?allowed,
            miss = %second_miss,
            "required call check: missed twice; failed"
        );
        Err(Error::Inference(format!(
            "tool_choice \"required\" over {allowed:?}: the reply {first_miss}; asked once \
             more, it {second_miss}"
        )))
    }
}

#[cfg(test)]
#[path = "call_check/tests.rs"]
mod tests;
