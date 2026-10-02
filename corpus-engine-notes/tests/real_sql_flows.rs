// SPDX-License-Identifier: AGPL-3.0-or-later
//! Real-SQL proofs of the note store, beside their owner (fp-27, §12 D6:
//! relocate the real-SQL proof, never fake it).
//!
//! These three flows were proven incidentally by `sovereign-core`'s logic
//! tests through a live `NoteStore` — the core crate dropped that
//! dependency (boundary-gate row sovereign-core→corpus-engine-notes) and the
//! proof moved here with literal payloads, so what is asserted is the STORE
//! contract the core code leans on: a written row round-trips through SQLite,
//! `update_note_payload` persists, and a retired predecessor is hidden.

use corpus_engine_notes::{NoteScope, NoteSource, NoteStore};

async fn open_store(dir: &tempfile::TempDir) -> NoteStore {
    NoteStore::open(&dir.path().join("notes.db")).unwrap()
}

/// Relocated from `sovereign-core/src/memory.rs`
/// `write_then_read_tool_decision_round_trips_payload` — the store half: the
/// tool_decision payload `core::memory::write_tool_decision` emits lands in
/// SQLite and `read_notes` returns it byte-for-byte.
#[tokio::test]
async fn a_tool_decision_payload_round_trips_through_sql() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let payload = serde_json::json!({
        "tool_id": "knowledge_lookup",
        "outcome": "no-results",
        "reasoning": "corpus has no entry for M5 Mac Studio",
        "applied_at_unix": 1,
        "conversation_id": "conv-A",
        "summary": null,
        "evidence_ids": [],
        "turn_index": 0
    })
    .to_string();

    let id = store
        .write_note_full(
            "tool_decision",
            "knowledge_lookup → no-results — corpus has no entry for M5 Mac Studio",
            vec!["knowledge_lookup".to_string()],
            vec![],
            "sess-mem-1",
            NoteScope::Session,
            None,
            None,
            NoteSource::Agent,
            None,
            Some(&payload),
        )
        .await
        .unwrap();
    assert!(!id.is_empty());

    let rows = store
        .read_notes(None, &[], &[], &["tool_decision".to_string()], 10, false)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[0].kind, "tool_decision");
    assert_eq!(rows[0].scope, "session");
    assert_eq!(
        rows[0].payload_json.as_deref(),
        Some(payload.as_str()),
        "payload_json must round-trip byte-for-byte through SQLite"
    );
}

/// Relocated from `sovereign-core/src/lessons.rs`
/// `loader_honors_enabled_flag_and_supersede` — the store half of the
/// disabled case: `update_note_payload` (the whisper's stamp path) persists,
/// so a re-read sees the updated payload and not the write-time one.
#[tokio::test]
async fn update_note_payload_persists() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let before = serde_json::json!({"display": "Keep answers short.", "enabled": true}).to_string();
    let id = store
        .write_note_full(
            "lesson",
            "Keep answers short.",
            vec![],
            vec![],
            "s1",
            NoteScope::Global,
            None,
            None,
            NoteSource::Agent,
            None,
            Some(&before),
        )
        .await
        .unwrap();

    let after = serde_json::json!({"display": "Keep answers short.", "enabled": false}).to_string();
    assert!(store.update_note_payload(&id, &after).await.unwrap());

    let rows = store
        .read_notes(None, &[], &[], &["lesson".to_string()], 10, false)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].payload_json.as_deref(), Some(after.as_str()));
}

/// Relocated from `sovereign-core/src/lessons.rs`
/// `loader_honors_enabled_flag_and_supersede` — the store half of the
/// supersede case: after `retire_by_id`, a default `read_notes` does not
/// return the predecessor. `load_active_lessons` selects from exactly this
/// list, so "a retired predecessor never loads" composes from this hiding
/// plus the loader's first-per-rung selection (proven over a double in core).
#[tokio::test]
async fn a_retired_predecessor_is_hidden_after_supersede() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let payload =
        serde_json::json!({"display": "Keep answers short.", "enabled": true}).to_string();
    let id_a = store
        .write_note_full(
            "lesson",
            "Keep answers short.",
            vec![],
            vec![],
            "s1",
            NoteScope::Global,
            None,
            None,
            NoteSource::Agent,
            None,
            Some(&payload),
        )
        .await
        .unwrap();
    // DISTINCT content from A on purpose: identical kind+content+entity
    // rows collapse to one representative in `read_notes_scoped` (named
    // dedup) — a third contract, not this test's subject.
    let payload_b =
        serde_json::json!({"display": "Keep answers very short.", "enabled": true}).to_string();
    let id_b = store
        .write_note_full(
            "lesson",
            "Keep answers very short.",
            vec![],
            vec![],
            "s1",
            NoteScope::Global,
            None,
            None,
            NoteSource::Agent,
            Some(&id_a),
            Some(&payload_b),
        )
        .await
        .unwrap();
    assert!(store.retire_by_id(&id_a, "superseded").await.unwrap());

    let rows = store
        .read_notes(None, &[], &[], &["lesson".to_string()], 10, false)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "the retired predecessor must be hidden");
    assert_eq!(rows[0].id, id_b);

    let all = store
        .read_notes(None, &[], &[], &["lesson".to_string()], 10, true)
        .await
        .unwrap();
    assert_eq!(all.len(), 2, "include_retired keeps history visible");
}
