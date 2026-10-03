// SPDX-License-Identifier: AGPL-3.0-or-later
//! The local in-flight accounting guards `InferenceRouter` hands into
//! its stream wrappers. Split out of `peer_inference.rs` (ARCH §3.1).
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// RAII guard for the per-model local in-flight counter. Decrements
/// the counter in `Drop`; safe to drop after the entry has been
/// pruned to zero (saturating subtract + no-op when absent).
///
/// Composes a [`LocalTotalGuard`] in `_total` so the gossiped
/// publisher decrements in lock-step. Rust's struct-field drop order
/// (declaration order) means the HashMap-entry decrement runs
/// before `_total`'s Drop fires — readers that race the decrement
/// see "either both committed or neither has", never "publisher
/// decremented while HashMap still high."
pub(crate) struct LocalInflightGuard {
    pub(crate) counter: Arc<std::sync::Mutex<std::collections::HashMap<String, u32>>>,
    pub(crate) model_id: String,
    pub(crate) _total: LocalTotalGuard,
}

impl Drop for LocalInflightGuard {
    fn drop(&mut self) {
        let Ok(mut map) = self.counter.lock() else {
            return;
        };
        if let Some(v) = map.get_mut(&self.model_id) {
            *v = v.saturating_sub(1);
            if *v == 0 {
                map.remove(&self.model_id);
            }
        }
    }
}

/// RAII guard for the gossiped total in-flight counter. Saturating
/// subtract in Drop — the counter is correctness-best-effort for
/// scoring purposes, not load-bearing for correctness, so we never
/// want a bug to underflow it to `u32::MAX`.
pub(crate) struct LocalTotalGuard {
    pub(crate) publisher: Arc<AtomicU32>,
}

impl Drop for LocalTotalGuard {
    fn drop(&mut self) {
        // Compare-exchange loop because `fetch_sub` would underflow
        // on a hypothetical unbalanced drop. The counter starts at 0
        // and every `enter_local_total` bumps it by 1 before yielding
        // the guard, so the only way to reach 0 with a live guard is
        // a logic bug — saturate rather than wrap.
        let mut cur = self.publisher.load(Ordering::Relaxed);
        loop {
            let new = cur.saturating_sub(1);
            match self.publisher.compare_exchange_weak(
                cur,
                new,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }
}
