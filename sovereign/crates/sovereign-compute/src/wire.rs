// SPDX-License-Identifier: AGPL-3.0-or-later
//! The native lossless wire contract between the daemon and a compute
//! child.
//!
//! # Why "native", not OpenAI-over-HTTP
//!
//! `CompletionRequest` carries ~20 sovereign-specific fields the OpenAI
//! wire cannot express — `sampling_mode`, `assistant_prefix`,
//! `cmd_prefix`, `url_allowlist`, `evidence_id_allowlist`, `lark_grammar`,
//! `structured_output`, the `oicp` envelope, the forced-choice sentinel.
//! Round-tripping through OpenAI JSON and back would silently drop them.
//! Instead the child speaks the contract types **verbatim** — `serde` in,
//! `serde` out — so grammar-constrained generation, allowlists, and every
//! other per-request knob survive the process boundary unchanged. The
//! engine consumes them exactly as it does in-process (`build_sampler`),
//! so nothing about generation behaviour differs across the seam.
//!
//! # Endpoints
//! - `POST /internal/complete` — [`CompletionRequest`] → [`CompletionResponse`]
//! - `POST /internal/complete_stream` — [`CompletionRequest`] → NDJSON of
//!   [`StreamFrame`] (one per line; MUST end with `Finish` or `Error`)
//! - `POST /internal/embed` — [`EmbedRequest`] → [`EmbedResponse`]
//! - `POST /internal/embed_batch` — [`EmbedBatchRequest`] → [`EmbedBatchResponse`]
//! - `GET /health` — [`HealthInfo`] (200 once the model is loaded, 503 while loading)

use serde::{Deserialize, Serialize};
use sovereign_contracts::{Error, Result, StreamFrame};

/// One-shot completion.
pub const ROUTE_COMPLETE: &str = "/internal/complete";
/// Streaming completion (NDJSON of [`StreamFrame`]).
pub const ROUTE_COMPLETE_STREAM: &str = "/internal/complete_stream";
/// Single-text embedding.
pub const ROUTE_EMBED: &str = "/internal/embed";
/// Batch embedding (one forward pass on the child).
pub const ROUTE_EMBED_BATCH: &str = "/internal/embed_batch";
/// Readiness + identity probe.
pub const ROUTE_HEALTH: &str = "/health";

/// `application/x-ndjson` — the streaming completion content type.
pub const NDJSON_CONTENT_TYPE: &str = "application/x-ndjson";

/// Document- vs query-side embedding (instruction-aware models prefix the
/// query differently — see `InferenceProvider::embed_query`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbedMode {
    /// Document-side embedding (`embed`).
    #[default]
    Document,
    /// Query-side embedding (`embed_query`), with the model's query prefix.
    Query,
}

/// Body of `POST /internal/embed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedRequest {
    /// Text to embed.
    pub input: String,
    /// Document (default) or query side.
    #[serde(default)]
    pub mode: EmbedMode,
}

/// Response of `POST /internal/embed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedResponse {
    /// The embedding vector.
    pub embedding: Vec<f32>,
}

/// Body of `POST /internal/embed_batch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedBatchRequest {
    /// Texts to embed in a single forward pass. Document-side.
    pub inputs: Vec<String>,
}

/// Response of `POST /internal/embed_batch` — one vector per input, in order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedBatchResponse {
    /// Embeddings, aligned index-for-index with the request `inputs`.
    pub embeddings: Vec<Vec<f32>>,
}

/// Body of `GET /health` — readiness + identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthInfo {
    /// `"ready"` (model loaded, serving) or `"loading"` (returned with 503).
    pub state: String,
    /// The child's role: `"generate"` | `"embed"` | `"mock"`.
    pub role: String,
    /// The resident model id (or `""` before load / for mock).
    pub model_id: String,
}

impl HealthInfo {
    /// `true` once the child's model is loaded and it is serving.
    pub fn is_ready(&self) -> bool {
        self.state == "ready"
    }
}

/// Error envelope returned on any non-2xx response. The `kind` is the
/// contract [`Error`] variant name so the client can reconstruct a typed
/// error; `message` is the human-readable detail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireError {
    /// The single-field wrapper mirrors OpenAI's `{ "error": { ... } }`.
    pub error: WireErrorBody,
}

/// Inner body of [`WireError`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireErrorBody {
    /// Contract [`Error`] variant name (e.g. `"inference"`, `"invalid_input"`).
    pub kind: String,
    /// Human-readable detail (the variant's `Display` payload).
    pub message: String,
}

impl WireError {
    /// Build the envelope from a contract [`Error`].
    pub fn from_error(err: &Error) -> Self {
        Self {
            error: WireErrorBody {
                kind: error_kind(err).to_string(),
                message: err.to_string(),
            },
        }
    }

    /// Reconstruct a best-effort typed [`Error`] from the envelope. Unknown
    /// kinds collapse to [`Error::Inference`] (the fault came from the
    /// inference child regardless).
    pub fn into_error(self) -> Error {
        let WireErrorBody { kind, message } = self.error;
        match kind.as_str() {
            "model_not_loaded" => Error::ModelNotLoaded(message),
            "invalid_input" => Error::InvalidInput(message),
            "not_implemented" => Error::NotImplemented(message),
            "routing" => Error::Routing(message),
            "cancelled" => Error::Cancelled,
            _ => Error::Inference(message),
        }
    }
}

/// Stable kebab/snake variant tag for a contract [`Error`], used as the
/// wire `kind`. Kept deliberately small — only the variants a compute
/// child can plausibly produce get a distinct tag; the rest map to
/// `"inference"`.
pub fn error_kind(err: &Error) -> &'static str {
    match err {
        Error::Inference(_) => "inference",
        Error::ComputeUnavailable { .. } => "compute_unavailable",
        Error::ModelNotLoaded(_) => "model_not_loaded",
        Error::Routing(_) => "routing",
        Error::InvalidInput(_) => "invalid_input",
        Error::NotImplemented(_) => "not_implemented",
        Error::Cancelled => "cancelled",
        _ => "inference",
    }
}

/// Encode one [`StreamFrame`] as a single NDJSON line (no trailing
/// newline; the caller joins with `\n`). JSON escapes any embedded
/// newlines in token text, so one object per line is always safe.
pub fn encode_frame(frame: &StreamFrame) -> Result<String> {
    Ok(serde_json::to_string(frame)?)
}

/// Decode one NDJSON line into a [`StreamFrame`]. Blank lines are a
/// protocol error (the caller should skip them before calling).
pub fn decode_frame(line: &str) -> Result<StreamFrame> {
    Ok(serde_json::from_str(line)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::{FinishReason, StreamUsage};

    #[test]
    fn embed_mode_defaults_to_document() {
        let r: EmbedRequest = serde_json::from_str(r#"{"input":"hi"}"#).unwrap();
        assert_eq!(r.mode, EmbedMode::Document);
    }

    #[test]
    fn stream_frames_ndjson_roundtrip() {
        let frames = vec![
            StreamFrame::Token("hello ".into()),
            StreamFrame::Token("world".into()),
            StreamFrame::Finish {
                reason: FinishReason::Stop,
                usage: Some(StreamUsage {
                    prompt_tokens: 3,
                    completion_tokens: 2,
                    total_tokens: 5,
                }),
            },
        ];
        let doc: String = frames
            .iter()
            .map(|f| encode_frame(f).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let back: Vec<StreamFrame> = doc.lines().map(|l| decode_frame(l).unwrap()).collect();
        assert_eq!(back.len(), 3);
        assert!(matches!(&back[0], StreamFrame::Token(t) if t == "hello "));
        assert!(matches!(
            &back[2],
            StreamFrame::Finish { reason: FinishReason::Stop, usage: Some(u) } if u.total_tokens == 5
        ));
    }

    #[test]
    fn error_kind_carries_across_the_envelope() {
        let e = Error::InvalidInput("prompt too long".into());
        let wire = WireError::from_error(&e);
        assert_eq!(wire.error.kind, "invalid_input");
        let back = wire.into_error();
        assert!(matches!(back, Error::InvalidInput(m) if m.contains("prompt too long")));
    }

    #[test]
    fn error_frame_carries_cause_unlike_finish_error() {
        // The wire uses StreamFrame::Error (not Finish{Error}) precisely
        // because FinishReason::Error drops its inner message on the wire.
        let frame = StreamFrame::Error("worker exited: signal 6".into());
        let line = encode_frame(&frame).unwrap();
        let back = decode_frame(&line).unwrap();
        assert!(matches!(back, StreamFrame::Error(m) if m == "worker exited: signal 6"));
    }
}
