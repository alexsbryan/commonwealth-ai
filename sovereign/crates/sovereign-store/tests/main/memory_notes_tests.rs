// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn's memory notes in svrn's own store (phase-b pb-notes-memory): the
//! `AgentNotes` / `RecipeNotes` ports over `SqliteStateStore`, real SQL.

use sovereign_core::notes::AgentNotes;
use sovereign_core::recipe::notes::{NoteScope, NoteSource, RecipeNotes, ScopeFilter};
use sovereign_core::traits::MemoryStore;
use sovereign_store::sqlite::SqliteStateStore;

fn open(dir: &tempfile::TempDir) -> SqliteStateStore {
    SqliteStateStore::open(&dir.path().join("sovereign.db")).expect("open store")
}

async fn lesson(store: &SqliteStateStore, content: &str) -> String {
    store
        .write_note_full(
            "lesson",
            content,
            vec![],
            vec![],
            "s1",
            NoteScope::Global,
            None,
            None,
            NoteSource::Agent,
            None,
            Some(r#"{"enabled":true}"#),
        )
        .await
        .expect("write lesson")
}

#[tokio::test]
async fn a_lesson_written_is_read_back_after_the_store_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let id = lesson(&open(&dir), "answer in two sentences").await;

    let store = open(&dir);
    let rows = store
        .read_notes(None, &[], &[], &["lesson".to_string()], 20, false)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[0].payload_json.as_deref(), Some(r#"{"enabled":true}"#));
    assert_eq!(rows[0].scope, "global");
}

#[tokio::test]
async fn reads_filter_by_kind_and_hide_retired_rows_unless_asked() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    let old = lesson(&store, "first").await;
    let new = lesson(&store, "second").await;
    store
        .write_note_with_relation(
            "commitment",
            "ship the migration",
            vec![],
            vec![],
            "conv-1",
            NoteScope::Session,
            None,
            Some("phase-b"),
        )
        .await
        .unwrap();
    assert!(store.retire_memory_note(&old, "superseded").await.unwrap());
    assert!(!store.retire_memory_note(&old, "again").await.unwrap());

    let live = store
        .read_notes(None, &[], &[], &["lesson".to_string()], 20, false)
        .await
        .unwrap();
    assert_eq!(
        live.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
        [new.as_str()]
    );
    let all = store
        .read_notes(None, &[], &[], &["lesson".to_string()], 20, true)
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[1].retired_by.as_deref(), Some("superseded"));

    let related = store
        .read_notes_by_related_entity("phase-b", &["commitment"])
        .await
        .unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0].content, "ship the migration");
    assert!(store
        .has_active_note_with_content("commitment", "ship the migration", NoteSource::Agent)
        .await
        .unwrap());
}

#[tokio::test]
async fn a_query_ranks_by_text_and_a_payload_patch_lands() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    lesson(&store, "never use bullet points").await;
    let hit = lesson(&store, "prefer metric units for distances").await;

    let rows = store
        .read_notes(Some("metric distances"), &[], &[], &[], 10, false)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, hit);

    assert!(store
        .update_note_payload(&hit, r#"{"enabled":false}"#)
        .await
        .unwrap());
    assert!(!store.update_note_payload("missing", "{}").await.unwrap());
    let one = store.memory_note_entry(&hit).await.unwrap().unwrap();
    assert_eq!(one.payload_json.as_deref(), Some(r#"{"enabled":false}"#));

    assert!(store.delete_memory_note(&hit).await.unwrap());
    assert!(store.memory_note_entry(&hit).await.unwrap().is_none());
}

#[tokio::test]
async fn the_scope_filter_keeps_one_feature_and_the_other_scopes() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    for (scope, fid) in [
        (NoteScope::Feature, Some("f-1")),
        (NoteScope::Feature, Some("f-2")),
        (NoteScope::Global, None),
    ] {
        store
            .write_note_full(
                "decision",
                &format!("{fid:?}"),
                vec![],
                vec![],
                "s",
                scope,
                fid,
                None,
                NoteSource::Agent,
                None,
                None,
            )
            .await
            .unwrap();
    }
    let filter = ScopeFilter {
        scopes: vec![NoteScope::Global, NoteScope::Feature],
        feature_id: Some("f-1".into()),
    };
    let rows = store
        .read_notes_scoped(None, &[], &[], &[], 10, false, &filter)
        .await
        .unwrap();
    let mut got: Vec<_> = rows.iter().map(|n| n.feature_id.clone()).collect();
    got.sort();
    assert_eq!(got, [None, Some("f-1".to_string())]);

    let refused = store
        .write_note_full(
            "decision",
            "x",
            vec![],
            vec![],
            "s",
            NoteScope::Feature,
            None,
            None,
            NoteSource::Agent,
            None,
            None,
        )
        .await;
    assert!(
        refused.is_err(),
        "feature scope without a feature id is refused"
    );
}

#[tokio::test]
async fn the_call_log_records_calls_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    store
        .log_tool_call("daemon-1", "knowledge_lookup", "success")
        .await
        .unwrap();
    store
        .log_tool_call("daemon-1", "web_search", "error")
        .await
        .unwrap();
    let rows = store.tool_call_log_rows(0, 10).await.unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r.tool_name.as_str())
            .collect::<Vec<_>>(),
        ["web_search", "knowledge_lookup"]
    );
    assert_eq!(rows[0].outcome, "error");
}

/// A memory note is not a memory: nothing svrn writes here is recalled
/// into the system prompt.
#[tokio::test]
async fn memory_notes_never_become_recall_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    lesson(&store, "always cite the source").await;
    assert!(store.get_all_memories().await.unwrap().is_empty());
    assert!(store
        .get_relevant_memories("cite the source", 5)
        .await
        .unwrap()
        .is_empty());
}
