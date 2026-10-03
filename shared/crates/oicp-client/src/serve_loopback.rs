// SPDX-License-Identifier: AGPL-3.0-or-later
//! The terminal arm's loopback mode (pb-svrn-dials-serve): a
//! [`SplitInferenceProvider`] pointed at `serve` on this host, which holds
//! the weights the svrn daemon no longer does.
//!
//! A terminal bound to a REMOTE entry node advertises nothing, so peers never
//! route to a node that holds no weights (§18.3). The loopback mode is the one
//! exception, because serve is this node's own model server: the provider
//! answers `model_id_for`, `resident_slots`, `edit_slot_info` and
//! `code_model_id` from serve's own self-report
//! ([`ServedSelf`], read by the daemon from serve's
//! `/v1/engine/self`), so the daemon's manifest names the models this node
//! really serves. And a raw-shaped completion — the FIM and next-edit model
//! call — goes to serve's `/v1/completions` as a `raw_prompt`, because the
//! chat wire would wrap it in a user turn.

use std::pin::Pin;

use futures::Stream;
use sovereign_contracts::engine_state::ServedSelf;
use sovereign_contracts::error::Result;
use sovereign_contracts::traits::ResidentSlot;
use sovereign_contracts::types::{
    CompletionRequest, EditSlotInfo, FinishReason, PromptShape, Speed, StreamFrame, StreamUsage,
};

use crate::openai_passthrough::TextChunk;
use crate::{RemoteApiProvider, SplitInferenceProvider};

impl SplitInferenceProvider {
    /// The loopback mode: answer this node's model facts from serve's
    /// self-report. Only for a provider pointed at `serve` on this host;
    /// a remote entry node's provider keeps advertising nothing.
    pub fn with_served(mut self, served: ServedSelf) -> Self {
        tracing::info!(
            primary = %served.primary_model,
            slots = served.resident_slots.len(),
            edit = served.edit_slot.is_some(),
            "terminal arm: loopback mode, answering this node's models from serve"
        );
        self.served = Some(served);
        self
    }
}

/// `model_id_for` in the loopback mode: serve's model for that speed.
pub(crate) fn model_id_for(served: &ServedSelf, speed: Speed) -> String {
    match speed {
        Speed::Slow => served.primary_model.clone(),
        Speed::Medium => served.medium_model.clone(),
        Speed::Fast => served.fast_model.clone(),
    }
}

pub(crate) fn resident_slots(served: &Option<ServedSelf>) -> Vec<ResidentSlot> {
    served
        .as_ref()
        .map(|s| s.resident_slots.clone())
        .unwrap_or_default()
}

pub(crate) fn edit_slot_info(served: &Option<ServedSelf>) -> Option<EditSlotInfo> {
    served.as_ref().and_then(|s| s.edit_slot.clone())
}

pub(crate) fn compute_children(
    served: &Option<ServedSelf>,
) -> Vec<sovereign_contracts::oicp::ComputeChildStatus> {
    served
        .as_ref()
        .map(|s| s.compute_children.clone())
        .unwrap_or_default()
}

/// Why runtime slot management refuses here: this process holds no weights.
/// In the loopback mode they are serve's, on this host; otherwise they are the
/// entry node's. (The trait default names "only the embedded llama.cpp
/// provider", which is the wrong reason once serve holds the slots.)
pub(crate) fn slot_refusal(
    served: &Option<ServedSelf>,
    verb: &str,
) -> sovereign_contracts::error::Error {
    let why = if served.is_some() {
        format!(
            "runtime slot {verb} is serve's: this daemon holds no weights, and serve \
             loads its slots from its config (reload it through serve)"
        )
    } else {
        format!("runtime slot {verb} belongs to the entry node: this node holds no weights")
    };
    tracing::debug!(target: "oicp_client", verb, loopback = served.is_some(), "slot management refused");
    sovereign_contracts::error::Error::Inference(why)
}

pub(crate) fn code_model_id(served: &Option<ServedSelf>) -> Option<String> {
    served.as_ref().and_then(|s| s.code_model.clone())
}

/// The primary row of serve's self-report, which is what the trait default
/// reads from `resident_slots()` for a provider that holds its weights.
pub(crate) fn primary_slot(served: &ServedSelf) -> Option<ResidentSlot> {
    served
        .resident_slots
        .iter()
        .find(|s| s.role == "primary")
        .cloned()
}

/// Does this request go to serve's `/v1/completions` rather than the chat
/// wire? Only in the loopback mode, and only for a raw-shaped prompt.
pub(crate) fn wants_raw_completion(
    served: &Option<ServedSelf>,
    request: &CompletionRequest,
) -> bool {
    served.is_some() && request.prompt_shape == Some(PromptShape::Raw)
}

impl RemoteApiProvider {
    /// A raw prompt through `/v1/completions`, streamed back as typed frames.
    /// A mid-stream error chunk is an `Error` frame; a stream that ends
    /// without a finish reason is a `Stop`, as on the chat wire.
    pub(crate) async fn raw_completion_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        let admitted = self.outbound(crate::Payload::Completion(request.oicp.as_ref()))?;
        let url = format!("{}/completions", self.endpoint.resolve().await?);
        let body = serde_json::json!({
            "model": request.model_id,
            "raw_prompt": request.prompt,
            "max_tokens": request.max_tokens,
            "temperature": request.temperature,
            "stream": true,
        });
        tracing::debug!(%url, prompt_chars = request.prompt.len(), "remote: raw completion to serve");
        let response = self
            .send_honouring_shed(
                || admitted.post(&url).json(&body),
                "Raw completion request",
            )
            .await?;
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamFrame>(32);
        let byte_stream = response.bytes_stream();
        tokio::spawn(async move {
            use futures::StreamExt;
            let mut byte_stream = byte_stream;
            let mut buf = String::new();
            let mut finish_reason: Option<FinishReason> = None;
            let mut usage: Option<StreamUsage> = None;
            'outer: while let Some(chunk) = byte_stream.next().await {
                let bytes = match chunk {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = tx
                            .send(StreamFrame::Error(format!(
                                "raw completion stream broke: {e}"
                            )))
                            .await;
                        return;
                    }
                };
                buf.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf.drain(..=pos);
                    if line == "data: [DONE]" {
                        break 'outer;
                    }
                    let Some(data) = line.strip_prefix("data: ") else {
                        continue;
                    };
                    let Ok(parsed) = serde_json::from_str::<TextChunk>(data) else {
                        tracing::debug!(chunk = data, "remote: raw completion chunk unparsed");
                        continue;
                    };
                    if let Some(e) = parsed.error {
                        let _ = tx.send(StreamFrame::Error(e.message)).await;
                        return;
                    }
                    if let Some(u) = parsed.usage {
                        usage = Some(StreamUsage {
                            prompt_tokens: u.prompt_tokens,
                            completion_tokens: u.completion_tokens,
                            total_tokens: u.total_tokens,
                        });
                    }
                    for choice in parsed.choices {
                        if !choice.text.is_empty()
                            && tx.send(StreamFrame::Token(choice.text)).await.is_err()
                        {
                            return;
                        }
                        if let Some(r) = choice.finish_reason {
                            finish_reason = FinishReason::from_openai_str(&r);
                        }
                    }
                }
            }
            let _ = tx
                .send(StreamFrame::Finish {
                    reason: finish_reason.unwrap_or(FinishReason::Stop),
                    usage,
                })
                .await;
        });
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::traits::InferenceProvider;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn served() -> ServedSelf {
        ServedSelf {
            primary_model: "big-27b".into(),
            medium_model: "big-27b".into(),
            fast_model: "small-4b".into(),
            embed_model: "embed-0.6b".into(),
            embed_family: Default::default(),
            code_model: Some("coder".into()),
            resident_slots: vec![ResidentSlot {
                role: "primary".into(),
                model_id: "big-27b".into(),
                resident: true,
                size_bytes: None,
                transitioning: false,
                placement: None,
            }],
            edit_slot: None,
            context_size: Some(8192),
            compute_children: Vec::new(),
        }
    }

    /// A daemon on the dialing path is asked to load a slot: the refusal names
    /// serve, which holds the weights, not "only the embedded provider".
    #[test]
    fn slot_management_is_refused_naming_where_the_weights_are() {
        let loopback = provider("http://127.0.0.1:1").with_served(served());
        let e = loopback
            .load_extra_slot("x".into(), "/m.gguf".into(), 4096)
            .expect_err("no weights here");
        assert!(e.to_string().contains("serve's"), "{e}");
        let e = loopback
            .unload_extra_slot("x")
            .expect_err("no weights here");
        assert!(e.to_string().contains("serve's"), "{e}");
        let terminal = provider("http://127.0.0.1:1");
        let e = terminal
            .unload_extra_slot("x")
            .expect_err("no weights here");
        assert!(e.to_string().contains("entry node"), "{e}");
    }

    fn provider(base: &str) -> SplitInferenceProvider {
        SplitInferenceProvider::new(
            &format!("{base}/v1"),
            "primary".into(),
            "embed-0.6b".into(),
            4096,
            String::new(),
        )
    }

    #[test]
    fn the_loopback_mode_answers_this_nodes_models_from_serve() {
        let p = provider("http://127.0.0.1:9").with_served(served());
        assert_eq!(p.model_id_for(Speed::Slow), "big-27b");
        assert_eq!(p.model_id_for(Speed::Fast), "small-4b");
        assert_eq!(p.resident_slots().len(), 1, "serve's slots are this node's");
        assert_eq!(p.code_model_id().as_deref(), Some("coder"));
    }

    #[test]
    fn a_terminal_on_a_remote_entry_still_advertises_nothing() {
        let p = provider("http://10.0.0.2:9741");
        assert!(p.resident_slots().is_empty());
        assert_eq!(p.model_id_for(Speed::Slow), "primary");
    }

    /// A one-shot HTTP server that answers any request with `body` as SSE and
    /// hands back the request it read.
    async fn sse_once(body: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let served = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = vec![0u8; 16 * 1024];
            let n = sock.read(&mut req).await.unwrap();
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(body.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&req[..n]).to_string()
        });
        (base, served)
    }

    #[tokio::test]
    async fn a_raw_prompt_goes_to_serves_completions_and_streams_back_typed() {
        use futures::StreamExt;
        let (base, request_seen) = sse_once(
            "data: {\"choices\":[{\"index\":0,\"text\":\"return a\",\"finish_reason\":null}]}\n\n\
             data: {\"choices\":[{\"index\":0,\"text\":\"\",\"finish_reason\":\"length\"}]}\n\n\
             data: [DONE]\n\n",
        )
        .await;
        let p = provider(&base).with_served(served());
        let mut req = CompletionRequest::new("<|fim_prefix|>fn f() {<|fim_suffix|>}<|fim_middle|>");
        req.prompt_shape = Some(PromptShape::Raw);
        req.model_id = Some("coder".into());
        let frames: Vec<StreamFrame> = p
            .complete_stream_with_finish(&req)
            .await
            .expect("stream")
            .collect()
            .await;
        let seen = request_seen.await.unwrap();
        assert!(seen.starts_with("POST /v1/completions "), "{seen}");
        assert!(
            seen.contains("\"raw_prompt\":\"<|fim_prefix|>fn f() {"),
            "{seen}"
        );
        assert!(
            matches!(&frames[0], StreamFrame::Token(t) if t == "return a"),
            "{frames:?}"
        );
        assert!(
            matches!(
                frames.last(),
                Some(StreamFrame::Finish {
                    reason: FinishReason::Length,
                    ..
                })
            ),
            "{frames:?}"
        );
    }
}
