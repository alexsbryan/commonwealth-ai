// SPDX-License-Identifier: AGPL-3.0-or-later
//! `CorpusStateStore` impl — corpus rows, visibility, readiness.

use super::*;

/// Decode the JSON `corpus_state.visibility` column. `NULL` or a parse
/// failure (a pre-migration row) maps to the default `Org` (shared).
fn decode_visibility(raw: Option<String>) -> CorpusVisibility {
    raw.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[async_trait]
impl CorpusStateStore for SqliteStateStore {
    async fn save_corpus_state(&self, state: &CorpusState) -> Result<()> {
        let conn = self.conn.lock().await;
        let visibility_json = serde_json::to_string(&state.visibility).ok();
        conn.execute(
            "INSERT OR REPLACE INTO corpus_state (corpus_id, installed_at, source_date, chunks_count, index_size_mb, last_updated, vector_index_ready, visibility)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                state.corpus_id,
                state.installed_at,
                state.source_date,
                state.chunks_count,
                state.index_size_mb,
                state.last_updated,
                state.vector_index_ready as i64,
                visibility_json,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn get_corpus_state(&self, corpus_id: &str) -> Result<CorpusState> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT corpus_id, installed_at, source_date, chunks_count, index_size_mb, last_updated, COALESCE(vector_index_ready, 0), visibility
             FROM corpus_state WHERE corpus_id = ?1 AND deleted_at IS NULL",
            rusqlite::params![corpus_id],
            |row| {
                Ok(CorpusState {
                    corpus_id: row.get(0)?,
                    installed_at: row.get(1)?,
                    source_date: row.get(2)?,
                    chunks_count: row.get(3)?,
                    index_size_mb: row.get(4)?,
                    last_updated: row.get(5)?,
                    version: 0,
                    deleted_at: None,
                    vector_index_ready: row.get::<_, i64>(6)? != 0,
                    visibility: decode_visibility(row.get::<_, Option<String>>(7)?),
                })
            },
        );

        match result {
            Ok(state) => Ok(state),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                Err(Error::NotFound(format!("Corpus {corpus_id}")))
            }
            Err(e) => Err(map_db(e)),
        }
    }

    async fn list_corpus_states(&self) -> Result<Vec<CorpusState>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT corpus_id, installed_at, source_date, chunks_count, index_size_mb, last_updated, COALESCE(vector_index_ready, 0), visibility
                 FROM corpus_state WHERE deleted_at IS NULL ORDER BY installed_at DESC",
            )
            .map_err(map_db)?;

        let states: Vec<CorpusState> = stmt
            .query_map([], |row| {
                Ok(CorpusState {
                    corpus_id: row.get(0)?,
                    installed_at: row.get(1)?,
                    source_date: row.get(2)?,
                    chunks_count: row.get(3)?,
                    index_size_mb: row.get(4)?,
                    last_updated: row.get(5)?,
                    version: 0,
                    deleted_at: None,
                    vector_index_ready: row.get::<_, i64>(6)? != 0,
                    visibility: decode_visibility(row.get::<_, Option<String>>(7)?),
                })
            })
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(states)
    }

    async fn set_vector_index_ready(&self, corpus_id: &str, ready: bool) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE corpus_state SET vector_index_ready = ?1 WHERE corpus_id = ?2",
            rusqlite::params![ready as i64, corpus_id],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn get_vector_index_ready(&self, corpus_id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let v: i64 = conn
            .query_row(
                "SELECT COALESCE(vector_index_ready, 0) FROM corpus_state WHERE corpus_id = ?1",
                rusqlite::params![corpus_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(v != 0)
    }

    async fn delete_corpus_state(&self, corpus_id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        let ts = now();
        conn.execute(
            "UPDATE corpus_state SET deleted_at = ?2, version = ?2 WHERE corpus_id = ?1 AND deleted_at IS NULL",
            rusqlite::params![corpus_id, ts],
        )
        .map_err(map_db)?;
        Ok(())
    }
}
