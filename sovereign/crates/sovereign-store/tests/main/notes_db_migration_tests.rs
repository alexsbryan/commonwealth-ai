// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one-time move of svrn's rows out of `notes.db` (phase-b
//! pb-notes-memory): conservation, idempotence, the pre-migration copy.
//!
//! The fixture is `notes.db`'s own schema written as raw SQL (the table, its
//! FTS mirror and triggers, and the embeddings side table that cascades):
//! svrn cannot link the code program's store to build one, and a fixture that
//! lacked the triggers would not show the move keeping them consistent.

use std::path::Path;

use rusqlite::Connection;
use sovereign_core::notes::AgentNotes;
use sovereign_store::sqlite::{notes_db_backup_path, SqliteStateStore};

const NOTES_SCHEMA: &str = "
CREATE TABLE notes (
    id TEXT PRIMARY KEY, kind TEXT NOT NULL, content TEXT NOT NULL,
    symbols TEXT NOT NULL DEFAULT '[]', files TEXT NOT NULL DEFAULT '[]',
    session_id TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
    tool_name TEXT, retired_at INTEGER, retired_by TEXT,
    scope TEXT NOT NULL DEFAULT 'global', feature_id TEXT, promoted_from TEXT,
    related_entity TEXT, source TEXT NOT NULL DEFAULT 'agent', supersedes TEXT,
    payload_json TEXT, private INTEGER NOT NULL DEFAULT 0, origin_node_id TEXT,
    tombstone INTEGER NOT NULL DEFAULT 0, content_hash TEXT, fork_of TEXT,
    sent_at INTEGER, received_at INTEGER
);
CREATE VIRTUAL TABLE notes_fts USING fts5(content, kind, content='notes', content_rowid='rowid');
CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;
CREATE TRIGGER notes_fts_ad AFTER DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind) VALUES ('delete', old.rowid, old.content, old.kind);
END;
CREATE TABLE note_embeddings (
    note_id TEXT PRIMARY KEY REFERENCES notes(id) ON DELETE CASCADE,
    embedding BLOB NOT NULL, model_id TEXT NOT NULL, dim INTEGER NOT NULL, created_at INTEGER NOT NULL
);
";

/// (id, kind, scope, related_entity, tombstone) — svrn's four, code's four.
const ROWS: &[(&str, &str, &str, Option<&str>, i64)] = &[
    ("svrn-lesson", "lesson", "global", None, 0),
    ("svrn-dossier", "tool_decision", "session", None, 1),
    ("svrn-remind", "todo", "session", None, 0),
    ("svrn-commit", "commitment", "session", Some("migration"), 0),
    ("code-backlog", "todo", "session", Some("backlog"), 0),
    ("code-todo", "todo", "global", None, 0),
    ("code-decision", "decision", "global", None, 0),
    ("code-commit", "commitment", "global", Some("phase-b"), 0),
];

fn fixture(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
    conn.execute_batch(NOTES_SCHEMA).unwrap();
    for (i, (id, kind, scope, entity, tombstone)) in ROWS.iter().enumerate() {
        conn.execute(
            "INSERT INTO notes (id, kind, content, session_id, created_at, updated_at,
                 scope, related_entity, tombstone, origin_node_id, payload_json)
             VALUES (?1, ?2, ?3, 's', ?4, ?4, ?5, ?6, ?7, 'node-a', '{\"k\":1}')",
            rusqlite::params![
                id,
                kind,
                format!("{id} body"),
                1_000 + i as i64,
                scope,
                entity,
                tombstone
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO note_embeddings VALUES (?1, x'00', 'm', 1, 0)",
            rusqlite::params![id],
        )
        .unwrap();
    }
}

fn ids(path: &Path, table: &str) -> Vec<String> {
    let conn = Connection::open(path).unwrap();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {} FROM {table} ORDER BY 1",
            if table == "notes" { "id" } else { "note_id" }
        ))
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn setup() -> (tempfile::TempDir, std::path::PathBuf, SqliteStateStore) {
    let dir = tempfile::tempdir().unwrap();
    let notes_db = dir.path().join("notes.db");
    fixture(&notes_db);
    let store = SqliteStateStore::open(&dir.path().join("sovereign.db")).unwrap();
    (dir, notes_db, store)
}

#[tokio::test]
async fn svrns_rows_move_and_the_row_count_is_conserved() {
    let (_dir, notes_db, store) = setup();
    let report = store.migrate_notes_db(&notes_db).await.unwrap();

    assert_eq!((report.rows_before, report.moved, report.kept), (8, 4, 4));
    assert_eq!(report.rows_before, report.moved + report.kept);
    assert_eq!(
        ids(&notes_db, "notes"),
        ["code-backlog", "code-commit", "code-decision", "code-todo"]
    );
    // The embeddings of moved rows cascade away with them.
    assert_eq!(ids(&notes_db, "note_embeddings"), ids(&notes_db, "notes"));

    let moved = store
        .read_notes(None, &[], &[], &[], 50, true)
        .await
        .unwrap();
    let mut got: Vec<&str> = moved.iter().map(|n| n.id.as_str()).collect();
    got.sort();
    assert_eq!(
        got,
        ["svrn-commit", "svrn-dossier", "svrn-lesson", "svrn-remind"]
    );
    // Columns cross whole: the tombstoned dossier row stays hidden from a
    // live read, and the commitment keeps its anchor.
    let live = store
        .read_notes(None, &[], &[], &[], 50, false)
        .await
        .unwrap();
    assert!(live.iter().all(|n| n.id != "svrn-dossier"));
    let anchored = store
        .read_notes_by_related_entity("migration", &["commitment"])
        .await
        .unwrap();
    assert_eq!(anchored[0].payload_json.as_deref(), Some("{\"k\":1}"));
    let entry = store
        .memory_note_entry("svrn-lesson")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entry.origin_node_id.as_deref(), Some("node-a"));
}

#[tokio::test]
async fn a_second_run_moves_nothing() {
    let (_dir, notes_db, store) = setup();
    store.migrate_notes_db(&notes_db).await.unwrap();
    // A svrn-shaped row written to notes.db afterwards (say, by an old
    // binary) stays where it is: the marker, not the selection, decides.
    Connection::open(&notes_db)
        .unwrap()
        .execute(
            "INSERT INTO notes (id, kind, content, session_id, created_at, updated_at)
             VALUES ('late', 'lesson', 'late', 's', 9, 9)",
            [],
        )
        .unwrap();
    let again = store.migrate_notes_db(&notes_db).await.unwrap();
    assert!(again.already_done);
    assert_eq!(again.moved, 0);
    assert!(ids(&notes_db, "notes").contains(&"late".to_string()));
}

#[tokio::test]
async fn the_copy_is_taken_before_a_row_moves() {
    let (_dir, notes_db, store) = setup();
    let report = store.migrate_notes_db(&notes_db).await.unwrap();

    let backup = notes_db_backup_path(&notes_db);
    assert_eq!(report.backup.as_deref(), Some(backup.as_path()));
    assert!(backup
        .to_string_lossy()
        .ends_with("notes.db.pre-pb-notes-memory"));
    // The copy holds every row as it was before the move.
    assert_eq!(ids(&backup, "notes").len(), ROWS.len());
}

#[tokio::test]
async fn a_fresh_install_has_nothing_to_move() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStateStore::open(&dir.path().join("sovereign.db")).unwrap();
    let notes_db = dir.path().join("notes.db");
    let report = store.migrate_notes_db(&notes_db).await.unwrap();
    assert_eq!((report.moved, report.backup.clone()), (0, None));
    assert!(
        !notes_db.exists(),
        "the migration never creates code's store"
    );
    assert!(
        store
            .migrate_notes_db(&notes_db)
            .await
            .unwrap()
            .already_done
    );
}
