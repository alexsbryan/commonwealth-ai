// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`ComputeChildClient`] — the daemon-side typed HTTP client that talks
//! to a compute child over the native wire ([`crate::wire`]).
//!
//! This is a thin transport: it serialises requests, parses the wire error
//! envelope back into a typed [`Error`], and reassembles the NDJSON
//! completion stream into a [`StreamFrame`] stream (the engine's own
//! byte-stream → line-split → frame → channel pattern). Fail-fast on child
//! death is layered on TOP of this by `ChildProvider` (increment 6), which
//! races these calls against the supervisor's child-exit signal — the
//! client itself just reports transport errors.

use std::pin::Pin;
use std::time::Duration;

use futures::{Stream, StreamExt};
use sovereign_contracts::oicp::openai_types::{RerankRequest, RerankResponse};
use sovereign_contracts::{CompletionRequest, CompletionResponse, Error, Result, StreamFrame};
use sovereign_inference::served_kind;
use tokio::sync::mpsc;

use crate::wire::{
    self, EmbedBatchRequest, EmbedBatchResponse, EmbedMode, EmbedRequest, EmbedResponse,
    HealthInfo, WireError, ROUTE_COMPLETE, ROUTE_COMPLETE_STREAM, ROUTE_EMBED, ROUTE_EMBED_BATCH,
    ROUTE_HEALTH,
};

/// How long one rerank call may wait on its child before it fails and the
/// search falls back to fusion order. A bound on a wedged child, not a latency
/// budget. The client itself carries no timeout: a streamed completion may
/// legitimately run for minutes.
pub const RERANK_TIMEOUT: Duration = Duration::from_secs(60);

/// A typed HTTP client for one compute child at `http://127.0.0.1:{port}`.
#[derive(Debug, Clone)]
pub struct ComputeChildClient {
    client: reqwest::Client,
    base_url: String,
}

impl ComputeChildClient {
    /// Build a client for `base_url` (e.g. `http://127.0.0.1:54321`).
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::Inference(format!("cannot build compute client: {e}")))?;
        Ok(Self {
            client,
            base_url: base_url.into(),
        })
    }

    /// Build a client for a localhost child on `port`.
    pub fn from_port(port: u16) -> Result<Self> {
        Self::new(format!("http://127.0.0.1:{port}"))
    }

    /// The base URL this client dials.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// One-shot completion.
    pub async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse> {
        let resp = self
            .client
            .post(format!("{}{ROUTE_COMPLETE}", self.base_url))
            .json(req)
            .send()
            .await
            .map_err(|e| Error::Inference(format!("compute child request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(error_from_response(resp).await);
        }
        resp.json::<CompletionResponse>()
            .await
            .map_err(|e| Error::Inference(format!("compute child response decode failed: {e}")))
    }

    /// Streaming completion. Reassembles the child's NDJSON body into a
    /// [`StreamFrame`] stream. If the child's stream ends WITHOUT a
    /// terminal `Finish`/`Error` frame (e.g. the process died mid-stream),
    /// a synthetic terminal `StreamFrame::Error` is appended so the
    /// consumer never hangs.
    pub async fn complete_stream_frames(
        &self,
        req: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        let resp = self
            .client
            .post(format!("{}{ROUTE_COMPLETE_STREAM}", self.base_url))
            .json(req)
            .send()
            .await
            .map_err(|e| Error::Inference(format!("compute child stream request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(error_from_response(resp).await);
        }

        let (tx, rx) = mpsc::channel::<StreamFrame>(32);
        tokio::spawn(async move {
            let mut byte_stream = resp.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            let mut saw_terminal = false;
            'outer: while let Some(chunk) = byte_stream.next().await {
                match chunk {
                    Ok(bytes) => {
                        buf.extend_from_slice(&bytes);
                        while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                            let line: Vec<u8> = buf.drain(..=nl).collect();
                            let line = &line[..line.len().saturating_sub(1)];
                            if let Some(frame) = decode_line(line) {
                                let terminal = is_terminal(&frame);
                                if tx.send(frame).await.is_err() {
                                    // Receiver dropped → caller cancelled.
                                    return;
                                }
                                if terminal {
                                    saw_terminal = true;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(StreamFrame::Error(format!(
                                "compute child stream transport error: {e}"
                            )))
                            .await;
                        saw_terminal = true;
                        break 'outer;
                    }
                }
            }
            // Flush a trailing line without a newline.
            if !saw_terminal && !buf.is_empty() {
                if let Some(frame) = decode_line(&buf) {
                    let terminal = is_terminal(&frame);
                    let _ = tx.send(frame).await;
                    if terminal {
                        saw_terminal = true;
                    }
                }
            }
            if !saw_terminal {
                // Stream closed with no terminal frame: the child died
                // mid-stream. Synthesise the terminal Error so the
                // consumer sees a clean end, not a silent truncation.
                let _ = tx
                    .send(StreamFrame::Error(
                        "compute child stream ended without a terminal frame".into(),
                    ))
                    .await;
            }
        });

        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|frame| (frame, rx))
        });
        Ok(Box::pin(stream))
    }

    /// Embed a single text, document- or query-side.
    pub async fn embed(&self, text: &str, mode: EmbedMode) -> Result<Vec<f32>> {
        let resp = self
            .client
            .post(format!("{}{ROUTE_EMBED}", self.base_url))
            .json(&EmbedRequest {
                input: text.to_string(),
                mode,
            })
            .send()
            .await
            .map_err(|e| Error::Inference(format!("compute child embed request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(error_from_response(resp).await);
        }
        let body: EmbedResponse = resp
            .json()
            .await
            .map_err(|e| Error::Inference(format!("compute child embed decode failed: {e}")))?;
        Ok(body.embedding)
    }

    /// Embed a batch of texts (document-side) in a single forward pass.
    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let resp = self
            .client
            .post(format!("{}{ROUTE_EMBED_BATCH}", self.base_url))
            .json(&EmbedBatchRequest {
                inputs: texts.to_vec(),
            })
            .send()
            .await
            .map_err(|e| {
                Error::Inference(format!("compute child embed_batch request failed: {e}"))
            })?;
        if !resp.status().is_success() {
            return Err(error_from_response(resp).await);
        }
        let body: EmbedBatchResponse = resp.json().await.map_err(|e| {
            Error::Inference(format!("compute child embed_batch decode failed: {e}"))
        })?;
        Ok(body.embeddings)
    }

    /// Score `docs` against `query` on the child's rerank kind route, which
    /// speaks the public `/v1/rerank` wire (`served_kind::RERANK`).
    pub async fn rerank(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        let resp = self
            .rerank_request(query, docs)?
            .send()
            .await
            .map_err(|e| Error::Inference(format!("compute child rerank request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(error_from_response(resp).await);
        }
        let body: RerankResponse = resp
            .json()
            .await
            .map_err(|e| Error::Inference(format!("compute child rerank decode failed: {e}")))?;
        let mut results = body.results;
        results.sort_by_key(|r| r.index);
        Ok(results.into_iter().map(|r| r.relevance_score).collect())
    }

    /// The rerank POST, bounded by [`RERANK_TIMEOUT`]: every search on a
    /// node with a rerank child waits on this call.
    fn rerank_request(&self, query: &str, docs: &[String]) -> Result<reqwest::RequestBuilder> {
        let path = served_kind::RERANK
            .route_path()
            .ok_or_else(|| Error::NotImplemented("rerank has no route".to_string()))?;
        Ok(self
            .client
            .post(format!("{}{path}", self.base_url))
            .timeout(RERANK_TIMEOUT)
            .json(&RerankRequest {
                model: String::new(),
                query: query.to_string(),
                documents: docs.to_vec(),
            }))
    }

    /// Probe readiness + identity. Both 200 (ready) and 503 (loading)
    /// carry a [`HealthInfo`] body; a short timeout keeps a wedged socket
    /// from stalling readiness polling.
    pub async fn health(&self) -> Result<HealthInfo> {
        let resp = self
            .client
            .get(format!("{}{ROUTE_HEALTH}", self.base_url))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map_err(|e| Error::Inference(format!("compute child health request failed: {e}")))?;
        resp.json::<HealthInfo>()
            .await
            .map_err(|e| Error::Inference(format!("compute child health decode failed: {e}")))
    }
}

/// Decode a single NDJSON line (bytes, newline already stripped) into a
/// frame, skipping blank/undecodable lines.
fn decode_line(line: &[u8]) -> Option<StreamFrame> {
    let s = std::str::from_utf8(line).ok()?.trim();
    if s.is_empty() {
        return None;
    }
    wire::decode_frame(s).ok()
}

fn is_terminal(frame: &StreamFrame) -> bool {
    matches!(frame, StreamFrame::Finish { .. } | StreamFrame::Error(_))
}

/// Parse a non-2xx response body into a typed [`Error`].
async fn error_from_response(resp: reqwest::Response) -> Error {
    let status = resp.status();
    match resp.json::<WireError>().await {
        Ok(w) => w.into_error(),
        Err(_) => Error::Inference(format!("compute child returned HTTP {status}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wedged rerank child cannot hold a search forever.
    #[test]
    fn the_rerank_request_is_bounded() {
        let client = ComputeChildClient::from_port(1).expect("client");
        let request = client
            .rerank_request("q", &["d".to_string()])
            .expect("rerank has a route")
            .build()
            .expect("request");
        assert_eq!(request.timeout(), Some(&RERANK_TIMEOUT));
    }
}
