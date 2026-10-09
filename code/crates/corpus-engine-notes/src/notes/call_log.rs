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
