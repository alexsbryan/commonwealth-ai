// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stream wrappers that observe TTFT + token-generation rate as a
//! synthesis response flows back to the caller. Folded back into the
//! per-(local|peer) `NodeObservations` EWMA so [`oicp_types::throughput_factor`]
//! sees real performance and not just the advertised benchmark.
//!
//! Moved out of `sovereign-mesh` by domains
//! REVIEW-build-serving-move-throughput-guest: its consumers are the host
//! modules `peer_inference`/`pinned_worker_source`, so it travels with the
//! knot (`sovereign/SERVING_BOUNDARY.md` "Corrected 2026-09-14"). Kept in its
//! own file (ARCH §3.2) — throughput accounting is structurally separate from
//! peer selection.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use futures::Stream;
use oicp_types::NodeObservations;
use tokio::sync::RwLock;

// The EWMA arithmetic lives on the leaf that owns `NodeObservations` so the
// Tier-1 simulator can fold observations without naming this host (domains
// REVIEW-build-mesh-sim-decouple). Re-exported here for existing callers.
pub use oicp_types::apply_throughput_observation;

/// Where a `ThroughputObservedStream` should write its measurements
/// when it terminates: either onto the local-side single
/// `NodeObservations` slot, or onto the per-peer map keyed by name.
/// Mirrors the dual storage already present on the mesh host's
/// `InferenceRouter::peer_observations` /
/// `InferenceRouter::local_observations`.
#[derive(Clone)]
pub enum ThroughputTarget {
    Local(Arc<RwLock<NodeObservations>>),
    Peer {
        name: String,
        map: Arc<RwLock<HashMap<String, NodeObservations>>>,
    },
}

/// Predicate that distinguishes "data" frames (count toward
/// `chunk_count`, mark TTFT on the first one) from "terminal" frames
/// (errors, typed `Finish` frames — measure but don't tally).
///
/// `Result<String, Error>` impls return `true` on `Ok` (the legacy
/// chat-completion text-chunk shape). [`oicp_types::StreamFrame`] impls return
/// `true` on `Token` and `false` on `Finish`/`Error` (the typed Phase 1+ shape
/// used by `complete_stream_with_finish`).
///
/// Without this predicate the typed shape would tally the terminal
/// `Finish { reason: Length }` frame as a generated token, inflating
/// throughput observations by one and miscounting TTFT on
/// zero-token-then-Length-truncate (edge case but real).
pub trait IsDataFrame {
    fn is_data_frame(&self) -> bool;
}

impl<E> IsDataFrame for std::result::Result<String, E> {
    fn is_data_frame(&self) -> bool {
        self.is_ok()
    }
}

impl IsDataFrame for oicp_types::StreamFrame {
    fn is_data_frame(&self) -> bool {
        matches!(self, oicp_types::StreamFrame::Token(_))
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
pub struct ThroughputObservedStream<S: Stream + Send + Unpin + 'static>
where
    S::Item: IsDataFrame + Send,
{
    inner: S,
    dispatched_at: Instant,
    first_chunk_at: Option<Instant>,
    chunk_count: u64,
    target: ThroughputTarget,
    completed: bool,
    /// The contribution-ledger port, when the host has one. On completion the
    /// wrapper mints the `InferenceReceived` fact from the terminal
    /// `RoutingOutcome` and hands it to this port
    /// (`crate::ledger`; `SERVING_BOUNDARY.md` (a) — Serving
    /// emits facts, Fabric prices them). `None` for a host with no ledger and
    /// for a pinned venue, whose synthetic node id is not a mesh member
    /// (spec §8).
    ledger: Option<Arc<dyn crate::ledger::LedgerEmitter>>,
    /// P1 of `docs/specs/SCHEDULER_QUALITY.md`: the completion half of
    /// the decision→outcome join.
    ///
    /// This wrapper is the natural home for it — it is already the
    /// one place that measures TTFT, wall time and token count for
    /// every dispatched stream, on every terminus (local, local
    /// fallback, peer) and on every exit (clean end, client abort,
    /// mid-stream death). Measuring those numbers a second time
    /// somewhere else would guarantee the two eventually disagree.
    outcome: Option<sovereign_scheduler::decision_log::OutcomeContext>,
}

impl<S: Stream + Send + Unpin + 'static> ThroughputObservedStream<S>
where
    S::Item: IsDataFrame + Send,
{
    pub fn new(inner: S, target: ThroughputTarget) -> Self {
        Self {
            inner,
            dispatched_at: Instant::now(),
            first_chunk_at: None,
            chunk_count: 0,
            target,
            completed: false,
            ledger: None,
            outcome: None,
        }
    }

    pub fn with_ledger(mut self, emitter: Arc<dyn crate::ledger::LedgerEmitter>) -> Self {
        self.ledger = Some(emitter);
        self
    }

    /// Attach the decision context so this stream's completion emits
    /// the outcome record that joins back to the routing decision.
    pub fn with_outcome(
        mut self,
        outcome: sovereign_scheduler::decision_log::OutcomeContext,
    ) -> Self {
        self.outcome = Some(outcome);
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
        let ledger = self.ledger.clone();
        let outcome = self.outcome.take();

        // Skip recording if no first token ever arrived AND no
        // chunks were yielded. Pure-failure case — nothing to
        // measure, and the failure tracker handles it via
        // `record_failure`.
        //
        // A stream carrying an outcome context is the exception: the
        // decision→outcome join must close even for a request that
        // produced nothing, or a zero-token dispatch would look like
        // a decision that never completed. The timings simply come
        // back `None`.
        let observable = first_chunk.is_some() || count > 0;
        if !observable && outcome.is_none() {
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

            // The outcome record is emitted first and unconditionally:
            // it is the join, and losing it because a later step
            // early-returned would silently break calibration.
            if let Some(ctx) = outcome {
                let total_ms = now.duration_since(dispatched).as_secs_f64() * 1000.0;
                let record = ctx.complete(
                    ttft_ms,
                    Some(total_ms),
                    observable.then_some(count),
                    crate::recorder::now_unix_ms(),
                );
                // Mint the ledger fact from the record just emitted —
                // peer-routed streams that yielded any chunks count as a
                // received inference. Spec §4.3 docs the symmetric pair
                // (`InferenceServed` on peer, `InferenceReceived` here);
                // the aggregator does NOT cross-pollinate, so we have to
                // emit both halves explicitly. A local serve, a failure or
                // a zero-token dispatch mints nothing (`emit_from_outcome`).
                if let Some(emitter) = ledger.as_ref() {
                    crate::ledger::emit_from_outcome(emitter.as_ref(), &record);
                }
            }

            if !observable {
                return;
            }

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
        });
    }
}
