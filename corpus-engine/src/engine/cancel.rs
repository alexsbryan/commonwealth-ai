//! Cooperative cancellation for long-running ingest tasks.
//!
//! The ingest loop is CPU-bound and holds a LanceDB writer open for hours
//! on a full Wikipedia run. We cannot simply abort the task — partial
//! LanceDB state would be corrupted — so we use cooperative cancellation:
//! the loop polls a boolean flag at safe points (between embed batches
//! and before each tier-2 flush), and on cancel returns cleanly so the
//! caller can wipe the partition directory without risking a half-written
//! transaction.
//!
//! `CancellationRegistry` is the single source of truth for "is an ingest
//! of `<corpus_id>` currently cancellable?" across the daemon. The
//! Desktop-originated install path and the peer `ingest_partition` HTTP
//! handler both register their flag here so a user-initiated cancel from
//! the UI stops either task.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

/// A shared, clonable "please stop" signal for one ingest task.
///
/// Backed by `Arc<AtomicBool>` so clones observe the same state. The flag
/// is set via [`cancel`](Self::cancel) (idempotent) and read via
/// [`is_cancelled`](Self::is_cancelled). `SeqCst` ordering is more than
/// strictly necessary but keeps the reasoning trivial — cancellation is
/// a once-per-task event, not a hot path.
#[derive(Debug, Clone, Default)]
pub struct CancellationFlag {
    flag: Arc<AtomicBool>,
}

impl CancellationFlag {
    pub fn new() -> Self {
        Self { flag: Arc::new(AtomicBool::new(false)) }
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// Registry of cancellation flags keyed by `corpus_id`.
///
/// Cheap to clone — the inner `Arc<RwLock<HashMap<…>>>` is shared so all
/// handles operate on the same table. Callers should use the typical
/// lifecycle:
///
/// ```ignore
/// let flag = registry.register("wikipedia");
/// // … long-running work, polls `flag.is_cancelled()` …
/// registry.unregister("wikipedia");
/// ```
///
/// `register` is idempotent: if an entry already exists it is returned
/// unchanged. This lets two concurrent tasks for the same corpus share a
/// single flag — useful when the coordinator's local partition and a
/// peer-ingesting partition both run on the same node.
#[derive(Debug, Default, Clone)]
pub struct CancellationRegistry {
    inner: Arc<RwLock<HashMap<String, CancellationFlag>>>,
}

impl CancellationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return an existing flag for `corpus_id` if one is registered.
    pub fn get(&self, corpus_id: &str) -> Option<CancellationFlag> {
        self.inner
            .read()
            .expect("cancel registry poisoned")
            .get(corpus_id)
            .cloned()
    }

    /// Register a flag for `corpus_id`. If an entry already exists it is
    /// returned unchanged — concurrent ingest tasks for the same corpus
    /// share cancellation.
    pub fn register(&self, corpus_id: &str) -> CancellationFlag {
        let mut map = self.inner.write().expect("cancel registry poisoned");
        map.entry(corpus_id.to_string())
            .or_default()
            .clone()
    }

    /// Fire cancellation for `corpus_id` if a flag is registered.
    /// Returns true when a flag was found and signalled.
    pub fn cancel(&self, corpus_id: &str) -> bool {
        if let Some(flag) = self.get(corpus_id) {
            flag.cancel();
            true
        } else {
            false
        }
    }

    /// Remove the flag for `corpus_id` from the registry.
    /// Safe to call even when no flag is registered.
    pub fn unregister(&self, corpus_id: &str) {
        self.inner
            .write()
            .expect("cancel registry poisoned")
            .remove(corpus_id);
    }

    /// Number of registered flags. Useful for diagnostic endpoints.
    pub fn len(&self) -> usize {
        self.inner.read().expect("cancel registry poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_is_not_cancelled_initially() {
        let flag = CancellationFlag::new();
        assert!(!flag.is_cancelled());
    }

    #[test]
    fn flag_clones_share_state() {
        let a = CancellationFlag::new();
        let b = a.clone();
        a.cancel();
        assert!(b.is_cancelled());
    }

    #[test]
    fn register_and_get_round_trip() {
        let reg = CancellationRegistry::new();
        assert!(reg.get("wikipedia").is_none());
        let _ = reg.register("wikipedia");
        assert!(reg.get("wikipedia").is_some());
    }

    #[test]
    fn register_returns_existing_flag_on_second_call() {
        let reg = CancellationRegistry::new();
        let first = reg.register("wikipedia");
        let second = reg.register("wikipedia");
        first.cancel();
        assert!(
            second.is_cancelled(),
            "second register must return the same flag as the first"
        );
    }

    #[test]
    fn cancel_propagates_to_previously_registered_flag() {
        let reg = CancellationRegistry::new();
        let flag = reg.register("wikipedia");
        assert!(!flag.is_cancelled());
        assert!(reg.cancel("wikipedia"));
        assert!(flag.is_cancelled());
    }

    #[test]
    fn cancel_returns_false_when_not_registered() {
        let reg = CancellationRegistry::new();
        assert!(!reg.cancel("nonexistent"));
    }

    #[test]
    fn unregister_removes_entry() {
        let reg = CancellationRegistry::new();
        let _ = reg.register("wikipedia");
        reg.unregister("wikipedia");
        assert!(reg.get("wikipedia").is_none());
    }

    #[test]
    fn registry_clones_share_backing_table() {
        let a = CancellationRegistry::new();
        let b = a.clone();
        let flag = a.register("wikipedia");
        assert!(b.get("wikipedia").is_some());
        b.cancel("wikipedia");
        assert!(flag.is_cancelled());
    }
}
