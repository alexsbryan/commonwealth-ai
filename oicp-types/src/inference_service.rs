// SPDX-License-Identifier: AGPL-3.0-or-later
//! The wire vocabulary of the local inference service seam: the fill-in-the-
//! middle (FIM) inline-completion request and stream, the editing-slot status,
//! and the error a local chat call reports.
//!
//! These four were defined on `sovereign-api::state`'s `LocalInferenceService`
//! trait. They are protocol vocabulary — an OICP client asks a host for a FIM
//! completion and reads the answer — so they live in the leaf every consumer
//! already reaches rather than in the API crate the serving host may not name
//! (`sovereign/SERVING_BOUNDARY.md` "The two tiers"; domains
//! `REVIEW-build-local-inference`).

use std::pin::Pin;

use futures::Stream;
use serde::Serialize;

use crate::openai_types::StreamFrame;

/// Fill-in-the-middle (inline completion) request — the daemon-side
/// view of `POST /v1/completions` (`sovereign/docs/INLINE_COMPLETION.md`).
/// The route has already unified the dual wire shape (OpenAI legacy
/// `prompt`+`suffix` vs rich `prefix`+`suffix`) into these fields.
#[derive(Debug, Clone)]
pub struct FimCompletionRequest {
    /// Code before the cursor (the route clamps nothing — the service
    /// keeps the TAIL beyond its configured `max_prefix_chars`).
    pub prefix: String,
    /// Code after the cursor (service keeps the HEAD).
    pub suffix: String,
    /// File path for language detection + debug echo.
    pub path: Option<String>,
    /// Explicit language id; the service falls back to an extension
    /// table over `path` when absent.
    pub language: Option<String>,
    /// Per-request generation cap override; `None` = slot default.
    pub max_tokens: Option<usize>,
    /// Per-request temperature override; `None` = slot default.
    pub temperature: Option<f32>,
    /// Client-supplied extra stop strings (unioned with the family's).
    pub stop: Vec<String>,
    /// Opt-in glassbox: terminal frame carries `sovereign_debug`.
    pub debug: bool,
    /// Pre-assembled raw prompt (daemon-internal, never on the wire).
    /// When set, the adapter skips prefix/suffix clamping and FIM
    /// assembly, tokenizes this string verbatim (`PromptShape::Raw`),
    /// and disables structural stop-craft — stop strings and EOG
    /// only. The next-edit model lane uses this for completion-style
    /// edit models (Zeta 2.x, Sweep) whose prompts it builds itself.
    pub raw_prompt: Option<String>,
}

/// A started FIM stream plus the static metadata the route needs for
/// response envelopes and the debug payload.
pub struct FimStreamStart {
    /// Token frames + terminal `Finish`/`Error`; may carry a
    /// `StreamFrame::Debug` frame immediately before the terminal one.
    pub stream: Pin<Box<dyn Stream<Item = StreamFrame> + Send>>,
    /// Model id that served the request (echoed in the response envelope).
    pub model_id: String,
    /// Slot that served: `"fim"` (dedicated) or `"fast"` (alias mode).
    pub slot: String,
    /// Detected marker family (`"qwen_coder"` / `"starcoder2"`).
    pub fim_style: String,
}

/// Static editing-slot description for `/status.inference.edit`.
/// `None` from `edit_status()` means no editing model at all.
///
/// Mirrors `sovereign_core::types::EditSlotInfo` across the seam. The
/// translation is `sovereign_mesh::fim_adapter::edit_status`, and it is the
/// only one.
///
/// **The two lanes are independent `Option`s.** A field is `None`
/// exactly when the slot cannot serve that lane, so a client decides
/// "can I use FIM here?" by testing `fim_style`, never by pattern-
/// matching a model name. The ordinary chat-model arrangement is
/// `next_edit_format: Some(_)`, `fim_style: None`.
#[derive(Debug, Clone, Serialize)]
pub struct EditSlotStatus {
    /// Slot serving (`"edit"` for a dedicated pinned extra, else the
    /// fast slot's name).
    pub slot: String,
    /// Advertised model id (gguf file stem).
    pub model_id: String,
    /// True when served from the shared fast slot (lean mode).
    pub aliased_to_fast: bool,
    /// True when next-edit is served by the resident chat model
    /// because no `[models.edit]` was configured — working, but not
    /// what a specialist would give. Drives the nudge in `advice`.
    pub degraded: bool,
    /// Next-edit dialect (`"region_instruct"` / `"zeta2"` /
    /// `"sweep"`), or `None` when the next-edit lane is not served.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_edit_format: Option<String>,
    /// FIM marker family (`"qwen_coder"`, `"mellum"`, …), or `None`
    /// when this model's vocab carries no FIM markers — in which case
    /// `POST /v1/completions` 503s and next-edit is unaffected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fim_style: Option<String>,
    /// One operator-facing next step, or `None` when the arrangement
    /// is already what it should be. Composed in exactly one place so
    /// `doctor`, `svrn status`, the desktop and the editor extension
    /// cannot each invent their own wording.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advice: Option<String>,
}

/// Why a local chat call failed.
///
/// A shed is backpressure, not a fault: the slot refused BEFORE parking
/// this caller because the predicted wait exceeded the bound, so the
/// route can answer `503` + `Retry-After`, while everything else is a
/// genuine backend failure. Collapsing the two — which is what a bare
/// `String` did — makes "busy, retry in 35s" indistinguishable from a
/// crash at the only place that distinction matters, the client.
#[derive(Debug, Clone)]
pub enum LocalInferenceError {
    /// The slot refused BEFORE parking this caller: predicted wait
    /// exceeded the bound. Fields mirror
    /// `sovereign_contracts::Error::QueueShed`, which is where the
    /// decision is actually made — this is its wire-facing shape.
    Shed {
        /// 1-based place this caller would have taken in line.
        position: u32,
        /// Predicted wait, from observed turn durations on this slot.
        predicted_wait_ms: u64,
        /// Hint for `Retry-After`; always >= 1.
        retry_after_secs: u64,
    },
    /// Any other backend failure. Renders as `backend_error`.
    Other(String),
}

impl std::fmt::Display for LocalInferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Keep the prose shape the old `String` had: existing call
            // sites log this with `%e` and their messages stay readable.
            Self::Shed {
                position,
                predicted_wait_ms,
                retry_after_secs,
            } => write!(
                f,
                "host busy: ~{predicted_wait_ms} ms predicted wait at queue \
                 position {position}; retry after {retry_after_secs}s"
            ),
            Self::Other(msg) => f.write_str(msg),
        }
    }
}

impl From<String> for LocalInferenceError {
    fn from(msg: String) -> Self {
        Self::Other(msg)
    }
}

impl From<&str> for LocalInferenceError {
    fn from(msg: &str) -> Self {
        Self::Other(msg.to_string())
    }
}
