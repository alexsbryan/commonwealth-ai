// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`OpenAiPassthrough`]: the OpenAI face a svrn daemon gets when no
//! distribution hands it serve's (pb-serve-ranks).
//!
//! The OpenAI translation (prompt accounting, tool profiles, grammar) and the
//! ranking of venues are serve's. A svrn running alone holds neither, so its
//! OpenAI routes post the request to the server it dials — serve on this host,
//! or a terminal's entry node — and relay the answer: the request goes out as
//! svrn's route parsed it, and the stream comes back frame by frame. The
//! admission id rides along only as svrn's route left it: the route clears a
//! caller-supplied one before this runs (`turn_admission`, phase-b-27).
//!
//! Every `InferenceProvider` method forwards to the provider svrn already
//! holds for the same server, so the two halves cannot disagree on what that
//! server serves. A server that does not answer is an `Err` naming it, which
//! the route renders as a 503 (principle 6).

use std::pin::Pin;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde::Deserialize;
use sovereign_contracts::error::Result;
use sovereign_contracts::oicp::openai_types::{
    ChatCompletionRequest, ChatCompletionResponse, CompletionsRequestWire,
    FinishReason as WireFinish, StopParam, StreamFrame as WireFrame, StreamUsage as WireUsage,
    ToolCall,
};
use sovereign_contracts::oicp::{
    ComputeChildStatus, EditSlotStatus, FimCompletionRequest, FimStreamStart, LocalInferenceError,
    ProviderManifest, SlotDecodeEvidence,
};
use sovereign_contracts::traits::{
    InferenceProvider, LocalInferenceService, ResidentSlot, ServingLocus,
};
use sovereign_contracts::types::*;

use crate::{Payload, RemoteApiProvider, SplitInferenceProvider};

/// The trace target of every event here.
const TARGET: &str = "openai_passthrough";

/// svrn's OpenAI routes, relayed to the server it dials.
pub struct OpenAiPassthrough {
    /// The provider svrn holds for the same server: every
    /// `InferenceProvider` method forwards here.
    local: Arc<dyn InferenceProvider>,
    /// The chat half of that provider's client: its endpoint, bearer,
    /// node stamp and shed rule are the ones every relay posts with.
    target: Arc<RemoteApiProvider>,
    /// The server's own manifest, as [`Self::read_manifest`] last read it.
    manifest: RwLock<Option<ProviderManifest>>,
}

impl OpenAiPassthrough {
    /// Relay to the server `target` dials; `local` answers the provider half
    /// (a reload cell over `target`, or `target` itself).
    pub fn new(local: Arc<dyn InferenceProvider>, target: &SplitInferenceProvider) -> Self {
        tracing::info!(target: TARGET, server = %target.chat.endpoint.describe(), "svrn's OpenAI routes relay to the server it dials");
        Self {
            local,
            target: Arc::clone(&target.chat),
            manifest: RwLock::new(None),
        }
    }

    /// Read the server's manifest (`GET /oicp/v1/capabilities`), which
    /// [`LocalInferenceService::provider_manifest`] answers until the next
    /// read. `false`, traced, when the server did not answer: the manifest
    /// read before stays, and a server never read answers `None`.
    pub async fn read_manifest(&self) -> bool {
        let base = match self.target.endpoint.resolve().await {
            Ok(base) => base,
            Err(e) => {
                tracing::warn!(target: TARGET, error = %e, "manifest not read: the server is not reachable");
                return false;
            }
        };
        let url = format!("{}/oicp/v1/capabilities", base.trim_end_matches("/v1"));
        let read = async {
            let response = self
                .target
                .outbound(Payload::Probe)
                .map_err(|e| e.to_string())?
                .get(&url)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !response.status().is_success() {
                return Err(format!("answered {}", response.status()));
            }
            response
                .json::<ProviderManifest>()
                .await
                .map_err(|e| e.to_string())
        };
        match read.await {
            Ok(manifest) => {
                tracing::info!(target: TARGET, %url, models = manifest.models.len(), "the server's manifest read");
                *self.manifest.write().unwrap_or_else(|p| p.into_inner()) = Some(manifest);
                true
            }
            Err(e) => {
                tracing::warn!(target: TARGET, %url, error = %e, "the server's manifest was not read");
                false
            }
        }
    }

    /// POST `body` to `{base}{path}`, admitted as `payload`, with the target's
    /// stamp and shed rule.
    async fn post(
        &self,
        path: &str,
        payload: Payload,
        body: &impl serde::Serialize,
        what: &'static str,
    ) -> Result<reqwest::Response> {
        let admitted = self.target.outbound(payload)?;
        let url = format!("{}{path}", self.target.endpoint.resolve().await?);
        tracing::debug!(target: TARGET, %url, what, "relayed");
        self.target
            .send_honouring_shed(|| admitted.post(&url).json(body), what)
            .await
    }
}

/// One chat stream chunk as `sovereign_contracts::openai_http::sse_item`
/// renders it.
#[derive(Deserialize)]
struct ChatChunk {
    #[serde(default)]
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
    #[serde(default)]
    error: Option<ChunkError>,
}

#[derive(Deserialize)]
struct ChatChoice {
    #[serde(default)]
    delta: ChatDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct ChatDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

/// One FIM stream chunk as `sovereign_contracts::fim_http::fim_sse_items`
/// renders it. The loopback raw completion (`serve_loopback`) reads it too.
#[derive(Deserialize)]
pub(crate) struct TextChunk {
    #[serde(default)]
    pub(crate) choices: Vec<TextChoice>,
    #[serde(default)]
    pub(crate) usage: Option<WireUsage>,
    #[serde(default)]
    pub(crate) error: Option<ChunkError>,
    #[serde(default)]
    sovereign_debug: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub(crate) struct TextChoice {
    #[serde(default)]
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) finish_reason: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ChunkError {
    pub(crate) message: String,
}

/// The frames one chat chunk carries, in the order `sse_item` emits them.
fn chat_frames(data: &str) -> Vec<WireFrame> {
    let chunk = match serde_json::from_str::<ChatChunk>(data) {
        Ok(c) => c,
        Err(e) => {
            return vec![WireFrame::Error(format!(
                "the server sent a chunk this relay cannot read ({e}): {data}"
            ))]
        }
    };
    if let Some(e) = chunk.error {
        return vec![WireFrame::Error(e.message)];
    }
    let mut frames = Vec::new();
    for choice in chunk.choices {
        if let Some(text) = choice.delta.content {
            frames.push(WireFrame::Token(text));
        }
        if let Some(calls) = choice.delta.tool_calls {
            frames.push(WireFrame::ToolCalls(calls));
        }
        if let Some(reason) = choice.finish_reason {
            frames.push(WireFrame::Finish {
                reason: WireFinish::from_openai_str(&reason),
                usage: chunk.usage.clone(),
            });
        }
    }
    frames
}

/// The frames one FIM chunk carries.
fn text_frames(data: &str) -> Vec<WireFrame> {
    let chunk = match serde_json::from_str::<TextChunk>(data) {
        Ok(c) => c,
        Err(e) => {
            return vec![WireFrame::Error(format!(
                "the server sent a chunk this relay cannot read ({e}): {data}"
            ))]
        }
    };
    if let Some(e) = chunk.error {
        return vec![WireFrame::Error(e.message)];
    }
    let mut frames = Vec::new();
    if let Some(debug) = chunk.sovereign_debug {
        frames.push(WireFrame::Debug(debug));
    }
    for choice in chunk.choices {
        if !choice.text.is_empty() {
            frames.push(WireFrame::Token(choice.text));
        }
        if let Some(reason) = choice.finish_reason {
            frames.push(WireFrame::Finish {
                reason: WireFinish::from_openai_str(&reason),
                usage: chunk.usage.clone(),
            });
        }
    }
    frames
}

/// Relay an SSE body as frames, each `data:` payload through `parse`. The
/// stream ends at `[DONE]`; one that ends without a terminal frame gets an
/// `Error` naming that, never an invented finish reason (principle 6).
fn relay_sse(
    response: reqwest::Response,
    parse: fn(&str) -> Vec<WireFrame>,
) -> Pin<Box<dyn Stream<Item = WireFrame> + Send>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<WireFrame>(32);
    let mut bytes = response.bytes_stream();
    tokio::spawn(async move {
        let mut buf = String::new();
        let mut terminal = false;
        'read: while let Some(chunk) = bytes.next().await {
            let chunk = match chunk {
                Ok(b) => b,
                Err(e) => {
                    let _ = tx
                        .send(WireFrame::Error(format!("the relayed stream broke: {e}")))
                        .await;
                    return;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim().to_string();
                buf.drain(..=pos);
                let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                    continue;
                };
                if data == sovereign_contracts::openai_http::DONE {
                    break 'read;
                }
                for frame in parse(data) {
                    terminal = matches!(frame, WireFrame::Finish { .. } | WireFrame::Error(_));
                    if tx.send(frame).await.is_err() {
                        return;
                    }
                    if terminal {
                        break 'read;
                    }
                }
            }
        }
        if !terminal {
            tracing::warn!(target: TARGET, "the relayed stream ended without a finish reason");
            let _ = tx
                .send(WireFrame::Error(
                    "the server's stream ended without a finish reason".to_string(),
                ))
                .await;
        }
    });
    Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
}

#[async_trait]
impl LocalInferenceService for OpenAiPassthrough {
    async fn chat_completion(
        &self,
        mut request: ChatCompletionRequest,
    ) -> std::result::Result<ChatCompletionResponse, LocalInferenceError> {
        request.stream = Some(false);
        let response = self
            .post(
                "/chat/completions",
                Payload::Completion,
                &request,
                "Relayed chat completion",
            )
            .await
            .map_err(|e| LocalInferenceError::Other(e.to_string()))?;
        response
            .json::<ChatCompletionResponse>()
            .await
            .map_err(|e| {
                LocalInferenceError::Other(format!("the server's answer did not parse: {e}"))
            })
    }

    async fn chat_completion_stream(
        &self,
        mut request: ChatCompletionRequest,
    ) -> std::result::Result<Pin<Box<dyn Stream<Item = WireFrame> + Send>>, LocalInferenceError>
    {
        request.stream = Some(true);
        let response = self
            .post(
                "/chat/completions",
                Payload::Completion,
                &request,
                "Relayed chat stream",
            )
            .await
            .map_err(|e| LocalInferenceError::Other(e.to_string()))?;
        Ok(relay_sse(response, chat_frames))
    }

    fn provider_manifest(&self) -> Option<ProviderManifest> {
        self.manifest
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    async fn fim_completion_stream(
        &self,
        request: FimCompletionRequest,
    ) -> std::result::Result<FimStreamStart, String> {
        let wire = CompletionsRequestWire {
            model: None,
            prompt: None,
            suffix: Some(request.suffix),
            prefix: Some(request.prefix),
            path: request.path,
            language: request.language,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            stop: Some(StopParam::Multi(request.stop)),
            stream: Some(true),
            debug: Some(request.debug),
            raw_prompt: request.raw_prompt,
        };
        let response = self
            .post(
                "/completions",
                Payload::Completion,
                &wire,
                "Relayed FIM completion",
            )
            .await
            .map_err(|e| e.to_string())?;
        // The wire names no slot and no marker family: the edit slot the
        // provider reports is the one serve completes on.
        let edit = self.local.edit_slot_info();
        Ok(FimStreamStart {
            stream: relay_sse(response, text_frames),
            model_id: edit
                .as_ref()
                .map(|e| e.model_id.clone())
                .unwrap_or_else(|| "unreported".to_string()),
            slot: edit
                .as_ref()
                .map(|e| e.slot.clone())
                .unwrap_or_else(|| "unreported".to_string()),
            fim_style: edit
                .and_then(|e| e.fim)
                .map(|l| l.style.as_str().to_string())
                .unwrap_or_else(|| "unreported".to_string()),
        })
    }

    fn edit_status(&self) -> Option<EditSlotStatus> {
        sovereign_contracts::fim_http::edit_status(&self.local)
    }
}

#[async_trait]
impl InferenceProvider for OpenAiPassthrough {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        self.local.complete(request).await
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        self.local.complete_stream(request).await
    }

    async fn complete_stream_with_id(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = Result<String>> + Send>>, String)> {
        self.local.complete_stream_with_id(request).await
    }

    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        self.local.complete_stream_with_finish(request).await
    }

    async fn complete_stream_with_id_and_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, String)> {
        self.local.complete_stream_with_id_and_finish(request).await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.local.embed(text).await
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.local.embed_batch(texts).await
    }

    async fn complete_batch(
        &self,
        requests: &[CompletionRequest],
    ) -> Result<Vec<CompletionResponse>> {
        self.local.complete_batch(requests).await
    }

    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.local.embed_query(query).await
    }

    async fn rerank_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        self.local.rerank_batch(query, docs).await
    }

    fn model_id_for(&self, speed: Speed) -> String {
        self.local.model_id_for(speed)
    }

    fn embed_model_id(&self) -> String {
        self.local.embed_model_id()
    }

    fn serving_locus(&self) -> ServingLocus {
        self.local.serving_locus()
    }

    fn effective_context_size(&self) -> Option<u32> {
        self.local.effective_context_size()
    }

    fn n_ctx_train_for_primary(&self) -> Option<u32> {
        self.local.n_ctx_train_for_primary()
    }

    fn count_tokens(&self, text: &str) -> u32 {
        self.local.count_tokens(text)
    }

    fn code_model_id(&self) -> Option<String> {
        self.local.code_model_id()
    }

    fn edit_slot_info(&self) -> Option<EditSlotInfo> {
        self.local.edit_slot_info()
    }

    async fn warmup_primary(&self) -> Result<()> {
        self.local.warmup_primary().await
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.local.capabilities()
    }

    fn load_extra_slot(
        &self,
        slot_name: String,
        path: std::path::PathBuf,
        context_size: u32,
    ) -> Result<String> {
        self.local.load_extra_slot(slot_name, path, context_size)
    }

    fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>> {
        self.local.unload_extra_slot(slot_name)
    }

    fn extras_inventory(&self) -> Vec<(String, String)> {
        self.local.extras_inventory()
    }

    fn resident_slots(&self) -> Vec<ResidentSlot> {
        self.local.resident_slots()
    }

    fn decode_evidence(&self) -> Vec<SlotDecodeEvidence> {
        self.local.decode_evidence()
    }

    async fn primary_slot_status(&self) -> Option<ResidentSlot> {
        self.local.primary_slot_status().await
    }

    fn compute_children(&self) -> Vec<ComputeChildStatus> {
        self.local.compute_children()
    }

    async fn peer_manifests(&self) -> Vec<(String, ProviderManifest)> {
        self.local.peer_manifests().await
    }

    async fn lender_manifest(&self) -> Option<(String, Vec<String>)> {
        self.local.lender_manifest().await
    }
}

// In a sibling file for this file's arch-gate slack; the names are unchanged.
#[cfg(test)]
#[path = "openai_passthrough_tests.rs"]
mod tests;
