//! End-to-end verification of `StateStoreObserver` wiring on
//! `SqliteStateStore`.
//!
//! The invariants under test:
//!
//! 1. A `SqliteStateStore::open_in_memory()` WITHOUT
//!    `with_observer` uses `noop_observer` — no panics, no side
//!    effects.
//! 2. `with_observer(Arc<CountingObserver>)` hooks post-commit
//!    notifications for `save_memory` / `save_message` /
//!    `delete_conversation`.
//! 3. The observer fires AFTER the transaction commits — a panic
//!    inside the observer handler does not corrupt the preceding
//!    write. (Shown via a `catch_unwind`-style probe.)
//! 4. Notifications carry the correct ids (memory id for memories,
//!    conversation id for messages + deletions).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sovereign_core::observer::{SharedStateStoreObserver, StateStoreObserver};
use sovereign_core::traits::{ConversationStore, MemoryStore};
use sovereign_core::types::{Memory, Message, Role};
use sovereign_store::sqlite::SqliteStateStore;

fn now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Default)]
struct CountingObserver {
    memory_writes: AtomicUsize,
    message_writes: AtomicUsize,
    deletions: AtomicUsize,
    memory_ids: Mutex<Vec<String>>,
    conversation_ids: Mutex<Vec<String>>,
    deleted_conversation_ids: Mutex<Vec<String>>,
}

impl StateStoreObserver for CountingObserver {
    fn on_memory_written(&self, memory_id: &str) {
        self.memory_writes.fetch_add(1, Ordering::SeqCst);
        self.memory_ids.lock().unwrap().push(memory_id.to_string());
    }
    fn on_message_written(&self, conversation_id: &str) {
        self.message_writes.fetch_add(1, Ordering::SeqCst);
        self.conversation_ids
            .lock()
            .unwrap()
            .push(conversation_id.to_string());
    }
    fn on_conversation_deleted(&self, conversation_id: &str) {
        self.deletions.fetch_add(1, Ordering::SeqCst);
        self.deleted_conversation_ids
            .lock()
            .unwrap()
            .push(conversation_id.to_string());
    }
}

fn mem(id: &str) -> Memory {
    Memory {
        id: id.to_string(),
        content: format!("memory content {id}"),
        source: "test".into(),
        confidence: 1.0,
        created_at: now(),
        last_used: now(),
        version: 0,
        deleted_at: None,
        source_conversation_id: None,
        source_skill_id: None,
        ..Default::default()
    }
}

fn msg(id: &str, conversation_id: &str) -> Message {
    Message {
        id: id.to_string(),
        conversation_id: conversation_id.to_string(),
        role: Role::User,
        content: format!("message content {id}"),
        created_at: now(),
        metadata: None,
        version: 0,
    }
}

#[tokio::test]
async fn default_store_uses_noop_observer() {
    // Smoke test: the default `open_in_memory` constructor is wired
    // with `noop_observer`. Writing memory should not panic even
    // though no observer has been explicitly installed.
    let store = SqliteStateStore::open_in_memory().unwrap();
    store.save_memory(&mem("m1")).await.unwrap();
    // Nothing to assert — the test passes if we didn't panic.
}

#[tokio::test]
async fn observer_fires_on_memory_write() {
    let observer = Arc::new(CountingObserver::default());
    let shared: SharedStateStoreObserver = observer.clone();
    let store = SqliteStateStore::open_in_memory()
        .unwrap()
        .with_observer(shared);

    store.save_memory(&mem("m1")).await.unwrap();
    store.save_memory(&mem("m2")).await.unwrap();
    store.save_memory(&mem("m3")).await.unwrap();

    assert_eq!(observer.memory_writes.load(Ordering::SeqCst), 3);
    assert_eq!(observer.message_writes.load(Ordering::SeqCst), 0);
    assert_eq!(
        observer.memory_ids.lock().unwrap().as_slice(),
        &["m1".to_string(), "m2".to_string(), "m3".to_string()]
    );
}

#[tokio::test]
async fn observer_fires_on_message_write_with_conversation_id() {
    let observer = Arc::new(CountingObserver::default());
    let shared: SharedStateStoreObserver = observer.clone();
    let store = SqliteStateStore::open_in_memory()
        .unwrap()
        .with_observer(shared);

    store.save_message(&msg("msg1", "conv-a")).await.unwrap();
    store.save_message(&msg("msg2", "conv-a")).await.unwrap();
    store.save_message(&msg("msg3", "conv-b")).await.unwrap();

    assert_eq!(observer.message_writes.load(Ordering::SeqCst), 3);
    assert_eq!(observer.memory_writes.load(Ordering::SeqCst), 0);
    // Same conversation id shows up twice; the debouncer is responsible
    // for coalescing. The observer's job is faithful per-write delivery.
    assert_eq!(
        observer.conversation_ids.lock().unwrap().as_slice(),
        &["conv-a".to_string(), "conv-a".to_string(), "conv-b".to_string()]
    );
}

#[tokio::test]
async fn observer_fires_on_conversation_delete() {
    let observer = Arc::new(CountingObserver::default());
    let shared: SharedStateStoreObserver = observer.clone();
    let store = SqliteStateStore::open_in_memory()
        .unwrap()
        .with_observer(shared);

    // Create a conversation by writing a message first, then delete.
    store.save_message(&msg("m1", "conv-a")).await.unwrap();
    store.delete_conversation("conv-a").await.unwrap();

    assert_eq!(observer.deletions.load(Ordering::SeqCst), 1);
    assert_eq!(
        observer.deleted_conversation_ids.lock().unwrap().as_slice(),
        &["conv-a".to_string()]
    );
}

#[tokio::test]
async fn write_still_commits_when_observer_panics() {
    // Post-commit ordering invariant: even if the observer panics,
    // the preceding write has already committed to SQLite. We verify
    // by reading the row back through the store.
    struct PanickingObserver;
    impl StateStoreObserver for PanickingObserver {
        fn on_memory_written(&self, _: &str) {
            // Catch the unwind so it doesn't abort the test.
            let result = std::panic::catch_unwind(|| panic!("observer explodes"));
            drop(result);
        }
    }

    let store = SqliteStateStore::open_in_memory()
        .unwrap()
        .with_observer(Arc::new(PanickingObserver));

    store.save_memory(&mem("committed")).await.unwrap();
    let all = store.get_all_memories().await.unwrap();
    assert_eq!(all.len(), 1, "write committed despite observer panic");
    assert_eq!(all[0].id, "committed");
}

#[tokio::test]
async fn store_catches_naked_observer_panic() {
    // Hardening sweep: the store's `fire_observer` helper now
    // wraps the observer invocation in `catch_unwind`, so an
    // observer that panics WITHOUT its own internal guard must
    // still leave the store usable (no poisoned locks, no aborted
    // test process). Covers the realistic case where a callback
    // dereferences a None and panics on unwrap().
    struct NakedPanickingObserver;
    impl StateStoreObserver for NakedPanickingObserver {
        fn on_memory_written(&self, _: &str) {
            panic!("observer panics without internal catch");
        }
        fn on_message_written(&self, _: &str) {
            panic!("observer panics without internal catch");
        }
    }

    let store = SqliteStateStore::open_in_memory()
        .unwrap()
        .with_observer(Arc::new(NakedPanickingObserver));

    // These would abort the test before the hardening fix.
    store.save_memory(&mem("m-panic-1")).await.unwrap();
    store.save_message(&msg("msg-panic", "conv-panic")).await.unwrap();

    // Store is still usable after a panicking handler.
    let all = store.get_all_memories().await.unwrap();
    assert_eq!(all.len(), 1);
    let conv = store.get_conversation("conv-panic").await.unwrap();
    assert_eq!(conv.messages.len(), 1);

    // A subsequent non-panicking observer must also work (the
    // isolation must not have left any poisoned state behind).
    let good = Arc::new(CountingObserver::default());
    let store2 = SqliteStateStore::open_in_memory()
        .unwrap()
        .with_observer(good.clone() as SharedStateStoreObserver);
    store2.save_memory(&mem("m-post-panic")).await.unwrap();
    assert_eq!(good.memory_writes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn observer_sees_writes_from_multiple_sources() {
    let observer = Arc::new(CountingObserver::default());
    let shared: SharedStateStoreObserver = observer.clone();
    let store = SqliteStateStore::open_in_memory()
        .unwrap()
        .with_observer(shared);

    store.save_memory(&mem("m1")).await.unwrap();
    store.save_message(&msg("msg1", "conv-a")).await.unwrap();
    store.save_memory(&mem("m2")).await.unwrap();
    store.delete_conversation("conv-a").await.unwrap();
    store.save_message(&msg("msg2", "conv-b")).await.unwrap();

    assert_eq!(observer.memory_writes.load(Ordering::SeqCst), 2);
    assert_eq!(observer.message_writes.load(Ordering::SeqCst), 2);
    assert_eq!(observer.deletions.load(Ordering::SeqCst), 1);
}
