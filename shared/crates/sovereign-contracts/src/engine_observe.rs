// SPDX-License-Identifier: AGPL-3.0-or-later
//! What an engine did with one request, recorded at the code that did it, for
//! the daemon's replay route (`/internal/engine/replay`).
//!
//! Production never installs a sink, so [`observe`] costs one thread-local
//! read and builds nothing. The daemon's replay route wraps one call in
//! [`observed`], and every site the call reaches records into that call's
//! sink: the prompt tokens where the engine tokenizes, the sampler where it
//! builds one, the wire body where the remote client sends it. The route
//! therefore observes the path that runs, never a copy of it.
//!
//! The sink lives in a thread-local that [`Observed`] installs around each
//! poll of the call's future, so it follows the call across awaits whichever
//! thread polls it. Work handed to another thread takes it along with
//! [`carry`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use serde::{Deserialize, Serialize};

/// An `InferenceProvider` method the daemon's replay route runs, under the
/// name `[engine] capture` records it by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayMethod {
    /// `complete`.
    Complete,
    /// `complete_batch`, replayed as one `complete`.
    CompleteBatch,
    /// `complete_stream`.
    CompleteStream,
    /// `complete_stream_with_id`.
    CompleteStreamWithId,
    /// `complete_stream_with_finish`.
    CompleteStreamWithFinish,
    /// `complete_stream_with_id_and_finish`.
    CompleteStreamWithIdAndFinish,
    /// `embed`.
    Embed,
    /// `embed_query`.
    EmbedQuery,
    /// `embed_batch`.
    EmbedBatch,
    /// `rerank_batch`.
    RerankBatch,
    /// `rerank_model_id`: the id a rerank request must name at this
    /// provider's far end. Not a captured method.
    RerankModelId,
    /// `count_tokens`.
    CountTokens,
    /// `count_tokens_exact`: the server's own count. Not a captured method.
    CountTokensExact,
    /// The provider's report about itself, read-only.
    Host,
    /// `warmup_primary`, `load_extra_slot` and `unload_extra_slot`, each
    /// called once and reported with how it ended (`host.lifecycle`, L I4).
    /// Separate from [`Self::Host`] because a provider's report about itself
    /// is read-only (ARCH 1): a status field must never be produced by a
    /// call that can block or load a model. Not a captured method.
    HostLifecycle,
    /// The provider's typed stream, cut and corrupted mid-flight: an SSE
    /// line split across two byte chunks (the line whole, as
    /// `projection.stream-split-line` asks) and a mid-stream `error` datum
    /// (which `projection.stream-error` asks ends in `Finish{Error}`). The
    /// driver's own fault injection (L I3), not a captured method.
    StreamFault,
    /// `probe_logprobs`: greedy steps with each step's top-k logprobs, the
    /// compute row's instrument. Not a captured method.
    Probe,
}

impl ReplayMethod {
    /// Whether the method streams. Every streaming method replays as the
    /// typed stream, the one the daemon serves local streams through.
    pub fn is_stream(self) -> bool {
        matches!(
            self,
            Self::CompleteStream
                | Self::CompleteStreamWithId
                | Self::CompleteStreamWithFinish
                | Self::CompleteStreamWithIdAndFinish
        )
    }

    /// Whether the input is a `CompletionRequest`.
    pub fn is_completion(self) -> bool {
        self.is_stream() || matches!(self, Self::Complete | Self::CompleteBatch)
    }
}

/// What the logprob probe decodes: `text` tokenized as llama-server's
/// `/tokenize` does with `add_special`, then `steps` greedy tokens, with the
/// `top_k` most likely tokens at each.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeRequest {
    /// The prompt, raw: no chat template.
    pub text: String,
    /// Greedy tokens to generate.
    pub steps: usize,
    /// Alternatives reported at each step.
    pub top_k: usize,
    /// Which chat model: `Speed`'s default, the fast one, unless the case
    /// says otherwise.
    #[serde(default)]
    pub speed: crate::types::Speed,
}

/// One greedy step of the probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeStep {
    /// The token greedy decoding chose.
    pub token: i64,
    /// The `top_k` most likely tokens and their log-probabilities over the
    /// whole vocabulary, most likely first.
    pub top: Vec<(i64, f64)>,
}

/// What the probe decoded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeAnswer {
    /// The prompt's token ids.
    pub prompt_ids: Vec<i64>,
    /// One entry per generated token.
    pub steps: Vec<ProbeStep>,
}

/// How the three lifecycle calls of `host.lifecycle` ended, each asked of
/// the provider once (`ReplayMethod::HostLifecycle`, L I4). The calls load
/// and unload nothing of their own: each reports how the provider's own
/// method answered, so a refusal and a success are both the evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostLifecycleAnswer {
    /// `warmup_primary`.
    pub warmup: LifecycleCall,
    /// `load_extra_slot`, if the provider was asked to load one.
    pub extra_load: Option<LifecycleCall>,
    /// `unload_extra_slot`, if the provider was asked to unload one.
    pub extra_unload: Option<LifecycleCall>,
    /// What the provider reports about residency after the calls, by the
    /// host report's keys: the router's `/models` state a remote target
    /// reads, so the row sees the state beside the call's own answer.
    #[serde(default)]
    pub host: std::collections::BTreeMap<String, serde_json::Value>,
}

impl Default for HostLifecycleAnswer {
    fn default() -> Self {
        // Not a substitute for a measurement: a default answer judges
        // could-not-judge on every key rather than reading as success
        // (ARCH 6), which is what `LifecycleCall::NotAsked` is for.
        Self {
            warmup: LifecycleCall::NotAsked,
            extra_load: None,
            extra_unload: None,
            host: Default::default(),
        }
    }
}

/// One lifecycle call, and how it ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum LifecycleCall {
    /// The call returned `Ok`. `model` is what it named, when it named one.
    Ok {
        /// The model the call loaded or unloaded, if any.
        model: Option<String>,
    },
    /// The call returned `Err`, with the provider's own message.
    Err {
        /// The error as the provider answered it.
        message: String,
    },
    /// The method was not asked (the request did not name an extra slot, or
    /// an earlier step left nothing to ask).
    NotAsked,
}

/// One thing an engine did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Observation {
    /// The token ids of the prompt as it was decoded.
    PromptTokens {
        /// Token ids.
        ids: Vec<i64>,
    },
    /// The sampler as built.
    Sampler {
        /// Parameters under llama-server's names.
        params: BTreeMap<String, f64>,
        /// Active stages in the order they run.
        order: Vec<String>,
        /// The grammar text the decode is constrained by, if any.
        grammar: Option<String>,
    },
    /// Prompt tokens evaluated for this call, and those reused from a cache.
    Prefill {
        /// Tokens decoded.
        evaluated: u64,
        /// Tokens restored or kept rather than decoded.
        reused: u64,
    },
    /// Generated token ids, in order.
    GeneratedTokens {
        /// Token ids.
        ids: Vec<i64>,
    },
    /// The body a remote client sent.
    WireRequest {
        /// The JSON body.
        body: serde_json::Value,
    },
    /// The raw body a remote client received.
    WireResponse {
        /// The HTTP status.
        status: u16,
        /// The body as text, an SSE stream included.
        body: String,
    },
}

type Sink = Arc<Mutex<Vec<Observation>>>;

thread_local! {
    static SINK: RefCell<Option<Sink>> = const { RefCell::new(None) };
}

fn current() -> Option<Sink> {
    SINK.with(|s| s.borrow().clone())
}

/// Record an observation if a sink is installed. `make` runs only then.
pub fn observe(make: impl FnOnce() -> Observation) {
    if let Some(sink) = current() {
        let observation = make();
        tracing::debug!(target: "engine_observe", ?observation, "observed");
        sink.lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(observation);
    }
}

/// Whether a sink is installed, for a site whose observation is costly to
/// assemble.
pub fn is_observing() -> bool {
    current().is_some()
}

/// Run `f` and return its output with everything observed while it ran.
pub async fn observed<F: Future>(f: F) -> (F::Output, Vec<Observation>) {
    let sink: Sink = Arc::default();
    let output = Observed {
        sink: Some(sink.clone()),
        inner: Box::pin(f),
    }
    .await;
    let observations = std::mem::take(&mut *sink.lock().unwrap_or_else(|p| p.into_inner()));
    (output, observations)
}

/// Wrap a closure that will run on another thread so it records into the
/// sink installed where it was created.
pub fn carry<R>(f: impl FnOnce() -> R) -> impl FnOnce() -> R {
    let sink = current();
    move || {
        let _guard = Installed::new(sink);
        f()
    }
}

/// Wrap a future that will be spawned as its own task so it records into the
/// sink installed where it was created.
pub fn carry_future<F: Future>(f: F) -> impl Future<Output = F::Output> {
    Observed {
        sink: current(),
        inner: Box::pin(f),
    }
}

/// Polls `inner` with `sink` installed, restoring what was there after.
struct Observed<F: Future> {
    sink: Option<Sink>,
    inner: Pin<Box<F>>,
}

impl<F: Future> Future for Observed<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let _guard = Installed::new(self.sink.clone());
        self.inner.as_mut().poll(cx)
    }
}

/// Installs a sink for its lifetime and puts back the previous one on drop,
/// so nesting and panics leave the thread as they found it.
struct Installed {
    previous: Option<Sink>,
}

impl Installed {
    fn new(sink: Option<Sink>) -> Self {
        let previous = SINK.with(|s| std::mem::replace(&mut *s.borrow_mut(), sink));
        Self { previous }
    }
}

impl Drop for Installed {
    fn drop(&mut self) {
        let previous = self.previous.take();
        SINK.with(|s| *s.borrow_mut() = previous);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(n: i64) -> Observation {
        Observation::PromptTokens { ids: vec![n] }
    }

    fn block_on<F: Future>(f: F) -> F::Output {
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut f = Box::pin(f);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    #[test]
    fn nothing_is_recorded_without_a_sink() {
        let mut built = false;
        observe(|| {
            built = true;
            tokens(1)
        });
        assert!(!built, "the observation is not even built");
        assert!(!is_observing());
    }

    #[test]
    fn a_call_records_into_its_own_sink_and_leaves_the_thread_clean() {
        let ((), seen) = block_on(observed(async { observe(|| tokens(1)) }));
        assert_eq!(seen, vec![tokens(1)]);
        assert!(!is_observing(), "the sink is gone after the call");
    }

    #[test]
    fn carried_work_on_another_thread_records_into_the_same_sink() {
        let ((), seen) = block_on(observed(async {
            let work = carry(|| observe(|| tokens(2)));
            std::thread::spawn(work).join().unwrap();
            observe(|| tokens(3));
        }));
        assert_eq!(seen, vec![tokens(2), tokens(3)]);
    }

    #[test]
    fn a_carried_future_records_into_the_sink_it_was_made_under() {
        let (spawned, seen) = block_on(observed(async {
            carry_future(async { observe(|| tokens(4)) })
        }));
        // Polled after the call returned, as a spawned task would be.
        block_on(spawned);
        assert_eq!(seen, Vec::<Observation>::new(), "taken before the task ran");
        let ((), seen) = block_on(observed(async {
            let spawned = carry_future(async { observe(|| tokens(5)) });
            spawned.await;
        }));
        assert_eq!(seen, vec![tokens(5)]);
    }

    #[test]
    fn nested_calls_keep_their_observations_apart() {
        let (inner, outer) = block_on(observed(async {
            observe(|| tokens(1));
            let ((), inner) = observed(async { observe(|| tokens(2)) }).await;
            observe(|| tokens(3));
            inner
        }));
        assert_eq!(inner, vec![tokens(2)]);
        assert_eq!(outer, vec![tokens(1), tokens(3)]);
    }
}
