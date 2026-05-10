//! Foreground-yield cooperative pause hook.
//!
//! Lets ingest workers back off temporarily when the host node is
//! serving foreground inference requests against the same llama.cpp
//! backend. The contention this exists to address is GPU-level: an
//! atomic `llama_decode` running an embed batch on the EmbedSlot can
//! occupy the device for ~7s, during which a chat token on the
//! primary slot cannot interleave — so foreground latency collapses
//! whenever an ingest is running.
//!
//! Implementations live outside this crate (commonwealth-api owns
//! the AppState atomics this trait reads from) — the seam keeps
//! `corpus-engine` independent of the daemon's request-state
//! machinery.
//!
//! Polled at two natural checkpoints in [`crate::engine::ingest`]:
//!   1. before each embed-batch flush, and
//!   2. before each enrichment phase.
//!
//! Embed batches and enrichment calls are atomic at the GPU layer —
//! mid-call preemption isn't possible — so checkpointing on these
//! boundaries is the finest granularity that actually frees the
//! device for the primary slot.

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
