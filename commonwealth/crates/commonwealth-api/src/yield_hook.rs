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
    fn should_yield(&self) -> bool {
        let window = self
            .inner
            .yield_window_secs
            .load(std::sync::atomic::Ordering::Relaxed);
        if window == 0 {
            return false;
        }
        let last = self
            .inner
            .foreground_last_active_ts
            .load(std::sync::atomic::Ordering::Relaxed);
        if last == 0 {
            return false;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let elapsed = now.saturating_sub(last);
        elapsed >= 0 && (elapsed as u64) < window
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
}
