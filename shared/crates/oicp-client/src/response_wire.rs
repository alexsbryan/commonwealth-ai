// SPDX-License-Identifier: AGPL-3.0-or-later
//! The response side of a streamed chat completion: OpenAI server-sent
//! events in, the provider's frames out.
//!
//! One reader serves the typed stream (`complete_stream_with_finish`, the
//! daemon's path) and the untyped one (`complete_stream`, bare Split
//! providers), so the two cannot disagree about the wire. They did: the
//! untyped parser read each byte chunk on its own and dropped a line split
//! across two (`projection.stream-split-line`), and both skipped an error
//! chunk or a broken connection and ended as a clean `Stop`
//! (`projection.stream-error`), where the engine ends in `Finish{Error}`.

use futures::{Stream, StreamExt};
use serde::Deserialize;
use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::types::{FinishReason, StreamFrame, StreamUsage};

use crate::UsageInfo;

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    /// Token usage. llama-server sends it on a chunk of its own with empty
    /// `choices`, after the finish chunk.
    #[serde(default)]
    usage: Option<UsageInfo>,
    /// A failure mid-stream: llama-server writes `{"error": {...}}` as one
    /// `data:` line and closes without `[DONE]` (`server-context.cpp:4294`
    /// at the battery's pinned revision).
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
    /// Set on the terminal chunk. An unknown string parses to `None`, and
    /// the stream then ends in a synthesised `Stop`.
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct StreamDelta {
    content: Option<String>,
}

/// Reads one SSE response, a byte chunk at a time.
pub(crate) struct SseReader {
    /// Bytes after the last newline: a line still arriving. Kept as bytes
    /// so a character split across chunks is decoded whole too.
    pending: Vec<u8>,
    finish: Option<FinishReason>,
    usage: Option<StreamUsage>,
    /// Why the stream failed, from an error chunk or the transport.
    error: Option<String>,
    /// `[DONE]` was read.
    done: bool,
}

impl SseReader {
    pub(crate) fn new() -> Self {
        Self {
            pending: Vec::new(),
            finish: None,
            usage: None,
            error: None,
            done: false,
        }
    }

    /// Read `bytes`: the text deltas the lines they complete carry, in order.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        while !self.ended() {
            let Some(end) = self.pending.iter().position(|b| *b == b'\n') else {
                break;
            };
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            self.line(String::from_utf8_lossy(&line).trim(), &mut out);
        }
        out
    }

    /// `[DONE]` was read or the stream failed: nothing after counts.
    pub(crate) fn ended(&self) -> bool {
        self.done || self.error.is_some()
    }

    /// The transport broke before the stream ended.
    pub(crate) fn fail(&mut self, cause: String) {
        tracing::warn!(target: "oicp_client", %cause, "remote stream: failed mid-stream");
        self.error.get_or_insert(cause);
    }

    fn line(&mut self, line: &str, out: &mut Vec<String>) {
        // A blank line ends an event; `:` opens a comment (llama-server's
        // keep-alive ping); `event:` and `id:` carry nothing read here.
        let Some(data) = line.strip_prefix("data:").map(str::trim_start) else {
            return;
        };
        if data == "[DONE]" {
            self.done = true;
            return;
        }
        let chunk: StreamChunk = match serde_json::from_str(data) {
            Ok(chunk) => chunk,
            Err(e) => {
                tracing::warn!(target: "oicp_client", error = %e, %data, "remote stream: a data line that is not a chunk, skipped");
                return;
            }
        };
        if let Some(error) = chunk.error {
            let cause = error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map_or_else(|| error.to_string(), str::to_string);
            tracing::warn!(target: "oicp_client", %cause, "remote stream: the server sent an error");
            self.error = Some(cause);
            return;
        }
        if let Some(u) = chunk.usage {
            self.usage = Some(StreamUsage {
                prompt_tokens: u.prompt_tokens as u32,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens as u32,
            });
        }
        for choice in chunk.choices {
            if let Some(text) = choice.delta.content.filter(|t| !t.is_empty()) {
                out.push(text);
            }
            if let Some(reason) = choice.finish_reason {
                tracing::debug!(finish_reason = %reason, "remote: stream - terminal finish_reason captured");
                self.finish = FinishReason::from_openai_str(&reason);
            }
        }
    }

    /// End the stream: any text still owed, then the terminal frame. A
    /// failed stream ends in `Finish{Error}`, as the engine's does.
    pub(crate) fn finish(mut self) -> (Vec<String>, StreamFrame) {
        let mut tail = Vec::new();
        if !self.ended() && !self.pending.is_empty() {
            // A last line the server did not end with a newline.
            let line = std::mem::take(&mut self.pending);
            self.line(String::from_utf8_lossy(&line).trim(), &mut tail);
        }
        let reason = match self.error {
            Some(cause) => {
                tail.clear();
                FinishReason::Error(cause)
            }
            None => self.finish.unwrap_or_else(|| {
                tracing::debug!(
                    target: "oicp_client",
                    done = self.done,
                    "remote stream: no finish_reason on the wire; reporting stop"
                );
                FinishReason::Stop
            }),
        };
        let terminal = StreamFrame::Finish {
            reason,
            usage: self.usage,
        };
        (tail, terminal)
    }
}

/// The untyped stream: each byte chunk's text as one item, and a failed
/// stream as a final `Err`.
pub(crate) fn text_stream<B, E>(
    bytes: impl Stream<Item = std::result::Result<B, E>> + Send + Unpin + 'static,
) -> impl Stream<Item = Result<String>> + Send + 'static
where
    B: AsRef<[u8]>,
    E: std::fmt::Display,
{
    futures::stream::unfold(Some((bytes, SseReader::new())), |state| async move {
        let (mut bytes, mut reader) = state?;
        while !reader.ended() {
            match bytes.next().await {
                Some(Ok(chunk)) => {
                    let text = reader.push(chunk.as_ref()).concat();
                    if !text.is_empty() {
                        return Some((vec![Ok(text)], Some((bytes, reader))));
                    }
                }
                Some(Err(e)) => reader.fail(format!("the stream broke: {e}")),
                None => break,
            }
        }
        let (tail, terminal) = reader.finish();
        let mut last: Vec<Result<String>> = tail.into_iter().map(Ok).collect();
        if let StreamFrame::Finish {
            reason: FinishReason::Error(cause),
            ..
        } = terminal
        {
            last.push(Err(Error::Inference(format!("remote stream: {cause}"))));
        }
        Some((last, None))
    })
    .flat_map(futures::stream::iter)
}

#[cfg(test)]
mod tests;
