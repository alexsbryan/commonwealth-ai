// SPDX-License-Identifier: AGPL-3.0-or-later
//! The node's in-flight request gauge — the counter the serving provider
//! increments and gossip publishes.
//!
//! `quality/DAEMON_CORE.md` §4.2 "Where an install slot breaks a cycle" works
//! this exact case: `InferenceRouter` used to mint the counter, the bootstrap
//! installed it into `AppState` so gossip could read it, and a reload handed
//! the same `Arc` back to the new provider so the count survived. The gauge
//! wants to exist *before* the provider, so the node creates it and gives it
//! to both — a signal object created first, never a slot filled later.
//!
//! It lives here beside [`crate::self_claims`] for the same reason: its
//! speakers sit in crates that may not name each other — `sovereign-mesh`
//! carries it on `ServingCore`, `sovereign-api` holds it on `ServingPart` and
//! answers the `SelfClaims` port from it. Cloning shares the counter: a gauge
//! is a handle, not a value.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// The node's local in-flight request count.
///
/// One `Arc<AtomicU32>`, created before the provider that mutates it through
/// RAII guards and read by the node's `SelfClaims` answer. A node with no
/// provider has no gauge, and its absence is the `None` gossip publishes —
/// never a zeroed default (ARCH 6).
#[derive(Clone, Default)]
pub struct LocalInFlightGauge(Arc<AtomicU32>);

impl LocalInFlightGauge {
    /// A fresh gauge at zero.
    pub fn new() -> Self {
        Self(Arc::new(AtomicU32::new(0)))
    }

    /// The shared atomic, for the provider whose RAII guards write it.
    pub fn arc(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.0)
    }

    /// The count right now. Lock-free.
    pub fn current(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }

    /// Set the count directly. The production writers are the provider's
    /// guards; this is for the tests and probes that drive the gauge without a
    /// router in the stack.
    pub fn set(&self, value: u32) {
        self.0.store(value, Ordering::Relaxed);
    }
}

impl std::fmt::Debug for LocalInFlightGauge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalInFlightGauge")
            .field("current", &self.current())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Positive: a clone is the same counter — the node and the provider hold
    /// one gauge, so a write through the provider's handle is visible to the
    /// node's reader.
    #[test]
    fn clone_shares_the_counter() {
        let gauge = LocalInFlightGauge::new();
        let provider_handle = gauge.arc();
        provider_handle.store(7, Ordering::Relaxed);
        assert_eq!(gauge.current(), 7);
        assert_eq!(LocalInFlightGauge::clone(&gauge).current(), 7);
    }

    /// Negative: two gauges are independent — the counter is a handle on one
    /// node, not a process-global cell (ARCH 8).
    #[test]
    fn two_gauges_are_independent() {
        let a = LocalInFlightGauge::new();
        let b = LocalInFlightGauge::new();
        a.set(3);
        assert_eq!(a.current(), 3);
        assert_eq!(b.current(), 0);
    }

    /// Positive: a fresh gauge is a real zero, not an absent signal — the
    /// `None` that means "no gauge" comes from the holder's `Option`, not from
    /// this type (ARCH 6).
    #[test]
    fn fresh_gauge_reads_zero() {
        assert_eq!(LocalInFlightGauge::new().current(), 0);
    }
}
