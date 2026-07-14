// SPDX-License-Identifier: AGPL-3.0-or-later
//! Injectable wall-clock and monotonic time.
//!
//! Why this exists: membership liveness (offline-decay) and the gossip LWW
//! merge read `SystemTime::now()` directly at many sites, which (1) makes
//! clock-skew behaviour impossible to test and (2) is the root of the
//! cross-node wall-clock comparison hazard — a peer with a fast clock wins
//! every `last_seen` LWW comparison, and offline-decay compares a *remote*
//! `last_seen` against the *local* now (the unresolved "~9 min flap").
//!
//! Routing every wall-clock read through a `Clock` lets the test harness drive
//! per-node skew deterministically while production keeps real time via
//! `SystemClock`. This module is the additive seam; the decay/LWW behaviour
//! change that exploits it lands separately (it must remain reviewable on its
//! own, per ARCH_PRINCIPLES §10).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Source of wall-clock (unix seconds) and monotonic time.
///
/// `now_unix_secs` is skewable in tests — it is the value that flows into
/// gossiped `last_seen` and offline-decay. `now_instant` is **never** skewed:
/// it backs latency measurement, which must stay truthful even when a node's
/// wall clock is offset, so the default impl returns the real `Instant::now`
/// and `TestClock` does not override it.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Wall-clock, seconds since the unix epoch.
    fn now_unix_secs(&self) -> u64;

    /// Monotonic instant for elapsed-time measurement. Not skewable.
    fn now_instant(&self) -> Instant {
        Instant::now()
    }
}

/// Production clock: real `SystemTime` / `Instant`. Behaviourally identical to
/// the `now_secs()` helpers it replaces.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

/// Unskewed wall-clock seconds — the canonical replacement for the scattered
/// context-free `now_secs()` helpers across the commonwealth crates. Equivalent
/// to `SystemClock.now_unix_secs()`.
///
/// Use this ONLY where no `Clock` is injected and skew does not matter (logging,
/// local timestamps). Skew-sensitive reads — membership `last_seen`, gossip LWW,
/// offline-decay — must take a `&dyn Clock` so tests can drive per-node skew (see
/// the module docs); this free helper is deliberately NOT skewable.
pub fn unix_now_secs() -> u64 {
    SystemClock.now_unix_secs()
}

/// Unskewed wall-clock milliseconds. Same skew caveat as [`unix_now_secs`].
pub fn unix_now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Test clock: a shared, settable base (the simulated *global* wall-clock) plus
/// a per-instance signed `offset` (this node's skew, in seconds).
///
/// Clones share the base via an `Arc<AtomicU64>`, so [`TestClock::advance`]
/// moves time for every node at once while each clone keeps its own skew —
/// exactly the shape a multi-node skew scenario needs ("global time advances;
/// each node reads it through its own offset").
#[derive(Debug, Clone)]
pub struct TestClock {
    base_secs: Arc<AtomicU64>,
    offset_secs: i64,
}

impl TestClock {
    /// New clock at `base_secs` with zero skew.
    pub fn new(base_secs: u64) -> Self {
        Self {
            base_secs: Arc::new(AtomicU64::new(base_secs)),
            offset_secs: 0,
        }
    }

    /// A clone sharing this clock's base but with a different skew. Positive
    /// `offset_secs` = this node's clock runs *ahead* of the shared base.
    pub fn with_offset(&self, offset_secs: i64) -> Self {
        Self {
            base_secs: Arc::clone(&self.base_secs),
            offset_secs,
        }
    }

    /// Set the shared base (affects this clock and every clone of it).
    pub fn set(&self, secs: u64) {
        self.base_secs.store(secs, Ordering::SeqCst);
    }

    /// Advance the shared base by `secs` (affects this clock and every clone).
    pub fn advance(&self, secs: u64) {
        self.base_secs.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_unix_secs(&self) -> u64 {
        let base = self.base_secs.load(Ordering::SeqCst) as i64;
        (base + self.offset_secs).max(0) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_matches_systemtime_within_tolerance() {
        let truth = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let got = SystemClock.now_unix_secs();
        // Allow a 2s window for the two reads straddling a second boundary.
        assert!(got.abs_diff(truth) <= 2, "got={got} truth={truth}");
    }

    #[test]
    fn test_clock_is_deterministic_and_settable() {
        let c = TestClock::new(1_000);
        assert_eq!(c.now_unix_secs(), 1_000);
        c.set(2_000);
        assert_eq!(c.now_unix_secs(), 2_000);
        c.advance(50);
        assert_eq!(c.now_unix_secs(), 2_050);
    }

    #[test]
    fn offset_clones_share_base_but_keep_skew() {
        let base = TestClock::new(1_000);
        let fast = base.with_offset(3_600); // 1h ahead
        let slow = base.with_offset(-100);
        assert_eq!(base.now_unix_secs(), 1_000);
        assert_eq!(fast.now_unix_secs(), 4_600);
        assert_eq!(slow.now_unix_secs(), 900);

        // Advancing through ANY handle moves the shared base for all of them.
        fast.advance(400);
        assert_eq!(base.now_unix_secs(), 1_400);
        assert_eq!(fast.now_unix_secs(), 5_000);
        assert_eq!(slow.now_unix_secs(), 1_300);
    }

    #[test]
    fn negative_skew_saturates_at_zero() {
        let c = TestClock::new(10).with_offset(-1_000);
        assert_eq!(c.now_unix_secs(), 0);
    }
}
