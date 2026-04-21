use std::time::{SystemTime, UNIX_EPOCH};

use sovereign_core::traits::*;
use sovereign_core::types::*;

use sovereign_store::memory::InMemoryStateStore;
use sovereign_store::sqlite::SqliteStateStore;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn make_message(id: &str, convo: &str, role: Role, content: &str) -> Message {
    Message {
        id: id.to_string(),
        conversation_id: convo.to_string(),
        role,
        content: content.to_string(),
        created_at: now(),
        metadata: None,
        version: 0,
    }
}

fn make_task(id: &str, convo: &str) -> Task {
    Task {
        id: id.to_string(),
        conversation_id: convo.to_string(),
        goal: "test goal".to_string(),
        plan: Plan {
            id: id.to_string(),
            goal: "test".to_string(),
            steps: vec![Step {
                id: 0,
                description: "step 0".to_string(),
                kind: StepKind::Reason {
                    prompt_template: "do it".to_string(),
                    speed: Speed::Fast,
                },
                requires_approval: false,
                inputs: vec![],
            sampling: None,
            evaluation: None,
            }],
            edges: vec![],
        },
        status: TaskStatus::Running,
        completed_steps: vec![(0, StepOutput::Text("done".to_string()))],
        created_at: now(),
        updated_at: now(),
        version: 0,
    }
}

// ─── Generic StateStore Tests (run against both impls) ─────────

async fn test_save_and_get_conversation(store: &dyn StateStore) {
    store
        .save_message(&make_message("m1", "c1", Role::User, "hello"))
        .await
        .unwrap();
    store
        .save_message(&make_message("m2", "c1", Role::Assistant, "hi there"))
        .await
        .unwrap();

    let convo = store.get_conversation("c1").await.unwrap();
    assert_eq!(convo.id, "c1");
    assert_eq!(convo.messages.len(), 2);
    assert_eq!(convo.messages[0].content, "hello");
    assert_eq!(convo.messages[1].content, "hi there");
}

async fn test_get_missing_conversation(store: &dyn StateStore) {
    let result = store.get_conversation("nonexistent").await;
    assert!(result.is_err());
}

async fn test_list_conversations(store: &dyn StateStore) {
    store
        .save_message(&make_message("m1", "c1", Role::User, "first"))
        .await
        .unwrap();
    store
        .save_message(&make_message("m2", "c2", Role::User, "second"))
        .await
        .unwrap();

    let convos = store.list_conversations(10, 0).await.unwrap();
    assert_eq!(convos.len(), 2);

    // Pagination.
    let page = store.list_conversations(1, 0).await.unwrap();
    assert_eq!(page.len(), 1);

    let page2 = store.list_conversations(1, 1).await.unwrap();
    assert_eq!(page2.len(), 1);
}

async fn test_delete_conversation(store: &dyn StateStore) {
    store
        .save_message(&make_message("m1", "c1", Role::User, "hello"))
        .await
        .unwrap();

    store.delete_conversation("c1").await.unwrap();

    let result = store.get_conversation("c1").await;
    assert!(result.is_err());
}

async fn test_save_and_get_task(store: &dyn StateStore) {
    // First create the conversation (for FK constraint in SQLite).
    store
        .save_message(&make_message("m1", "c1", Role::User, "hi"))
        .await
        .unwrap();

    let task = make_task("t1", "c1");
    store.save_task(&task).await.unwrap();

    let loaded = store.get_task("t1").await.unwrap();
    assert_eq!(loaded.id, "t1");
    assert_eq!(loaded.goal, "test goal");
    assert!(matches!(loaded.status, TaskStatus::Running));
    assert_eq!(loaded.plan.steps.len(), 1);
    assert_eq!(loaded.completed_steps.len(), 1);
}

async fn test_get_missing_task(store: &dyn StateStore) {
    let result = store.get_task("missing").await;
    assert!(result.is_err());
}

async fn test_permissions(store: &dyn StateStore) {
    // Initially unset.
    let perm = store.get_permission("shell", "exec").await.unwrap();
    assert!(perm.is_none());

    // Grant.
    store.set_permission("shell", "exec", true).await.unwrap();
    let perm = store.get_permission("shell", "exec").await.unwrap();
    assert_eq!(perm, Some(true));

    // Revoke.
    store.set_permission("shell", "exec", false).await.unwrap();
    let perm = store.get_permission("shell", "exec").await.unwrap();
    assert_eq!(perm, Some(false));
}

async fn test_memory_save_and_retrieve(store: &dyn StateStore) {
    let mem1 = Memory {
        id: "mem1".to_string(),
        content: "User prefers Rust programming".to_string(),
        source: "test".to_string(),
        confidence: 1.0,
        created_at: now(),
        last_used: now(),
        version: 0,
        deleted_at: None,
        source_conversation_id: None,
    };
    let mem2 = Memory {
        id: "mem2".to_string(),
        content: "User lives in Portland Oregon".to_string(),
        source: "test".to_string(),
        confidence: 1.0,
        created_at: now(),
        last_used: now(),
        version: 0,
        deleted_at: None,
        source_conversation_id: None,
    };
    store.save_memory(&mem1).await.unwrap();
    store.save_memory(&mem2).await.unwrap();

    let all = store.get_all_memories().await.unwrap();
    assert_eq!(all.len(), 2);
}

async fn test_memory_delete(store: &dyn StateStore) {
    let mem = Memory {
        id: "del1".to_string(),
        content: "Temporary fact".to_string(),
        source: "test".to_string(),
        confidence: 1.0,
        created_at: now(),
        last_used: now(),
        version: 0,
        deleted_at: None,
        source_conversation_id: None,
    };
    store.save_memory(&mem).await.unwrap();
    assert_eq!(store.get_all_memories().await.unwrap().len(), 1);

    store.delete_memory("del1").await.unwrap();
    assert!(store.get_all_memories().await.unwrap().is_empty());
}

async fn test_memory_confidence_update(store: &dyn StateStore) {
    let mem = Memory {
        id: "conf1".to_string(),
        content: "Decaying fact".to_string(),
        source: "test".to_string(),
        confidence: 1.0,
        created_at: now(),
        last_used: now(),
        version: 0,
        deleted_at: None,
        source_conversation_id: None,
    };
    store.save_memory(&mem).await.unwrap();
    store.update_memory_confidence("conf1", 0.5).await.unwrap();

    let all = store.get_all_memories().await.unwrap();
    assert_eq!(all.len(), 1);
    assert!((all[0].confidence - 0.5).abs() < 0.01);
}

async fn test_routing_log(store: &dyn StateStore) {
    store
        .log_routing("hash1", "SimpleQuery", 50)
        .await
        .unwrap();
    store
        .log_routing("hash2", "DeepQuery", 100)
        .await
        .unwrap();

    // No corrections yet (all are unknown).
    let corrections = store.get_routing_corrections(10).await.unwrap();
    assert!(corrections.is_empty());

    // Mark one as incorrect.
    store.mark_routing_correct("hash1", false).await.unwrap();
    let corrections = store.get_routing_corrections(10).await.unwrap();
    assert_eq!(corrections.len(), 1);
    assert_eq!(corrections[0].classified_as, "SimpleQuery");
}

async fn test_documents_stub(store: &dyn StateStore) {
    let docs = store
        .search_documents(&[0.1, 0.2], "query", 10)
        .await
        .unwrap();
    assert!(docs.is_empty());
}

// ─── InMemoryStateStore Tests ──────────────────────────────────

#[tokio::test]
async fn memory_save_and_get_conversation() {
    test_save_and_get_conversation(&InMemoryStateStore::new()).await;
}

#[tokio::test]
async fn memory_get_missing_conversation() {
    test_get_missing_conversation(&InMemoryStateStore::new()).await;
}

#[tokio::test]
async fn memory_list_conversations() {
    test_list_conversations(&InMemoryStateStore::new()).await;
}

#[tokio::test]
async fn memory_delete_conversation() {
    test_delete_conversation(&InMemoryStateStore::new()).await;
}

#[tokio::test]
async fn memory_save_and_get_task() {
    test_save_and_get_task(&InMemoryStateStore::new()).await;
}

#[tokio::test]
async fn memory_get_missing_task() {
    test_get_missing_task(&InMemoryStateStore::new()).await;
}

#[tokio::test]
async fn memory_permissions() {
    test_permissions(&InMemoryStateStore::new()).await;
}

#[tokio::test]
async fn memory_save_and_retrieve() {
    test_memory_save_and_retrieve(&InMemoryStateStore::new()).await;
}

#[tokio::test]
async fn memory_delete() {
    test_memory_delete(&InMemoryStateStore::new()).await;
}

#[tokio::test]
async fn memory_confidence_update() {
    test_memory_confidence_update(&InMemoryStateStore::new()).await;
}

#[tokio::test]
async fn memory_routing_log() {
    test_routing_log(&InMemoryStateStore::new()).await;
}

#[tokio::test]
async fn memory_documents_stub() {
    test_documents_stub(&InMemoryStateStore::new()).await;
}

#[tokio::test]
async fn memory_search_messages() {
    let store = InMemoryStateStore::new();
    store
        .save_message(&make_message("m1", "c1", Role::User, "I love Rust programming"))
        .await
        .unwrap();
    store
        .save_message(&make_message("m2", "c1", Role::User, "Python is also good"))
        .await
        .unwrap();

    let results = store.search_messages("rust").await.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].content.contains("Rust"));
}

// ─── SqliteStateStore Tests ────────────────────────────────────

fn sqlite_store() -> SqliteStateStore {
    SqliteStateStore::open_in_memory().unwrap()
}

#[tokio::test]
async fn sqlite_save_and_get_conversation() {
    test_save_and_get_conversation(&sqlite_store()).await;
}

#[tokio::test]
async fn sqlite_get_missing_conversation() {
    test_get_missing_conversation(&sqlite_store()).await;
}

#[tokio::test]
async fn sqlite_list_conversations() {
    test_list_conversations(&sqlite_store()).await;
}

#[tokio::test]
async fn sqlite_delete_conversation() {
    test_delete_conversation(&sqlite_store()).await;
}

#[tokio::test]
async fn sqlite_save_and_get_task() {
    test_save_and_get_task(&sqlite_store()).await;
}

#[tokio::test]
async fn sqlite_get_missing_task() {
    test_get_missing_task(&sqlite_store()).await;
}

#[tokio::test]
async fn sqlite_permissions() {
    test_permissions(&sqlite_store()).await;
}

#[tokio::test]
async fn sqlite_save_and_retrieve_memories() {
    test_memory_save_and_retrieve(&sqlite_store()).await;
}

#[tokio::test]
async fn sqlite_delete_memory() {
    test_memory_delete(&sqlite_store()).await;
}

#[tokio::test]
async fn sqlite_memory_confidence_update() {
    test_memory_confidence_update(&sqlite_store()).await;
}

#[tokio::test]
async fn sqlite_routing_log() {
    test_routing_log(&sqlite_store()).await;
}

#[tokio::test]
async fn sqlite_memory_fts5_retrieval() {
    let store = sqlite_store();
    let mem1 = Memory {
        id: "fts1".to_string(),
        content: "User prefers Rust programming language".to_string(),
        source: "test".to_string(),
        confidence: 1.0,
        created_at: now(),
        last_used: now(),
        version: 0,
        deleted_at: None,
        source_conversation_id: None,
    };
    let mem2 = Memory {
        id: "fts2".to_string(),
        content: "User lives in Portland Oregon".to_string(),
        source: "test".to_string(),
        confidence: 1.0,
        created_at: now(),
        last_used: now(),
        version: 0,
        deleted_at: None,
        source_conversation_id: None,
    };
    store.save_memory(&mem1).await.unwrap();
    store.save_memory(&mem2).await.unwrap();

    // Search for "Rust" — should find only the first memory.
    let results = store.get_relevant_memories("Rust", 10).await.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].content.contains("Rust"));

    // Search for "Portland" — should find only the second memory.
    let results = store.get_relevant_memories("Portland", 10).await.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].content.contains("Portland"));

    // Empty query returns nothing.
    let results = store.get_relevant_memories("", 10).await.unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn sqlite_documents_stub() {
    test_documents_stub(&sqlite_store()).await;
}

#[tokio::test]
async fn sqlite_fts5_search() {
    let store = sqlite_store();
    store
        .save_message(&make_message("m1", "c1", Role::User, "I love Rust programming"))
        .await
        .unwrap();
    store
        .save_message(&make_message("m2", "c1", Role::User, "Python is also great"))
        .await
        .unwrap();
    store
        .save_message(&make_message("m3", "c1", Role::Assistant, "Rust is fast and safe"))
        .await
        .unwrap();

    let results = store.search_messages("Rust").await.unwrap();
    assert_eq!(results.len(), 2);

    let results = store.search_messages("Python").await.unwrap();
    assert_eq!(results.len(), 1);

    let results = store.search_messages("nonexistent").await.unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn sqlite_message_metadata_roundtrip() {
    let store = sqlite_store();
    let mut msg = make_message("m1", "c1", Role::Assistant, "hi");
    msg.metadata = Some(serde_json::json!({
        "model": "test-model",
        "tokens": 42,
        "latency_ms": 100,
    }));
    store.save_message(&msg).await.unwrap();

    let convo = store.get_conversation("c1").await.unwrap();
    let loaded = &convo.messages[0];
    let meta = loaded.metadata.as_ref().unwrap();
    assert_eq!(meta["model"], "test-model");
    assert_eq!(meta["tokens"], 42);
}

#[tokio::test]
async fn sqlite_multiple_conversations_isolated() {
    let store = sqlite_store();
    store
        .save_message(&make_message("m1", "c1", Role::User, "convo 1"))
        .await
        .unwrap();
    store
        .save_message(&make_message("m2", "c2", Role::User, "convo 2"))
        .await
        .unwrap();

    let c1 = store.get_conversation("c1").await.unwrap();
    assert_eq!(c1.messages.len(), 1);
    assert_eq!(c1.messages[0].content, "convo 1");

    let c2 = store.get_conversation("c2").await.unwrap();
    assert_eq!(c2.messages.len(), 1);
    assert_eq!(c2.messages[0].content, "convo 2");
}

// ─── Document Source Tests ─────────────────────────────────────

fn make_chunk(id: &str, source: &str, content: &str, index: usize) -> DocumentChunk {
    DocumentChunk {
        id: id.to_string(),
        source: source.to_string(),
        content: content.to_string(),
        chunk_index: index,
        embedding: None,
        created_at: now(),
        source_type: SourceType::UserDocument,
        version: 0,
        deleted_at: None,
    }
}

async fn test_get_chunks_by_source(store: &dyn StateStore) {
    store
        .store_chunks(&[
            make_chunk("a:0", "a.txt", "chunk 0 of a", 0),
            make_chunk("a:1", "a.txt", "chunk 1 of a", 1),
            make_chunk("b:0", "b.txt", "chunk 0 of b", 0),
        ])
        .await
        .unwrap();

    let chunks = store.get_chunks_by_source("a.txt").await.unwrap();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].chunk_index, 0);
    assert_eq!(chunks[1].chunk_index, 1);

    let chunks = store.get_chunks_by_source("b.txt").await.unwrap();
    assert_eq!(chunks.len(), 1);

    let chunks = store.get_chunks_by_source("nonexistent").await.unwrap();
    assert!(chunks.is_empty());
}

async fn test_list_sources(store: &dyn StateStore) {
    store
        .store_chunks(&[
            make_chunk("a:0", "alpha.txt", "content a", 0),
            make_chunk("b:0", "beta.md", "content b", 0),
            make_chunk("a:1", "alpha.txt", "content a2", 1),
        ])
        .await
        .unwrap();

    let sources = store.list_sources().await.unwrap();
    assert_eq!(sources.len(), 2);
    assert!(sources.contains(&"alpha.txt".to_string()));
    assert!(sources.contains(&"beta.md".to_string()));
}

#[tokio::test]
async fn memory_get_chunks_by_source() {
    test_get_chunks_by_source(&InMemoryStateStore::new()).await;
}

#[tokio::test]
async fn memory_list_sources() {
    test_list_sources(&InMemoryStateStore::new()).await;
}

#[tokio::test]
async fn sqlite_get_chunks_by_source() {
    test_get_chunks_by_source(&sqlite_store()).await;
}

#[tokio::test]
async fn sqlite_list_sources() {
    test_list_sources(&sqlite_store()).await;
}
