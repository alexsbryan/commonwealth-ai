// SPDX-License-Identifier: AGPL-3.0-or-later
//! Test doubles of the contract's ports, behind `test-fixtures`.
//!
//! Moved from sovereign-daemon's tests/main/common (pb-serve-ranks-tests-helpers)
//! so a crate's tests can hold the double without linking the daemon.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{Error, Result as SovResult};
use crate::traits::InferenceProvider;
use crate::types::{
    CompletionRequest, CompletionResponse, ProviderCapabilities, Speed, StreamFrame,
};

// ── Configurable InferenceProvider stub ─────────────────────────

/// A builder-style `InferenceProvider` used across integration tests.
///
/// Defaults to "every method returns `NotImplemented`". Tests opt
/// into specific behaviors via `with_*` builder methods. The intent
/// is to make the per-test code expressive about what the stub
/// supports — a test that never exercises `embed` doesn't have to
/// configure it, and a future regression that starts calling it
/// surfaces as a `NotImplemented` error rather than silent success.
///
/// Replaces the per-file `LocalStub` / `StubProvider` / `EmbedStub`
/// / `NoopProvider` / `ManifestProvider` / `FixedFinishProvider` /
/// `LegacyStreamProvider` copies that accumulated as the test suite
/// grew. ARCH §10.3's "four or more" threshold for trait extraction
/// is exceeded; this is that extraction.
pub struct TestProvider {
    model_id: String,
    code_model_id: Option<String>,
    complete_text: Option<String>,
    stream_chunks: Option<Vec<String>>,
    /// Wall-clock delay before each streamed chunk. Lets a test hold a turn
    /// open long enough to assert on what the host does WHILE one is running
    /// — the receive loop staying responsive, principally.
    stream_delay: Option<std::time::Duration>,
    embed_fn: Option<Arc<dyn Fn(&str) -> Vec<f32> + Send + Sync>>,
    /// When set, `complete_stream_with_finish` returns exactly these
    /// frames. Use to test finish_reason wire fidelity (Length,
    /// ContentFilter, etc.). When None, the trait's default impl
    /// wraps `complete_stream` and appends a synthetic Stop.
    typed_frames: Option<Vec<StreamFrame>>,
    /// Fires while a generation is genuinely in flight. See
    /// [`TestProvider::with_on_complete`].
    on_complete: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Every request `complete` / `complete_stream` received. See
    /// [`TestProvider::with_request_log`].
    request_log: Option<Arc<std::sync::Mutex<Vec<CompletionRequest>>>>,
    /// Capabilities reported to manifest synthesis. The test rarely
    /// inspects this beyond a sanity check; defaults are conservative.
    capabilities: ProviderCapabilities,
}

impl TestProvider {
    pub fn new() -> Self {
        Self {
            model_id: "test-provider".into(),
            code_model_id: None,
            complete_text: None,
            stream_chunks: None,
            stream_delay: None,
            embed_fn: None,
            typed_frames: None,
            on_complete: None,
            request_log: None,
            capabilities: ProviderCapabilities {
                max_context_tokens: 4_096,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: crate::types::Depth::Moderate,
            },
        }
    }

    pub fn with_model_id(mut self, id: impl Into<String>) -> Self {
        self.model_id = id.into();
        self
    }

    pub fn with_code_model_id(mut self, id: impl Into<String>) -> Self {
        self.code_model_id = Some(id.into());
        self
    }

    /// `complete()` returns a `CompletionResponse` carrying this text.
    pub fn with_complete_text(mut self, text: impl Into<String>) -> Self {
        self.complete_text = Some(text.into());
        self
    }

    /// `complete_stream()` (legacy `Result<String>` surface) yields
    /// these chunks in order. The default-impl
    /// `complete_stream_with_finish` then wraps them with a synthetic
    /// terminal `Stop`. To override the terminal frame, use
    /// [`Self::with_typed_frames`].
    pub fn with_stream_chunks(mut self, chunks: Vec<String>) -> Self {
        self.stream_chunks = Some(chunks);
        self
    }

    /// Sleep this long before each chunk, so a turn takes a knowable amount of
    /// wall clock. Used to make "while a turn is in flight" a testable window
    /// rather than a race.
    pub fn with_stream_delay(mut self, d: std::time::Duration) -> Self {
        self.stream_delay = Some(d);
        self
    }

    /// `embed(input)` runs this closure on the input and returns the
    /// resulting vector. Tests that want a marker-encoded vector
    /// (e.g. `|input| vec![input.len() as f32; 8]`) pass a closure;
    /// tests that just want a zero vector pass `|_| vec![0.0; N]`.
    pub fn with_embed_marker(
        mut self,
        f: impl Fn(&str) -> Vec<f32> + Send + Sync + 'static,
    ) -> Self {
        self.embed_fn = Some(Arc::new(f));
        self
    }

    /// `complete_stream_with_finish()` yields these typed frames.
    /// Use the `StreamFrame::Finish { reason, .. }` variant to pin
    /// non-Stop finish reasons.
    pub fn with_typed_frames(mut self, frames: Vec<StreamFrame>) -> Self {
        self.typed_frames = Some(frames);
        self
    }

    /// Observation hook, fired at the top of `complete` and
    /// `complete_stream` — i.e. while a generation is genuinely in
    /// flight on this provider.
    ///
    /// Exists because some process state is only observable *during*
    /// the serve: an RAII guard that bumps a counter on the way in and
    /// drops it on the way out leaves nothing to assert on once the
    /// response has been returned. A caller that samples such a
    /// counter after `send().await` cannot distinguish "never
    /// incremented" from "incremented and correctly released".
    ///
    /// Caveat: `complete_stream_with_finish` reaches the hook only on
    /// the path that delegates to `complete_stream`. When
    /// [`Self::with_typed_frames`] is set it returns those frames
    /// directly and no generation entry point runs, so the hook does
    /// not fire.
    pub fn with_on_complete(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_complete = Some(Arc::new(f));
        self
    }

    /// Append every request `complete` / `complete_stream` receives to `log`,
    /// so a test asserts on what actually reached the model call.
    pub fn with_request_log(mut self, log: Arc<std::sync::Mutex<Vec<CompletionRequest>>>) -> Self {
        self.request_log = Some(log);
        self
    }

    fn fire_on_complete(&self, req: &CompletionRequest) {
        if let Some(log) = self.request_log.as_ref() {
            log.lock().unwrap().push(req.clone());
        }
        if let Some(f) = self.on_complete.as_ref() {
            f();
        }
    }
}

impl Default for TestProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InferenceProvider for TestProvider {
    async fn complete(&self, req: &CompletionRequest) -> SovResult<CompletionResponse> {
        self.fire_on_complete(req);
        match self.complete_text.as_ref() {
            Some(t) => Ok(CompletionResponse {
                text: t.clone(),
                tokens_used: 1,
                prompt_tokens: 1,
                model_id: self.model_id.clone(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            }),
            None => Err(Error::NotImplemented(
                "TestProvider::complete not configured — \
                 call .with_complete_text(...) on the builder"
                    .into(),
            )),
        }
    }

    async fn complete_stream(
        &self,
        req: &CompletionRequest,
    ) -> SovResult<Pin<Box<dyn Stream<Item = SovResult<String>> + Send>>> {
        self.fire_on_complete(req);
        match self.stream_chunks.as_ref() {
            Some(chunks) => {
                let delay = self.stream_delay;
                let items: Vec<String> = chunks.clone();
                Ok(Box::pin(futures::StreamExt::then(
                    futures::stream::iter(items),
                    move |c| async move {
                        if let Some(d) = delay {
                            tokio::time::sleep(d).await;
                        }
                        Ok(c)
                    },
                )))
            }
            None => Err(Error::NotImplemented(
                "TestProvider::complete_stream not configured — \
                 call .with_stream_chunks(...) on the builder"
                    .into(),
            )),
        }
    }

    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> SovResult<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        if let Some(frames) = self.typed_frames.as_ref() {
            return Ok(Box::pin(futures::stream::iter(frames.clone())));
        }
        // Reproduce the trait's default impl inline — we can't
        // dispatch to it without infinite recursion. Wraps
        // `complete_stream` with `Token(text)` frames and appends
        // a synthetic terminal `Stop` (unless the body already
        // emitted an `Error` terminator). Matches the documented
        // behaviour of `InferenceProvider::complete_stream_with_finish`'s
        // default impl in `sovereign-contracts::traits`.
        use futures::StreamExt;
        use std::sync::atomic::{AtomicBool, Ordering};

        let inner = self.complete_stream(request).await?;
        let terminal_emitted = Arc::new(AtomicBool::new(false));
        let body_flag = Arc::clone(&terminal_emitted);
        let mapped = inner.flat_map(move |item| {
            let frames: Vec<StreamFrame> = match item {
                Ok(text) => vec![StreamFrame::Token(text)],
                Err(e) => {
                    body_flag.store(true, Ordering::Relaxed);
                    vec![StreamFrame::Finish {
                        reason: crate::types::FinishReason::Error(format!("{e}")),
                        usage: None,
                    }]
                }
            };
            futures::stream::iter(frames)
        });
        let tail_flag = terminal_emitted;
        let tail = futures::stream::once(async move {
            if tail_flag.load(Ordering::Relaxed) {
                None
            } else {
                Some(StreamFrame::Finish {
                    reason: crate::types::FinishReason::Stop,
                    usage: None,
                })
            }
        })
        .filter_map(|f| async move { f });
        Ok(Box::pin(mapped.chain(tail)))
    }

    async fn embed(&self, input: &str) -> SovResult<Vec<f32>> {
        match self.embed_fn.as_ref() {
            Some(f) => Ok(f(input)),
            None => Err(Error::NotImplemented(
                "TestProvider::embed not configured — \
                 call .with_embed_marker(...) on the builder"
                    .into(),
            )),
        }
    }

    fn model_id_for(&self, _speed: Speed) -> String {
        self.model_id.clone()
    }

    fn code_model_id(&self) -> Option<String> {
        self.code_model_id.clone()
    }

    /// Report the configured models as resident slots.
    ///
    /// `build_self_manifest` reads this to decide whether the node holds
    /// anything at all: an empty report means "forwards to a remote, owns no
    /// weights" and advertises nothing, which is how a `terminal`-class daemon
    /// and the attach-mode desktop avoid claiming their entry node's model as
    /// their own. A `TestProvider` stands in for a node that DOES hold its
    /// models, so it has to say so — inheriting the empty default would model a
    /// thin client while every other method claims to serve.
    fn resident_slots(&self) -> Vec<crate::traits::ResidentSlot> {
        let slot = |role: &str, model_id: String| crate::traits::ResidentSlot {
            role: role.to_string(),
            model_id,
            resident: true,
            size_bytes: None,
            transitioning: false,
            placement: None,
        };
        let mut slots = vec![slot("primary", self.model_id.clone())];
        if let Some(code) = &self.code_model_id {
            slots.push(slot("code", code.clone()));
        }
        slots
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }
}
