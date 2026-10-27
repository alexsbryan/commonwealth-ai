// SPDX-License-Identifier: AGPL-3.0-or-later
//! The response side of a chat completion: the host's answer read into the
//! engine's shape. A message becomes the caller's text
//! ([`ChatMessage::answer`]); server-sent events become the provider's frames
//! ([`SseReader`]).
//!
//! One reader serves the typed stream (`complete_stream_with_finish`, the
//! daemon's path) and the untyped one (`complete_stream`, bare Split
//! providers), so the two cannot disagree about the wire. They did: the
//! untyped parser read each byte chunk on its own and dropped a line split
//! across two (`projection.stream-split-line`), and both skipped an error
//! chunk or a broken connection and ended as a clean `Stop`
//! (`projection.stream-error`), where the engine ends in `Finish{Error}`.
//!
//! Reasoning: llama-server moves thinking into `reasoning_content`; the
//! engine returns it inline, opening its text with the `<think>` the prompt
//! left open. The provider contract is the engine's: the client writes
//! `reasoning_content` back as that block, and the adapter's turn parser
//! (`oicp_types::chat_turn`) is the one place that splits it out again,
//! into Reasoning frames and the wire's `reasoning_content`, with the
//! answer alone as content (`projection.reasoning`).

use futures::{Stream, StreamExt};
use serde::Deserialize;
use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::types::{CompletionRequest, FinishReason, StreamFrame, StreamUsage};

use crate::{ChatCompletionResponse, ChatMessage, StructuredOutputMode, UsageInfo};

/// Read a non-streaming chat completion off the wire and parse it.
pub(crate) async fn read_chat(response: reqwest::Response) -> Result<ChatCompletionResponse> {
    let raw = response
        .text()
        .await
        .map_err(|e| Error::Inference(format!("Failed to read API response: {e}")))?;
    serde_json::from_str(&raw)
        .map_err(|e| Error::Inference(format!("Failed to parse API response: {e}")))
}

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
    reasoning_content: Option<String>,
    /// A fragment of a native call: llama-server sends the id and name
    /// first, then the arguments in pieces, each tagged with the call's
    /// `index` (`server-chat.cpp:607`).
    #[serde(default)]
    tool_calls: Vec<ToolCallDelta>,
}

#[derive(Deserialize)]
struct ToolCallDelta {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    function: FunctionDelta,
}

#[derive(Deserialize, Default)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

/// A native call as the `<tool_call>{"name":..,"arguments":{..}}</tool_call>`
/// envelope every tool loop parses, the shape both paths re-emit native
/// calls in (`ChatMessage::as_text`). Arguments arrive JSON-encoded as a
/// string and are re-parsed into an object; an unparseable string is
/// carried as a string rather than dropped.
pub(crate) fn tool_call_envelope(name: &str, arguments: Option<&str>) -> String {
    let args: serde_json::Value = match arguments {
        Some(raw) => {
            serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
        }
        None => serde_json::json!({}),
    };
    format!(
        "<tool_call>{}</tool_call>",
        serde_json::json!({ "name": name, "arguments": args })
    )
}

/// A server's finish reason as the provider contract states it.
///
/// `tool_calls` is folded into `stop` (C1). The engine reports `stop` for a
/// turn that ended at EOS with a call in it; llama-server reports
/// `tool_calls` for exactly that case and no other (EOS or a stop word with
/// calls, `server-task.cpp:465`). At this layer a call is carried in the
/// text, and the one decider of `tool_calls` is the adapter, which derives
/// it from the parsed text for every provider (`inference_adapter.rs`, the
/// non-stream finish, and `conversation.rs` `stream_turn`). No caller of a
/// provider reads `ToolCalls` from it.
pub(crate) fn provider_finish(wire: &str) -> Option<FinishReason> {
    match FinishReason::from_openai_str(wire) {
        Some(FinishReason::ToolCalls) => {
            tracing::debug!(
                target: "oicp_client",
                "remote: finish_reason tool_calls reported as stop; the call is in the text"
            );
            Some(FinishReason::Stop)
        }
        other => other,
    }
}

/// How the client writes reasoning back into the text: the block the
/// engine's text opens with, and its close before the answer.
const THINK_OPEN: &str = "<think>\n";
const THINK_CLOSE: &str = "\n</think>\n\n";

/// Whether a reply to `request` carries its reasoning inline. A structured
/// answer does not: its text is the JSON the caller parses, and a think
/// block in front of it would break that parse.
pub(crate) fn inlines_reasoning(request: &CompletionRequest) -> bool {
    request.structured_output.is_none()
}

/// `answer` with the host's `reasoning` in front of it as the engine writes
/// it, when `request` takes reasoning inline.
pub(crate) fn with_reasoning(
    reasoning: Option<&str>,
    answer: String,
    request: &CompletionRequest,
) -> String {
    let Some(reasoning) = reasoning.filter(|r| !r.trim().is_empty()) else {
        return answer;
    };
    if !inlines_reasoning(request) {
        tracing::debug!(
            target: "oicp_client",
            reasoning_chars = reasoning.len(),
            "remote: a structured answer; the host's reasoning_content is not put in front of it"
        );
        return answer;
    }
    format!("{THINK_OPEN}{reasoning}{THINK_CLOSE}{answer}")
}

/// Make `body` a streamed request that asks for usage. llama-server, like
/// OpenAI, sends usage on a stream only when `stream_options.include_usage`
/// is set (`server-task.cpp:502`); the engine's Finish frame always carries
/// it (`projection.stream-usage`).
pub(crate) fn ask_to_stream(body: &mut serde_json::Value) {
    body["stream"] = serde_json::json!(true);
    body["stream_options"] = serde_json::json!({"include_usage": true});
}

impl ChatMessage {
    /// The assistant turn as the caller's answer. A schema that rode a
    /// function call comes back as that call's arguments, the JSON the caller
    /// asked for whichever spelling the host needed; anything else is
    /// [`ChatMessage::as_text`] with the reasoning in front of it, as the
    /// engine writes it ([`with_reasoning`]).
    pub(crate) fn answer(&self, request: &CompletionRequest, mode: StructuredOutputMode) -> String {
        let Some(schema) = request
            .structured_output
            .as_ref()
            .filter(|_| mode.via_tool())
        else {
            return self.with_reasoning(request);
        };
        let name = crate::chat_wire::schema_function_name(schema);
        let call = self.tool_calls.iter().find(|c| c.function.name == name);
        if let Some(args) = call.and_then(|c| c.function.arguments.clone()) {
            return args;
        }
        // `tool_choice: auto` lets the model answer in text. Use it, and say
        // so: the schema was offered, not enforced.
        tracing::warn!(
            target: "oicp_client",
            function = %name,
            ?mode,
            "schema offered as a function call but the model answered in text; schema not enforced on this call"
        );
        self.with_reasoning(request)
    }

    fn with_reasoning(&self, request: &CompletionRequest) -> String {
        with_reasoning(self.reasoning_content.as_deref(), self.as_text(), request)
    }

    /// Drop `prefix`, the assistant message the request ended on
    /// ([`crate::chat_wire::prefix_as_message`]), from the front of the
    /// content: the engine's text is the continuation alone. llama-server's
    /// whole message opens with it, because it parses the generation prompt,
    /// prefix included, ahead of the output (LS common/chat.cpp:3841-3843);
    /// its stream does not repeat it (LS server-task.cpp:156-159), so only
    /// this path strips.
    pub(crate) fn strip_continued(&mut self, prefix: &str) {
        let rest = self
            .content
            .as_deref()
            .and_then(|c| c.strip_prefix(prefix))
            .map(str::to_string);
        tracing::debug!(
            target: "oicp_client",
            stripped = rest.is_some(),
            "remote: the continued assistant prefix removed from the answer, when it opens it"
        );
        if rest.is_some() {
            self.content = rest;
        }
    }
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
    /// Whether reasoning is written into the text ([`inlines_reasoning`]).
    inline_reasoning: bool,
    /// A think block is open in the text written so far.
    thinking: bool,
    /// Reasoning not written, for a request that does not take it inline.
    reasoning_dropped: usize,
    /// Native calls by index: name and arguments, accumulated.
    calls: std::collections::BTreeMap<usize, (String, String)>,
    /// The text written so far is empty or ends a line.
    ends_line: bool,
}

impl SseReader {
    pub(crate) fn new(inline_reasoning: bool) -> Self {
        Self {
            pending: Vec::new(),
            finish: None,
            usage: None,
            error: None,
            done: false,
            inline_reasoning,
            thinking: false,
            reasoning_dropped: 0,
            calls: std::collections::BTreeMap::new(),
            ends_line: true,
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
            if let Some(r) = choice.delta.reasoning_content.filter(|r| !r.is_empty()) {
                self.reasoning(r, out);
            }
            if let Some(text) = choice.delta.content.filter(|t| !t.is_empty()) {
                self.ends_line = text.ends_with('\n');
                out.push(self.close_think() + &text);
            }
            for call in choice.delta.tool_calls {
                let (name, arguments) = self.calls.entry(call.index).or_default();
                name.push_str(call.function.name.as_deref().unwrap_or(""));
                arguments.push_str(call.function.arguments.as_deref().unwrap_or(""));
            }
            if let Some(reason) = choice.finish_reason {
                tracing::debug!(finish_reason = %reason, "remote: stream - terminal finish_reason captured");
                self.finish = provider_finish(&reason);
            }
        }
    }

    fn reasoning(&mut self, piece: String, out: &mut Vec<String>) {
        if !self.inline_reasoning {
            self.reasoning_dropped += piece.len();
        } else if self.thinking {
            out.push(piece);
        } else {
            self.thinking = true;
            out.push(format!("{THINK_OPEN}{piece}"));
        }
    }

    /// The close of an open think block, before what follows it.
    fn close_think(&mut self) -> String {
        if std::mem::take(&mut self.thinking) {
            THINK_CLOSE.to_string()
        } else {
            String::new()
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
        if !self.calls.is_empty() {
            // The calls whole, once no fragment can follow, as the envelope
            // the non-stream path writes (`projection.stream-tool-calls`).
            let mut text = self.close_think();
            let mut ends_line = self.ends_line || !text.is_empty();
            for (name, arguments) in self.calls.values() {
                if !ends_line {
                    text.push('\n');
                }
                text.push_str(&tool_call_envelope(name, Some(arguments)));
                ends_line = false;
            }
            tracing::debug!(
                target: "oicp_client",
                calls = self.calls.len(),
                "remote stream: native tool calls written as envelopes"
            );
            tail.push(text);
        }
        if self.reasoning_dropped > 0 {
            tracing::debug!(
                target: "oicp_client",
                reasoning_chars = self.reasoning_dropped,
                "remote stream: a structured answer; the host's reasoning_content is not put in front of it"
            );
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
    reader: SseReader,
) -> impl Stream<Item = Result<String>> + Send + 'static
where
    B: AsRef<[u8]>,
    E: std::fmt::Display,
{
    futures::stream::unfold(Some((bytes, reader)), |state| async move {
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
