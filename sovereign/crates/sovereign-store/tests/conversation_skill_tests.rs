//! Tests for `ConversationStore::set_conversation_skill_if_unset`
//! and the round-trip of `Memory.source_conversation_id`.
//!
//! These are the backing assertions for Tier 2 items 1 & 2: once the
//! Runtime tags a conversation with its starting skill and stamps
//! extracted memories with the originating conversation id, those
//! fields must persist correctly through the SQLite store.

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

fn msg(id: &str, conv_id: &str) -> Message {
    Message {
        id: id.to_string(),
        conversation_id: conv_id.to_string(),
        role: Role::User,
        content: "hello".to_string(),
        created_at: now(),
        metadata: None,
        version: 0,
    }
}

#[tokio::test]
async fn set_conversation_skill_if_unset_writes_on_first_call() {
    let store = SqliteStateStore::open_in_memory().unwrap();
    store.save_message(&msg("m1", "conv-a")).await.unwrap();
    // skill_id starts NULL on a freshly-upserted conversation.
    let before = store.get_conversation("conv-a").await.unwrap();
    assert!(before.skill_id.is_none());

    store
        .set_conversation_skill_if_unset("conv-a", "research-analyst")
        .await
        .unwrap();
    let after = store.get_conversation("conv-a").await.unwrap();
    assert_eq!(after.skill_id.as_deref(), Some("research-analyst"));
}

#[tokio::test]
async fn set_conversation_skill_if_unset_is_idempotent() {
    // First-writer-wins: a second call with a different skill must
    // NOT overwrite the original tag. Matches the semantic
    // "conversation belongs to the skill it was started under".
    let store = SqliteStateStore::open_in_memory().unwrap();
    store.save_message(&msg("m1", "conv-a")).await.unwrap();
    store
        .set_conversation_skill_if_unset("conv-a", "inner-work")
        .await
        .unwrap();
    store
        .set_conversation_skill_if_unset("conv-a", "research-analyst")
        .await
        .unwrap();
    let conv = store.get_conversation("conv-a").await.unwrap();
    assert_eq!(
        conv.skill_id.as_deref(),
        Some("inner-work"),
        "second call must not overwrite the first tag"
    );
}

#[tokio::test]
async fn set_conversation_skill_if_unset_on_missing_conversation_is_noop() {
    // Calling on a non-existent id must not error — the Runtime
    // calls this on every message write and it should be safe even
    // when the store hasn't seen the conversation yet for any
    // reason (e.g. test scaffolding out of order).
    let store = SqliteStateStore::open_in_memory().unwrap();
    let result = store
        .set_conversation_skill_if_unset("conv-ghost", "inner-work")
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn memory_source_conversation_id_round_trips() {
    // Tier 2 item 2: a Memory saved with source_conversation_id
    // Some(_) must come back populated. Confirms the migration +
    // read/write paths are all in lock-step.
    let store = SqliteStateStore::open_in_memory().unwrap();
    let mem = Memory {
        id: "m1".into(),
        content: "User keeps coming back to the question of meaningful work".into(),
        source: "conversation_extraction".into(),
        confidence: 0.9,
        created_at: now(),
        last_used: now(),
        version: 0,
        deleted_at: None,
        source_conversation_id: Some("conv-source".to_string()),
        source_skill_id: None,
        ..Default::default()
    };
    store.save_memory(&mem).await.unwrap();

    let all = store.get_all_memories().await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(
        all[0].source_conversation_id.as_deref(),
        Some("conv-source"),
        "source_conversation_id must round-trip through SQLite"
    );

    let relevant = store
        .get_relevant_memories("meaningful", 5)
        .await
        .unwrap();
    assert_eq!(relevant.len(), 1);
    assert_eq!(
        relevant[0].source_conversation_id.as_deref(),
        Some("conv-source"),
        "FTS-retrieved memory also carries source_conversation_id"
    );
}

#[tokio::test]
async fn legacy_memory_without_source_conversation_id_reads_as_none() {
    // Backwards-compat: memories predating the KnowledgeView
    // migration (or extracted outside a conversational context)
    // carry `source_conversation_id: None` and must still be
    // retrievable.
    let store = SqliteStateStore::open_in_memory().unwrap();
    let mem = Memory {
        id: "m-legacy".into(),
        content: "some older fact about the user".into(),
        source: "manual".into(),
        confidence: 0.7,
        created_at: now() - 365 * 86400,
        last_used: now() - 365 * 86400,
        version: 0,
        deleted_at: None,
        source_conversation_id: None,
        source_skill_id: None,
        ..Default::default()
    };
    store.save_memory(&mem).await.unwrap();

    let all = store.get_all_memories().await.unwrap();
    assert_eq!(all.len(), 1);
    assert!(all[0].source_conversation_id.is_none());
}

// ─── Inner-work memory wall ────────────────────────────────

fn mem_with_skill(id: &str, content: &str, skill: Option<&str>) -> Memory {
    Memory {
        id: id.to_string(),
        content: content.to_string(),
        source: "test".into(),
        confidence: 0.9,
        created_at: now(),
        last_used: now(),
        version: 0,
        deleted_at: None,
        source_conversation_id: None,
        source_skill_id: skill.map(|s| s.to_string()),
        ..Default::default()
    }
}

#[tokio::test]
async fn inner_work_scope_recall_excludes_general_memories() {
    // The wall, direction A: an inner-work conversation MUST NOT
    // recall memories from sales-call/research/general surfaces.
    let store = SqliteStateStore::open_in_memory().unwrap();
    store.save_memory(&mem_with_skill("g1", "user prefers Rust", None)).await.unwrap();
    store.save_memory(&mem_with_skill("g2", "user attended SaaS conference", None)).await.unwrap();
    store.save_memory(&mem_with_skill("iw1", "user has been processing grief about mother", Some("inner-work"))).await.unwrap();

    let scope = sovereign_core::MemoryScope::Scoped("inner-work".into());

    let all = store.get_all_memories_for_scope(&scope).await.unwrap();
    assert_eq!(all.len(), 1, "scoped recall returns only inner-work memories");
    assert_eq!(all[0].id, "iw1");

    let relevant = store
        .get_relevant_memories_for_scope(&scope, "user", 10)
        .await
        .unwrap();
    assert_eq!(
        relevant.len(),
        1,
        "FTS recall in inner-work scope excludes general memories"
    );
    assert_eq!(relevant[0].id, "iw1");
}

#[tokio::test]
async fn general_scope_recall_excludes_inner_work_memories() {
    // The wall, direction B: a general conversation MUST NOT see
    // inner-work memories. This is the privacy contract — nothing
    // the user said in journaling can leak into a professional chat.
    let store = SqliteStateStore::open_in_memory().unwrap();
    store.save_memory(&mem_with_skill("g1", "user prefers Rust", None)).await.unwrap();
    store.save_memory(&mem_with_skill("iw1", "user has been processing grief about mother", Some("inner-work"))).await.unwrap();

    let scope = sovereign_core::MemoryScope::General;

    let all = store.get_all_memories_for_scope(&scope).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, "g1", "general recall excludes inner-work memories");

    let relevant = store
        .get_relevant_memories_for_scope(&scope, "user", 10)
        .await
        .unwrap();
    assert_eq!(relevant.len(), 1);
    assert_eq!(
        relevant[0].id, "g1",
        "FTS recall in general scope never returns inner-work-tagged memories"
    );
}

#[tokio::test]
async fn tombstoned_memory_excluded_from_scope_recall() {
    // delete_memory soft-deletes. Tombstoned rows must not surface
    // in either scope path — this is what makes the "drop" UX
    // gracefully invalidate without losing the audit trail.
    let store = SqliteStateStore::open_in_memory().unwrap();
    store.save_memory(&mem_with_skill("iw1", "fact A", Some("inner-work"))).await.unwrap();
    store.save_memory(&mem_with_skill("iw2", "fact B", Some("inner-work"))).await.unwrap();
    store.delete_memory("iw1").await.unwrap();

    let scope = sovereign_core::MemoryScope::Scoped("inner-work".into());
    let all = store.get_all_memories_for_scope(&scope).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, "iw2");
}

#[tokio::test]
async fn scope_constructor_maps_none_to_general() {
    // Document the semantic: a conversation with no skill_id maps
    // to the general pool. An empty string is treated the same way
    // (defensive against a buggy upstream that writes "" instead of
    // NULL).
    use sovereign_core::MemoryScope;
    assert_eq!(MemoryScope::from_conversation_skill(None), MemoryScope::General);
    assert_eq!(MemoryScope::from_conversation_skill(Some("")), MemoryScope::General);
    assert_eq!(
        MemoryScope::from_conversation_skill(Some("inner-work")),
        MemoryScope::Scoped("inner-work".into()),
    );
}
