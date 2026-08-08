// SPDX-License-Identifier: AGPL-3.0-or-later
//! FIM inline-completion adapter — the seam implementation behind
//! `LocalInferenceService::fim_completion_stream`
//! (`sovereign/docs/INLINE_COMPLETION.md`, decisions D3/D6).
//!
//! Pipeline per request: gate on `edit_slot_info()` **and on its FIM
//! lane** (either missing → the 503 message the route surfaces
//! verbatim, carrying the config fix) → clamp prefix (TAIL kept) /
//! suffix (HEAD kept) to the lane's configured windows →
//! `build_fim_prompt` (PSM) → a Raw-shaped `CompletionRequest` routed
//! to the editing slot by `model_id` → `complete_stream_with_finish`
//! → the pure [`FimStopTracker`] combinator (marker stops in F0; full
//! stop-craft in F1) → wire frames plus an out-of-band `Debug` frame
//! carrying the glassbox payload.
//!
//! **The gate is two-stage on purpose.** An editing slot can exist and
//! serve next-edit while carrying no FIM markers at all — that is the
//! ordinary arrangement for a general chat model. Gating only on the
//! slot's existence would let such a request reach `markers_for`, which
//! would assemble a prompt with the wrong marker convention and return
//! confident garbage instead of an error (ARCH §18.3: absence is
//! reported, never defaulted).
//!
//! Cancellation: when the tracker fires we emit the terminal frames
//! and return `None` from the `scan` on the next poll, which DROPS
//! the inner stream — receiver-drop cancels the decode
//! (`model_slot.rs` convention). A client disconnect drops this whole
//! combinator with the same effect. Stop craft lives here, NOT in
//! the shared decode loop — zero changes to chat-path generation.

use std::sync::Arc;
use std::time::Instant;

use commonwealth_api::openai_types::{self as wire};
use commonwealth_api::state::{EditSlotStatus, FimCompletionRequest, FimStreamStart};
use futures::StreamExt;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, EditSlotInfo, PromptShape, SamplingMode, StreamFrame,
};
use sovereign_inference::fim::{decide_mode, Feed, FimMode, FimStopTracker, StopOutcome};

use crate::inference_adapter::{translate_finish_reason, translate_stream_usage};

/// 503 message when no editing model exists at all — carries the exact
/// config fix so a first-time setup never needs to read daemon logs.
const FIM_NOT_CONFIGURED: &str = "FIM is not configured on this daemon. \
     Add to ~/.sovereign/config.toml:\n\
     \n\
     [models.edit]\n\
     path = \"/path/to/Qwen2.5-Coder-1.5B.gguf\"\n\
     \n\
     then restart the daemon (`sovereign daemon restart`). The model's \
     tokenizer must carry FIM markers — Mellum2 (JetBrains) or \
     Qwen2.5-Coder are known-good.";

/// 503 message when an editing model IS serving, but cannot do FIM.
///
/// A distinct message from [`FIM_NOT_CONFIGURED`] because the user's
/// situation is genuinely different and so is the fix: something is
/// installed and working (next-edit serves), and telling them "FIM is
/// not configured" would send them to re-add config they already have.
/// Naming the real cause — this model's vocabulary has no FIM markers
/// — is the difference between a five-minute fix and an afternoon.
const FIM_NO_MARKERS: &str = "This daemon has an editing model, but it cannot serve \
     fill-in-the-middle completion: its tokenizer carries no FIM markers. \
     Next-edit suggestions (POST /v1/edit_predictions) are unaffected and \
     working.\n\
     \n\
     To enable /v1/completions, point [models.edit].path in \
     ~/.sovereign/config.toml at a coder GGUF whose tokenizer carries FIM \
     markers — Mellum2 (JetBrains) or Qwen2.5-Coder are known-good — then \
     restart the daemon (`sovereign daemon restart`).";

/// Keep the TAIL of `s` beyond `max` bytes (char-boundary safe).
/// Server-side clamp for the client prefix (decision D5).
fn tail_chars(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[s.ceil_char_boundary(s.len() - max)..]
    }
}

/// Keep the HEAD of `s` up to `max` bytes (char-boundary safe).
fn head_chars(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..s.floor_char_boundary(max)]
    }
}

/// Static editing-slot description for `/status.inference.edit`.
///
/// The one translation across the commonwealth-api seam (that crate
/// deliberately names no `sovereign_*` types — same convention as
/// `ResidentSlot`). Each lane maps to an `Option`: absent means the
/// slot cannot serve that lane, which is a reportable state rather
/// than an error.
pub(crate) fn edit_status(provider: &Arc<dyn InferenceProvider>) -> Option<EditSlotStatus> {
    provider.edit_slot_info().map(|info| {
        let advice = edit_slot_advice(&info);
        EditSlotStatus {
            slot: info.slot,
            model_id: info.model_id,
            aliased_to_fast: info.aliased_to_fast,
            degraded: info.degraded,
            next_edit_format: info.next_edit.map(|l| l.format.as_str().to_string()),
            fim_style: info.fim.map(|l| l.style.as_str().to_string()),
            advice,
        }
    })
}

/// The operator-facing next step for this arrangement, or `None` when
/// nothing is worth saying.
///
/// One decider for the nudge (ARCH §10.6): `doctor`, `svrn status`, the
/// desktop and the editor extension all render `/status`, and each
/// composing its own advice string is how three surfaces end up giving
/// three different answers to "what should I do about this".
///
/// Deliberately silent for a fully-specialised slot — advice nobody
/// needs is noise, and a status field that always has content stops
/// being read.
fn edit_slot_advice(info: &EditSlotInfo) -> Option<String> {
    match (info.degraded, info.fim.is_some()) {
        // The graceful-degradation case: suggestions work off the
        // resident chat model. Name the trade, not just the state —
        // measured 2026-08-07, a 1.5B specialist matched this quality
        // (19/30 vs 21/30 on the 60-case gen bank) at ~3x the speed.
        (true, _) => Some(
            "Next-edit is being served by the resident chat model because no \
             [models.edit] is configured. Suggestions work. A dedicated edit \
             model (~1.5 GB) returns them roughly 3x faster and adds \
             /v1/completions: set [models.edit].path in ~/.sovereign/config.toml."
                .to_string(),
        ),
        // Operator chose this model, but it cannot do FIM. Worth
        // saying once, because /v1/completions will 503 and the cause
        // is invisible from the route's perspective.
        (false, false) => Some(
            "This editing model serves next-edit but not fill-in-the-middle: its \
             tokenizer carries no FIM markers, so /v1/completions returns 503. \
             Point [models.edit].path at a coder GGUF (Mellum2, Qwen2.5-Coder) \
             if you need inline completion."
                .to_string(),
        ),
        (false, true) => None,
    }
}

/// Combinator state for the stop-craft `scan` — plain values so the
/// pipeline stays unit-testable without a model.
struct TrackState {
    tracker: FimStopTracker,
    done: bool,
    started: Instant,
    ttft_ms: Option<u64>,
    emitted_chars: usize,
    trimmed_chars: usize,
    stop_rule: Option<&'static str>,
    #[allow(dead_code)] // F1 surfaces the full outcome; F0 reports rule+trimmed.
    outcome: Option<StopOutcome>,
}

impl TrackState {
    fn new(tracker: FimStopTracker) -> Self {
        Self {
            tracker,
            done: false,
            started: Instant::now(),
            ttft_ms: None,
            emitted_chars: 0,
            trimmed_chars: 0,
            stop_rule: None,
            outcome: None,
        }
    }

    fn note_emit(&mut self, text: &str) {
        if self.ttft_ms.is_none() {
            self.ttft_ms = Some(self.started.elapsed().as_millis() as u64);
        }
        self.emitted_chars += text.chars().count();
    }

    /// The `sovereign_debug` payload (INLINE_COMPLETION.md §6).
    fn debug_payload(
        &self,
        model_id: &str,
        slot: &str,
        fim_style: &str,
        mode: sovereign_inference::fim::FimMode,
        prompt_chars: usize,
        finish_reason: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "model_id": model_id,
            "slot": slot,
            "fim_style": fim_style,
            "mode": mode.as_str(),
            "prompt_chars": prompt_chars,
            "emitted_chars": self.emitted_chars,
            "stop_rule": self.stop_rule.unwrap_or("none"),
            "trimmed_chars": self.trimmed_chars,
            "finish_reason": finish_reason,
            "timings_ms": {
                "ttft": self.ttft_ms.unwrap_or(0),
                "total": self.started.elapsed().as_millis() as u64,
            },
        })
    }
}

/// The seam implementation. See module docs for the pipeline.
pub(crate) async fn fim_completion_stream(
    provider: &Arc<dyn InferenceProvider>,
    request: FimCompletionRequest,
) -> Result<FimStreamStart, String> {
    let info = provider
        .edit_slot_info()
        .ok_or_else(|| FIM_NOT_CONFIGURED.to_string())?;
    // Second stage: the slot exists, but can it do FIM? Everything
    // below reads the LANE, never the slot, so there is no path on
    // which a marker-less model reaches `build_fim_prompt`.
    let lane = info
        .fim
        .as_ref()
        .ok_or_else(|| FIM_NO_MARKERS.to_string())?;

    // Daemon-internal raw prompt (next-edit model lane): the caller
    // built the whole prompt for a completion-style edit model and
    // owns its contract — no clamping, no FIM assembly, and Verbatim
    // mode so no structural stop rule can truncate a region rewrite.
    let (fim_prompt, prompt_chars, mode, probe_suffix) = if let Some(raw) = &request.raw_prompt {
        tracing::info!(
            target: "fim",
            prompt_chars = raw.len(),
            "fim: raw prompt from internal caller — verbatim, stop-strings only"
        );
        (raw.clone(), raw.len(), FimMode::Verbatim, "")
    } else {
        // Interop: some clients (JetBrains AI Assistant's "prompt schema"
        // flow) assemble the FIM string CLIENT-side and send it whole.
        // Detect a pre-assembled prompt BEFORE clamping (a tail-clamp
        // would decapitate the opening marker) — pass it through
        // verbatim; the client owns its structure, clamps and all.
        let family_prefix_marker = sovereign_inference::fim::markers_for(lane.style).prefix;
        let pre_assembled = request.prefix.starts_with(family_prefix_marker);
        let (prefix, suffix) = if pre_assembled {
            tracing::info!(
                target: "fim",
                prompt_chars = request.prefix.len(),
                "fim: pre-assembled prompt from client — passing through verbatim"
            );
            (request.prefix.as_str(), "")
        } else {
            (
                tail_chars(&request.prefix, lane.max_prefix_chars),
                head_chars(&request.suffix, lane.max_suffix_chars),
            )
        };
        let prompt_chars = prefix.len() + suffix.len();
        let fim_prompt = if pre_assembled {
            prefix.to_string()
        } else {
            sovereign_inference::fim::build_fim_prompt(lane.style, prefix, suffix)
        };
        // Single vs multi-line is decided HERE, not by the model (§3.3):
        // the text immediately before the cursor tells us which shape
        // the completion should take. For a pre-assembled prompt the
        // cursor context is the code BETWEEN the prefix and suffix
        // markers — decide from that, and feed the embedded suffix code
        // to the tracker's duplication probe.
        let (mode, probe_suffix) = if pre_assembled {
            let markers = sovereign_inference::fim::markers_for(lane.style);
            let body = prefix.strip_prefix(family_prefix_marker).unwrap_or(prefix);
            let mut parts = body.split(markers.suffix);
            let code_before = parts.next().unwrap_or(body);
            let after_suffix_marker = parts.next().unwrap_or("");
            let embedded_suffix = after_suffix_marker
                .split(markers.middle)
                .next()
                .unwrap_or("");
            (decide_mode(code_before), embedded_suffix)
        } else {
            (decide_mode(prefix), suffix)
        };
        (fim_prompt, prompt_chars, mode, probe_suffix)
    };

    let max_tokens = request.max_tokens.unwrap_or(lane.max_tokens);
    let temperature = request.temperature.unwrap_or(lane.temperature);

    let mut req = CompletionRequest::new(&fim_prompt);
    req.prompt_shape = Some(PromptShape::Raw);
    req.sampling_mode = Some(SamplingMode::Code);
    req.model_id = Some(info.model_id.clone());
    req.max_tokens = Some(max_tokens);
    req.temperature = Some(temperature);

    let inner = provider
        .complete_stream_with_finish(&req)
        .await
        .map_err(|e| format!("FIM generation failed to start: {e}"))?;

    let model_id = info.model_id.clone();
    let slot = info.slot.clone();
    let fim_style = lane.style.as_str().to_string();
    let model_for_debug = model_id.clone();
    let slot_for_debug = slot.clone();
    let style_for_debug = fim_style.clone();

    // ── Stop-craft combinator ──────────────────────────────────────
    // Pure tracker over the decoded text. `scan` returning None ends
    // the stream and drops the inner decode (receiver-drop
    // cancellation) — this is how a fired stop rule halts generation
    // mid-stream instead of letting the model run to max_tokens.
    let state = TrackState::new(FimStopTracker::new_with_extra(
        lane.style,
        request.stop.clone(),
        mode,
        probe_suffix,
    ));
    let combined = inner.scan(state, move |st, frame| {
        if st.done {
            return std::future::ready(None);
        }
        let mut out: Vec<wire::StreamFrame> = Vec::new();
        match frame {
            StreamFrame::Token(text) => match st.tracker.feed(&text) {
                Feed::Emit(safe) => {
                    if !safe.is_empty() {
                        st.note_emit(&safe);
                        out.push(wire::StreamFrame::Token(safe));
                    }
                }
                Feed::Stop { text, outcome } => {
                    st.done = true;
                    if !text.is_empty() {
                        st.note_emit(&text);
                        out.push(wire::StreamFrame::Token(text));
                    }
                    st.trimmed_chars += outcome.trimmed;
                    st.stop_rule = Some(outcome.rule.as_str());
                    st.outcome = Some(outcome);
                    out.push(wire::StreamFrame::Debug(st.debug_payload(
                        &model_for_debug,
                        &slot_for_debug,
                        &style_for_debug,
                        mode,
                        prompt_chars,
                        "stop",
                    )));
                    out.push(wire::StreamFrame::Finish {
                        reason: wire::FinishReason::Stop,
                        usage: None,
                    });
                }
            },
            StreamFrame::Finish { reason, usage } => {
                st.done = true;
                // Model hit EOS/EOG or the token budget before any
                // tracker rule fired — flush the holdback (safe now:
                // no split stop string can still complete).
                let rest = st.tracker.flush();
                if !rest.is_empty() {
                    st.note_emit(&rest);
                    out.push(wire::StreamFrame::Token(rest));
                }
                let wire_reason = translate_finish_reason(reason);
                out.push(wire::StreamFrame::Debug(st.debug_payload(
                    &model_for_debug,
                    &slot_for_debug,
                    &style_for_debug,
                    mode,
                    prompt_chars,
                    wire_reason.as_openai_str(),
                )));
                out.push(wire::StreamFrame::Finish {
                    reason: wire_reason,
                    usage: usage.map(translate_stream_usage),
                });
            }
            StreamFrame::Error(e) => {
                st.done = true;
                out.push(wire::StreamFrame::Error(e));
            }
        }
        std::future::ready(Some(out))
    });
    let flattened = combined.flat_map(futures::stream::iter);

    Ok(FimStreamStart {
        stream: Box::pin(flattened),
        model_id,
        slot,
        fim_style,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sovereign_core::error::Result as CoreResult;
    use sovereign_core::types::{
        CompletionResponse, EditSlotInfo, FimLane, FimStyle, NextEditLane, ProviderCapabilities,
        StreamUsage,
    };
    use std::pin::Pin;
    use std::sync::Mutex;

    /// Stub provider: a FIM slot plus a canned token stream whose
    /// tokens split a stop string across a boundary — the case the
    /// holdback buffer exists for.
    struct StubFimProvider {
        seen: Mutex<Option<CompletionRequest>>,
    }

    impl StubFimProvider {
        fn new() -> Self {
            Self {
                seen: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl InferenceProvider for StubFimProvider {
        async fn complete(&self, _r: &CompletionRequest) -> CoreResult<CompletionResponse> {
            unimplemented!("stream-only stub")
        }
        async fn complete_stream(
            &self,
            _r: &CompletionRequest,
        ) -> CoreResult<Pin<Box<dyn futures::Stream<Item = CoreResult<String>> + Send>>> {
            unimplemented!("with_finish-only stub")
        }
        async fn complete_stream_with_finish(
            &self,
            request: &CompletionRequest,
        ) -> CoreResult<Pin<Box<dyn futures::Stream<Item = StreamFrame> + Send>>> {
            *self.seen.lock().unwrap() = Some(request.clone());
            let frames = vec![
                StreamFrame::Token("return a ".to_string()),
                // Split marker: "<|endo" + "ftext|>" across tokens.
                StreamFrame::Token("+ b;<|endo".to_string()),
                StreamFrame::Token("ftext|>TRAILING GARBAGE".to_string()),
                StreamFrame::Finish {
                    reason: sovereign_core::types::FinishReason::Stop,
                    usage: Some(StreamUsage {
                        prompt_tokens: 12,
                        completion_tokens: 3,
                        total_tokens: 15,
                    }),
                },
            ];
            Ok(Box::pin(futures::stream::iter(frames)))
        }
        async fn embed(&self, _t: &str) -> CoreResult<Vec<f32>> {
            unimplemented!()
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: sovereign_core::types::Speed::Fast,
                relative_reasoning: sovereign_core::types::Depth::Shallow,
            }
        }
        fn edit_slot_info(&self) -> Option<EditSlotInfo> {
            Some(EditSlotInfo {
                slot: "edit".into(),
                model_id: "qwen-coder-1.5b".into(),
                aliased_to_fast: false,
                degraded: false,
                next_edit: Some(NextEditLane {
                    format: Default::default(),
                }),
                fim: Some(FimLane {
                    style: FimStyle::QwenCoder,
                    max_tokens: 48,
                    temperature: 0.2,
                    max_prefix_chars: 40, // deliberately tiny: exercises tail-clamp
                    max_suffix_chars: 8,  // and head-clamp
                }),
            })
        }
    }

    fn fim_request() -> FimCompletionRequest {
        FimCompletionRequest {
            prefix: "X".repeat(100),
            suffix: "Y".repeat(100),
            path: Some("t.py".into()),
            language: None,
            max_tokens: None,
            temperature: None,
            stop: vec![],
            debug: false,
            raw_prompt: None,
        }
    }

    #[tokio::test]
    async fn raw_request_reaches_provider_with_fim_markers_and_clamps() {
        let stub = Arc::new(StubFimProvider::new());
        let provider: Arc<dyn InferenceProvider> = stub.clone();
        let start = fim_completion_stream(&provider, fim_request())
            .await
            .expect("stream starts");
        // Drain to drive the request through.
        let frames: Vec<_> = start.stream.collect().await;
        assert!(!frames.is_empty());

        let seen = stub.seen.lock().unwrap().clone().expect("request seen");
        assert_eq!(seen.prompt_shape, Some(PromptShape::Raw));
        assert_eq!(seen.sampling_mode, Some(SamplingMode::Code));
        assert_eq!(seen.model_id.as_deref(), Some("qwen-coder-1.5b"));
        assert_eq!(seen.max_tokens, Some(48));
        // PSM assembly over CLAMPED windows: 40 prefix bytes (tail of
        // the 100-X input) + 8 suffix bytes (head of the 100-Y input).
        let expected_prefix = "X".repeat(40);
        let expected_suffix = "Y".repeat(8);
        let want =
            format!("<|fim_prefix|>{expected_prefix}<|fim_suffix|>{expected_suffix}<|fim_middle|>");
        assert_eq!(seen.prompt, want);
    }

    #[tokio::test]
    async fn stop_string_split_across_tokens_is_withheld_and_stream_terminates() {
        let provider: Arc<dyn InferenceProvider> = Arc::new(StubFimProvider::new());
        let start = fim_completion_stream(&provider, fim_request())
            .await
            .expect("stream starts");
        let frames: Vec<wire::StreamFrame> = start.stream.collect().await;

        let text: String = frames
            .iter()
            .filter_map(|f| match f {
                wire::StreamFrame::Token(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert!(
            text.contains("return a + b;"),
            "safe text should pass through: {text:?}"
        );
        assert!(
            !text.contains("<|endoftext|>") && !text.contains("GARBAGE"),
            "split stop string must never leak: {text:?}"
        );
        // Terminal Finish with reason Stop (the tracker fired, not
        // the model's own EOG — usage is None on this path).
        let terminal = frames.last().expect("terminal frame");
        assert!(
            matches!(
                terminal,
                wire::StreamFrame::Finish {
                    reason: wire::FinishReason::Stop,
                    ..
                }
            ),
            "terminal should be Finish/Stop: {terminal:?}"
        );
        // A Debug frame rides just before the terminal one, with the
        // stop_rule + timings the glassbox surface renders.
        let debug = frames.iter().find_map(|f| match f {
            wire::StreamFrame::Debug(v) => Some(v.clone()),
            _ => None,
        });
        let debug = debug.expect("debug frame present");
        assert_eq!(debug["stop_rule"], "stop_string");
        assert_eq!(debug["model_id"], "qwen-coder-1.5b");
        // The reserved dedicated slot is named "edit" since the
        // FIM/next-edit lane split; "fim" stays reserved and pinned
        // (`LEGACY_EDIT_SLOT_NAME`) but is no longer what installs.
        assert_eq!(debug["slot"], "edit");
        assert!(debug["timings_ms"]["total"].is_number());
    }

    #[tokio::test]
    async fn pre_assembled_prompt_passes_through_verbatim() {
        let stub = Arc::new(StubFimProvider::new());
        let provider: Arc<dyn InferenceProvider> = stub.clone();
        // JetBrains-style: the client assembled the FIM string itself.
        let assembled = "<|fim_prefix|>fn main() {\n<|fim_suffix|>\n}\n<|fim_middle|>";
        let req = FimCompletionRequest {
            prefix: assembled.to_string(),
            suffix: String::new(),
            path: None,
            language: None,
            max_tokens: None,
            temperature: None,
            stop: vec![],
            debug: false,
            raw_prompt: None,
        };
        let start = fim_completion_stream(&provider, req)
            .await
            .expect("stream starts");
        let frames: Vec<_> = start.stream.collect().await;
        assert!(!frames.is_empty());
        let seen = stub.seen.lock().unwrap().clone().expect("request seen");
        assert_eq!(
            seen.prompt, assembled,
            "pre-assembled prompt must pass through verbatim — no double-wrap, no clamp"
        );
        assert_eq!(seen.prompt.matches("<|fim_prefix|>").count(), 1);
    }

    #[tokio::test]
    async fn unconfigured_provider_errors_with_actionable_message() {
        struct NoFim;
        #[async_trait]
        impl InferenceProvider for NoFim {
            async fn complete(&self, _r: &CompletionRequest) -> CoreResult<CompletionResponse> {
                unimplemented!()
            }
            async fn complete_stream(
                &self,
                _r: &CompletionRequest,
            ) -> CoreResult<Pin<Box<dyn futures::Stream<Item = CoreResult<String>> + Send>>>
            {
                unimplemented!()
            }
            async fn embed(&self, _t: &str) -> CoreResult<Vec<f32>> {
                unimplemented!()
            }
            fn capabilities(&self) -> ProviderCapabilities {
                unimplemented!()
            }
        }
        let provider: Arc<dyn InferenceProvider> = Arc::new(NoFim);
        let err = match fim_completion_stream(&provider, fim_request()).await {
            Ok(_) => panic!("must fail when edit_slot_info is None"),
            Err(e) => e,
        };
        assert!(err.contains("[models.edit]"), "actionable fix: {err}");
        assert!(edit_status(&provider).is_none());
    }

    /// A provider whose editing slot serves next-edit but carries no
    /// FIM markers — the ordinary arrangement for a general chat model,
    /// and the one the graceful-degradation fallback creates.
    struct ChatOnlyEditSlot;
    #[async_trait]
    impl InferenceProvider for ChatOnlyEditSlot {
        async fn complete(&self, _r: &CompletionRequest) -> CoreResult<CompletionResponse> {
            unimplemented!()
        }
        async fn complete_stream(
            &self,
            _r: &CompletionRequest,
        ) -> CoreResult<Pin<Box<dyn futures::Stream<Item = CoreResult<String>> + Send>>> {
            unimplemented!()
        }
        async fn embed(&self, _t: &str) -> CoreResult<Vec<f32>> {
            unimplemented!()
        }
        fn capabilities(&self) -> ProviderCapabilities {
            unimplemented!()
        }
        fn edit_slot_info(&self) -> Option<EditSlotInfo> {
            Some(EditSlotInfo {
                slot: "fast".into(),
                model_id: "some-chat-model".into(),
                aliased_to_fast: true,
                degraded: true,
                next_edit: Some(NextEditLane {
                    format: Default::default(),
                }),
                fim: None,
            })
        }
    }

    /// THE failure this two-stage gate exists to prevent. Before the
    /// lane split, `/v1/completions` gated only on the slot existing —
    /// so a marker-less model reached `markers_for` and got a prompt
    /// built with a marker convention it does not speak, returning
    /// confident garbage instead of an error (ARCH §18.3).
    #[tokio::test]
    async fn chat_only_slot_refuses_fim_rather_than_guessing_markers() {
        let provider: Arc<dyn InferenceProvider> = Arc::new(ChatOnlyEditSlot);
        let err = match fim_completion_stream(&provider, fim_request()).await {
            Ok(_) => panic!("a slot with no FIM lane must not serve /v1/completions"),
            Err(e) => e,
        };
        assert!(
            err.contains("no FIM markers"),
            "must name the real cause, not 'unconfigured': {err}"
        );
        assert!(
            err.contains("Next-edit suggestions"),
            "must say next-edit still works, or the user re-adds config they already have: {err}"
        );
    }

    /// The slot is still reported — withholding a lane is not the same
    /// as having no editing model, and `/status` must be able to tell
    /// the two apart.
    #[test]
    fn chat_only_slot_is_reported_with_next_edit_lane_and_no_fim_lane() {
        let provider: Arc<dyn InferenceProvider> = Arc::new(ChatOnlyEditSlot);
        let status = edit_status(&provider).expect("an editing model exists");
        assert_eq!(status.next_edit_format.as_deref(), Some("region_instruct"));
        assert_eq!(status.fim_style, None, "no markers means no FIM lane");
        assert!(status.degraded);
    }

    /// The nudge is the whole user-facing point of graceful
    /// degradation: suggestions work, and the user learns what a
    /// specialist would buy them.
    #[test]
    fn degraded_slot_advises_installing_a_specialist() {
        let provider: Arc<dyn InferenceProvider> = Arc::new(ChatOnlyEditSlot);
        let advice = edit_status(&provider)
            .expect("an editing model exists")
            .advice
            .expect("a degraded slot must carry advice");
        assert!(
            advice.contains("[models.edit]"),
            "advice must be actionable: {advice}"
        );
        assert!(
            advice.contains("faster"),
            "advice must name the trade, not just the state: {advice}"
        );
    }

    /// Silence is the correct output for an arrangement that is already
    /// right. A status field that always has content stops being read.
    #[test]
    fn fully_specialised_slot_gets_no_advice() {
        let provider: Arc<dyn InferenceProvider> = Arc::new(StubFimProvider::new());
        let status = edit_status(&provider).expect("stub has an editing slot");
        assert_eq!(status.advice, None);
        assert!(!status.degraded);
        assert_eq!(status.fim_style.as_deref(), Some("qwen_coder"));
    }
}
