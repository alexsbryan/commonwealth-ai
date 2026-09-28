// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one-time move of svrn's rows out of the code program's `notes.db`
//! into svrn's store (phase-b pb-notes-memory, phase-b-30 F4 (a),
//! phase-b-34). Before this, one `notes.db` held both programs' notes.
//!
//! svrn's rows are selected by kind plus writer: every `lesson` and
//! `tool_decision`, and the commissive handler's `todo` / `commitment`
//! (Session scope, not anchored to the backlog). Every other row stays
//! code's. The move takes a consistent copy of `notes.db` first, conserves
//! the row count or rolls back, and writes a marker last, so a second run
//! moves nothing.

use std::path::{Path, PathBuf};

use super::*;

/// The marker's name in `store_markers`; also the copy's suffix.
pub const NOTES_DB_MIGRATION: &str = "pb-notes-memory";

/// svrn's rows in `notes.db`, as SQL over its `notes` table.
const SVRN_ROWS: &str = "(kind IN ('lesson', 'tool_decision') \
     OR (kind IN ('todo', 'commitment') AND scope = 'session' \
         AND COALESCE(related_entity, '') != 'backlog'))";

/// The columns both tables share, in one order.
const MOVED_COLUMNS: &str = "id, kind, content, symbols, files, session_id, created_at, \
     updated_at, tool_name, retired_at, retired_by, scope, feature_id, promoted_from, \
     related_entity, source, supersedes, payload_json, private, origin_node_id, tombstone";

/// What one run of [`SqliteStateStore::migrate_notes_db`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotesDbMigration {
    /// The pre-migration copy, when this run needed one.
    pub backup: Option<PathBuf>,
    /// Rows in `notes.db` before the move.
    pub rows_before: i64,
    /// svrn's rows moved into this store.
    pub moved: i64,
    /// Rows left in `notes.db`, code's.
    pub kept: i64,
    /// The marker was already there: nothing ran.
    pub already_done: bool,
}

/// `<notes.db>.pre-pb-notes-memory`, beside the file it copies.
pub fn notes_db_backup_path(notes_db: &Path) -> PathBuf {
    let mut name = notes_db.as_os_str().to_owned();
    name.push(format!(".pre-{NOTES_DB_MIGRATION}"));
    PathBuf::from(name)
}

pub(super) fn run_store_markers_migration(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS store_markers (
             name       TEXT    PRIMARY KEY,
             applied_at INTEGER NOT NULL,
             detail     TEXT    NOT NULL
         );",
    )
}

fn marker_done(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM store_markers WHERE name = ?1)",
        rusqlite::params![NOTES_DB_MIGRATION],
        |r| r.get(0),
    )
    .map_err(map_db)
}

fn write_marker(conn: &Connection, detail: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO store_markers (name, applied_at, detail) VALUES (?1, ?2, ?3)",
        rusqlite::params![NOTES_DB_MIGRATION, now(), detail],
    )
}

/// A consistent copy of the live file, taken by SQLite (`VACUUM INTO`), so a
/// WAL still holding recent writes is in the copy. An existing copy is kept:
/// it predates any earlier, interrupted run.
fn take_copy(notes_db: &Path) -> Result<PathBuf> {
    let backup = notes_db_backup_path(notes_db);
    if backup.exists() {
        tracing::warn!(backup = %backup.display(),
            "notes migration: a pre-migration copy already exists; keeping the older one");
        return Ok(backup);
    }
    let src = Connection::open(notes_db).map_err(map_db)?;
    let _ = src.busy_timeout(std::time::Duration::from_secs(5));
    src.execute(
        "VACUUM INTO ?1",
        rusqlite::params![backup.to_string_lossy()],
    )
    .map_err(map_db)?;
    Ok(backup)
}

fn count(conn: &Connection, sql: &str) -> rusqlite::Result<i64> {
    conn.query_row(sql, [], |r| r.get(0))
}

/// The move itself, inside one transaction on this store's connection with
/// `notes.db` attached. Conservation is checked before the commit.
fn move_rows(conn: &Connection, backup: &Path) -> Result<NotesDbMigration> {
    let rows_before = count(conn, "SELECT COUNT(*) FROM code_notes.notes").map_err(map_db)?;
    let inserted = conn
        .execute(
            &format!(
                "INSERT INTO main.memory_notes ({MOVED_COLUMNS}) \
                 SELECT {MOVED_COLUMNS} FROM code_notes.notes WHERE {SVRN_ROWS}"
            ),
            [],
        )
        .map_err(map_db)? as i64;
    let moved = conn
        .execute(
            &format!("DELETE FROM code_notes.notes WHERE {SVRN_ROWS}"),
            [],
        )
        .map_err(map_db)? as i64;
    let kept = count(conn, "SELECT COUNT(*) FROM code_notes.notes").map_err(map_db)?;
    if inserted != moved || rows_before != moved + kept {
        return Err(Error::Storage(format!(
            "notes migration: row count not conserved: {rows_before} before, \
             {inserted} copied, {moved} removed, {kept} kept"
        )));
    }
    write_marker(
        conn,
        &format!(
            "moved {moved} of {rows_before} rows; {kept} kept in notes.db; copy {}",
            backup.display()
        ),
    )
    .map_err(map_db)?;
    Ok(NotesDbMigration {
        backup: Some(backup.to_path_buf()),
        rows_before,
        moved,
        kept,
        already_done: false,
    })
}

impl SqliteStateStore {
    /// Move svrn's rows out of `notes_db` into this store, once. A run after
    /// the marker moves nothing and reports `already_done`. With no
    /// `notes.db` (a fresh install) there is nothing to move and the marker
    /// is written. An `Err` leaves the marker unwritten, so the next run
    /// tries again, and every row where it was.
    pub async fn migrate_notes_db(&self, notes_db: &Path) -> Result<NotesDbMigration> {
        let conn = self.conn.lock().await;
        if marker_done(&conn)? {
            tracing::debug!(notes_db = %notes_db.display(), "notes migration: already done");
            return Ok(NotesDbMigration {
                backup: None,
                rows_before: 0,
                moved: 0,
                kept: 0,
                already_done: true,
            });
        }
        if !notes_db.exists() {
            write_marker(&conn, "no notes.db: nothing to move").map_err(map_db)?;
            tracing::info!(notes_db = %notes_db.display(),
                "notes migration: no notes.db, nothing to move; marked done");
            return Ok(NotesDbMigration {
                backup: None,
                rows_before: 0,
                moved: 0,
                kept: 0,
                already_done: false,
            });
        }
        let backup = take_copy(notes_db)?;
        conn.execute(
            "ATTACH DATABASE ?1 AS code_notes",
            rusqlite::params![notes_db.to_string_lossy()],
        )
        .map_err(map_db)?;
        let outcome = conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(map_db)
            .and_then(|()| move_rows(&conn, &backup));
        let finished = match outcome {
            Ok(report) => conn.execute_batch("COMMIT").map(|()| report).map_err(map_db),
            Err(e) => {
                if let Err(rb) = conn.execute_batch("ROLLBACK") {
                    tracing::error!(error = %rb, "notes migration: rollback failed");
                }
                Err(e)
            }
        };
        if let Err(e) = conn.execute_batch("DETACH DATABASE code_notes") {
            tracing::warn!(error = %e, "notes migration: detach failed");
        }
        let report = finished?;
        tracing::info!(
            notes_db = %notes_db.display(),
            backup = %backup.display(),
            rows_before = report.rows_before,
            moved = report.moved,
            kept = report.kept,
            "notes migration: svrn's rows moved out of notes.db"
        );
        Ok(report)
    }
}
