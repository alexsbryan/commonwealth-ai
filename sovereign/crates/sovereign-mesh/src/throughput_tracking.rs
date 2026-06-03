//! Stream wrappers that observe TTFT + token-generation rate as a
//! synthesis response flows back to the caller. Folded back into the
//! per-(local|peer) `NodeObservations` EWMA so [`oicp::throughput_factor`]
//! sees real performance and not just the advertised benchmark.
//!
//! Lives alongside `peer_inference` so the routing path can wrap the
//! returned stream with no extra crate boundary, but kept in its own
//! file (ARCH §3.2) — throughput accounting is structurally separate
//! from peer selection.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use futures::Stream;
use sovereign_core::oicp::{NodeObservations, THROUGHPUT_EWMA_ALPHA};
use tokio::sync::RwLock;

/// Where a `ThroughputObservedStream` should write its measurements
/// when it terminates: either onto the local-side single
/// `NodeObservations` slot, or onto the per-peer map keyed by name.
/// Mirrors the dual storage already present on
/// [`crate::peer_inference::MeshInferenceProvider::peer_observations`] /
/// [`crate::peer_inference::MeshInferenceProvider::local_observations`].
#[derive(Clone)]
pub(crate) enum ThroughputTarget {
    Local(Arc<RwLock<NodeObservations>>),
    Peer {
        name: String,
        map: Arc<RwLock<HashMap<String, NodeObservations>>>,
    },
}

/// Optional ledger-event emission attached to the stream wrapper.
/// When `Some`, the stream's `Drop` impl fires an
/// `InferenceReceived` event on completion with `tokens_generated`
/// equal to the chunk count. Local-served streams set this to
/// `None` — the dimensional ledger is intra-mesh-only per spec
/// §10, and a "received from self" event is meaningless.
///
/// `pub` because the [`crate::peer_inference::PeerEndpointSource`]
/// trait method `ledger_emission_for` returns `Option<LedgerEmission>`
/// — implementors outside this module need to construct values of
/// this type. Fields stay `pub(crate)` so the construction shape
/// is still controlled.
#[derive(Clone)]
pub struct LedgerEmission {
    pub(crate) from_node: commonwealth_core::ids::NodeId,
    pub(crate) model_id: String,
    pub(crate) emitter: commonwealth_state::ContributionEmitter,
}

impl LedgerEmission {
    /// Construct a `LedgerEmission`. The production wiring goes
    /// through `EmbeddedDaemon::ledger_emission_for`; tests outside
    /// this crate use this constructor to plug a controlled
    /// `ContributionEmitter` into a stub `PeerEndpointSource` and
    /// observe `InferenceReceived` events end-to-end.
    pub fn new(
        from_node: commonwealth_core::ids::NodeId,
        model_id: impl Into<String>,
        emitter: commonwealth_state::ContributionEmitter,
    ) -> Self {
        Self {
            from_node,
            model_id: model_id.into(),
            emitter,
        }
    }
}

/// Predicate that distinguishes "data" frames (count toward
/// `chunk_count`, mark TTFT on the first one) from "terminal" frames
/// (errors, typed `Finish` frames — measure but don't tally).
///
/// `Result<String, Error>` impls return `true` on `Ok` (the legacy
/// chat-completion text-chunk shape). [`sovereign_core::types::
/// StreamFrame`] impls return `true` on `Token` and `false` on
/// `Finish`/`Error` (the typed Phase 1+ shape used by
/// `complete_stream_with_finish`).
///
/// Without this predicate the typed shape would tally the terminal
/// `Finish { reason: Length }` frame as a generated token, inflating
/// throughput observations by one and miscounting TTFT on
/// zero-token-then-Length-truncate (edge case but real).
pub(crate) trait IsDataFrame {
    fn is_data_frame(&self) -> bool;
}

impl<E> IsDataFrame for std::result::Result<String, E> {
    fn is_data_frame(&self) -> bool {
        self.is_ok()
    }
}

impl IsDataFrame for sovereign_core::types::StreamFrame {
    fn is_data_frame(&self) -> bool {
        matches!(self, sovereign_core::types::StreamFrame::Token(_))
    }
}

/// Stream wrapper that records TTFT (time-to-first-token) and
/// observed token-generation rate when the stream completes. Both
/// metrics fold into the per-(local|peer) [`NodeObservations`] EWMA
/// so [`oicp::throughput_factor`] sees real performance, not just
/// the advertised benchmark.
///
/// Generic over the inner stream's item type so the same wrapper
/// works for the legacy `Result<String, Error>` text-chunk shape and
/// the typed `StreamFrame` shape that `complete_stream_with_finish`
/// returns. The `IsDataFrame` predicate gates token-tallying so
/// terminal frames don't inflate the EWMA.
///
/// Implementation notes:
///
/// - Token count is approximated as **data frames** yielded. SSE-
///   streamed output from llama.cpp emits one chunk per token in
///   practice. This is a coarse proxy for routing — the absolute
///   number may be off, but the relative ordering across peers is
///   preserved (every peer is measured the same way).
/// - We record on `Drop` so that streams aborted mid-completion
///   still surface their TTFT — abort timing is a useful signal
///   too. A stream that ended with zero chunks contributes only
///   the TTFT data.
/// - Recording is `tokio::spawn`'d because `Drop` runs in a
///   non-async context. The spawned task uses the same EWMA α as
///   the latency probe to stay consistent with the rest of the
///   observation pipeline.
pub(crate) struct ThroughputObservedStream<S: Stream + Send + Unpin + 'static>
where
    S::Item: IsDataFrame + Send,
{
    inner: S,
    dispatched_at: Instant,
    first_chunk_at: Option<Instant>,
    chunk_count: u64,
    target: ThroughputTarget,
    completed: bool,
    ledger_emission: Option<LedgerEmission>,
}

impl<S: Stream + Send + Unpin + 'static> ThroughputObservedStream<S>
where
    S::Item: IsDataFrame + Send,
{
    pub(crate) fn new(inner: S, target: ThroughputTarget) -> Self {
        Self {
            inner,
            dispatched_at: Instant::now(),
            first_chunk_at: None,
            chunk_count: 0,
            target,
            completed: false,
            ledger_emission: None,
        }
    }

    pub(crate) fn with_ledger_emission(mut self, emission: LedgerEmission) -> Self {
        self.ledger_emission = Some(emission);
        self
    }
}

impl<S: Stream + Send + Unpin + 'static> Stream for ThroughputObservedStream<S>
where
    S::Item: IsDataFrame + Send,
{
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(item)) => {
                if item.is_data_frame() {
                    if self.first_chunk_at.is_none() {
                        self.first_chunk_at = Some(Instant::now());
                    }
                    self.chunk_count += 1;
                }
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => {
                self.completed = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S: Stream + Send + Unpin + 'static> Drop for ThroughputObservedStream<S>
where
    S::Item: IsDataFrame + Send,
{
    fn drop(&mut self) {
        let dispatched = self.dispatched_at;
        let first_chunk = self.first_chunk_at;
        let count = self.chunk_count;
        let target = self.target.clone();
        let emission = self.ledger_emission.clone();

        // Skip recording if no first token ever arrived AND no
        // chunks were yielded. Pure-failure case — nothing to
        // measure, and the failure tracker handles it via
        // `record_failure`.
        if first_chunk.is_none() && count == 0 {
            return;
        }

        tokio::spawn(async move {
            let now = Instant::now();
            let ttft_ms = first_chunk.map(|t| t.duration_since(dispatched).as_secs_f64() * 1000.0);
            let tg_tok_s = first_chunk.and_then(|fc| {
                let gen_secs = now.duration_since(fc).as_secs_f64();
                if gen_secs > 0.0 && count > 0 {
                    Some(count as f64 / gen_secs)
                } else {
                    None
                }
            });

            match target {
                ThroughputTarget::Local(obs) => {
                    let mut o = obs.write().await;
                    apply_throughput_observation(&mut o, ttft_ms, tg_tok_s);
                }
                ThroughputTarget::Peer { name, map } => {
                    let mut m = map.write().await;
                    let entry = m.entry(name).or_insert_with(NodeObservations::default);
                    apply_throughput_observation(entry, ttft_ms, tg_tok_s);
                }
            }

            // Emit `InferenceReceived` on the completion path —
            // peer-routed streams that yielded any chunks count as
            // a received inference. Spec §4.3 docs the symmetric
            // pair (`InferenceServed` on peer, `InferenceReceived`
            // here); the aggregator does NOT cross-pollinate, so
            // we have to emit both halves explicitly.
            if let Some(em) = emission {
                if count > 0 {
                    em.emitter.record(
                        commonwealth_core::contributions::LedgerEventKind::InferenceReceived {
                            from_node: em.from_node,
                            model_id: em.model_id,
                            tokens_generated: count,
                        },
                    );
                }
            }
        });
    }
}

/// EWMA update for the throughput-observation fields on
/// [`NodeObservations`]. α follows
/// [`THROUGHPUT_EWMA_ALPHA`] so this stays in lock-step with the
/// latency probe and other observation paths.
pub(crate) fn apply_throughput_observation(
    obs: &mut NodeObservations,
    ttft_ms: Option<f64>,
    tg_tok_s: Option<f64>,
) {
    let alpha = THROUGHPUT_EWMA_ALPHA;
    if let Some(ttft) = ttft_ms {
        obs.ttft_ewma_ms = if obs.ttft_ewma_ms == 0.0 {
            ttft
        } else {
            alpha * ttft + (1.0 - alpha) * obs.ttft_ewma_ms
        };
    }
    if let Some(tg) = tg_tok_s {
        obs.tg_tok_s_ewma = if obs.tg_tok_s_ewma == 0.0 {
            tg
        } else {
            alpha * tg + (1.0 - alpha) * obs.tg_tok_s_ewma
        };
    }
}

#[cfg(test)]
mod throughput_tests {
    use super::*;

    #[test]
    fn ewma_seed_takes_first_value_when_zero() {
        let mut obs = NodeObservations::default();
        apply_throughput_observation(&mut obs, Some(120.0), Some(15.0));
        assert!((obs.ttft_ewma_ms - 120.0).abs() < 1e-9);
        assert!((obs.tg_tok_s_ewma - 15.0).abs() < 1e-9);
    }

    #[test]
    fn ewma_blends_subsequent_samples_at_alpha() {
        let mut obs = NodeObservations::default();
        apply_throughput_observation(&mut obs, Some(100.0), Some(20.0));
        apply_throughput_observation(&mut obs, Some(200.0), Some(10.0));
        // alpha=0.3; 0.3*200 + 0.7*100 = 130
        assert!((obs.ttft_ewma_ms - 130.0).abs() < 1e-9);
        // 0.3*10 + 0.7*20 = 17
        assert!((obs.tg_tok_s_ewma - 17.0).abs() < 1e-9);
    }

    #[test]
    fn ewma_ignores_none_inputs() {
        let mut obs = NodeObservations::default();
        apply_throughput_observation(&mut obs, Some(100.0), None);
        assert_eq!(obs.tg_tok_s_ewma, 0.0);
        apply_throughput_observation(&mut obs, None, Some(15.0));
        assert!((obs.ttft_ewma_ms - 100.0).abs() < 1e-9);
        assert!((obs.tg_tok_s_ewma - 15.0).abs() < 1e-9);
    }
}
