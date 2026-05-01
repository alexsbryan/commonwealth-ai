//! Registry of active watched-folder corpora.
//!
//! Owns three things:
//!   - the set of corpus_ids the scheduler should sweep
//!   - a per-corpus async mutex so two sweeps for the same corpus
//!     never run concurrently (a slow sweep delays the next start,
//!     never overlaps itself)
//!   - a per-corpus "last started at" timestamp the scheduler reads
//!     to decide which corpus is due for a sweep
//!
//! Registration / deregistration is idempotent so the daemon can call
//! `register` from auto-resume on startup without double-spawning.
//!
//! Per ARCH §4: this is the registry pattern. New watched-folder
//! corpora register without touching scheduler code.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};

#[derive(Debug)]
struct CorpusSlot {
    /// Per-corpus mutex acquired by the worker for the duration of
    /// one sweep. `try_lock_owned` from the scheduler returns
    /// immediately if a sweep is already in flight.
    lock: Arc<Mutex<()>>,
    /// Last sweep start time (Unix seconds), 0 if never run. Updated
    /// by the scheduler when it dispatches a tick to the worker.
    last_started_unix: u64,
    /// Per-corpus sweep cadence (seconds). Copied from the
    /// `WatchedFolderConfig` at registration time and refreshed when
    /// the registry is re-populated from the manager.
    sweep_interval_secs: u64,
}

#[derive(Default)]
pub struct WatchedFolderRegistry {
    slots: RwLock<HashMap<String, CorpusSlot>>,
}

impl WatchedFolderRegistry {
    pub fn new() -> Self {
        Self {
            slots: RwLock::new(HashMap::new()),
        }
    }

    /// Idempotent: re-registering an existing corpus refreshes its
    /// sweep interval but preserves the lock + `last_started_unix`
    /// (so a re-register doesn't reset the cadence clock).
    pub async fn register(&self, corpus_id: impl Into<String>, sweep_interval_secs: u64) {
        let id = corpus_id.into();
        let mut slots = self.slots.write().await;
        let entry = slots.entry(id).or_insert_with(|| CorpusSlot {
            lock: Arc::new(Mutex::new(())),
            last_started_unix: 0,
            sweep_interval_secs,
        });
        entry.sweep_interval_secs = sweep_interval_secs;
    }

    /// Remove a corpus from the registry. Idempotent.
    pub async fn deregister(&self, corpus_id: &str) {
        self.slots.write().await.remove(corpus_id);
    }

    /// Snapshot of all registered corpus_ids. Order is not stable
    /// across calls (HashMap iteration); the scheduler sorts as
    /// needed for fair-share dispatch.
    pub async fn list(&self) -> Vec<String> {
        self.slots.read().await.keys().cloned().collect()
    }

    /// Current count — used by tests and the status route.
    pub async fn len(&self) -> usize {
        self.slots.read().await.len()
    }

    pub async fn is_registered(&self, corpus_id: &str) -> bool {
        self.slots.read().await.contains_key(corpus_id)
    }

    /// Try to acquire the per-corpus sweep lock without blocking.
    /// Returns `None` when a sweep is already in flight; otherwise
    /// returns the owned guard the caller holds for the duration of
    /// the sweep.
    pub async fn try_acquire(&self, corpus_id: &str) -> Option<OwnedMutexGuard<()>> {
        let lock = self.slots.read().await.get(corpus_id)?.lock.clone();
        lock.try_lock_owned().ok()
    }

    /// Mark a sweep as started — updates `last_started_unix`. The
    /// scheduler calls this immediately before dispatching to the
    /// worker so the next tick's "is this corpus due?" check sees the
    /// new value.
    pub async fn mark_started(&self, corpus_id: &str, now_unix: u64) {
        if let Some(slot) = self.slots.write().await.get_mut(corpus_id) {
            slot.last_started_unix = now_unix;
        }
    }

    /// `(last_started_unix, sweep_interval_secs)` for the scheduler's
    /// "is this corpus due?" check.
    pub async fn cadence_info(&self, corpus_id: &str) -> Option<(u64, u64)> {
        self.slots
            .read()
            .await
            .get(corpus_id)
            .map(|s| (s.last_started_unix, s.sweep_interval_secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_and_list() {
        let r = WatchedFolderRegistry::new();
        r.register("c1", 120).await;
        r.register("c2", 60).await;
        assert_eq!(r.len().await, 2);
        let mut ids = r.list().await;
        ids.sort();
        assert_eq!(ids, vec!["c1", "c2"]);
    }

    #[tokio::test]
    async fn register_is_idempotent_and_refreshes_interval() {
        let r = WatchedFolderRegistry::new();
        r.register("c1", 120).await;
        r.mark_started("c1", 100).await;
        r.register("c1", 60).await;
        let (started, interval) = r.cadence_info("c1").await.unwrap();
        assert_eq!(started, 100, "cadence clock preserved across re-register");
        assert_eq!(interval, 60, "interval refreshed");
    }

    #[tokio::test]
    async fn try_acquire_returns_none_while_held() {
        let r = WatchedFolderRegistry::new();
        r.register("c1", 120).await;
        let g1 = r.try_acquire("c1").await;
        assert!(g1.is_some());
        let g2 = r.try_acquire("c1").await;
        assert!(g2.is_none(), "second acquire on the same corpus must return None");
        drop(g1);
        // After the first guard drops, the lock is available again.
        let g3 = r.try_acquire("c1").await;
        assert!(g3.is_some());
    }

    #[tokio::test]
    async fn deregister_removes_corpus() {
        let r = WatchedFolderRegistry::new();
        r.register("c1", 120).await;
        assert!(r.is_registered("c1").await);
        r.deregister("c1").await;
        assert!(!r.is_registered("c1").await);
    }
}
