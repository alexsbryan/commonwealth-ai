// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn's memory notes: the rows svrn writes about its own turns (lessons,
//! the `tool_decision` dossier, the commissive handler's commitments and
//! todos) and svrn's MCP call log, kept in svrn's own store (phase-b
//! pb-notes-memory, phase-b-30 F4 (a)). `notes.db` is the code program's.
//!
//! It implements the two ports svrn's consumers already take
//! ([`AgentNotes`], [`RecipeNotes`]), so no consumer names the store behind
//! them. A table of its own, not `memories` rows: every `memories` row is a
//! recall candidate for the system prompt, and a lesson or a dossier row is
//! not a memory of the user. Nothing here gossips; svrn's store has no
//! propagation sink.

use super::*;
use sovereign_core::daemon_wire::NoteEntry;
use sovereign_core::notes::{AgentNotes, ToolCallLogRow};
use sovereign_core::recipe::notes::{Note, NoteScope, NoteSource, RecipeNotes, ScopeFilter};

/// The kinds svrn keeps for itself. Every other kind is the code program's
/// decision notes; svrn's routes answer a request for one with a pointer to
/// code's `notes` tool rather than an empty list (principle 6).
pub const MEMORY_NOTE_KINDS: [&str; 4] = ["lesson", "tool_decision", "todo", "commitment"];

/// Whether `kind` is one svrn keeps ([`MEMORY_NOTE_KINDS`]).
pub fn is_memory_note_kind(kind: &str) -> bool {
    MEMORY_NOTE_KINDS.contains(&kind)
}

/// A read returns at most this many rows, whatever the caller asks for —
/// the cap `notes.db` applies, kept so the lesson pane pages the same.
const READ_CAP: usize = 100;

/// Call-log rows kept; older rows are pruned on write (as `notes.db` did).
const CALL_LOG_KEEP: i64 = 10_000;

const ENTRY_COLUMNS: &str = "n.id, n.kind, n.content, n.symbols, n.files, n.session_id, \
     n.created_at, n.tool_name, n.retired_at, n.retired_by, n.scope, n.feature_id, \
     n.promoted_from, n.related_entity, n.source, n.supersedes, n.payload_json, \
     n.origin_node_id";

pub(super) fn run_memory_notes_migration(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS memory_notes (
            id             TEXT    PRIMARY KEY,
            kind           TEXT    NOT NULL,
            content        TEXT    NOT NULL,
            symbols        TEXT    NOT NULL DEFAULT '[]',
            files          TEXT    NOT NULL DEFAULT '[]',
            session_id     TEXT    NOT NULL,
            created_at     INTEGER NOT NULL,
            updated_at     INTEGER NOT NULL,
            tool_name      TEXT,
            retired_at     INTEGER,
            retired_by     TEXT,
            scope          TEXT    NOT NULL DEFAULT 'global',
            feature_id     TEXT,
            promoted_from  TEXT,
            related_entity TEXT,
            source         TEXT    NOT NULL DEFAULT 'agent',
            supersedes     TEXT,
            payload_json   TEXT,
            private        INTEGER NOT NULL DEFAULT 0,
            origin_node_id TEXT,
            tombstone      INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_memory_notes_kind    ON memory_notes(kind, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_memory_notes_entity
            ON memory_notes(related_entity) WHERE related_entity IS NOT NULL;

        CREATE VIRTUAL TABLE IF NOT EXISTS memory_notes_fts USING fts5(
            content, kind, content='memory_notes', content_rowid='rowid'
        );
        CREATE TRIGGER IF NOT EXISTS memory_notes_fts_ai AFTER INSERT ON memory_notes BEGIN
            INSERT INTO memory_notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
        END;
        CREATE TRIGGER IF NOT EXISTS memory_notes_fts_ad AFTER DELETE ON memory_notes BEGIN
            INSERT INTO memory_notes_fts(memory_notes_fts, rowid, content, kind)
            VALUES ('delete', old.rowid, old.content, old.kind);
        END;
        CREATE TRIGGER IF NOT EXISTS memory_notes_fts_au AFTER UPDATE ON memory_notes BEGIN
            INSERT INTO memory_notes_fts(memory_notes_fts, rowid, content, kind)
            VALUES ('delete', old.rowid, old.content, old.kind);
            INSERT INTO memory_notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
        END;

        CREATE TABLE IF NOT EXISTS tool_call_log (
            id         TEXT    PRIMARY KEY,
            session_id TEXT    NOT NULL,
            tool_name  TEXT    NOT NULL,
            outcome    TEXT    NOT NULL,
            called_at  INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_tool_call_log_called ON tool_call_log(called_at DESC);
        ",
    )
}

/// A JSON-array column; a malformed one fails the read rather than reading
/// as an empty list.
fn json_list(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Vec<String>> {
    let raw: String = row.get(idx)?;
    serde_json::from_str(&raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn map_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteEntry> {
    let created_at: i64 = row.get(6)?;
    Ok(NoteEntry {
        id: row.get(0)?,
        kind: row.get(1)?,
        content: row.get(2)?,
        symbols: json_list(row, 3)?,
        files: json_list(row, 4)?,
        session_id: row.get(5)?,
        // RFC 3339 as `notes.db` renders it; a stamp chrono cannot place
        // is shown as the raw seconds, never as a made-up date.
        created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(created_at, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| created_at.to_string()),
        tool_name: row.get(7)?,
        retired_at: row.get(8)?,
        retired_by: row.get(9)?,
        scope: row.get(10)?,
        feature_id: row.get(11)?,
        promoted_from: row.get(12)?,
        related_entity: row.get(13)?,
        source: row.get(14)?,
        supersedes: row.get(15)?,
        payload_json: row.get(16)?,
        origin_node_id: row.get(17)?,
        // Gossip receipts: svrn's store never sends or receives a note.
        sent_at: None,
        received_at: None,
    })
}

fn to_note(e: NoteEntry) -> Note {
    Note {
        id: e.id,
        kind: e.kind,
        content: e.content,
        symbols: e.symbols,
        files: e.files,
        session_id: e.session_id,
        created_at: e.created_at,
        tool_name: e.tool_name,
        retired_at: e.retired_at,
        retired_by: e.retired_by,
        scope: e.scope,
        feature_id: e.feature_id,
        promoted_from: e.promoted_from,
        related_entity: e.related_entity,
        source: e.source,
        supersedes: e.supersedes,
        payload_json: e.payload_json,
    }
}

/// `AND n.<col> IN (?,…)` over a JSON array column, or over a plain column.
fn push_in(sql: &mut String, bound: &mut Vec<rusqlite::types::Value>, head: &str, vals: &[String]) {
    if vals.is_empty() {
        return;
    }
    sql.push_str(head);
    for (i, v) in vals.iter().enumerate() {
        sql.push_str(if i == 0 { "?" } else { ",?" });
        bound.push(rusqlite::types::Value::Text(v.clone()));
    }
    sql.push(')');
    if head.contains("json_each") {
        sql.push(')');
    }
}

/// The one read: filters, then FTS rank when `query` has terms, else newest
/// first. Mirrors `notes.db`'s filter semantics (any listed symbol / file /
/// kind; retired and tombstoned rows hidden unless asked for).
#[allow(clippy::too_many_arguments)]
fn select_entries(
    conn: &Connection,
    query: Option<&str>,
    symbols: &[String],
    files: &[String],
    kinds: &[String],
    limit: usize,
    include_retired: bool,
    scope_filter: &ScopeFilter,
) -> Result<Vec<NoteEntry>> {
    let mut filter = String::new();
    let mut bound: Vec<rusqlite::types::Value> = Vec::new();
    if !include_retired {
        filter.push_str(" AND n.retired_at IS NULL AND n.tombstone = 0");
    }
    push_in(&mut filter, &mut bound, " AND n.kind IN (", kinds);
    push_in(
        &mut filter,
        &mut bound,
        " AND EXISTS (SELECT 1 FROM json_each(n.symbols) WHERE value IN (",
        symbols,
    );
    push_in(
        &mut filter,
        &mut bound,
        " AND EXISTS (SELECT 1 FROM json_each(n.files) WHERE value IN (",
        files,
    );
    let scopes: Vec<String> = scope_filter
        .scopes
        .iter()
        .map(|s| s.as_str().to_string())
        .collect();
    push_in(&mut filter, &mut bound, " AND n.scope IN (", &scopes);
    if let Some(fid) = &scope_filter.feature_id {
        filter.push_str(" AND (n.scope != 'feature' OR n.feature_id = ?)");
        bound.push(rusqlite::types::Value::Text(fid.clone()));
    }

    let terms = query.map(sanitize_fts5_query).unwrap_or_default();
    let (sql, mut params) = if terms.is_empty() {
        (
            format!(
                "SELECT {ENTRY_COLUMNS} FROM memory_notes n WHERE 1=1{filter} \
                 ORDER BY n.created_at DESC, n.rowid DESC LIMIT ?"
            ),
            bound,
        )
    } else {
        let mut p = vec![rusqlite::types::Value::Text(terms)];
        p.extend(bound);
        (
            format!(
                "SELECT {ENTRY_COLUMNS} FROM memory_notes n \
                 JOIN (SELECT rowid, bm25(memory_notes_fts) AS rank FROM memory_notes_fts \
                       WHERE memory_notes_fts MATCH ?) r ON r.rowid = n.rowid \
                 WHERE 1=1{filter} ORDER BY r.rank LIMIT ?"
            ),
            p,
        )
    };
    params.push(rusqlite::types::Value::Integer(limit.min(READ_CAP) as i64));
    let mut stmt = conn.prepare(&sql).map_err(map_db)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params), map_entry)
        .map_err(map_db)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(map_db)?;
    Ok(rows)
}

impl SqliteStateStore {
    /// Filtered read of svrn's memory notes in the wire shape `/v1/notes`
    /// serves.
    #[allow(clippy::too_many_arguments)]
    pub async fn memory_note_entries(
        &self,
        query: Option<&str>,
        symbols: &[String],
        files: &[String],
        kinds: &[String],
        limit: usize,
        include_retired: bool,
    ) -> Result<Vec<NoteEntry>> {
        let conn = self.conn.lock().await;
        select_entries(
            &conn,
            query,
            symbols,
            files,
            kinds,
            limit,
            include_retired,
            &ScopeFilter::default(),
        )
    }

    /// One memory note by id, retired or not.
    pub async fn memory_note_entry(&self, id: &str) -> Result<Option<NoteEntry>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {ENTRY_COLUMNS} FROM memory_notes n WHERE n.id = ?1"),
            rusqlite::params![id],
            map_entry,
        )
        .optional()
        .map_err(map_db)
    }

    /// Write one memory note; returns its id. `private` is kept with the
    /// row for the day svrn's notes gossip again (phase-c); nothing reads it
    /// today because nothing sends.
    #[allow(clippy::too_many_arguments)]
    pub async fn write_memory_note(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
        source: NoteSource,
        supersedes: Option<&str>,
        payload_json: Option<&str>,
        private: bool,
    ) -> Result<String> {
        if scope == NoteScope::Feature && feature_id.is_none() {
            return Err(Error::InvalidInput(
                "memory note: scope 'feature' requires a feature_id".into(),
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let ts = now();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO memory_notes (id, kind, content, symbols, files, session_id,
                 created_at, updated_at, scope, feature_id, related_entity, source,
                 supersedes, payload_json, private)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                id,
                kind,
                content,
                serde_json::to_string(&symbols).map_err(map_json)?,
                serde_json::to_string(&files).map_err(map_json)?,
                session_id,
                ts,
                scope.as_str(),
                feature_id,
                related_entity,
                source.as_str(),
                supersedes,
                payload_json,
                private as i64,
            ],
        )
        .map_err(map_db)?;
        tracing::debug!(note_id = %id, kind, scope = scope.as_str(), "memory_notes: written");
        Ok(id)
    }

    /// Strike a note through, keeping the row; `false` when no live row matched.
    pub async fn retire_memory_note(&self, id: &str, reason: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE memory_notes SET retired_at = ?1, retired_by = ?2, updated_at = ?1
                 WHERE id = ?3 AND retired_at IS NULL",
                rusqlite::params![now(), reason, id],
            )
            .map_err(map_db)?;
        Ok(n > 0)
    }

    /// Delete a note outright; `false` when there was none.
    pub async fn delete_memory_note(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM memory_notes WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(map_db)?;
        Ok(n > 0)
    }
}

#[async_trait]
impl RecipeNotes for SqliteStateStore {
    async fn write_note_full(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
        source: NoteSource,
        supersedes: Option<&str>,
        payload_json: Option<&str>,
    ) -> Result<String> {
        self.write_memory_note(
            kind,
            content,
            symbols,
            files,
            session_id,
            scope,
            feature_id,
            related_entity,
            source,
            supersedes,
            payload_json,
            false,
        )
        .await
    }

    async fn read_notes_scoped(
        &self,
        query: Option<&str>,
        symbols: &[String],
        files: &[String],
        kinds: &[String],
        limit: usize,
        include_retired: bool,
        scope_filter: &ScopeFilter,
    ) -> Result<Vec<Note>> {
        let conn = self.conn.lock().await;
        let rows = select_entries(
            &conn,
            query,
            symbols,
            files,
            kinds,
            limit,
            include_retired,
            scope_filter,
        )?;
        Ok(rows.into_iter().map(to_note).collect())
    }
}

#[async_trait]
impl AgentNotes for SqliteStateStore {
    async fn read_notes(
        &self,
        query: Option<&str>,
        symbols: &[String],
        files: &[String],
        kinds: &[String],
        limit: usize,
        include_retired: bool,
    ) -> Result<Vec<Note>> {
        let rows = self
            .memory_note_entries(query, symbols, files, kinds, limit, include_retired)
            .await?;
        Ok(rows.into_iter().map(to_note).collect())
    }

    async fn read_notes_by_related_entity(
        &self,
        related_entity: &str,
        kinds: &[&str],
    ) -> Result<Vec<Note>> {
        let conn = self.conn.lock().await;
        let mut sql = format!(
            "SELECT {ENTRY_COLUMNS} FROM memory_notes n WHERE n.related_entity = ?1 \
             AND n.retired_at IS NULL AND n.tombstone = 0"
        );
        let mut params = vec![rusqlite::types::Value::Text(related_entity.to_string())];
        let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
        push_in(&mut sql, &mut params, " AND n.kind IN (", &kinds);
        sql.push_str(" ORDER BY n.created_at DESC, n.rowid DESC");
        let mut stmt = conn.prepare(&sql).map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params), map_entry)
            .map_err(map_db)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_db)?;
        Ok(rows.into_iter().map(to_note).collect())
    }

    async fn has_active_note_with_content(
        &self,
        kind: &str,
        content: &str,
        source: NoteSource,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM memory_notes WHERE kind = ?1 AND content = ?2
                 AND source = ?3 AND retired_at IS NULL AND tombstone = 0)",
            rusqlite::params![kind, content, source.as_str()],
            |r| r.get(0),
        )
        .map_err(map_db)
    }

    async fn write_note_with_source(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
        source: NoteSource,
        supersedes: Option<&str>,
    ) -> Result<String> {
        self.write_memory_note(
            kind,
            content,
            symbols,
            files,
            session_id,
            scope,
            feature_id,
            related_entity,
            source,
            supersedes,
            None,
            false,
        )
        .await
    }

    async fn write_note_with_relation(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
    ) -> Result<String> {
        self.write_memory_note(
            kind,
            content,
            symbols,
            files,
            session_id,
            scope,
            feature_id,
            related_entity,
            NoteSource::Agent,
            None,
            None,
            false,
        )
        .await
    }

    async fn update_note_payload(&self, id: &str, payload_json: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE memory_notes SET payload_json = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![payload_json, now(), id],
            )
            .map_err(map_db)?;
        Ok(n > 0)
    }

    async fn log_tool_call(&self, session_id: &str, tool_name: &str, outcome: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO tool_call_log (id, session_id, tool_name, outcome, called_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                session_id,
                tool_name,
                outcome,
                now()
            ],
        )
        .map_err(map_db)?;
        conn.execute(
            "DELETE FROM tool_call_log WHERE id IN (
                 SELECT id FROM tool_call_log ORDER BY called_at DESC, rowid DESC
                 LIMIT -1 OFFSET ?1)",
            rusqlite::params![CALL_LOG_KEEP],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn tool_call_log_rows(&self, since: i64, limit: usize) -> Result<Vec<ToolCallLogRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, tool_name, outcome, called_at FROM tool_call_log
                 WHERE called_at >= ?1 ORDER BY called_at DESC, rowid DESC LIMIT ?2",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![since, limit as i64], |r| {
                Ok(ToolCallLogRow {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    tool_name: r.get(2)?,
                    outcome: r.get(3)?,
                    called_at: r.get(4)?,
                })
            })
            .map_err(map_db)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_db)?;
        Ok(rows)
    }
}
