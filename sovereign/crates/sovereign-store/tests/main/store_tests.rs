// SPDX-License-Identifier: AGPL-3.0-or-later
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

fn corpus_state(id: &str, visibility: CorpusVisibility) -> CorpusState {
    CorpusState {
        corpus_id: id.to_string(),
        installed_at: now(),
        source_date: "test".to_string(),
        chunks_count: 1,
        index_size_mb: 0,
        last_updated: now(),
        version: 0,
        deleted_at: None,
        vector_index_ready: false,
        visibility,
    }
}

/// A1 + A2b: `Private { owner }` round-trips through the store's new
/// `visibility` column, and `build_context` scopes the corpus set per
/// principal — another principal's Private corpus never enters
/// `installed_corpora`, while the owner's and shared `Org` corpora do, and a
/// `None` principal (single-user / desktop) hides nothing.
#[tokio::test]
async fn build_context_scopes_corpora_by_principal() {
    let store = SqliteStateStore::open_in_memory().unwrap();
    store
        .save_corpus_state(&corpus_state("shared", CorpusVisibility::Org))
        .await
        .unwrap();
    store
        .save_corpus_state(&corpus_state(
            "alice-secret",
            CorpusVisibility::Private {
                owner: "alice".to_string(),
            },
        ))
        .await
        .unwrap();

    // A1: the `Private` visibility round-trips through the column.
    let got = store.get_corpus_state("alice-secret").await.unwrap();
    assert_eq!(
        got.visibility,
        CorpusVisibility::Private {
            owner: "alice".to_string()
        }
    );

    // A2b: Alice owns the private corpus → she retrieves over both.
    let alice = sovereign_core::context::build_context(&store, "alice:c", "q", Some("alice"))
        .await
        .unwrap();
    assert!(alice.installed_corpora.contains(&"shared".to_string()));
    assert!(alice
        .installed_corpora
        .contains(&"alice-secret".to_string()));
    // A3b: the PURE principal ceiling (independent of selection) is what
    // Filter 5 enforces at every corpus-chunk search. Alice owns the private
    // corpus → both are in her ceiling.
    let alice_ceiling = alice
        .corpus_ceiling
        .expect("a principal was supplied ⇒ ceiling must be Some");
    assert!(alice_ceiling.contains(&"shared".to_string()));
    assert!(alice_ceiling.contains(&"alice-secret".to_string()));

    // Bob does NOT own it → he retrieves over the shared `Org` corpus only.
    let bob = sovereign_core::context::build_context(&store, "bob:c", "q", Some("bob"))
        .await
        .unwrap();
    assert!(bob.installed_corpora.contains(&"shared".to_string()));
    assert!(
        !bob.installed_corpora.contains(&"alice-secret".to_string()),
        "ISOLATION LEAK: Bob's retrieval scope includes Alice's private corpus: {:?}",
        bob.installed_corpora
    );
    // A3b: and his CEILING — the airtight Filter-5 bound, which a forged or
    // absent `enabled_corpora` cannot widen past — excludes it too.
    let bob_ceiling = bob
        .corpus_ceiling
        .expect("a principal was supplied ⇒ ceiling must be Some");
    assert!(bob_ceiling.contains(&"shared".to_string()));
    assert!(
        !bob_ceiling.contains(&"alice-secret".to_string()),
        "ISOLATION LEAK: Bob's retrieval CEILING includes Alice's private corpus: {:?}",
        bob_ceiling
    );

    // No principal (single-user / desktop) → nothing is hidden.
    let solo = sovereign_core::context::build_context(&store, "c", "q", None)
        .await
        .unwrap();
    assert!(solo.installed_corpora.contains(&"shared".to_string()));
    assert!(solo.installed_corpora.contains(&"alice-secret".to_string()));
    // A3b: a `None` principal carries NO ceiling, so Filter 5 is a no-op and
    // retrieval is bit-identical to pre-multi-tenant behaviour.
    assert!(
        solo.corpus_ceiling.is_none(),
        "single-user path must carry no ceiling (None), got {:?}",
        solo.corpus_ceiling
    );
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
        source_skill_id: None,
        ..Default::default()
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
        source_skill_id: None,
        ..Default::default()
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
        source_skill_id: None,
        ..Default::default()
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
        source_skill_id: None,
        ..Default::default()
    };
    store.save_memory(&mem).await.unwrap();
    store.update_memory_confidence("conf1", 0.5).await.unwrap();

    let all = store.get_all_memories().await.unwrap();
    assert_eq!(all.len(), 1);
    assert!((all[0].confidence - 0.5).abs() < 0.01);
}

async fn test_routing_log(store: &dyn StateStore) {
    store.log_routing("hash1", "SimpleQuery", 50).await.unwrap();
    store.log_routing("hash2", "DeepQuery", 100).await.unwrap();

    // No corrections yet (all are unknown).
    let corrections = store.get_routing_corrections(10).await.unwrap();
    assert!(corrections.is_empty());

    // Mark one as incorrect.
    store.mark_routing_correct("hash1", false).await.unwrap();
    let corrections = store.get_routing_corrections(10).await.unwrap();
    assert_eq!(corrections.len(), 1);
    assert_eq!(corrections[0].classified_as, "SimpleQuery");

    // PR4 — mark_routing_redirected is a best-effort write. No
    // public read path on the trait, so we rely on it not erroring
    // (sqlite UPDATE affects 0 rows on a missing hash — still Ok).
    // The full read-through-sqlite path is exercised by
    // routing_moves.rs in sovereign-core.
    store
        .mark_routing_redirected("hash1", "deep_query")
        .await
        .unwrap();
    // Unknown hash also shouldn't error (0 rows updated is fine).
    store
        .mark_routing_redirected("nonexistent", "knowledge_query")
        .await
        .unwrap();
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
        .save_message(&make_message(
            "m1",
            "c1",
            Role::User,
            "I love Rust programming",
        ))
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
        source_skill_id: None,
        ..Default::default()
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
        source_skill_id: None,
        ..Default::default()
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
        .save_message(&make_message(
            "m1",
            "c1",
            Role::User,
            "I love Rust programming",
        ))
        .await
        .unwrap();
    store
        .save_message(&make_message(
            "m2",
            "c1",
            Role::User,
            "Python is also great",
        ))
        .await
        .unwrap();
    store
        .save_message(&make_message(
            "m3",
            "c1",
            Role::Assistant,
            "Rust is fast and safe",
        ))
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

// ─── RAPTOR Atlas roundtrip ────────────────────────────────────

fn make_asset(id: &str) -> DocumentAsset {
    DocumentAsset {
        id: id.to_string(),
        title: "Test Doc".to_string(),
        filename: "test.txt".to_string(),
        file_size_mb: 0.1,
        word_count: 100,
        chunk_count: 4,
        document_type: DocumentTypeTag::Narrative,
        ingested_at: chrono::Utc::now(),
        index_id: format!("asset:{id}"),
        skeleton: None,
        state: AssetState::Pending,
        owner: None,
    }
}

fn make_raptor_node(id: &str, level: u8, children: Vec<String>, members: Vec<u32>) -> RaptorNode {
    RaptorNode {
        node_id: id.to_string(),
        level,
        summary: format!("summary for {id}"),
        summary_embedding: vec![0.1, 0.2, 0.3, 0.4],
        centroid_embedding: vec![0.5, 0.6, 0.7, 0.8],
        children_node_ids: children,
        direct_member_chunk_ids: members.clone(),
        evidence_chunk_ids: members,
        quote_spans: vec![QuoteSpan {
            chunk_id: 1,
            char_start: 10,
            char_end: 50,
            text: "verbatim quote from the source chunk".to_string(),
        }],
        primary_entities: vec!["Winnie".to_string(), "Stevie".to_string()],
        cluster_coherence: 0.85,
        created_at: chrono::Utc::now(),
        prompt_version: String::new(),
        summarizer_model: String::new(),
    }
}

#[tokio::test]
async fn sqlite_raptor_node_roundtrip() {
    let store = sqlite_store();
    let asset_id = "doc-raptor-1";
    store
        .save_document_asset(&make_asset(asset_id))
        .await
        .unwrap();

    let leaf_a = make_raptor_node("leaf-a", 0, vec![], vec![0, 1, 2]);
    let leaf_b = make_raptor_node("leaf-b", 0, vec![], vec![3, 4, 5]);
    let parent = make_raptor_node(
        "root",
        1,
        vec!["leaf-a".to_string(), "leaf-b".to_string()],
        vec![],
    );

    store
        .save_raptor_nodes(asset_id, &[leaf_a.clone(), leaf_b.clone(), parent.clone()])
        .await
        .unwrap();

    let loaded = store.list_raptor_nodes(asset_id).await.unwrap();
    assert_eq!(loaded.len(), 3);
    // Ordered by level ASC — leaves first, then root.
    assert_eq!(loaded[0].level, 0);
    assert_eq!(loaded[2].level, 1);
    assert_eq!(loaded[2].children_node_ids.len(), 2);

    let fetched_leaf = store.get_raptor_node("leaf-a").await.unwrap().unwrap();
    assert_eq!(fetched_leaf.summary, "summary for leaf-a");
    assert_eq!(fetched_leaf.summary_embedding, vec![0.1, 0.2, 0.3, 0.4]);
    assert_eq!(
        fetched_leaf.quote_spans[0].text,
        "verbatim quote from the source chunk"
    );

    // The parent has empty direct_member_chunk_ids (NULL on disk),
    // and the round-trip should keep it empty (not error on missing column).
    let fetched_parent = store.get_raptor_node("root").await.unwrap().unwrap();
    assert!(fetched_parent.direct_member_chunk_ids.is_empty());

    // Re-saving replaces existing nodes atomically.
    let updated_leaf = make_raptor_node("leaf-a", 0, vec![], vec![0, 1, 2, 99]);
    store
        .save_raptor_nodes(asset_id, &[updated_leaf])
        .await
        .unwrap();
    let after_replace = store.list_raptor_nodes(asset_id).await.unwrap();
    assert_eq!(after_replace.len(), 1);
    assert_eq!(after_replace[0].direct_member_chunk_ids, vec![0, 1, 2, 99]);

    // Cascade: deleting the asset removes its raptor nodes.
    store.delete_document_asset(asset_id).await.unwrap();
    let after_delete = store.list_raptor_nodes(asset_id).await.unwrap();
    assert!(after_delete.is_empty());
}

#[tokio::test]
async fn sqlite_asset_motif_roundtrip() {
    let store = sqlite_store();
    let asset_id = "doc-motif-1";
    store
        .save_document_asset(&make_asset(asset_id))
        .await
        .unwrap();

    let motifs = vec![
        AssetMotif {
            term: "incurious".to_string(),
            tf_idf_score: 9.8,
            occurrence_chunk_ids: vec![234, 567, 712, 891, 943],
            is_distinctive: true,
        },
        AssetMotif {
            term: "circles".to_string(),
            tf_idf_score: 7.1,
            occurrence_chunk_ids: vec![78, 956],
            is_distinctive: true,
        },
        AssetMotif {
            term: "frill".to_string(),
            tf_idf_score: 4.2,
            occurrence_chunk_ids: vec![412],
            is_distinctive: false,
        },
    ];
    store.save_asset_motifs(asset_id, &motifs).await.unwrap();

    let loaded = store.list_asset_motifs(asset_id).await.unwrap();
    assert_eq!(loaded.len(), 3);
    // Distinctive first, then by tf_idf_score DESC.
    assert_eq!(loaded[0].term, "incurious");
    assert_eq!(loaded[1].term, "circles");
    assert_eq!(loaded[2].term, "frill");
    assert!(!loaded[2].is_distinctive);

    // Re-saving replaces.
    store
        .save_asset_motifs(
            asset_id,
            &[AssetMotif {
                term: "professor".to_string(),
                tf_idf_score: 8.3,
                occurrence_chunk_ids: vec![1, 2, 3],
                is_distinctive: true,
            }],
        )
        .await
        .unwrap();
    let after_replace = store.list_asset_motifs(asset_id).await.unwrap();
    assert_eq!(after_replace.len(), 1);
    assert_eq!(after_replace[0].term, "professor");

    // Cascade.
    store.delete_document_asset(asset_id).await.unwrap();
    let after_delete = store.list_asset_motifs(asset_id).await.unwrap();
    assert!(after_delete.is_empty());
}

// ─── Phase B incremental chunk-entity tests ──────────────────────────
//
// `list_extracted_chunk_ids_for_corpus` is the membership lookup the
// Phase B incremental hook uses to compute the delta between Lance
// chunks and `chunk_entities` rows. Spec:
// `sovereign/docs/specs/PROGRESSIVE_ENRICHMENT.md` §B.

fn mk_entity_row(
    corpus_id: &str,
    chunk_id: u64,
    text: &str,
) -> sovereign_core::conv_tiered::ChunkEntityRow {
    sovereign_core::conv_tiered::ChunkEntityRow {
        corpus_id: corpus_id.to_string(),
        chunk_id,
        text: text.to_string(),
        label: "Person".to_string(),
        char_start: 0,
        char_end: text.len() as i64,
        score: 0.85,
        conv_uuid: Some("conv-x".to_string()),
        extracted_at: 42,
    }
}

#[tokio::test]
async fn list_extracted_chunk_ids_empty_for_unseen_corpus() {
    let store = sqlite_store();
    let ids = store
        .list_extracted_chunk_ids_for_corpus("never-seen")
        .await
        .unwrap();
    assert!(ids.is_empty());
}

#[tokio::test]
async fn list_extracted_chunk_ids_unions_for_and_non_grouped_writes() {
    let store = sqlite_store();
    // Path 1 — conv-grouped write (the path Phase A backfill uses).
    store
        .save_chunk_entities_for_conv(
            "corpus-a",
            "conv-x",
            &[
                mk_entity_row("corpus-a", 10, "Borges"),
                mk_entity_row("corpus-a", 11, "Bach"),
            ],
        )
        .await
        .unwrap();
    // Path 2 — non-destructive append (the path Phase B incremental uses).
    store
        .save_chunk_entities(&[mk_entity_row("corpus-a", 12, "Italo Calvino")])
        .await
        .unwrap();
    // Sibling corpus — must not appear in the corpus-a result set.
    store
        .save_chunk_entities(&[mk_entity_row("corpus-b", 99, "Other")])
        .await
        .unwrap();

    let ids = store
        .list_extracted_chunk_ids_for_corpus("corpus-a")
        .await
        .unwrap();
    assert_eq!(ids.len(), 3);
    assert!(ids.contains(&10));
    assert!(ids.contains(&11));
    assert!(ids.contains(&12));
    assert!(!ids.contains(&99));
}

// The entity-less-chunk convergence guard. A chunk GliNER finds no
// entities in writes no `chunk_entities` row, so the incremental delta
// used to treat it as unprocessed forever and re-run NER on it every
// pass. `record_ner_processed_chunks` + `list_ner_processed_chunk_ids`
// give the delta a durable "processed, entities or not" signal so it
// converges to zero.
#[tokio::test]
async fn ner_processed_marker_makes_empty_chunks_converge() {
    let store = sqlite_store();
    // Chunk 10 produced an entity; chunks 11 and 12 were NER'd but empty.
    store
        .save_chunk_entities(&[mk_entity_row("corpus-a", 10, "Borges")])
        .await
        .unwrap();
    store
        .record_ner_processed_chunks("corpus-a", &[10, 11, 12])
        .await
        .unwrap();

    // The processed set unions entity-bearing + explicitly-marked chunks,
    // so all three count as done — the empty ones no longer reappear in
    // the delta.
    let processed = store
        .list_ner_processed_chunk_ids("corpus-a")
        .await
        .unwrap();
    assert_eq!(processed.len(), 3);
    assert!(processed.contains(&10));
    assert!(processed.contains(&11));
    assert!(processed.contains(&12));

    // The entity view is unchanged — the marker never leaks into
    // `chunk_entities`, so entity aggregation still sees only chunk 10.
    let extracted = store
        .list_extracted_chunk_ids_for_corpus("corpus-a")
        .await
        .unwrap();
    assert_eq!(extracted.len(), 1);
    assert!(extracted.contains(&10));

    // Scoping: a sibling corpus's markers stay out.
    store
        .record_ner_processed_chunks("corpus-b", &[500])
        .await
        .unwrap();
    let a = store
        .list_ner_processed_chunk_ids("corpus-a")
        .await
        .unwrap();
    assert!(!a.contains(&500));

    // Idempotent — re-recording an already-marked chunk is a no-op.
    store
        .record_ner_processed_chunks("corpus-a", &[11])
        .await
        .unwrap();
    let again = store
        .list_ner_processed_chunk_ids("corpus-a")
        .await
        .unwrap();
    assert_eq!(again.len(), 3);
}

// The other half of that marker's contract: it must also be possible to
// UNDO. `delete_tiered_for_corpus` cleared every tiered table except
// `chunk_ner_processed`, which made the omission invisible and permanent
// — `list_ner_processed_chunk_ids` still reported every chunk as done,
// so `extract_delta_for_corpus` returned 0 instantly and a rebuilt
// corpus came back with no entities at all. No error, no warning; the
// NER phase just stopped existing. Found 2026-08-02 when `bench
// vault-report` timed a cold rebuild's NER phase at 0.0s against 39.5s
// for the same fixture one run earlier.
#[tokio::test]
async fn delete_tiered_clears_the_ner_processed_markers_too() {
    let store = sqlite_store();
    store
        .save_chunk_entities(&[mk_entity_row("corpus-a", 10, "Borges")])
        .await
        .unwrap();
    store
        .record_ner_processed_chunks("corpus-a", &[10, 11, 12])
        .await
        .unwrap();
    store
        .record_ner_processed_chunks("corpus-b", &[500])
        .await
        .unwrap();

    store.delete_tiered_for_corpus("corpus-a").await.unwrap();

    // Nothing left to skip: a re-enriched corpus must actually re-run NER.
    let processed = store
        .list_ner_processed_chunk_ids("corpus-a")
        .await
        .unwrap();
    assert!(
        processed.is_empty(),
        "a torn-down corpus still claims {} NER-processed chunk(s); the delta pass \
         would skip all of them and the rebuild would produce zero entities",
        processed.len()
    );

    // Teardown stays scoped — a sibling corpus keeps its markers.
    let b = store
        .list_ner_processed_chunk_ids("corpus-b")
        .await
        .unwrap();
    assert!(
        b.contains(&500),
        "teardown must not cross corpus boundaries"
    );
}

// The conversation runner's skip-already-built marker. `record_conv_content_hash`
// stamps the fingerprint of a conv's last successful enrichment;
// `get_conv_content_hash` reads it back so a re-import can skip an
// unchanged conv. Absent → None (fail-safe re-enrich); upsert overwrites
// on a content-changed re-enrich; scoped per (corpus_id, conv_uuid).
#[tokio::test]
async fn conv_content_hash_marker_roundtrips_and_upserts() {
    let store = sqlite_store();

    // Never enriched → None, so the runner re-enriches (fail-safe).
    assert_eq!(
        store
            .get_conv_content_hash("corpus-a", "note-1")
            .await
            .unwrap(),
        None
    );

    // First enrichment stamps a hash; read-back matches.
    store
        .record_conv_content_hash("corpus-a", "note-1", "hash-v1")
        .await
        .unwrap();
    assert_eq!(
        store
            .get_conv_content_hash("corpus-a", "note-1")
            .await
            .unwrap(),
        Some("hash-v1".to_string())
    );

    // A content-changed re-enrich overwrites (upsert on the PK), it does
    // not accumulate a second row.
    store
        .record_conv_content_hash("corpus-a", "note-1", "hash-v2")
        .await
        .unwrap();
    assert_eq!(
        store
            .get_conv_content_hash("corpus-a", "note-1")
            .await
            .unwrap(),
        Some("hash-v2".to_string())
    );

    // Scoped: a sibling conv and a sibling corpus keep their own state.
    assert_eq!(
        store
            .get_conv_content_hash("corpus-a", "note-2")
            .await
            .unwrap(),
        None
    );
    store
        .record_conv_content_hash("corpus-b", "note-1", "other")
        .await
        .unwrap();
    assert_eq!(
        store
            .get_conv_content_hash("corpus-a", "note-1")
            .await
            .unwrap(),
        Some("hash-v2".to_string())
    );
}

fn mk_entity_row_labeled(
    corpus_id: &str,
    chunk_id: u64,
    text: &str,
    label: &str,
    conv_uuid: Option<&str>,
) -> sovereign_core::conv_tiered::ChunkEntityRow {
    sovereign_core::conv_tiered::ChunkEntityRow {
        corpus_id: corpus_id.to_string(),
        chunk_id,
        text: text.to_string(),
        label: label.to_string(),
        char_start: 0,
        char_end: text.len() as i64,
        score: 0.85,
        conv_uuid: conv_uuid.map(|s| s.to_string()),
        extracted_at: 42,
    }
}

#[tokio::test]
async fn aggregate_entity_case_insensitive_with_label_breakdown() {
    // Atlas-view drawer pivot: case-fold + per-label split. Live
    // case 2026-05-23 — "Borges" + "borges" + "BORGES" should fold
    // to one row; "Swift" Person + "SWIFT" Organization should
    // surface as two label buckets in the same row.
    let store = sqlite_store();
    store
        .save_chunk_entities(&[
            mk_entity_row_labeled("c", 10, "Borges", "Person", Some("conv-1")),
            mk_entity_row_labeled("c", 11, "borges", "Person", Some("conv-1")),
            mk_entity_row_labeled("c", 12, "BORGES", "Person", Some("conv-2")),
            // Homonym pair on different rows — same surface form, different label.
            mk_entity_row_labeled("c", 20, "Swift", "Person", Some("conv-3")),
            mk_entity_row_labeled("c", 21, "SWIFT", "Organization", Some("conv-4")),
            // Sibling corpus must not bleed in.
            mk_entity_row_labeled("other", 99, "Borges", "Person", Some("conv-x")),
        ])
        .await
        .unwrap();

    let agg = store.aggregate_entity("c", "borges", 10, 10).await.unwrap();
    assert_eq!(agg.mention_count, 3);
    assert_eq!(agg.conv_count, 2);
    assert_eq!(agg.chunk_count, 3);
    assert_eq!(agg.labels.len(), 1);
    assert_eq!(agg.labels[0].label, "Person");
    assert_eq!(agg.labels[0].count, 3);
    // Canonical text comes from the most-common variant. All three
    // appear once apiece; tie broken by alphabetical so "BORGES" wins
    // ("B" < "b" in ASCII, but query is COLLATE NOCASE; alphabetical
    // here is binary-sorted text. Don't assert the exact winner — just
    // that it's one of the three case variants.)
    assert!(["Borges", "borges", "BORGES"].contains(&agg.text.as_str()));

    let homonym = store.aggregate_entity("c", "swift", 10, 10).await.unwrap();
    assert_eq!(homonym.mention_count, 2);
    assert_eq!(homonym.labels.len(), 2);
    let labels: std::collections::HashSet<&str> =
        homonym.labels.iter().map(|l| l.label.as_str()).collect();
    assert!(labels.contains("Person"));
    assert!(labels.contains("Organization"));
}

#[tokio::test]
async fn aggregate_entity_co_occurrence_intra_chunk_only() {
    // Co-occurrence is intra-chunk, not intra-conv. Two entities
    // in the same chunk count; two entities in the same conv but
    // different chunks do not. Guards against an accidental
    // GROUP BY conv_uuid join refactor.
    let store = sqlite_store();
    store
        .save_chunk_entities(&[
            // Chunk 10: Borges + Bach (shared chunk).
            mk_entity_row_labeled("c", 10, "Borges", "Person", Some("conv-1")),
            mk_entity_row_labeled("c", 10, "Bach", "Person", Some("conv-1")),
            // Chunk 11 (same conv): Borges only. Calvino is separate chunk.
            mk_entity_row_labeled("c", 11, "Borges", "Person", Some("conv-1")),
            mk_entity_row_labeled("c", 12, "Calvino", "Person", Some("conv-1")),
            // Chunk 20 (different conv): Borges + Bach again.
            mk_entity_row_labeled("c", 20, "Borges", "Person", Some("conv-2")),
            mk_entity_row_labeled("c", 20, "Bach", "Person", Some("conv-2")),
        ])
        .await
        .unwrap();

    let agg = store.aggregate_entity("c", "Borges", 10, 10).await.unwrap();
    // Bach shares chunks 10 + 20 with Borges (2). Calvino shares
    // zero chunks (same conv, different chunk). Calvino must NOT
    // appear in the co_occurring list.
    let bach = agg
        .co_occurring
        .iter()
        .find(|c| c.text == "Bach")
        .expect("Bach should appear");
    assert_eq!(bach.shared_chunk_count, 2);
    assert!(
        agg.co_occurring.iter().all(|c| c.text != "Calvino"),
        "Calvino shares no chunk with Borges; should not co-occur"
    );
}

#[tokio::test]
async fn aggregate_entity_unknown_returns_zero() {
    // Drawer must handle "no mentions" without crashing — surface a
    // zero-row aggregate so the UI can render its empty-state hint.
    let store = sqlite_store();
    let agg = store
        .aggregate_entity("c", "never-seen-entity", 10, 10)
        .await
        .unwrap();
    assert_eq!(agg.mention_count, 0);
    assert_eq!(agg.conv_count, 0);
    assert_eq!(agg.chunk_count, 0);
    assert!(agg.labels.is_empty());
    assert!(agg.top_convs.is_empty());
    assert!(agg.co_occurring.is_empty());
    // Canonical text falls back to the query when no rows exist.
    assert_eq!(agg.text, "never-seen-entity");
}

#[tokio::test]
async fn save_chunk_entities_preserves_prior_rows_non_destructive() {
    // The non-destructive append is load-bearing for Phase B — a
    // delta pass writes only the new chunks; prior conv data must
    // survive. Regression guard for an accidental DELETE-then-INSERT
    // refactor.
    let store = sqlite_store();
    store
        .save_chunk_entities_for_conv(
            "corpus-a",
            "conv-x",
            &[mk_entity_row("corpus-a", 10, "Borges")],
        )
        .await
        .unwrap();
    store
        .save_chunk_entities(&[mk_entity_row("corpus-a", 11, "Bach")])
        .await
        .unwrap();

    let ids = store
        .list_extracted_chunk_ids_for_corpus("corpus-a")
        .await
        .unwrap();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&10));
    assert!(ids.contains(&11));
}

/// `corpus_raptor_version` is the cheap build-version the RAPTOR summary-index
/// freshness gate reads: the newest `created_at` across a corpus's nodes, 0
/// when none, and scoped to the corpus (no cross-corpus bleed).
#[tokio::test]
async fn corpus_raptor_version_returns_scoped_max_created_at() {
    let store = SqliteStateStore::open_in_memory().unwrap();

    // Empty corpus → 0 (freshness gate treats this as "nothing to be stale").
    assert_eq!(store.corpus_raptor_version("sep").await.unwrap(), 0);

    let mk = |corpus: &str, node_id: &str, conv: &str, created: i64| {
        sovereign_core::conv_tiered::ConvRaptorNodeRow {
            node_id: node_id.to_string(),
            corpus_id: corpus.to_string(),
            conv_uuid: conv.to_string(),
            level: 1,
            summary: "s".to_string(),
            summary_embedding: vec![0.1, 0.2, 0.3],
            centroid_embedding: vec![0.1, 0.2, 0.3],
            children_node_ids_json: "[]".to_string(),
            direct_member_chunk_ids_json: None,
            evidence_chunk_ids_json: "[]".to_string(),
            quote_spans_json: "[]".to_string(),
            primary_entities_json: "[]".to_string(),
            cluster_coherence: 1.0,
            created_at: created,
            prompt_version: String::new(),
            summarizer_model: String::new(),
        }
    };

    store
        .save_conv_raptor_nodes("sep", "doc-a", &[mk("sep", "a1", "doc-a", 100)])
        .await
        .unwrap();
    store
        .save_conv_raptor_nodes("sep", "doc-b", &[mk("sep", "b1", "doc-b", 250)])
        .await
        .unwrap();
    // A different corpus with a higher timestamp must NOT bleed into 'sep'.
    store
        .save_conv_raptor_nodes("other", "doc-c", &[mk("other", "c1", "doc-c", 999)])
        .await
        .unwrap();

    assert_eq!(store.corpus_raptor_version("sep").await.unwrap(), 250);
    assert_eq!(store.corpus_raptor_version("other").await.unwrap(), 999);
    assert_eq!(store.corpus_raptor_version("missing").await.unwrap(), 0);
}

/// The summary-correction ledger (the "flag a wrong summary" revision
/// loop, docs/specs/SUMMARY_REVISION_LOOP.md): upsert writes a `pending`
/// row, `get_active_correction` reads it back, `set_correction_status`
/// flips `pending` → `applied`, re-flagging supersedes (clears
/// `applied_at`), and rows are scoped per (corpus, note).
#[tokio::test]
async fn summary_correction_ledger_roundtrips_and_flips_status() {
    let store = SqliteStateStore::open_in_memory().unwrap();

    // No correction yet.
    assert!(store
        .get_active_correction("vault", "Note.md")
        .await
        .unwrap()
        .is_none());

    // Flag it (pending).
    store
        .upsert_summary_correction(
            "vault",
            "Note.md",
            Some("Yakumo is the village, not a person"),
            Some("Following Yakumo's death…"),
            "pending",
            1000,
        )
        .await
        .unwrap();
    let c = store
        .get_active_correction("vault", "Note.md")
        .await
        .unwrap()
        .expect("correction present after upsert");
    assert_eq!(c.status, "pending");
    assert_eq!(
        c.correction_hint.as_deref(),
        Some("Yakumo is the village, not a person")
    );
    assert_eq!(c.applied_at, None);

    // Provider flips it to applied after the guided re-enrich.
    store
        .set_correction_status("vault", "Note.md", "applied", Some(1005))
        .await
        .unwrap();
    let c = store
        .get_active_correction("vault", "Note.md")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c.status, "applied");
    assert_eq!(c.applied_at, Some(1005));

    // Re-flagging supersedes: status back to pending, applied_at cleared.
    store
        .upsert_summary_correction(
            "vault",
            "Note.md",
            Some("still wrong"),
            None,
            "pending",
            2000,
        )
        .await
        .unwrap();
    let c = store
        .get_active_correction("vault", "Note.md")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c.status, "pending");
    assert_eq!(c.applied_at, None);
    assert_eq!(c.correction_hint.as_deref(), Some("still wrong"));

    // Scoped per (corpus, note): a sibling note is unaffected.
    assert!(store
        .get_active_correction("vault", "Other.md")
        .await
        .unwrap()
        .is_none());
}

// ─── T1 memory embeddings + T3 mem-raptor (tiered memory port) ────

/// T1: `update_memory_embedding` persists and round-trips through the
/// full-columns reader; a fresh row carries None.
#[tokio::test]
async fn sqlite_memory_embedding_roundtrip() {
    let store = sqlite_store();
    let mem = Memory {
        id: "emb1".to_string(),
        content: "walked by the river".to_string(),
        source: "test".to_string(),
        confidence: 1.0,
        created_at: now(),
        last_used: now(),
        ..Default::default()
    };
    store.save_memory(&mem).await.unwrap();

    let all = store.get_all_memories().await.unwrap();
    assert!(
        all[0].embedding.is_none(),
        "fresh row must carry no embedding"
    );

    store
        .update_memory_embedding("emb1", &[0.25f32, -1.5, 3.0], "test-embedder")
        .await
        .unwrap();
    let all = store.get_all_memories().await.unwrap();
    assert_eq!(all[0].embedding.as_deref(), Some(&[0.25f32, -1.5, 3.0][..]));
    assert_eq!(all[0].embedding_model.as_deref(), Some("test-embedder"));
}

/// T1: a save_memory carrying an embedding persists it (compute-on-write
/// path shape).
#[tokio::test]
async fn sqlite_memory_save_with_embedding() {
    let store = sqlite_store();
    let mem = Memory {
        id: "emb2".to_string(),
        content: "quiet evening".to_string(),
        source: "test".to_string(),
        confidence: 1.0,
        created_at: now(),
        last_used: now(),
        embedding: Some(vec![1.0, 2.0]),
        embedding_model: Some("m1".to_string()),
        ..Default::default()
    };
    store.save_memory(&mem).await.unwrap();
    let all = store.get_all_memories().await.unwrap();
    assert_eq!(all[0].embedding.as_deref(), Some(&[1.0f32, 2.0][..]));
    assert_eq!(all[0].embedding_model.as_deref(), Some("m1"));
}

fn mk_mem_node(node_id: &str, scope: &str, members: &[&str]) -> MemRaptorNodeRow {
    MemRaptorNodeRow {
        node_id: node_id.to_string(),
        scope: scope.to_string(),
        level: 0,
        summary: format!("summary of {node_id}"),
        summary_embedding: vec![0.1, 0.2],
        centroid_embedding: vec![0.3, 0.4],
        children_node_ids: vec![],
        direct_member_memory_ids: members.iter().map(|s| s.to_string()).collect(),
        evidence_memory_ids: members.iter().map(|s| s.to_string()).collect(),
        primary_entities: vec!["River".to_string()],
        cluster_coherence: 0.75,
        embedding_model: "test-embedder".to_string(),
        created_at: 1234,
        ..Default::default()
    }
}

/// T3: save/list/delete roundtrip, and — the sequestration property —
/// scope keys never bleed into each other's listings.
#[tokio::test]
async fn sqlite_mem_raptor_scope_isolation() {
    let store = sqlite_store();
    store
        .save_mem_raptor_nodes(
            "mem:inner-work",
            &[mk_mem_node("n1", "mem:inner-work", &["m-a", "m-b"])],
        )
        .await
        .unwrap();
    store
        .save_mem_raptor_nodes("mem:general", &[mk_mem_node("n2", "mem:general", &["m-c"])])
        .await
        .unwrap();

    let scoped = store.list_mem_raptor_nodes("mem:inner-work").await.unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].node_id, "n1");
    assert_eq!(scoped[0].direct_member_memory_ids, vec!["m-a", "m-b"]);
    assert_eq!(scoped[0].summary_embedding, vec![0.1, 0.2]);
    assert_eq!(scoped[0].embedding_model, "test-embedder");

    let general = store.list_mem_raptor_nodes("mem:general").await.unwrap();
    assert_eq!(general.len(), 1);
    assert_eq!(general[0].node_id, "n2");

    // Replace semantics: a second save for the same scope drops the
    // prior tree instead of accumulating.
    store
        .save_mem_raptor_nodes(
            "mem:inner-work",
            &[mk_mem_node("n3", "mem:inner-work", &["m-d"])],
        )
        .await
        .unwrap();
    let scoped = store.list_mem_raptor_nodes("mem:inner-work").await.unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].node_id, "n3");
    // Other scope untouched.
    assert_eq!(
        store
            .list_mem_raptor_nodes("mem:general")
            .await
            .unwrap()
            .len(),
        1
    );

    store
        .delete_mem_raptor_nodes_for_scope("mem:inner-work")
        .await
        .unwrap();
    assert!(store
        .list_mem_raptor_nodes("mem:inner-work")
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .list_mem_raptor_nodes("mem:general")
            .await
            .unwrap()
            .len(),
        1
    );
}
