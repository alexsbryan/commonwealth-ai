// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bridge between [`crate::state::AppState`] and the
//! `corpus_engine::YieldHook` trait.
//!
//! A daemon-side `CorpusEngine` is constructed with one of these
//! installed via `with_yield_hook(...)` so that ingest workers
//! consult the same atomics that `chat_completions` writes to. The
//! result: peer-collaboration ingests and solo Wikipedia ingests
//! both pause automatically while the user is chatting, and resume
//! `yield_window_secs` after the last request — no operator
//! intervention, no scheduler config required.
//!
//! Why a thin wrapper rather than `corpus_engine::YieldHook for AppState`:
//! `commonwealth-api` depends on `corpus-engine`, but the reverse is
//! not — and must not — be true (corpus-engine is intentionally
//! decoupled from the daemon's HTTP plane). The trait sits in
//! corpus-engine; the bridge that knows about `AppState` lives here.
//!
//! Cycle note: storing this hook on the `CorpusEngine` that
//! `AppStateInner` itself owns creates a reference cycle
//! (`AppStateInner -> Arc<CorpusEngine> -> Arc<YieldHook> -> Arc<AppStateInner>`).
//! The cycle is intentional and harmless: the daemon runs for the
//! process lifetime, so `AppStateInner::drop` never fires. Tests
//! that don't install the hook pay nothing.

use std::sync::Arc;

use corpus_engine::YieldHook;

use crate::state::AppStateInner;

/// `YieldHook` impl backed by `AppStateInner`. Reads the same
/// `foreground_last_active_ts` / `yield_window_secs` atomics
/// `chat_completions` writes — single source of truth.
pub struct AppStateYieldHook {
    /// `Arc<AppStateInner>` rather than `Arc<AppState>` because the
    /// engine lives behind `AppStateInner.corpus_engine`, and the
    /// outer `AppState::Clone` indirection is just a wrapper. We
    /// need exactly the inner state's atomics.
    inner: Arc<AppStateInner>,
}

impl AppStateYieldHook {
    /// Construct from a strong `Arc<AppStateInner>` — typically the
    /// caller has just built the AppState and is about to construct
    /// a CorpusEngine to install into it. Cycles are accepted (see
    /// module docs).
    pub fn new(inner: Arc<AppStateInner>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

impl YieldHook for AppStateYieldHook {
    /// Delegates to `AppStateInner::foreground_yield_remaining_secs` — the one
    /// implementation of this predicate (ARCH §10.6). It used to be a third
    /// copy of the same arithmetic; if the hook and the
    /// `/internal/daemon/foreground_state` route ever disagreed again, ingest
    /// would be pausing for a reason the introspection surface denied.
    ///
    /// This answers "should I stand aside right now", NOT "may I stand aside
    /// indefinitely". It is a level predicate with no notion of how long the
    /// caller has already been parked, so it can be held true forever by any
    /// request cadence shorter than the window. Every consumer pairs it with
    /// `corpus_engine_yield::DeferralBudget`; that is where the liveness bound
    /// lives, and it is not optional.
    fn should_yield(&self) -> bool {
        self.inner.foreground_yield_remaining_secs().is_some()
    }

    fn throttle_factor(&self) -> f32 {
        let raw = self
            .inner
            .ingest_throttle_milli
            .load(std::sync::atomic::Ordering::Relaxed);
        ((raw.max(1) as f32) / 1000.0).clamp(0.001, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_app_state;

    #[test]
    fn should_yield_false_when_window_is_zero() {
        let app = test_app_state();
        // Default state: window = 0 (disabled). Even with a fresh
        // bump, the hook returns false.
        app.bump_foreground_active();
        let hook = AppStateYieldHook::new(app.inner.clone());
        assert!(!hook.should_yield());
    }

    #[test]
    fn should_yield_false_when_no_activity_yet() {
        let app = test_app_state();
        app.set_yield_window_secs(60);
        // No bump: timestamp = 0 sentinel. Hook returns false even
        // with a positive window — fresh boot shouldn't pause.
        let hook = AppStateYieldHook::new(app.inner.clone());
        assert!(!hook.should_yield());
    }

    #[test]
    fn should_yield_true_within_window() {
        let app = test_app_state();
        app.set_yield_window_secs(60);
        app.bump_foreground_active();
        let hook = AppStateYieldHook::new(app.inner.clone());
        assert!(hook.should_yield());
    }

    #[test]
    fn should_yield_false_after_window_expires() {
        let app = test_app_state();
        app.set_yield_window_secs(60);
        // Manually rewind the timestamp by 120s — past the 60s
        // window. Hook should no longer yield.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        app.inner
            .foreground_last_active_ts
            .store(now - 120, std::sync::atomic::Ordering::Relaxed);
        let hook = AppStateYieldHook::new(app.inner.clone());
        assert!(!hook.should_yield());
    }

    #[test]
    fn seconds_until_idle_reports_remaining_window() {
        let app = test_app_state();
        app.set_yield_window_secs(60);
        app.bump_foreground_active();
        // Fresh bump: ~60s remain. Allow some slack for test timing.
        let remaining = app.seconds_until_foreground_idle().unwrap_or(0);
        assert!(remaining > 55 && remaining <= 60, "got {remaining}");
    }

    #[test]
    fn seconds_until_idle_none_when_disabled() {
        let app = test_app_state();
        app.bump_foreground_active();
        // window = 0 disables: helper returns None even after a
        // bump.
        assert!(app.seconds_until_foreground_idle().is_none());
    }

    // ─── One decider (ARCH §10.6) ─────────────────────────────────

    fn rewind_foreground_to(app: &crate::state::AppState, secs_ago: i64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        app.inner
            .foreground_last_active_ts
            .store(now - secs_ago, std::sync::atomic::Ordering::Relaxed);
    }

    /// The hook, `AppState::should_yield_to_foreground` (what the
    /// `/internal/daemon/foreground_state` route reports) and
    /// `seconds_until_foreground_idle` must never be able to disagree — that
    /// is the whole reason they share one implementation now. Includes the
    /// backwards-clock case, which is exactly where the old copies HAD
    /// diverged.
    #[test]
    fn hook_route_and_countdown_never_disagree() {
        for secs_ago in [-5_i64, 0, 1, 30, 59, 60, 120] {
            let app = test_app_state();
            app.set_yield_window_secs(60);
            rewind_foreground_to(&app, secs_ago);
            let hook = AppStateYieldHook::new(app.inner.clone());

            assert_eq!(
                hook.should_yield(),
                app.should_yield_to_foreground(),
                "hook and route disagreed at secs_ago={secs_ago}"
            );
            assert_eq!(
                hook.should_yield(),
                app.seconds_until_foreground_idle().is_some(),
                "hook and countdown disagreed at secs_ago={secs_ago}"
            );
        }
    }

    /// A timestamp in the FUTURE (clock jumped backwards) is read as "a
    /// foreground request landed very recently", not as "idle". Yielding is
    /// the conservative reading and the deferral bound caps its cost.
    #[test]
    fn a_backwards_clock_yields_rather_than_barging_in() {
        let app = test_app_state();
        app.set_yield_window_secs(60);
        rewind_foreground_to(&app, -30);
        let hook = AppStateYieldHook::new(app.inner.clone());
        assert!(hook.should_yield());
        assert_eq!(app.seconds_until_foreground_idle(), Some(60));
    }

    // ─── Quiesce flag ─────────────────────────────────────────────

    #[test]
    fn mesh_quiesce_round_trips_through_setter() {
        let app = test_app_state();
        assert!(!app.mesh_quiesced(), "default = participate");
        app.set_mesh_quiesced(true);
        assert!(app.mesh_quiesced());
        app.set_mesh_quiesced(false);
        assert!(!app.mesh_quiesced());
    }

    // ─── Throttle factor ─────────────────────────────────────────

    #[test]
    fn ingest_throttle_default_is_full_speed() {
        let app = test_app_state();
        // Default = 1.0 (no throttle). YieldHook impl must agree.
        assert!((app.ingest_throttle_factor() - 1.0).abs() < 1e-3);
        let hook = AppStateYieldHook::new(app.inner.clone());
        assert!((hook.throttle_factor() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn ingest_throttle_setter_clamps_and_round_trips() {
        let app = test_app_state();
        // Half speed: round-trips cleanly through fixed-point ‰.
        let applied = app.set_ingest_throttle_factor(0.5).unwrap();
        assert!((applied - 0.5).abs() < 1e-3);
        let hook = AppStateYieldHook::new(app.inner.clone());
        assert!((hook.throttle_factor() - 0.5).abs() < 1e-3);

        // >1.0 clamps to 1.0 (use pause to fully stop, not >100%).
        let applied = app.set_ingest_throttle_factor(2.0).unwrap();
        assert!((applied - 1.0).abs() < 1e-3);
    }

    #[test]
    fn ingest_throttle_rejects_zero_and_negative() {
        let app = test_app_state();
        assert!(app.set_ingest_throttle_factor(0.0).is_err());
        assert!(app.set_ingest_throttle_factor(-0.5).is_err());
        assert!(app.set_ingest_throttle_factor(f32::NAN).is_err());
        // The reject path must not have written anything.
        assert!((app.ingest_throttle_factor() - 1.0).abs() < 1e-3);
    }
}
