//! Working notes store — persistent, searchable scratchpad for agents.
//!
//! Notes survive across sessions and are retrieved by full-text search,
//! symbol name, file path, or kind filter. Unlike test/lint stores (which
//! are overwritten on every run), notes are only deleted explicitly via
//! [`NoteStore::delete_note`].
//!
//! ## Kinds
//!
//! - `"decision"` — architectural or implementation choices made
//! - `"attempt"` — approaches tried and abandoned (so future sessions don't repeat)
//! - `"invariant"` — constraints that must never be violated
//! - `"todo"` — follow-up work for a future session
//! - `"reflection"` — post-task structured feedback on tool quality
//!
//! ## Schema
//!
//! - **`notes`** — one row per note with JSON arrays for `symbols` and `files`.
//!   Three nullable columns (`tool_name`, `retired_at`, `retired_by`) support
//!   the reflection lifecycle: write → surface → fix → retire.
//! - **`notes_fts`** — FTS5 virtual table backed by `notes`, kept in sync by
//!   three triggers (after insert, before delete, after update).
//! - **`tool_call_log`** — ring buffer (10,000 rows) of MCP tool invocations.
//!   Records tool names and outcomes only — no parameters, no content.
//!
//! ## Threading
//!
//! `NoteStore` wraps a synchronous `rusqlite::Connection` in a
//! `tokio::sync::Mutex`. All operations are microsecond-fast; no
//! `spawn_blocking` is needed.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use tokio::sync::Mutex;

use crate::error::{Error, Result};

// ─── Types ────────────────────────────────────────────────────────────────────

/// A single note row returned from [`NoteStore::read_notes`].
#[derive(Debug, Clone)]
pub struct NoteRow {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub symbols: Vec<String>,
    pub files: Vec<String>,
    pub session_id: String,
    /// RFC 3339 timestamp string.
    pub created_at: String,
    /// Primary tool this note concerns (reflections only; `None` for other kinds).
    pub tool_name: Option<String>,
    /// Unix timestamp when this note was retired; `None` means active.
    pub retired_at: Option<i64>,
    /// Human-readable reason for retirement (e.g. "fixed in PR #88").
    pub retired_by: Option<String>,
}

/// A single row from the tool call ring buffer.
#[derive(Debug, Clone)]
pub struct ToolCallLogRow {
    pub id: String,
    pub session_id: String,
    pub tool_name: String,
    /// `"success"` | `"error"` | `"empty_result"`
    pub outcome: String,
    pub called_at: i64,
}

// ─── Store ────────────────────────────────────────────────────────────────────

/// SQLite + FTS5 store for agent working notes.
pub struct NoteStore {
    conn: Arc<Mutex<Connection>>,
}

impl NoteStore {
    /// Open or create the database at `db_path`. Schema migrations are
    /// idempotent — safe to call on an existing database.
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let conn = Connection::open(db_path).map_err(|e| {
            Error::Io(std::io::Error::other(format!(
                "NoteStore::open {}: {e}",
                db_path.display()
            )))
        })?;

        // WAL mode is always desirable — idempotent, safe to repeat.
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| Error::Io(std::io::Error::other(format!("NoteStore WAL: {e}"))))?;

        // Determine whether this is a fresh DB or an existing one, then
        // run the appropriate setup (full schema vs. incremental migration).
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);

        if version == 0 {
            let table_exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='notes'",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);

            if table_exists > 0 {
                // Existing database on old schema — apply migration.
                conn.execute_batch(MIGRATION_V1).map_err(|e| {
                    Error::Io(std::io::Error::other(format!("NoteStore migrate v1: {e}")))
                })?;
            } else {
                // Brand-new database — create full schema and mark it current.
                conn.execute_batch(SCHEMA_NEW).map_err(|e| {
                    Error::Io(std::io::Error::other(format!("NoteStore schema: {e}")))
                })?;
            }
        }
        // version >= 1: schema is current, nothing to do.

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    // ── Note writes ────────────────────────────────────────────────────────

    /// Persist a new note. Returns the generated note ID.
    ///
    /// `kind` must be one of `"decision"`, `"attempt"`, `"invariant"`, `"todo"`.
    /// Use [`write_reflection`] for `kind = "reflection"` (it additionally
    /// accepts a `tool_name`).
    pub async fn write_note(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = unix_now();
        let symbols_json = serde_json::to_string(&symbols).unwrap_or_else(|_| "[]".to_string());
        let files_json = serde_json::to_string(&files).unwrap_or_else(|_| "[]".to_string());

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO notes (id, kind, content, symbols, files, session_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![id, kind, content, symbols_json, files_json, session_id, now],
        )
        .map_err(sqlite_err)?;

        Ok(id)
    }

    /// Persist a reflection note. Returns the generated note ID.
    ///
    /// `content` should be a JSON blob containing structured reflection fields
    /// (task_summary, tools_that_helped, etc.). `tool_name` is the primary tool
    /// the reflection concerns — used as the grouping key by `sovereign reflect`.
    pub async fn write_reflection(
        &self,
        content: &str,
        tool_name: Option<&str>,
        session_id: &str,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = unix_now();

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO notes (id, kind, content, symbols, files, session_id, created_at, updated_at, tool_name)
             VALUES (?1, 'reflection', ?2, '[]', '[]', ?3, ?4, ?4, ?5)",
            params![id, content, session_id, now, tool_name],
        )
        .map_err(sqlite_err)?;

        Ok(id)
    }

    // ── Note reads ─────────────────────────────────────────────────────────

    /// Query notes. Filters compose with AND.
    ///
    /// - `query` — FTS5 full-text search, ordered by BM25 relevance.
    ///   When `None`, results are ordered by recency (newest first).
    /// - `symbols` — retain notes that mention any of these symbol names.
    /// - `files` — retain notes that mention any of these file paths.
    /// - `kinds` — retain notes whose `kind` is in this list.
    /// - `limit` — maximum number of results (capped at 100 internally).
    /// - `include_retired` — when `false` (default for agents), retired reflections
    ///   are filtered out. Pass `true` for developer history views.
    pub async fn read_notes(
        &self,
        query: Option<&str>,
        symbols: &[String],
        files: &[String],
        kinds: &[String],
        limit: usize,
        include_retired: bool,
    ) -> Result<Vec<NoteRow>> {
        let cap = limit.min(100);
        // Over-fetch when FTS is active to leave room for post-filtering.
        let fetch_limit = if query.is_some() { cap * 10 } else { cap };

        // No table alias — works in both the FTS path (n.retired_at and
        // retired_at are equivalent since only notes has this column) and the
        // recency path (no alias defined).
        let retired_clause = if include_retired {
            ""
        } else {
            "AND retired_at IS NULL"
        };

        let rows: Vec<NoteRow> = {
            let conn = self.conn.lock().await;
            if let Some(q) = query.filter(|s| !s.is_empty()) {
                // FTS5 path — BM25 relevance order.
                let sql = format!(
                    "WITH ranked AS (
                        SELECT rowid, bm25(notes_fts) AS rank
                        FROM notes_fts
                        WHERE notes_fts MATCH ?
                        LIMIT {fetch_limit}
                    )
                    SELECT n.id, n.kind, n.content, n.symbols, n.files, n.session_id,
                           n.created_at, n.tool_name, n.retired_at, n.retired_by
                    FROM notes n
                    JOIN ranked r ON r.rowid = n.rowid
                    WHERE 1=1 {retired_clause}
                    ORDER BY r.rank"
                );
                let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;
                let mapped = stmt.query_map(params![q], map_note_row).map_err(sqlite_err)?;
                let mut out = Vec::new();
                for row in mapped {
                    out.push(row.map_err(sqlite_err)?);
                }
                out
            } else {
                // Recency path.
                let sql = format!(
                    "SELECT id, kind, content, symbols, files, session_id,
                            created_at, tool_name, retired_at, retired_by
                     FROM notes
                     WHERE 1=1 {retired_clause}
                     ORDER BY created_at DESC
                     LIMIT ?"
                );
                let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;
                let mapped = stmt
                    .query_map(params![fetch_limit as i64], map_note_row)
                    .map_err(sqlite_err)?;
                let mut out = Vec::new();
                for row in mapped {
                    out.push(row.map_err(sqlite_err)?);
                }
                out
            }
        };

        // Post-filter: kinds, symbols, files (conn lock released above).
        let mut out: Vec<NoteRow> = rows
            .into_iter()
            .filter(|n| kinds.is_empty() || kinds.iter().any(|k| k == &n.kind))
            .filter(|n| symbols.is_empty() || symbols.iter().any(|s| n.symbols.contains(s)))
            .filter(|n| files.is_empty() || files.iter().any(|f| n.files.contains(f)))
            .collect();

        out.truncate(cap);
        Ok(out)
    }

    /// Return reflection notes for the developer-facing `sovereign reflect` command.
    ///
    /// - `since` — unix timestamp lower bound (0 = all time)
    /// - `tool_filter` — restrict to notes with this `tool_name`
    /// - `include_retired` — include retired reflections (for `--history`)
    pub async fn read_reflections(
        &self,
        since: i64,
        tool_filter: Option<&str>,
        include_retired: bool,
    ) -> Result<Vec<NoteRow>> {
        let retired_clause = if include_retired {
            ""
        } else {
            "AND retired_at IS NULL"
        };
        let tool_clause = if tool_filter.is_some() {
            "AND tool_name = ?"
        } else {
            ""
        };

        let sql = format!(
            "SELECT id, kind, content, symbols, files, session_id,
                    created_at, tool_name, retired_at, retired_by
             FROM notes
             WHERE kind = 'reflection'
               AND created_at >= ?
               {retired_clause}
               {tool_clause}
             ORDER BY created_at DESC
             LIMIT 1000"
        );

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;

        let mapped = if let Some(tool) = tool_filter {
            stmt.query_map(params![since, tool], map_note_row)
                .map_err(sqlite_err)?
        } else {
            stmt.query_map(params![since], map_note_row)
                .map_err(sqlite_err)?
        };

        let mut out = Vec::new();
        for row in mapped {
            out.push(row.map_err(sqlite_err)?);
        }
        Ok(out)
    }

    // ── Note deletion / retirement ─────────────────────────────────────────

    /// Delete a note by ID. Returns `true` if a row was removed, `false` if
    /// the ID was not found.
    pub async fn delete_note(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let affected = conn
            .execute("DELETE FROM notes WHERE id = ?", params![id])
            .map_err(sqlite_err)?;
        Ok(affected > 0)
    }

    /// Mark all active reflections for `tool_name` as retired.
    ///
    /// Returns the IDs of the notes that were retired. Returns an empty vec
    /// if no matching active reflections exist.
    pub async fn retire_by_tool(&self, tool_name: &str, reason: &str) -> Result<Vec<String>> {
        let now = unix_now();
        let conn = self.conn.lock().await;

        // Collect IDs first so we can return them.
        let mut stmt = conn
            .prepare(
                "SELECT id FROM notes WHERE tool_name = ? AND kind = 'reflection' AND retired_at IS NULL",
            )
            .map_err(sqlite_err)?;
        let ids: Vec<String> = stmt
            .query_map(params![tool_name], |r| r.get(0))
            .map_err(sqlite_err)?
            .filter_map(|r| r.ok())
            .collect();

        if ids.is_empty() {
            return Ok(ids);
        }

        conn.execute(
            "UPDATE notes SET retired_at = ?1, retired_by = ?2
             WHERE tool_name = ?3 AND kind = 'reflection' AND retired_at IS NULL",
            params![now, reason, tool_name],
        )
        .map_err(sqlite_err)?;

        Ok(ids)
    }

    /// Mark a single reflection as retired by its ID.
    ///
    /// Returns `true` if the note existed and was not already retired,
    /// `false` otherwise.
    pub async fn retire_by_id(&self, id: &str, reason: &str) -> Result<bool> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE notes SET retired_at = ?1, retired_by = ?2
                 WHERE id = ?3 AND retired_at IS NULL",
                params![now, reason, id],
            )
            .map_err(sqlite_err)?;
        Ok(affected > 0)
    }

    // ── Todo summary ───────────────────────────────────────────────────────

    /// Return the most recent open `todo` notes (for the startup summary).
    pub async fn open_todos(&self, limit: usize) -> Result<Vec<NoteRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, content, symbols, files, session_id,
                        created_at, tool_name, retired_at, retired_by
                 FROM notes
                 WHERE kind = 'todo' AND retired_at IS NULL
                 ORDER BY created_at DESC
                 LIMIT ?",
            )
            .map_err(sqlite_err)?;
        let mapped = stmt
            .query_map(params![limit as i64], map_note_row)
            .map_err(sqlite_err)?;
        let mut out = Vec::new();
        for row in mapped {
            out.push(row.map_err(sqlite_err)?);
        }
        Ok(out)
    }

    // ── Tool call ring buffer ──────────────────────────────────────────────

    /// Record a single MCP tool invocation. Fire-and-forget: errors are
    /// silently ignored by callers so a logging failure never kills a tool call.
    ///
    /// Automatically purges rows beyond the 10,000-row ring buffer limit.
    pub async fn log_tool_call(
        &self,
        session_id: &str,
        tool_name: &str,
        outcome: &str,
    ) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = unix_now();
        let conn = self.conn.lock().await;

        conn.execute(
            "INSERT INTO tool_call_log (id, session_id, tool_name, outcome, called_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, session_id, tool_name, outcome, now],
        )
        .map_err(sqlite_err)?;

        // Trim to ring buffer limit.
        conn.execute(
            "DELETE FROM tool_call_log WHERE id IN (
                SELECT id FROM tool_call_log ORDER BY called_at DESC LIMIT -1 OFFSET 10000
             )",
            [],
        )
        .map_err(sqlite_err)?;

        Ok(())
    }

    /// Return recent tool call log entries for the developer-facing `sovereign reflect --log`.
    pub async fn tool_call_log_rows(
        &self,
        since: i64,
        limit: usize,
    ) -> Result<Vec<ToolCallLogRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, tool_name, outcome, called_at
                 FROM tool_call_log
                 WHERE called_at >= ?
                 ORDER BY called_at DESC, rowid DESC
                 LIMIT ?",
            )
            .map_err(sqlite_err)?;
        let mapped = stmt
            .query_map(params![since, limit as i64], |r| {
                Ok(ToolCallLogRow {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    tool_name: r.get(2)?,
                    outcome: r.get(3)?,
                    called_at: r.get(4)?,
                })
            })
            .map_err(sqlite_err)?;
        let mut out = Vec::new();
        for row in mapped {
            out.push(row.map_err(sqlite_err)?);
        }
        Ok(out)
    }
}

// ─── Schema (new databases) ───────────────────────────────────────────────────

/// Full schema for brand-new databases. Includes reflection support and
/// tool_call_log from the start.
const SCHEMA_NEW: &str = "
PRAGMA journal_mode=WAL;

CREATE TABLE IF NOT EXISTS notes (
    id         TEXT    PRIMARY KEY,
    kind       TEXT    NOT NULL CHECK(kind IN ('decision','attempt','invariant','todo','reflection')),
    content    TEXT    NOT NULL,
    symbols    TEXT    NOT NULL DEFAULT '[]',
    files      TEXT    NOT NULL DEFAULT '[]',
    session_id TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    tool_name  TEXT,
    retired_at INTEGER,
    retired_by TEXT
);
CREATE INDEX IF NOT EXISTS idx_notes_kind       ON notes(kind);
CREATE INDEX IF NOT EXISTS idx_notes_created    ON notes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_tool_name  ON notes(tool_name);
CREATE INDEX IF NOT EXISTS idx_notes_retired_at ON notes(retired_at);

CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
    content, kind,
    content='notes',
    content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TRIGGER IF NOT EXISTS notes_fts_ad BEFORE DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
END;

CREATE TRIGGER IF NOT EXISTS notes_fts_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TABLE IF NOT EXISTS tool_call_log (
    id         TEXT    PRIMARY KEY,
    session_id TEXT    NOT NULL,
    tool_name  TEXT    NOT NULL,
    outcome    TEXT    NOT NULL,
    called_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_log_session ON tool_call_log(session_id);
CREATE INDEX IF NOT EXISTS idx_log_tool    ON tool_call_log(tool_name);
CREATE INDEX IF NOT EXISTS idx_log_called  ON tool_call_log(called_at DESC);

PRAGMA user_version = 1;
";

// ─── Schema migration v0 → v1 ────────────────────────────────────────────────

/// Applied to existing databases whose `user_version = 0`. Uses SQLite's
/// standard rename-recreate-copy-drop pattern to update the CHECK constraint
/// and add three new nullable columns. Rebuilds FTS5 and triggers.
const MIGRATION_V1: &str = "
BEGIN;

ALTER TABLE notes RENAME TO notes_v0;

CREATE TABLE notes (
    id         TEXT    PRIMARY KEY,
    kind       TEXT    NOT NULL CHECK(kind IN ('decision','attempt','invariant','todo','reflection')),
    content    TEXT    NOT NULL,
    symbols    TEXT    NOT NULL DEFAULT '[]',
    files      TEXT    NOT NULL DEFAULT '[]',
    session_id TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    tool_name  TEXT,
    retired_at INTEGER,
    retired_by TEXT
);

INSERT INTO notes (id, kind, content, symbols, files, session_id, created_at, updated_at)
SELECT id, kind, content, symbols, files, session_id, created_at, updated_at FROM notes_v0;

DROP TABLE notes_v0;

CREATE INDEX IF NOT EXISTS idx_notes_kind       ON notes(kind);
CREATE INDEX IF NOT EXISTS idx_notes_created    ON notes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_tool_name  ON notes(tool_name);
CREATE INDEX IF NOT EXISTS idx_notes_retired_at ON notes(retired_at);

DROP TABLE IF EXISTS notes_fts;
CREATE VIRTUAL TABLE notes_fts USING fts5(
    content, kind,
    content='notes',
    content_rowid='rowid'
);
INSERT INTO notes_fts(notes_fts) VALUES('rebuild');

DROP TRIGGER IF EXISTS notes_fts_ai;
DROP TRIGGER IF EXISTS notes_fts_ad;
DROP TRIGGER IF EXISTS notes_fts_au;

CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
END;

CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TABLE IF NOT EXISTS tool_call_log (
    id         TEXT    PRIMARY KEY,
    session_id TEXT    NOT NULL,
    tool_name  TEXT    NOT NULL,
    outcome    TEXT    NOT NULL,
    called_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_log_session ON tool_call_log(session_id);
CREATE INDEX IF NOT EXISTS idx_log_tool    ON tool_call_log(tool_name);
CREATE INDEX IF NOT EXISTS idx_log_called  ON tool_call_log(called_at DESC);

PRAGMA user_version = 1;

COMMIT;
";

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn sqlite_err(e: rusqlite::Error) -> Error {
    Error::Io(std::io::Error::other(format!("NoteStore sqlite: {e}")))
}

fn map_note_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteRow> {
    let symbols_json: String = row.get(3)?;
    let files_json: String = row.get(4)?;
    let created_at_secs: i64 = row.get(6)?;

    let symbols: Vec<String> = serde_json::from_str(&symbols_json).unwrap_or_default();
    let files: Vec<String> = serde_json::from_str(&files_json).unwrap_or_default();

    // Convert unix timestamp to RFC 3339.
    let created_at = chrono::DateTime::<chrono::Utc>::from_timestamp(created_at_secs, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| created_at_secs.to_string());

    Ok(NoteRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        content: row.get(2)?,
        symbols,
        files,
        session_id: row.get(5)?,
        created_at,
        tool_name: row.get(7)?,
        retired_at: row.get(8)?,
        retired_by: row.get(9)?,
    })
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_store() -> NoteStore {
        let dir = tempfile::tempdir().unwrap();
        NoteStore::open(&dir.path().join("notes.db")).unwrap()
    }

    // ── Existing note tests (must continue to pass) ──────────────────────

    #[tokio::test]
    async fn write_note_roundtrip() {
        let store = make_store().await;
        let id = store
            .write_note(
                "decision",
                "Use BFS for blast radius",
                vec!["blast_radius".into()],
                vec!["src/lib.rs".into()],
                "test-session",
            )
            .await
            .unwrap();
        assert!(!id.is_empty());

        let notes = store
            .read_notes(None, &[], &[], &[], 10, false)
            .await
            .unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].kind, "decision");
        assert_eq!(notes[0].content, "Use BFS for blast radius");
        assert_eq!(notes[0].symbols, vec!["blast_radius"]);
        assert!(notes[0].tool_name.is_none());
        assert!(notes[0].retired_at.is_none());
    }

    #[tokio::test]
    async fn read_notes_fts_search() {
        let store = make_store().await;
        store
            .write_note("decision", "Use BFS for blast radius traversal", vec![], vec![], "s1")
            .await
            .unwrap();
        store
            .write_note("todo", "Unrelated note about caching", vec![], vec![], "s1")
            .await
            .unwrap();

        let results = store
            .read_notes(Some("blast radius"), &[], &[], &[], 10, false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("blast radius"));
    }

    #[tokio::test]
    async fn read_notes_symbol_filter() {
        let store = make_store().await;
        store
            .write_note("attempt", "tried foo", vec!["foo_fn".into()], vec![], "s1")
            .await
            .unwrap();
        store
            .write_note("attempt", "tried bar", vec!["bar_fn".into()], vec![], "s1")
            .await
            .unwrap();

        let results = store
            .read_notes(None, &["foo_fn".to_string()], &[], &[], 10, false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("foo"));
    }

    #[tokio::test]
    async fn read_notes_kind_filter() {
        let store = make_store().await;
        store
            .write_note("decision", "keep this", vec![], vec![], "s1")
            .await
            .unwrap();
        store
            .write_note("todo", "do this later", vec![], vec![], "s1")
            .await
            .unwrap();

        let results = store
            .read_notes(None, &[], &[], &["todo".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "todo");
    }

    #[tokio::test]
    async fn delete_note_removes() {
        let store = make_store().await;
        let id = store
            .write_note("invariant", "never call this twice", vec![], vec![], "s1")
            .await
            .unwrap();

        let deleted = store.delete_note(&id).await.unwrap();
        assert!(deleted);

        let notes = store.read_notes(None, &[], &[], &[], 10, false).await.unwrap();
        assert!(notes.is_empty());

        // Deleting again returns false.
        let deleted_again = store.delete_note(&id).await.unwrap();
        assert!(!deleted_again);
    }

    #[tokio::test]
    async fn open_todos_returns_only_todos() {
        let store = make_store().await;
        store
            .write_note("todo", "fix the thing", vec![], vec![], "s1")
            .await
            .unwrap();
        store
            .write_note("decision", "keep it", vec![], vec![], "s1")
            .await
            .unwrap();

        let todos = store.open_todos(10).await.unwrap();
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].content, "fix the thing");
    }

    // ── Reflection tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn reflection_stored_with_kind() {
        let store = make_store().await;
        let id = store
            .write_reflection(
                r#"{"task_summary":"Refactored EmbedFn"}"#,
                Some("blast_radius"),
                "s1",
            )
            .await
            .unwrap();
        assert!(!id.is_empty());

        let results = store
            .read_notes(None, &[], &[], &["reflection".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "reflection");
        assert_eq!(results[0].tool_name.as_deref(), Some("blast_radius"));
    }

    #[tokio::test]
    async fn reflection_active_by_default() {
        let store = make_store().await;
        let id = store
            .write_reflection(r#"{"task_summary":"test"}"#, None, "s1")
            .await
            .unwrap();

        let notes = store.read_notes(None, &[], &[], &[], 10, false).await.unwrap();
        let note = notes.iter().find(|n| n.id == id).unwrap();
        assert!(note.retired_at.is_none());
        assert!(note.retired_by.is_none());
    }

    #[tokio::test]
    async fn retired_reflection_hidden_by_default() {
        let store = make_store().await;
        let id = store
            .write_reflection(r#"{"task_summary":"test"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();

        let retired = store.retire_by_id(&id, "fixed in PR #1").await.unwrap();
        assert!(retired);

        // Default read (include_retired=false) should not return it.
        let notes = store.read_notes(None, &[], &[], &[], 10, false).await.unwrap();
        assert!(notes.is_empty());

        // But read_notes with include_retired=true should.
        let all = store.read_notes(None, &[], &[], &[], 10, true).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].retired_by.as_deref(), Some("fixed in PR #1"));
        assert!(all[0].retired_at.is_some());
    }

    #[tokio::test]
    async fn retired_reflection_visible_in_history() {
        let store = make_store().await;
        let id = store
            .write_reflection(r#"{"task_summary":"test"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();
        store.retire_by_id(&id, "v0.4.2").await.unwrap();

        let reflections = store.read_reflections(0, None, true).await.unwrap();
        assert_eq!(reflections.len(), 1);
        assert_eq!(reflections[0].retired_by.as_deref(), Some("v0.4.2"));
    }

    #[tokio::test]
    async fn retire_by_tool_matches_all() {
        let store = make_store().await;
        for _ in 0..3 {
            store
                .write_reflection(r#"{"task_summary":"blast radius miss"}"#, Some("blast_radius"), "s1")
                .await
                .unwrap();
        }
        // Unrelated reflection — should not be retired.
        store
            .write_reflection(r#"{"task_summary":"project context miss"}"#, Some("project_context"), "s1")
            .await
            .unwrap();

        let retired_ids = store.retire_by_tool("blast_radius", "macro support added").await.unwrap();
        assert_eq!(retired_ids.len(), 3);

        // blast_radius reflections gone from default read.
        let active = store
            .read_notes(None, &[], &[], &["reflection".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].tool_name.as_deref(), Some("project_context"));
    }

    #[tokio::test]
    async fn retire_by_id_leaves_others_active() {
        let store = make_store().await;
        let id1 = store
            .write_reflection(r#"{"task_summary":"a"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();
        let _id2 = store
            .write_reflection(r#"{"task_summary":"b"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();

        let retired = store.retire_by_id(&id1, "fixed").await.unwrap();
        assert!(retired);

        let active = store
            .read_notes(None, &[], &[], &["reflection".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, _id2);
    }

    #[tokio::test]
    async fn retire_by_id_already_retired_returns_false() {
        let store = make_store().await;
        let id = store
            .write_reflection(r#"{"task_summary":"a"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();
        store.retire_by_id(&id, "first").await.unwrap();
        let second = store.retire_by_id(&id, "second").await.unwrap();
        assert!(!second);
    }

    // ── tool_call_log tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn tool_call_log_records_outcome() {
        let store = make_store().await;
        store.log_tool_call("sess-1", "lint_status", "success").await.unwrap();
        store.log_tool_call("sess-1", "blast_radius", "error").await.unwrap();

        let rows = store.tool_call_log_rows(0, 100).await.unwrap();
        assert_eq!(rows.len(), 2);
        // Most recent first.
        assert_eq!(rows[0].tool_name, "blast_radius");
        assert_eq!(rows[0].outcome, "error");
        assert_eq!(rows[1].tool_name, "lint_status");
        assert_eq!(rows[1].outcome, "success");
    }

    #[tokio::test]
    async fn tool_call_log_ring_buffer() {
        let store = make_store().await;
        for i in 0..10_001usize {
            store
                .log_tool_call("sess", "lint_status", "success")
                .await
                .unwrap();
            let _ = i; // suppress unused warning
        }

        let rows = store.tool_call_log_rows(0, 20_000).await.unwrap();
        assert_eq!(rows.len(), 10_000);
    }

    // ── Migration test ────────────────────────────────────────────────────

    #[tokio::test]
    async fn migration_v0_to_v1_preserves_data_and_enables_reflections() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");

        // Simulate an old-schema database (no tool_name/retired_at/retired_by,
        // restricted CHECK constraint, no tool_call_log).
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE notes (
                     id TEXT PRIMARY KEY,
                     kind TEXT NOT NULL CHECK(kind IN ('decision','attempt','invariant','todo')),
                     content TEXT NOT NULL,
                     symbols TEXT NOT NULL DEFAULT '[]',
                     files TEXT NOT NULL DEFAULT '[]',
                     session_id TEXT NOT NULL,
                     created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 CREATE VIRTUAL TABLE notes_fts USING fts5(content, kind, content='notes', content_rowid='rowid');
                 INSERT INTO notes VALUES ('id-1','todo','old note','[]','[]','s0',1000,1000);",
            )
            .unwrap();
            // user_version stays 0 (default).
        }

        // Open with new NoteStore — migration should run.
        let store = NoteStore::open(&db_path).unwrap();

        // Old note is preserved.
        let notes = store.read_notes(None, &[], &[], &[], 10, false).await.unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].content, "old note");

        // Reflection kind is now accepted.
        let id = store
            .write_reflection(r#"{"task_summary":"post-migration"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();
        assert!(!id.is_empty());

        // tool_call_log is available.
        store.log_tool_call("sess", "lint_status", "success").await.unwrap();
        let rows = store.tool_call_log_rows(0, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
    }
}
