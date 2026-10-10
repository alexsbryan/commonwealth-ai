// SPDX-License-Identifier: AGPL-3.0-or-later
//! Forced choice on a host that does not answer it natively.
//!
//! A Sovereign daemon answers an `x_forced_choice` call (the sentinel schema
//! of `oicp_types::forced_choice`) with a label-to-probability map from one
//! forward pass. A plain OpenAI-compatible server such as llama-server
//! instead enforces the enum and returns the label it sampled, which
//! `forced_choice::parse` rejects. The grounding gate's judge then fails open,
//! and the usefulness judge goes uncalibrated (bench/lanes/engine-swap,
//! 2026-10-09).
//!
//! The same distribution is available from such a host. Ask for one token
//! with its top logprobs, with no schema and thinking off, and renormalise the
//! labels' mass. A label outside the top [`TOP_LOGPROBS`] gets no mass. If no
//! label is there at all, the answer is empty: the caller reads that as no
//! answer, never as a zero.
//!
//! A host is asked natively first. The logprobs route is taken when the
//! native answer is not a map, and it is remembered once it has produced one,
//! the same way `chat_wire` remembers a host that refuses `json_schema`.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use serde_json::{json, Value};
use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::types::CompletionRequest;

use crate::RemoteApiProvider;

/// How many alternatives the host is asked to report for the one token.
/// Measured on 2026-10-09: 70 of 70 judge calls found every label within 20,
/// and the labels' combined mass was never below 0.964.
const TOP_LOGPROBS: u32 = 20;

/// The label-to-probability map from one token's top logprobs, renormalised
/// over `labels`. `None` when no label appears among them.
pub(crate) fn label_distribution(
    top: &[Value],
    labels: &[String],
) -> Option<BTreeMap<String, f64>> {
    let mut mass: BTreeMap<String, f64> = labels.iter().map(|l| (l.clone(), 0.0)).collect();
    for alt in top {
        let token = alt.get("token").and_then(Value::as_str).unwrap_or("");
        let Some(logprob) = alt.get("logprob").and_then(Value::as_f64) else {
            continue;
        };
        let label = if mass.contains_key(token) {
            token
        } else {
            token.trim()
        };
        if let Some(m) = mass.get_mut(label) {
            *m += logprob.exp();
        }
    }
    let total: f64 = mass.values().sum();
    (total > 0.0).then(|| mass.into_iter().map(|(l, m)| (l, m / total)).collect())
}

impl RemoteApiProvider {
    /// Whether this host has needed the logprobs route before.
    pub(crate) fn forced_choice_by_logprobs_known(&self) -> bool {
        self.forced_choice_by_logprobs.load(Ordering::Relaxed)
    }

    /// Answer `request`, a forced-choice call over `labels`, from one token's
    /// top logprobs. `Ok(None)` when the host returned no logprobs (it cannot
    /// serve this route either), so the caller keeps the native answer.
    pub(crate) async fn forced_choice_by_logprobs(
        &self,
        url: &str,
        request: &CompletionRequest,
        labels: &[String],
    ) -> Result<Option<String>> {
        let admitted = self.outbound(crate::Payload::Completion)?;
        let mut body = self.build_request(request);
        if let Some(obj) = body.as_object_mut() {
            for key in [
                "response_format",
                "tools",
                "tool_choice",
                "think_budget",
                "thinking",
            ] {
                obj.remove(key);
            }
        }
        body["max_tokens"] = json!(1);
        body["temperature"] = json!(0);
        body["logprobs"] = json!(true);
        body["top_logprobs"] = json!(TOP_LOGPROBS);
        body["chat_template_kwargs"] = json!({"enable_thinking": false});
        let response = self
            .send_honouring_shed(
                || admitted.post(url).json(&body),
                "Forced-choice logprobs request",
            )
            .await?;
        let answer: Value = response.json().await.map_err(|e| {
            Error::Inference(format!(
                "Failed to parse forced-choice logprobs response: {e}"
            ))
        })?;
        let Some(top) = answer
            .pointer("/choices/0/logprobs/content/0/top_logprobs")
            .and_then(Value::as_array)
        else {
            tracing::warn!(
                target: "oicp_client",
                %url,
                "forced choice: the host returned no logprobs either; keeping its native answer"
            );
            return Ok(None);
        };
        self.forced_choice_by_logprobs
            .store(true, Ordering::Relaxed);
        let distribution = label_distribution(top, labels);
        tracing::debug!(
            target: "oicp_client",
            %url,
            labels = ?labels,
            alternatives = top.len(),
            distribution = ?distribution,
            "forced choice answered from one token's top logprobs"
        );
        Ok(Some(match distribution {
            Some(map) => serde_json::to_string(&map).map_err(|e| {
                Error::Inference(format!(
                    "forced choice: the label distribution did not serialise: {e}"
                ))
            })?,
            None => {
                tracing::info!(
                    target: "oicp_client",
                    %url,
                    labels = ?labels,
                    "forced choice: no label among the top logprobs; answering with no answer"
                );
                String::new()
            }
        }))
    }
}

#[cfg(test)]
#[path = "forced_choice/tests.rs"]
mod tests;
