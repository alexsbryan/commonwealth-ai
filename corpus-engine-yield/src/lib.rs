// SPDX-License-Identifier: AGPL-3.0-or-later
//! Foreground-yield cooperative pause hook — a leaf contract crate.
//!
//! `YieldHook` is the shared seam that lets ingest workers and the
//! background lint/test watchers back off temporarily when the host
//! node is serving foreground inference requests against the same
//! llama.cpp backend. The contention this exists to address is
//! GPU-level: an atomic `llama_decode` running an embed batch on the
//! EmbedSlot can occupy the device for ~7s, during which a chat token
//! on the primary slot cannot interleave — so foreground latency
//! collapses whenever an ingest is running.
//!
//! This crate holds only the trait so that both the corpus data plane
//! (`corpus-engine`, which polls it in `engine::ingest`) and the
//! reactive watchers (`corpus-engine-watchers`, which install it on
//! their subprocess runs) share a single trait identity — the daemon
//! builds one `Arc<dyn YieldHook>` and installs the same object on the
//! `CorpusEngine` and on the watchers. Implementations live outside
//! this crate (commonwealth-api owns the AppState atomics the trait
//! reads from), keeping every consumer independent of the daemon's
//! request-state machinery.
//!
//! `corpus-engine` polls it at two natural checkpoints in its ingest
//! pipeline: before each embed-batch flush, and before each enrichment
//! phase. Embed batches and enrichment calls are atomic at the GPU
//! layer — mid-call preemption isn't possible — so checkpointing on
//! these boundaries is the finest granularity that actually frees the
//! device for the primary slot.
//!
//! Besides the yield seam, this leaf also carries [`time`] — the canonical
//! wall-clock helpers shared across the corpus-engine subtree — for the same
//! reason it holds `YieldHook`: it is the lowest crate every corpus-engine-*
//! member can depend on, so a trivial shared primitive lives here exactly once.

pub mod time;

/// Hook the ingest pipeline polls before starting the next embed
/// batch / enrichment phase. Implementations are expected to be
/// cheap (atomic load + comparison) so polling at every batch
/// boundary doesn't introduce its own throughput tax.
pub trait YieldHook: Send + Sync {
    /// `true` when the worker should pause and re-poll later.
    /// Returning `false` (the default state) lets the worker proceed
    /// immediately. Called from async context; do NOT block.
    fn should_yield(&self) -> bool;

    /// Per-batch throttle factor in `(0.0, 1.0]`. `1.0` (the default)
    /// = full speed; the ingest pipeline runs back-to-back batches
    /// with no extra sleep. A value `< 1.0` instructs the pipeline
    /// to sleep `(1/factor − 1) * batch_wall_time` after each
    /// embed batch — yielding a duty cycle of `factor` even on a
    /// machine with no concurrent foreground inference.
    ///
    /// The yield hook (`should_yield`) is the right tool for "stop
    /// completely while the user is chatting"; this knob is the
    /// right tool for "share the machine 50/50 with whatever else
    /// the user is doing for the next 24 hours of ingest." Default
    /// returns `1.0` so existing implementors stay fast.
    fn throttle_factor(&self) -> f32 {
        1.0
    }
}
