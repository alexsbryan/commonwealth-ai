//! `StateStoreObserver` — a write-path notification trait.
//!
//! `SqliteStateStore` and `PostgresStateStore` fire events on this
//! observer **after** transactions commit so downstream consumers
//! (notably the KnowledgeView manager, which debounces Tier-3 full
//! enrichment) can react without inlining their concerns into the
//! store.
//!
//! ## Invariants
//!
//! 1. **Fire after commit, not before.** A committed row that is never
//!    announced is recoverable — the next Tier-3 sweep picks it up.
//!    An announced row that rolled back is a phantom that corrupts
//!    the enriched index.
//!
//! 2. **Default methods are no-ops.** Adding a new notification method
//!    must not break existing observer implementations. Consumers that
//!    don't care about a particular event simply inherit the default.
//!
//! 3. **Notifications are best-effort.** The observer MUST NOT block
//!    the write path with synchronous long-running work. Enqueue to a
//!    debouncer or background task instead.
//!
//! 4. **Panics are isolated.** The store calls the observer inside a
//!    `catch_unwind` wrapper in debug builds; a panicking observer
//!    must never take the write path down with it. In release builds
//!    the call is uncaught but the "after commit" ordering means the
//!    write has already succeeded.
//!
//! The default-registered observer is [`NoopObserver`], so code that
//! does not opt into KnowledgeView wiring sees zero behaviour change.

use std::sync::Arc;

/// Observer trait for write-side state-store events. Thread-safe by
/// construction (`Send + Sync`). Concrete implementations typically
/// hold an `Arc` to a long-lived manager and forward events.
pub trait StateStoreObserver: Send + Sync {
    /// A memory row was inserted or updated. `memory_id` is the
    /// primary key of the row in the `memories` table.
    #[allow(unused_variables)]
    fn on_memory_written(&self, memory_id: &str) {}

    /// A message was written — the `conversations` row was upserted
    /// and the `messages` row inserted. `conversation_id` is the
    /// conversation the message belongs to.
    #[allow(unused_variables)]
    fn on_message_written(&self, conversation_id: &str) {}

    /// A conversation was soft-deleted (its `deleted_at` was set).
    /// The KnowledgeView conversational acquirer uses this to drop
    /// the conversation's chunks from the enriched index.
    #[allow(unused_variables)]
    fn on_conversation_deleted(&self, conversation_id: &str) {}
}

/// No-op observer. Used by default so stores constructed without
/// KnowledgeView plumbing behave exactly as before.
pub struct NoopObserver;

impl StateStoreObserver for NoopObserver {}

/// Shared-ownership handle that stores hold onto. An `Arc<dyn
/// StateStoreObserver>` makes swapping the observer at runtime cheap
/// (e.g., tests construct a mock observer without re-opening the DB).
pub type SharedStateStoreObserver = Arc<dyn StateStoreObserver>;

/// Convenience constructor: produce a [`SharedStateStoreObserver`]
/// that drops every event. Equivalent to
/// `Arc::new(NoopObserver) as SharedStateStoreObserver`.
pub fn noop_observer() -> SharedStateStoreObserver {
    Arc::new(NoopObserver)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counter {
        memory_writes: AtomicUsize,
        message_writes: AtomicUsize,
        deletes: AtomicUsize,
    }

    impl StateStoreObserver for Counter {
        fn on_memory_written(&self, _: &str) {
            self.memory_writes.fetch_add(1, Ordering::SeqCst);
        }
        fn on_message_written(&self, _: &str) {
            self.message_writes.fetch_add(1, Ordering::SeqCst);
        }
        fn on_conversation_deleted(&self, _: &str) {
            self.deletes.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn noop_observer_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>(_: &T) {}
        let obs = NoopObserver;
        assert_send_sync(&obs);
    }

    #[test]
    fn counter_observer_receives_all_events() {
        let counter = Arc::new(Counter {
            memory_writes: AtomicUsize::new(0),
            message_writes: AtomicUsize::new(0),
            deletes: AtomicUsize::new(0),
        });
        let obs: SharedStateStoreObserver = counter.clone();
        obs.on_memory_written("m1");
        obs.on_memory_written("m2");
        obs.on_message_written("c1");
        obs.on_conversation_deleted("c1");
        assert_eq!(counter.memory_writes.load(Ordering::SeqCst), 2);
        assert_eq!(counter.message_writes.load(Ordering::SeqCst), 1);
        assert_eq!(counter.deletes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn noop_observer_factory() {
        let obs = noop_observer();
        obs.on_memory_written("ignored");
        obs.on_message_written("ignored");
        obs.on_conversation_deleted("ignored");
    }
}
