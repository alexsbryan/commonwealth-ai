// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`ChildProvider`] — a daemon-side `InferenceProvider` backed by ONE
//! supervised compute child, with fail-fast: if the child isn't serving
//! (warming / restarting / gone) a call returns [`Error::ComputeUnavailable`]
//! immediately, and an in-flight call races the child's exit so it never
//! hangs on a dead socket.
//!
//! There is deliberately no N-replica pool here. A live embed run
//! (DISTRIBUTED_PILOT_READINESS.md P1) showed process replicas LOSE to
//! in-process multi-sequence batching for a fits-on-one-box model, so the
//! replica-pool machinery was removed. The boundary exists for crash
//! isolation and the can't-fit-one-box (distributed) case, where a slot maps
//! to exactly one child.

use std::pin::Pin;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde::Serialize;
use sovereign_contracts::{
    CompletionRequest, CompletionResponse, Depth, Error, InferenceProvider, ProviderCapabilities,
    Result, Speed, StreamFrame,
};
use tokio::sync::watch;

use crate::client::ComputeChildClient;
use crate::wire::EmbedMode;

/// The observable lifecycle of one compute child — the P3 session states,
/// mapped from `SupervisorState` by the manager's state collector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildLifecycle {
    /// Spawned, awaiting the port handshake.
    Starting,
    /// Port announced; the model is loading (health probes 503).
    Warming,
    /// Model loaded, health probe green — routable.
    Serving,
    /// Was serving, but the last health probes failed.
    Degraded,
    /// Crashed; the supervisor is backing off before respawn.
    Restarting,
    /// Crash-loop ceiling hit — the supervisor stopped auto-restarting.
    Failed,
}

impl ChildLifecycle {
    /// snake_case tag (matches the serde form).
    pub fn as_str(self) -> &'static str {
        match self {
            ChildLifecycle::Starting => "starting",
            ChildLifecycle::Warming => "warming",
            ChildLifecycle::Serving => "serving",
            ChildLifecycle::Degraded => "degraded",
            ChildLifecycle::Restarting => "restarting",
            ChildLifecycle::Failed => "failed",
        }
    }
}

/// Live runtime state of one child, published on a `watch` channel by the
/// manager. `client` is `Some` only while [`ChildLifecycle::Serving`].
#[derive(Clone)]
pub struct ChildRuntimeState {
    /// Current lifecycle phase.
    pub lifecycle: ChildLifecycle,
    /// The child's OS process id (from the handshake), when known.
    pub pid: Option<u32>,
    /// The child's current ephemeral port (from the handshake), when known.
    pub port: Option<u16>,
    /// A ready client — present iff serving.
    pub client: Option<ComputeChildClient>,
    /// How many times this child has restarted.
    pub restarts: u32,
    /// Human-facing reason for the most recent transition.
    pub last_transition_reason: String,
    /// Reason for the most recent exit/crash, if any.
    pub last_exit: Option<String>,
}

impl ChildRuntimeState {
    /// The initial (pre-spawn) state.
    pub fn starting() -> Self {
        Self {
            lifecycle: ChildLifecycle::Starting,
            pid: None,
            port: None,
            client: None,
            restarts: 0,
            last_transition_reason: "starting".to_string(),
            last_exit: None,
        }
    }
}

/// An `InferenceProvider` backed by a single compute child.
pub struct ChildProvider {
    name: String,
    state: watch::Receiver<ChildRuntimeState>,
}

impl ChildProvider {
    /// Build a provider reading `state` (published by the manager).
    pub fn new(name: String, state: watch::Receiver<ChildRuntimeState>) -> Self {
        Self { name, state }
    }

    /// The child/replica name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `true` iff the child is currently serving (has a live client).
    pub fn is_serving(&self) -> bool {
        self.state.borrow().client.is_some()
    }

    fn client_or_unavailable(&self) -> Result<ComputeChildClient> {
        let st = self.state.borrow();
        st.client.clone().ok_or_else(|| Error::ComputeUnavailable {
            slot: self.name.clone(),
            reason: format!("child not serving ({})", st.lifecycle.as_str()),
        })
    }

    fn mid_request_unavailable(&self) -> Error {
        Error::ComputeUnavailable {
            slot: self.name.clone(),
            reason: "child exited mid-request".to_string(),
        }
    }

    /// Resolves once the child stops serving (its client disappears) — the
    /// fail-fast trigger raced against an in-flight request.
    async fn wait_lost_serving(mut state: watch::Receiver<ChildRuntimeState>) {
        loop {
            if state.borrow_and_update().client.is_none() {
                return;
            }
            if state.changed().await.is_err() {
                return;
            }
        }
    }
}

#[async_trait]
impl InferenceProvider for ChildProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let client = self.client_or_unavailable()?;
        let watch = self.state.clone();
        tokio::select! {
            r = client.complete(request) => r,
            _ = Self::wait_lost_serving(watch) => Err(self.mid_request_unavailable()),
        }
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        let frames = self.complete_stream_with_finish(request).await?;
        Ok(Box::pin(frames.filter_map(|f| async move {
            match f {
                StreamFrame::Token(t) => Some(Ok(t)),
                StreamFrame::Error(e) => Some(Err(Error::Inference(e))),
                StreamFrame::Finish { .. } => None,
            }
        })))
    }

    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        // The client's stream reassembly already synthesises a terminal
        // Error frame if the child dies mid-stream, so no extra race here.
        let client = self.client_or_unavailable()?;
        client.complete_stream_frames(request).await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let client = self.client_or_unavailable()?;
        let watch = self.state.clone();
        tokio::select! {
            r = client.embed(text, EmbedMode::Document) => r,
            _ = Self::wait_lost_serving(watch) => Err(self.mid_request_unavailable()),
        }
    }

    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let client = self.client_or_unavailable()?;
        let watch = self.state.clone();
        tokio::select! {
            r = client.embed(query, EmbedMode::Query) => r,
            _ = Self::wait_lost_serving(watch) => Err(self.mid_request_unavailable()),
        }
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        // One child, one native multi-sequence batch — no cross-process
        // sharding (that was the removed replica path).
        let client = self.client_or_unavailable()?;
        let watch = self.state.clone();
        tokio::select! {
            r = client.embed_batch(texts) => r,
            _ = Self::wait_lost_serving(watch) => Err(self.mid_request_unavailable()),
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        // The child's real caps aren't cheaply reachable here; advertise the
        // conservative shape (generate children DO honour grammar).
        ProviderCapabilities {
            max_context_tokens: 0,
            supports_structured_output: true,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}
