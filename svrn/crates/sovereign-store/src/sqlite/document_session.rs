// SPDX-License-Identifier: AGPL-3.0-or-later
//! `DocumentSessionStore` impl — per-conversation document sessions.

use super::*;

// ─── DocumentSessionStore ────────────────────────────────────

#[async_trait]
impl DocumentSessionStore for SqliteStateStore {
    async fn create_document_session(&self, session: &DocumentSession) -> Result<()> {
        let conn = self.conn.lock().await;
        let history_json =
            serde_json::to_string(&session.history).unwrap_or_else(|_| "[]".to_string());
        conn.execute(
            "INSERT INTO document_sessions
             (id, conversation_id, filename, source, word_count, chunk_count,
              created_at, operation, map_prompt, reduce_prompt, last_output, history)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                session.id,
                session.conversation_id,
                session.filename,
                session.source,
                session.word_count as i64,
                session.chunk_count as i64,
                session.created_at,
                session.operation,
                session.map_prompt,
                session.reduce_prompt,
                session.last_output,
                history_json,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn get_document_session(&self, session_id: &str) -> Result<Option<DocumentSession>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, conversation_id, filename, source, word_count, chunk_count,
                        created_at, operation, map_prompt, reduce_prompt, last_output, history
                 FROM document_sessions WHERE id = ?",
            )
            .map_err(map_db)?;

        let result = stmt
            .query_row(rusqlite::params![session_id], |row| {
                Ok(row_to_document_session(row))
            })
            .optional()
            .map_err(map_db)?;

        Ok(result)
    }

    async fn get_document_session_by_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<DocumentSession>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, conversation_id, filename, source, word_count, chunk_count,
                        created_at, operation, map_prompt, reduce_prompt, last_output, history
                 FROM document_sessions
                 WHERE conversation_id = ?
                 ORDER BY created_at DESC
                 LIMIT 1",
            )
            .map_err(map_db)?;

        let result = stmt
            .query_row(rusqlite::params![conversation_id], |row| {
                Ok(row_to_document_session(row))
            })
            .optional()
            .map_err(map_db)?;

        Ok(result)
    }

    async fn update_document_session(&self, session: &DocumentSession) -> Result<()> {
        let conn = self.conn.lock().await;
        let history_json =
            serde_json::to_string(&session.history).unwrap_or_else(|_| "[]".to_string());
        conn.execute(
            "UPDATE document_sessions SET
                operation = ?, map_prompt = ?, reduce_prompt = ?,
                last_output = ?, history = ?
             WHERE id = ?",
            rusqlite::params![
                session.operation,
                session.map_prompt,
                session.reduce_prompt,
                session.last_output,
                history_json,
                session.id,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }
}

fn row_to_document_session(row: &rusqlite::Row) -> DocumentSession {
    let history_json: String = row.get(11).unwrap_or_else(|_| "[]".to_string());
    let history: Vec<DocumentOperation> = serde_json::from_str(&history_json).unwrap_or_default();
    DocumentSession {
        id: row.get(0).unwrap_or_default(),
        conversation_id: row.get(1).unwrap_or_default(),
        filename: row.get(2).unwrap_or_default(),
        source: row.get(3).unwrap_or_default(),
        word_count: row.get::<_, i64>(4).unwrap_or(0) as usize,
        chunk_count: row.get::<_, i64>(5).unwrap_or(0) as usize,
        created_at: row.get(6).unwrap_or(0),
        operation: row.get(7).unwrap_or_default(),
        map_prompt: row.get(8).unwrap_or_default(),
        reduce_prompt: row.get(9).unwrap_or_default(),
        last_output: row.get(10).ok(),
        history,
    }
}
