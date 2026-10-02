// SPDX-License-Identifier: AGPL-3.0-or-later
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

use crate::local_corpus::config::SyncMode;

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
    /// Sync cadence policy. `Continuous` corpora sweep on the
    /// scheduler tick using `sweep_interval_secs`. `Manual` corpora
    /// are skipped on the tick — they only sweep when
    /// `manual_sync_pending` is set (typically by the
    /// `/internal/corpus/watch/sync-now/{id}` route).
    sync_mode: SyncMode,
    /// Pending manual-sync flag. Only meaningful when
    /// `sync_mode == Manual`. The scheduler clears it on the same
    /// tick it dispatches the sweep, mirroring the state-file
    /// flag the worker also clears post-sweep. Defence in depth:
    /// either layer alone keeps Manual cadence honest.
    manual_sync_pending: bool,
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
    /// sweep interval and sync_mode but preserves the lock +
    /// `last_started_unix` (so a re-register doesn't reset the
    /// cadence clock) and any in-flight `manual_sync_pending` flag.
    pub async fn register(&self, corpus_id: impl Into<String>, sweep_interval_secs: u64) {
        self.register_with_mode(corpus_id, sweep_interval_secs, SyncMode::Continuous)
            .await;
    }

    /// Same as `register` but with an explicit sync_mode. The
    /// no-mode form keeps the pre-v1 callers (manager auto-resume,
    /// integration tests) working without touching every site;
    /// production registration paths thread the actual mode through.
    pub async fn register_with_mode(
        &self,
        corpus_id: impl Into<String>,
        sweep_interval_secs: u64,
        sync_mode: SyncMode,
    ) {
        let id = corpus_id.into();
        let mut slots = self.slots.write().await;
        let entry = slots.entry(id).or_insert_with(|| CorpusSlot {
            lock: Arc::new(Mutex::new(())),
            last_started_unix: 0,
            sweep_interval_secs,
            sync_mode,
            manual_sync_pending: false,
        });
        entry.sweep_interval_secs = sweep_interval_secs;
        entry.sync_mode = sync_mode;
    }

    /// Flip the pending flag for a Manual-mode corpus. The next
    /// scheduler tick dispatches the sweep and clears the flag.
    /// No-op for Continuous corpora — the scheduler ignores the
    /// flag on those, so flipping it has no effect either way.
    /// Returns `true` if the corpus was registered (i.e. the
    /// caller should expect a sweep), `false` otherwise.
    pub async fn request_manual_sync(&self, corpus_id: &str) -> bool {
        let mut slots = self.slots.write().await;
        match slots.get_mut(corpus_id) {
            Some(slot) => {
                slot.manual_sync_pending = true;
                true
            }
            None => false,
        }
    }

    /// `true` if the corpus is registered AND its sync_mode is
    /// `Manual`. The HTTP `/sync-now` handler reads this to return
    /// 409 Conflict when the caller targets a Continuous corpus
    /// (which would be a no-op).
    pub async fn is_manual(&self, corpus_id: &str) -> bool {
        self.slots
            .read()
            .await
            .get(corpus_id)
            .map(|s| s.sync_mode == SyncMode::Manual)
            .unwrap_or(false)
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
    /// "is this corpus due?" check. Continuous-mode only — for
    /// Manual the scheduler reads `manual_dispatch` instead.
    pub async fn cadence_info(&self, corpus_id: &str) -> Option<(u64, u64)> {
        self.slots
            .read()
            .await
            .get(corpus_id)
            .map(|s| (s.last_started_unix, s.sweep_interval_secs))
    }

    /// Snapshot the per-corpus dispatch decision the scheduler needs
    /// in one read-locked pass. Returning a single struct means the
    /// scheduler doesn't have to call `is_manual` + `cadence_info` +
    /// re-acquire the lock per tick.
    ///
    /// `Some(DispatchDecision::Due { take_pending })` means the
    /// scheduler should run the corpus now and call
    /// `clear_pending_if_due(corpus_id)` to consume the flag (when
    /// `take_pending == true`). `Some(DispatchDecision::NotDue)`
    /// means skip this tick. `None` means the corpus was
    /// deregistered between tick start and this read.
    pub async fn dispatch_decision(
        &self,
        corpus_id: &str,
        now_unix: u64,
        floor_secs: u64,
    ) -> Option<DispatchDecision> {
        let slots = self.slots.read().await;
        let slot = slots.get(corpus_id)?;
        let decision = match slot.sync_mode {
            SyncMode::Continuous => {
                let interval = slot.sweep_interval_secs.max(floor_secs);
                let due = slot.last_started_unix == 0
                    || now_unix.saturating_sub(slot.last_started_unix) >= interval;
                if due {
                    DispatchDecision::Due {
                        take_pending: false,
                    }
                } else {
                    DispatchDecision::NotDue
                }
            }
            SyncMode::Manual => {
                if slot.manual_sync_pending {
                    DispatchDecision::Due { take_pending: true }
                } else {
                    DispatchDecision::NotDue
                }
            }
        };
        Some(decision)
    }

    /// Atomically clear the `manual_sync_pending` flag. Called by
    /// the scheduler immediately after dispatching a Manual sweep
    /// so the same pending request doesn't fire repeatedly. The
    /// worker also clears the on-disk state mirror for restart
    /// resilience.
    pub async fn clear_manual_pending(&self, corpus_id: &str) {
        if let Some(slot) = self.slots.write().await.get_mut(corpus_id) {
            slot.manual_sync_pending = false;
        }
    }
}

/// Per-corpus scheduler dispatch decision. See
/// `WatchedFolderRegistry::dispatch_decision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchDecision {
    /// Run the sweep now. `take_pending` is `true` for a Manual
    /// dispatch (caller should clear `manual_sync_pending` on the
    /// same tick); `false` for a Continuous dispatch.
    Due { take_pending: bool },
    /// Skip this tick.
    NotDue,
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
        assert!(
            g2.is_none(),
            "second acquire on the same corpus must return None"
        );
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

    #[tokio::test]
    async fn continuous_dispatch_decision_due_on_first_tick() {
        let r = WatchedFolderRegistry::new();
        r.register_with_mode("c1", 120, SyncMode::Continuous).await;
        // last_started == 0 → due immediately on first tick
        let d = r.dispatch_decision("c1", 1000, 60).await;
        assert_eq!(
            d,
            Some(DispatchDecision::Due {
                take_pending: false
            })
        );
    }

    #[tokio::test]
    async fn continuous_dispatch_decision_respects_interval() {
        let r = WatchedFolderRegistry::new();
        r.register_with_mode("c1", 120, SyncMode::Continuous).await;
        r.mark_started("c1", 1000).await;
        // 60 seconds in: not due
        assert_eq!(
            r.dispatch_decision("c1", 1060, 60).await,
            Some(DispatchDecision::NotDue),
        );
        // 120 seconds in: due
        assert_eq!(
            r.dispatch_decision("c1", 1120, 60).await,
            Some(DispatchDecision::Due {
                take_pending: false
            }),
        );
    }

    #[tokio::test]
    async fn continuous_dispatch_decision_floors_below_60s() {
        let r = WatchedFolderRegistry::new();
        // Caller asked for a 10-second sweep — the floor must clamp
        // it to 60 so a misconfigured corpus can't hammer the disk.
        r.register_with_mode("c1", 10, SyncMode::Continuous).await;
        r.mark_started("c1", 1000).await;
        assert_eq!(
            r.dispatch_decision("c1", 1030, 60).await,
            Some(DispatchDecision::NotDue),
        );
        assert_eq!(
            r.dispatch_decision("c1", 1060, 60).await,
            Some(DispatchDecision::Due {
                take_pending: false
            }),
        );
    }

    #[tokio::test]
    async fn manual_dispatch_decision_skips_until_pending_set() {
        let r = WatchedFolderRegistry::new();
        r.register_with_mode("c1", 120, SyncMode::Manual).await;

        // Manual + no pending → NotDue regardless of elapsed time.
        assert_eq!(
            r.dispatch_decision("c1", 9_999_999, 60).await,
            Some(DispatchDecision::NotDue),
        );

        // request_manual_sync flips pending → Due with take_pending.
        let registered = r.request_manual_sync("c1").await;
        assert!(registered);
        assert_eq!(
            r.dispatch_decision("c1", 0, 60).await,
            Some(DispatchDecision::Due { take_pending: true }),
        );

        // Scheduler clears the flag on dispatch.
        r.clear_manual_pending("c1").await;
        assert_eq!(
            r.dispatch_decision("c1", 0, 60).await,
            Some(DispatchDecision::NotDue),
            "pending must clear after dispatch — same request mustn't re-fire"
        );
    }

    #[tokio::test]
    async fn request_manual_sync_returns_false_for_unknown_corpus() {
        let r = WatchedFolderRegistry::new();
        assert!(!r.request_manual_sync("never-registered").await);
    }

    #[tokio::test]
    async fn is_manual_distinguishes_modes() {
        let r = WatchedFolderRegistry::new();
        r.register_with_mode("cont", 120, SyncMode::Continuous)
            .await;
        r.register_with_mode("man", 120, SyncMode::Manual).await;
        assert!(!r.is_manual("cont").await);
        assert!(r.is_manual("man").await);
        assert!(!r.is_manual("missing").await);
    }
}
