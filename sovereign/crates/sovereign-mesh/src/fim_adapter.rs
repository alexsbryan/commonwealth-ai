// SPDX-License-Identifier: AGPL-3.0-or-later
//! FIM inline-completion adapter — the seam implementation behind
//! `LocalInferenceService::fim_completion_stream`
//! (`sovereign/docs/INLINE_COMPLETION.md`, decisions D3/D6).
//!
//! Pipeline per request: gate on `fim_slot_info()` (None → the 503
//! message the route surfaces verbatim, with the `[models.fim]` fix)
//! → clamp prefix (TAIL kept) / suffix (HEAD kept) to the slot's
//! configured windows → `build_fim_prompt` (PSM) → a Raw-shaped
//! `CompletionRequest` routed to the FIM slot by `model_id` →
//! `complete_stream_with_finish` → the pure [`FimStopTracker`]
//! combinator (marker stops in F0; full stop-craft in F1) → wire
//! frames plus an out-of-band `Debug` frame carrying the glassbox
//! payload.
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
use commonwealth_api::state::{FimCompletionRequest, FimSlotStatus, FimStreamStart};
use futures::StreamExt;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, PromptShape, SamplingMode, StreamFrame};
use sovereign_inference::fim::{decide_mode, Feed, FimStopTracker, StopOutcome};

use crate::inference_adapter::{translate_finish_reason, translate_stream_usage};

/// 503 message when FIM isn't installed — carries the exact config
/// fix so a first-time setup never needs to read daemon logs.
const FIM_NOT_CONFIGURED: &str = "FIM is not configured on this daemon. \
     Add to ~/.sovereign/config.toml:\n\
     \n\
     [models.fim]\n\
     path = \"/path/to/Qwen2.5-Coder-1.5B.gguf\"\n\
     \n\
     then restart the daemon (`sovereign daemon restart`). The model's \
     tokenizer must carry FIM markers — Mellum2 (JetBrains) or \
     Qwen2.5-Coder are known-good.";

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

/// Static FIM slot description for `/status.inference.fim`.
pub(crate) fn fim_status(provider: &Arc<dyn InferenceProvider>) -> Option<FimSlotStatus> {
    provider.fim_slot_info().map(|info| FimSlotStatus {
        slot: info.slot,
        model_id: info.model_id,
        fim_style: info.fim_style.as_str().to_string(),
        aliased_to_fast: info.aliased_to_fast,
    })
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
        .fim_slot_info()
        .ok_or_else(|| FIM_NOT_CONFIGURED.to_string())?;

    // Interop: some clients (JetBrains AI Assistant's "prompt schema"
    // flow) assemble the FIM string CLIENT-side and send it whole.
    // Detect a pre-assembled prompt BEFORE clamping (a tail-clamp
    // would decapitate the opening marker) — pass it through
    // verbatim; the client owns its structure, clamps and all.
    let family_prefix_marker = sovereign_inference::fim::markers_for(info.fim_style).prefix;
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
            tail_chars(&request.prefix, info.max_prefix_chars),
            head_chars(&request.suffix, info.max_suffix_chars),
        )
    };
    let prompt_chars = prefix.len() + suffix.len();
    let fim_prompt = if pre_assembled {
        prefix.to_string()
    } else {
        sovereign_inference::fim::build_fim_prompt(info.fim_style, prefix, suffix)
    };
    // Single vs multi-line is decided HERE, not by the model (§3.3):
    // the text immediately before the cursor tells us which shape
    // the completion should take. For a pre-assembled prompt the
    // cursor context is the code BETWEEN the prefix and suffix
    // markers — decide from that, and feed the embedded suffix code
    // to the tracker's duplication probe.
    let (mode, probe_suffix) = if pre_assembled {
        let markers = sovereign_inference::fim::markers_for(info.fim_style);
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

    let max_tokens = request.max_tokens.unwrap_or(info.max_tokens);
    let temperature = request.temperature.unwrap_or(info.temperature);

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
    let fim_style = info.fim_style.as_str().to_string();
    let model_for_debug = model_id.clone();
    let slot_for_debug = slot.clone();
    let style_for_debug = fim_style.clone();

    // ── Stop-craft combinator ──────────────────────────────────────
    // Pure tracker over the decoded text. `scan` returning None ends
    // the stream and drops the inner decode (receiver-drop
    // cancellation) — this is how a fired stop rule halts generation
    // mid-stream instead of letting the model run to max_tokens.
    let state = TrackState::new(FimStopTracker::new_with_extra(
        info.fim_style,
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
        CompletionResponse, FimSlotInfo, FimStyle, ProviderCapabilities, StreamUsage,
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
        fn fim_slot_info(&self) -> Option<FimSlotInfo> {
            Some(FimSlotInfo {
                slot: "fim".into(),
                model_id: "qwen-coder-1.5b".into(),
                fim_style: FimStyle::QwenCoder,
                max_tokens: 48,
                temperature: 0.2,
                max_prefix_chars: 40, // deliberately tiny: exercises tail-clamp
                max_suffix_chars: 8,  // and head-clamp
                aliased_to_fast: false,
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
        assert_eq!(debug["slot"], "fim");
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
            Ok(_) => panic!("must fail when fim_slot_info is None"),
            Err(e) => e,
        };
        assert!(err.contains("[models.fim]"), "actionable fix: {err}");
        assert!(fim_status(&provider).is_none());
    }
}
