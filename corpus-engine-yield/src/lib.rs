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
//!
//! It also carries the seam's **liveness bound** — [`DeferralBudget`] and
//! [`MAX_FOREGROUND_DEFERRAL`] — for the same reason. `should_yield()` on its
//! own is a *level* predicate with no deadline; every consumer that parks on it
//! needs the same ceiling, and a ceiling that each consumer re-derives is a
//! ceiling that some consumer will get wrong. The policy lives here once; the
//! sleep/tracing mechanics stay with each consumer, because their side effects
//! (progress callbacks, subprocess launches) differ. This crate stays
//! dependency-free — the budget is `std::time` only.

pub mod time;

use std::time::{Duration, Instant};

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

/// Ceiling on the **cumulative** time one worker will defer at a single
/// yield checkpoint before proceeding regardless of foreground activity.
///
/// ## Why a ceiling has to exist at all
///
/// [`YieldHook::should_yield`] is a *level* predicate — "was there foreground
/// inference within the last `window` seconds" — not an edge. Any caller
/// cadence shorter than `window` holds it true forever. That is not a
/// hypothetical: on 2026-08-18 a 30-second liveness probe against the 60-second
/// default window held `corpus-engine`'s pre-enrichment wait open for the whole
/// life of three consecutive runs and enrichment never started once
/// (`runs/sec-filings-ship-e2e/evidence/real-daemon.log`; `resuming enrichment`
/// appears zero times). A person sending a chat every 30 seconds — an ordinary
/// way to use the product — starves it identically.
///
/// No value of `window` fixes this: for every window there is a cadence below
/// it. Shrinking the threshold treats the symptom. Deferral needs a *deadline*.
///
/// ## Why 300s
///
/// Three constraints meet here, and 300s is the interval they leave:
///
/// * **It must be several whole windows**, or yielding stops being yielding.
///   The default `yield_to_foreground_secs` is 60
///   (`sovereign-contracts/src/setup_config.rs`), so this is five consecutive
///   uninterrupted windows of giving way before we stop giving way.
/// * **It must be well inside the caller's own deadline.** The desktop's
///   `enrich-once` client gives the daemon 600s
///   (`sovereign-desktop/src-tauri/src/local_corpus_commands.rs:545`). A bound
///   that fires after the caller has already given up publishes its terminal
///   event to nobody.
/// * **It must fit in one sitting.** A user who starts an ingest and keeps
///   working should see enrichment begin, not learn later that it never ran.
///
/// ## Why it is not a knob
///
/// Deliberately not an env flag and not a config field. A liveness bound
/// someone has to remember to switch on is not a liveness bound. See
/// [`DeferralBudget::with_cap_at_most`] for the only permitted adjustment —
/// and note it can only tighten.
pub const MAX_FOREGROUND_DEFERRAL: Duration = Duration::from_secs(300);

/// What a [`DeferralBudget`] says to do on this trip round a wait loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferralStep {
    /// The hook is not asking us to yield. Get on with the work.
    Proceed,
    /// Still yielding and the budget has room. Sleep and re-poll.
    Defer {
        /// Wall-clock spent deferring at this checkpoint so far.
        waited: Duration,
    },
    /// Still yielding, but the budget is spent. Proceed anyway — and say so
    /// out loud, because the foreground is about to contend.
    CapReached {
        /// Wall-clock spent deferring at this checkpoint.
        waited: Duration,
    },
}

/// A bounded deferral allowance for one yield checkpoint.
///
/// Construct it at the checkpoint and call [`step`](Self::step) each time
/// round the wait loop. Holding the start instant inside the budget is the
/// point: a call site cannot forget to measure how long it has been parked,
/// because the object it must ask for permission is the same object that
/// knows.
#[derive(Debug, Clone, Copy)]
pub struct DeferralBudget {
    started: Instant,
    cap: Duration,
}

impl DeferralBudget {
    /// A budget carrying the standing [`MAX_FOREGROUND_DEFERRAL`] bound.
    /// The clock starts now.
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            cap: MAX_FOREGROUND_DEFERRAL,
        }
    }

    /// A budget with a **tighter** bound than the default.
    ///
    /// `cap` is clamped to [`MAX_FOREGROUND_DEFERRAL`], so no caller — and no
    /// test — can weaken the invariant, only opt into a stricter one. That
    /// clamp is why this constructor can stay public: the standing bound is
    /// enforced structurally rather than by reviewer memory.
    pub fn with_cap_at_most(cap: Duration) -> Self {
        Self {
            started: Instant::now(),
            cap: if cap < MAX_FOREGROUND_DEFERRAL {
                cap
            } else {
                MAX_FOREGROUND_DEFERRAL
            },
        }
    }

    /// The bound actually in force for this budget.
    pub fn cap(&self) -> Duration {
        self.cap
    }

    /// Wall-clock elapsed since the budget was constructed.
    pub fn waited(&self) -> Duration {
        self.started.elapsed()
    }

    /// Decide what the wait loop should do next.
    ///
    /// `Proceed` takes precedence over `CapReached`: if the foreground has
    /// genuinely gone idle we exit by the polite door and no override event is
    /// emitted, even on a budget that happens to be spent.
    pub fn step(&self, hook: &dyn YieldHook) -> DeferralStep {
        if !hook.should_yield() {
            return DeferralStep::Proceed;
        }
        let waited = self.waited();
        if waited >= self.cap {
            DeferralStep::CapReached { waited }
        } else {
            DeferralStep::Defer { waited }
        }
    }
}

impl Default for DeferralBudget {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod deferral_tests {
    use super::*;

    struct Always(bool);
    impl YieldHook for Always {
        fn should_yield(&self) -> bool {
            self.0
        }
    }

    #[test]
    fn idle_foreground_proceeds_without_consuming_the_budget() {
        let budget = DeferralBudget::new();
        assert_eq!(budget.step(&Always(false)), DeferralStep::Proceed);
    }

    #[test]
    fn active_foreground_defers_while_the_budget_has_room() {
        let budget = DeferralBudget::new();
        assert!(matches!(
            budget.step(&Always(true)),
            DeferralStep::Defer { .. }
        ));
    }

    #[test]
    fn a_spent_budget_overrides_a_still_active_foreground() {
        // The whole point of the bound: foreground is STILL active and we
        // proceed anyway.
        let budget = DeferralBudget::with_cap_at_most(Duration::ZERO);
        assert!(matches!(
            budget.step(&Always(true)),
            DeferralStep::CapReached { .. }
        ));
    }

    #[test]
    fn a_spent_budget_still_prefers_the_polite_exit() {
        let budget = DeferralBudget::with_cap_at_most(Duration::ZERO);
        assert_eq!(budget.step(&Always(false)), DeferralStep::Proceed);
    }

    #[test]
    fn with_cap_at_most_cannot_weaken_the_standing_bound() {
        // A caller asking for a week gets the standing bound, not a week.
        let generous = DeferralBudget::with_cap_at_most(Duration::from_secs(7 * 24 * 3600));
        assert_eq!(generous.cap(), MAX_FOREGROUND_DEFERRAL);
        // A caller asking for less than the bound gets what it asked for.
        let strict = DeferralBudget::with_cap_at_most(Duration::from_secs(1));
        assert_eq!(strict.cap(), Duration::from_secs(1));
    }

    #[test]
    fn standing_bound_is_five_whole_default_yield_windows() {
        // `yield_to_foreground_secs` defaults to 60 and is an operator-owned
        // threshold. If that default ever moves, this assertion is the place
        // that notices the bound is no longer "five whole windows".
        const DEFAULT_YIELD_WINDOW_SECS: u64 = 60;
        assert_eq!(
            MAX_FOREGROUND_DEFERRAL.as_secs(),
            5 * DEFAULT_YIELD_WINDOW_SECS
        );
    }

    #[test]
    fn standing_bound_leaves_room_inside_the_desktop_enrich_once_timeout() {
        // local_corpus_commands.rs:545 — the desktop gives the daemon 600s for
        // ingest+enrich. The bound must leave time for enrichment ITSELF after
        // it fires, or the terminal event lands after the caller gave up.
        const DESKTOP_ENRICH_ONCE_TIMEOUT_SECS: u64 = 600;
        assert!(
            MAX_FOREGROUND_DEFERRAL.as_secs() * 2 <= DESKTOP_ENRICH_ONCE_TIMEOUT_SECS,
            "the bound must leave at least as much time for enrichment as it \
             spends waiting"
        );
    }
}
