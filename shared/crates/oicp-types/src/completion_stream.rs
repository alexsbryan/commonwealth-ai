// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stream framing: the typed `finish_reason` and the frames a completion
//! stream carries (moved whole from `completion.rs`, re-exported there).

use serde::{Deserialize, Serialize};

// ─── Stream framing (typed finish_reason) ──────────────────────
//
// The OpenAI streaming surface ends each stream with a chunk
// carrying `finish_reason` ∈ {"stop","length","tool_calls",
// "content_filter"}. The legacy `Stream<Item = Result<String>>`
// shape on `InferenceProvider::complete_stream` could not carry
// that signal — every truncation looked identical to a clean stop
// at the wire, so the desktop saw a clipped reply with no way to
// tell whether the model actually finished or hit its budget.
//
// `complete_stream_with_finish` (new on `InferenceProvider`,
// optional override) yields `StreamFrame` instead, with a
// terminal `Finish { reason, usage }` frame. Providers that want
// accurate semantics override; the default impl wraps the legacy
// stream and synthesises `Stop`.

/// Why a stream stopped emitting tokens. Mirrors the OpenAI
/// `finish_reason` enum plus a `Cancelled` variant for our
/// CancellationToken / receiver-drop paths and a free-form
/// `Error` variant for provider-side faults.
///
/// Serialization uses the OpenAI-compatible lowercase string
/// (`"stop"` / `"length"` / `"tool_calls"` / `"content_filter"` /
/// `"cancelled"` / `"error"`) rather than the Rust enum-variant
/// shape. Frontend code, persisted message metadata, and SSE wire
/// frames all consume this string form; using the derive default
/// (`"Stop"`, `"Length"`) would silently break the desktop chip
/// renderer + any legacy message persisted under the original
/// `Option<String>` shape on `ResponseProvenance.finish_reason`.
/// `Error(msg)` collapses to `"error"` on the wire — the inner
/// message is internal-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    /// Model emitted EOS (or an equivalent end-of-generation token).
    Stop,
    /// Generation hit `max_tokens`.
    Length,
    /// Model produced a tool-call payload. Reserved — streaming +
    /// tools is not currently supported on this surface, but the
    /// variant exists so callers don't have to guess.
    ToolCalls,
    /// Content filter or safety layer blocked the output.
    ContentFilter,
    /// Cancellation token tripped or the receiver was dropped.
    Cancelled,
    /// Provider-side error mid-stream. The string is the
    /// human-readable cause; the SSE bridge maps this to a wire
    /// `finish_reason: "error"`.
    Error(String),
}

impl FinishReason {
    /// OpenAI-compatible string for the SSE `finish_reason` field.
    /// `Cancelled` and `Error` are not OpenAI-native; we surface
    /// `"cancelled"` and `"error"` so clients that need to
    /// distinguish them can, while OpenAI-strict clients can treat
    /// any non-`"stop"` value as truncation.
    pub const fn as_openai_str(&self) -> &'static str {
        match self {
            FinishReason::Stop => "stop",
            FinishReason::Length => "length",
            FinishReason::ToolCalls => "tool_calls",
            FinishReason::ContentFilter => "content_filter",
            FinishReason::Cancelled => "cancelled",
            FinishReason::Error(_) => "error",
        }
    }

    /// Parse the OpenAI wire string back into the typed enum. Used
    /// when decoding peer-emitted SSE chunks and when deserializing
    /// legacy persisted message metadata (originally shipped as
    /// `Option<String>` on `ResponseProvenance.finish_reason`).
    /// Unknown strings round-trip to `None` so callers can decide
    /// whether to treat them as `Stop` (lenient) or surface the bug.
    pub fn from_openai_str(s: &str) -> Option<Self> {
        match s {
            "stop" => Some(FinishReason::Stop),
            "length" => Some(FinishReason::Length),
            "tool_calls" => Some(FinishReason::ToolCalls),
            "content_filter" => Some(FinishReason::ContentFilter),
            "cancelled" => Some(FinishReason::Cancelled),
            // Inner message is lost on the wire — recover the variant
            // with an empty payload. Callers that need the message
            // get it from the surrounding tracing event.
            "error" => Some(FinishReason::Error(String::new())),
            _ => None,
        }
    }
}

impl Serialize for FinishReason {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_openai_str())
    }
}

impl<'de> Deserialize<'de> for FinishReason {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s: String = Deserialize::deserialize(d)?;
        FinishReason::from_openai_str(&s).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown OpenAI finish_reason: {s:?} (expected one of \
                 stop/length/tool_calls/content_filter/cancelled/error)"
            ))
        })
    }
}

/// Token-usage counters carried on the terminal stream frame.
/// Mirrors the OpenAI `usage` object so the SSE bridge can emit a
/// matching final chunk without a second source of truth.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamUsage {
    /// Tokens in the rendered prompt.
    pub prompt_tokens: u32,
    /// Tokens generated (excludes prompt).
    pub completion_tokens: u32,
    /// Prompt + completion.
    pub total_tokens: u32,
}

/// One frame on a typed completion stream. Replaces the legacy
/// `Result<String, Error>` items: `Token` carries a partial text
/// delta, `Finish` is the terminal frame with the reason we
/// stopped, and `Error` lets a provider abort mid-stream without
/// ambiguity. Streams MUST end with either `Finish` or `Error`;
/// receivers treat a closed channel without a terminal frame as
/// `Cancelled`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamFrame {
    /// A partial text delta.
    Token(String),
    /// Terminal frame: why the stream stopped. Every stream must end with `Finish` or `Error`.
    Finish {
        /// Why generation stopped.
        reason: FinishReason,
        /// Token counters, when the provider tracks them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<StreamUsage>,
    },
    /// Provider-side abort mid-stream (the string is the cause). Also terminal.
    Error(String),
}
