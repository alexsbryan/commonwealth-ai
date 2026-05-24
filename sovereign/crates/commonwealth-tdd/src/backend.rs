//! Chat-completion backend abstraction.
//!
//! Per ARCH_PRINCIPLES §12.4 ("tests must not require GPU, network,
//! or real model weights") and §12.5 (`DeterministicInference` and
//! friends exist — use them), every solver takes a
//! `&dyn ChatBackend` rather than calling `reqwest` directly.
//!
//! Two impls ship out of the box:
//!
//! - [`ReqwestChatBackend`] — production. Posts OpenAI-compatible
//!   `/v1/chat/completions` requests to a provider URL (the
//!   sovereign daemon at `localhost:9741/v1` by default).
//! - [`DeterministicChatBackend`] — test mock. Returns scripted
//!   responses in order. Lets unit tests pin the loop's reaction
//!   to specific model outputs (e.g. "what happens when round 0
//!   improves and round 1 stalls?") without a daemon.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

/// Errors a backend can surface to the solver loop. The solver
/// treats every variant as a candidate-level failure (not a
/// trial-level abort) so a transient daemon hiccup doesn't kill
/// the parallel cohort.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BackendError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("provider {status}: {body}")]
    Provider { status: u16, body: String },
    #[error("malformed response: {0}")]
    Malformed(String),
    /// `DeterministicChatBackend` ran out of scripted responses.
    /// In production this would be `Provider`; the variant is
    /// kept distinct so test failures are unambiguous.
    #[error("test backend script exhausted at call index {0}")]
    ScriptExhausted(usize),
}

/// Minimal response surface — enough for the solver to score a
/// candidate. Token usage is included so the loop can enforce its
/// own budget caps (separate from the per-request `max_tokens`).
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// Single async method — `complete` — keeps the trait narrow per
/// ARCH §5.1. New backends just implement this; the registry
/// doesn't need to grow.
#[async_trait]
pub trait ChatBackend: Send + Sync {
    async fn complete(
        &self,
        model: &str,
        messages: Vec<Value>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<ChatResponse, BackendError>;
}

// ── production impl ─────────────────────────────────────────────────

/// Posts OpenAI-compatible chat-completions to a configurable
/// provider URL. Matches the wire shape the sovereign daemon
/// exposes today so a TDD solver can run against the local daemon
/// out of the box.
pub struct ReqwestChatBackend {
    http: reqwest::Client,
    provider_url: String,
}

impl ReqwestChatBackend {
    pub fn new(provider_url: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            provider_url: provider_url.into(),
        }
    }
}

#[async_trait]
impl ChatBackend for ReqwestChatBackend {
    async fn complete(
        &self,
        model: &str,
        messages: Vec<Value>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<ChatResponse, BackendError> {
        let url = format!("{}/chat/completions", self.provider_url);
        let body = json!({
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": false,
        });
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| BackendError::Transport(format!("read body: {e}")))?;
        if !status.is_success() {
            return Err(BackendError::Provider {
                status: status.as_u16(),
                body: text.chars().take(500).collect(),
            });
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| BackendError::Malformed(format!("parse: {e}")))?;
        let content = v
            .pointer("/choices/0/message/content")
            .and_then(|x| x.as_str())
            .ok_or_else(|| {
                BackendError::Malformed("missing choices[0].message.content".into())
            })?
            .to_string();
        let prompt_tokens = v
            .pointer("/usage/prompt_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        let completion_tokens = v
            .pointer("/usage/completion_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        tracing::debug!(
            %model, temperature,
            prompt_tokens, completion_tokens,
            "backend:reqwest: completion received"
        );
        Ok(ChatResponse {
            content,
            prompt_tokens,
            completion_tokens,
        })
    }
}

// ── test mock ───────────────────────────────────────────────────────

/// Scripted backend for unit tests. Hands out canned `ChatResponse`s
/// in call order, returning `BackendError::ScriptExhausted` when the
/// queue runs out. The queue is `Mutex<VecDeque<…>>` rather than
/// channel-based so tests can inspect the remaining script after a
/// run (`remaining()` and `call_count()`).
pub struct DeterministicChatBackend {
    script: Mutex<std::collections::VecDeque<ChatResponse>>,
    calls: Mutex<usize>,
}

impl DeterministicChatBackend {
    pub fn new(script: impl IntoIterator<Item = ChatResponse>) -> Self {
        Self {
            script: Mutex::new(script.into_iter().collect()),
            calls: Mutex::new(0),
        }
    }

    /// Construct from plain `String` contents — most tests only
    /// care about the model's output text, not the token counts.
    pub fn from_strs(messages: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::new(messages.into_iter().map(|s| ChatResponse {
            content: s.into(),
            prompt_tokens: 0,
            completion_tokens: 0,
        }))
    }

    pub fn call_count(&self) -> usize {
        *self.calls.lock().unwrap()
    }

    pub fn remaining(&self) -> usize {
        self.script.lock().unwrap().len()
    }
}

#[async_trait]
impl ChatBackend for DeterministicChatBackend {
    async fn complete(
        &self,
        _model: &str,
        _messages: Vec<Value>,
        _temperature: f32,
        _max_tokens: u32,
    ) -> Result<ChatResponse, BackendError> {
        let mut calls = self.calls.lock().unwrap();
        let idx = *calls;
        *calls += 1;
        let mut script = self.script.lock().unwrap();
        match script.pop_front() {
            Some(r) => Ok(r),
            None => Err(BackendError::ScriptExhausted(idx)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deterministic_returns_scripted_responses_in_order() {
        let b = DeterministicChatBackend::from_strs(["first", "second", "third"]);
        let r1 = b.complete("m", vec![], 0.0, 100).await.unwrap();
        let r2 = b.complete("m", vec![], 0.0, 100).await.unwrap();
        let r3 = b.complete("m", vec![], 0.0, 100).await.unwrap();
        assert_eq!(r1.content, "first");
        assert_eq!(r2.content, "second");
        assert_eq!(r3.content, "third");
    }

    #[tokio::test]
    async fn deterministic_errors_when_script_exhausted() {
        let b = DeterministicChatBackend::from_strs(["only"]);
        let _ = b.complete("m", vec![], 0.0, 100).await.unwrap();
        let err = b.complete("m", vec![], 0.0, 100).await.unwrap_err();
        assert!(matches!(err, BackendError::ScriptExhausted(1)));
    }

    #[tokio::test]
    async fn deterministic_exposes_call_count_and_remaining() {
        // Test fixtures often assert "the solver made N calls"
        // without caring about contents; the inspector methods
        // make that ergonomic without grepping logs.
        let b = DeterministicChatBackend::from_strs(["a", "b", "c"]);
        assert_eq!(b.call_count(), 0);
        assert_eq!(b.remaining(), 3);
        let _ = b.complete("m", vec![], 0.0, 100).await.unwrap();
        let _ = b.complete("m", vec![], 0.0, 100).await.unwrap();
        assert_eq!(b.call_count(), 2);
        assert_eq!(b.remaining(), 1);
    }
}
