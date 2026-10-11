// SPDX-License-Identifier: AGPL-3.0-or-later
//! Code's tool-call ring buffer: the one writer and reader of
//! `notes.db`'s `tool_call_log`, moved out of `notes.rs` whole.

use rusqlite::params;

use corpus_engine_yield::time::unix_now;

use super::{sqlite_err, NoteStore};
use crate::error::Result;

/// A single row from the tool call ring buffer.
#[derive(Debug, Clone)]
pub struct ToolCallLogRow {
    pub id: String,
    pub session_id: String,
    pub tool_name: String,
    /// `"success"` | `"error"` | `"empty_result"`
    pub outcome: String,
    pub called_at: i64,
    /// Who called: the principal label the host's auth layer resolved
    /// (`asserted:claude-code`). `None` when none was resolved.
    pub caller: Option<String>,
}

impl NoteStore {
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
        self.log_tool_call_by(session_id, tool_name, outcome, None)
            .await
    }

    /// [`Self::log_tool_call`] naming `caller`, the principal label of who
    /// made the call. THE one writer of code's call log, the twin of svrn's
    /// `SqliteStateStore::log_tool_call_by`.
    pub async fn log_tool_call_by(
        &self,
        session_id: &str,
        tool_name: &str,
        outcome: &str,
        caller: Option<&str>,
    ) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = unix_now();
        let conn = self.conn.lock().await;

        conn.execute(
            "INSERT INTO tool_call_log (id, session_id, tool_name, outcome, called_at, caller)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, session_id, tool_name, outcome, now, caller],
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
                "SELECT id, session_id, tool_name, outcome, called_at, caller
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
                    caller: r.get(5)?,
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

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::NoteStore;
    use crate::notes_schema::*;

    /// A store at v12 opens to v13 with a `caller` column; its old rows'
    /// caller is NULL, never filled in.
    #[tokio::test]
    async fn migration_v12_to_v13_adds_the_caller_null_for_old_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("notes.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA_NEW).unwrap();
            for m in [
                MIGRATION_V2,
                MIGRATION_V3,
                MIGRATION_V4,
                MIGRATION_V5,
                MIGRATION_V6,
                MIGRATION_V7,
                MIGRATION_V8,
                MIGRATION_V9,
                MIGRATION_V10,
                MIGRATION_V11,
                MIGRATION_V12,
            ] {
                conn.execute_batch(m).unwrap();
            }
            conn.execute_batch(
                "INSERT INTO tool_call_log (id, session_id, tool_name, outcome, called_at)
                 VALUES ('old', 's0', 'blast', 'success', 1000);",
            )
            .unwrap();
        }
        let store = NoteStore::open(&db).unwrap();
        store
            .log_tool_call_by("s1", "build", "success", Some("asserted:opencode"))
            .await
            .unwrap();
        let rows = store.tool_call_log_rows(0, 10).await.unwrap();
        let caller = |id_tool: &str| {
            rows.iter()
                .find(|r| r.tool_name == id_tool)
                .unwrap()
                .caller
                .clone()
        };
        assert_eq!(caller("blast"), None);
        assert_eq!(caller("build").as_deref(), Some("asserted:opencode"));
        drop(store);
        let v: i64 = Connection::open(&db)
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 13);
    }
}
