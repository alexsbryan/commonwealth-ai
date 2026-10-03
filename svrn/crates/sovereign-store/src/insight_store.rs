// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::Connection;
use tokio::sync::Mutex;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::InsightStore;
use sovereign_core::types::*;

fn map_db(e: rusqlite::Error) -> Error {
    Error::Storage(e.to_string())
}

fn map_json(e: serde_json::Error) -> Error {
    Error::Storage(format!("JSON error: {e}"))
}

/// Cosine similarity between two vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

/// Encode a Vec<f32> as raw little-endian bytes for SQLite BLOB storage.
fn encode_embedding(embedding: &[f32]) -> Vec<u8> {
    embedding.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Decode raw little-endian bytes back to Vec<f32>.
fn decode_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

pub struct SqliteInsightStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteInsightStore {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    fn row_to_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<InsightNode> {
        let id_str: String = row.get(0)?;
        let clipped_text: String = row.get(1)?;
        let message_id_str: String = row.get(2)?;
        let paragraph_index: usize = row.get(3)?;
        let source_json: String = row.get(4)?;
        let position_json: Option<String> = row.get(5)?;
        let adjacent_json: String = row.get(6)?;
        let embedding_bytes: Option<Vec<u8>> = row.get(7)?;
        let created_at_ts: i64 = row.get(8)?;
        let sink_state_json: String = row.get(9)?;

        let id = uuid::Uuid::parse_str(&id_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?;
        let message_id = uuid::Uuid::parse_str(&message_id_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?;
        let source: InsightSource = serde_json::from_str(&source_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?;
        let position: Option<InsightPosition> = position_json
            .map(|j| serde_json::from_str(&j))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
        let adjacent: Vec<String> = serde_json::from_str(&adjacent_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e))
        })?;
        let embedding = embedding_bytes.map(|b| decode_embedding(&b));
        let created_at = chrono::DateTime::from_timestamp(created_at_ts, 0).unwrap_or_default();
        let sink_state: InsightSinkState = serde_json::from_str(&sink_state_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(e))
        })?;

        Ok(InsightNode {
            id,
            clipped_text,
            message_id,
            paragraph_index,
            source,
            position,
            adjacent,
            embedding,
            created_at,
            sink_state,
        })
    }
}

#[async_trait]
impl InsightStore for SqliteInsightStore {
    async fn save(&self, node: &InsightNode) -> Result<()> {
        let conn = self.conn.lock().await;
        let source_json = serde_json::to_string(&node.source).map_err(map_json)?;
        let position_json = node
            .position
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(map_json)?;
        let adjacent_json = serde_json::to_string(&node.adjacent).map_err(map_json)?;
        let embedding_blob = node.embedding.as_ref().map(|e| encode_embedding(e));
        let created_at = node.created_at.timestamp();
        let sink_state_json = serde_json::to_string(&node.sink_state).map_err(map_json)?;

        conn.execute(
            "INSERT INTO insight_nodes (id, clipped_text, message_id, paragraph_index, \
             source_json, position_json, adjacent_json, embedding, created_at, sink_state_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                node.id.to_string(),
                node.clipped_text,
                node.message_id.to_string(),
                node.paragraph_index,
                source_json,
                position_json,
                adjacent_json,
                embedding_blob,
                created_at,
                sink_state_json,
            ],
        )
        .map_err(map_db)?;

        Ok(())
    }

    async fn get(&self, id: uuid::Uuid) -> Result<InsightNode> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, clipped_text, message_id, paragraph_index, source_json, \
             position_json, adjacent_json, embedding, created_at, sink_state_json \
             FROM insight_nodes WHERE id = ?1 AND deleted_at IS NULL",
            [id.to_string()],
            Self::row_to_node,
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("Insight {id}")),
            other => map_db(other),
        })
    }

    async fn list(&self, limit: usize) -> Result<Vec<InsightNode>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, clipped_text, message_id, paragraph_index, source_json, \
                 position_json, adjacent_json, embedding, created_at, sink_state_json \
                 FROM insight_nodes WHERE deleted_at IS NULL \
                 ORDER BY created_at DESC LIMIT ?1",
            )
            .map_err(map_db)?;

        let rows = stmt.query_map([limit], Self::row_to_node).map_err(map_db)?;

        let mut nodes = Vec::new();
        for row in rows {
            nodes.push(row.map_err(map_db)?);
        }
        Ok(nodes)
    }

    async fn list_by_ids(&self, ids: &[uuid::Uuid]) -> Result<Vec<InsightNode>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let conn = self.conn.lock().await;
        let placeholders: Vec<String> = ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect();
        let sql = format!(
            "SELECT id, clipped_text, message_id, paragraph_index, source_json, \
             position_json, adjacent_json, embedding, created_at, sink_state_json \
             FROM insight_nodes WHERE id IN ({}) AND deleted_at IS NULL",
            placeholders.join(", ")
        );
        let mut stmt = conn.prepare(&sql).map_err(map_db)?;
        let params: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();

        let rows = stmt
            .query_map(param_refs.as_slice(), Self::row_to_node)
            .map_err(map_db)?;

        let mut nodes = Vec::new();
        for row in rows {
            nodes.push(row.map_err(map_db)?);
        }
        Ok(nodes)
    }

    async fn search_text(&self, query: &str, limit: usize) -> Result<Vec<InsightNode>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT n.id, n.clipped_text, n.message_id, n.paragraph_index, n.source_json, \
                 n.position_json, n.adjacent_json, n.embedding, n.created_at, n.sink_state_json \
                 FROM insight_nodes_fts fts \
                 JOIN insight_nodes n ON fts.id = n.id \
                 WHERE insight_nodes_fts MATCH ?1 AND n.deleted_at IS NULL \
                 LIMIT ?2",
            )
            .map_err(map_db)?;

        let rows = stmt
            .query_map(rusqlite::params![query, limit], Self::row_to_node)
            .map_err(map_db)?;

        let mut nodes = Vec::new();
        for row in rows {
            nodes.push(row.map_err(map_db)?);
        }
        Ok(nodes)
    }

    async fn adjacent_by_embedding(
        &self,
        embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<InsightNode>> {
        // Load all nodes with embeddings and compute cosine similarity in Rust.
        // For collections up to ~10K nodes this is fast enough.
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, clipped_text, message_id, paragraph_index, source_json, \
                 position_json, adjacent_json, embedding, created_at, sink_state_json \
                 FROM insight_nodes WHERE deleted_at IS NULL AND embedding IS NOT NULL",
            )
            .map_err(map_db)?;

        let rows = stmt.query_map([], Self::row_to_node).map_err(map_db)?;

        let mut scored: Vec<(f32, InsightNode)> = Vec::new();
        for row in rows {
            let node = row.map_err(map_db)?;
            if let Some(ref emb) = node.embedding {
                let score = cosine_similarity(embedding, emb);
                scored.push((score, node));
            }
        }

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().take(limit).map(|(_, n)| n).collect())
    }

    async fn update_sink_state(
        &self,
        node_id: uuid::Uuid,
        sink_state: InsightSinkState,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let sink_state_json = serde_json::to_string(&sink_state).map_err(map_json)?;
        conn.execute(
            "UPDATE insight_nodes SET sink_state_json = ?1 WHERE id = ?2",
            rusqlite::params![sink_state_json, node_id.to_string()],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn delete(&self, node_id: uuid::Uuid) -> Result<()> {
        let conn = self.conn.lock().await;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE insight_nodes SET deleted_at = ?1 WHERE id = ?2",
            rusqlite::params![now, node_id.to_string()],
        )
        .map_err(map_db)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::SqliteStateStore;

    fn make_test_node() -> InsightNode {
        InsightNode {
            id: uuid::Uuid::new_v4(),
            clipped_text: "Frankfurt cases show that moral responsibility doesn't require alternative possibilities.".to_string(),
            message_id: uuid::Uuid::new_v4(),
            paragraph_index: 0,
            source: InsightSource {
                corpus_id: Some("sep".to_string()),
                article_title: Some("Free Will".to_string()),
                conversation_id: uuid::Uuid::new_v4(),
            },
            position: Some(InsightPosition {
                name: "Compatibilism".to_string(),
                style: PositionStyle::Compatibilism,
            }),
            adjacent: vec![],
            embedding: Some(vec![0.1, 0.2, 0.3, 0.4, 0.5]),
            created_at: chrono::Utc::now(),
            sink_state: InsightSinkState::Local,
        }
    }

    #[tokio::test]
    async fn test_save_and_get() {
        let state_store = SqliteStateStore::open_in_memory().unwrap();
        let store = SqliteInsightStore::new(state_store.connection());

        let node = make_test_node();
        store.save(&node).await.unwrap();

        let retrieved = store.get(node.id).await.unwrap();
        assert_eq!(retrieved.id, node.id);
        assert_eq!(retrieved.clipped_text, node.clipped_text);
        assert_eq!(retrieved.paragraph_index, 0);
        assert_eq!(retrieved.source.corpus_id, Some("sep".to_string()));
        assert!(retrieved.position.is_some());
        assert!(retrieved.embedding.is_some());
        assert_eq!(retrieved.sink_state, InsightSinkState::Local);
    }

    #[tokio::test]
    async fn test_list() {
        let state_store = SqliteStateStore::open_in_memory().unwrap();
        let store = SqliteInsightStore::new(state_store.connection());

        let n1 = make_test_node();
        let n2 = make_test_node();
        store.save(&n1).await.unwrap();
        store.save(&n2).await.unwrap();

        let all = store.list(50).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn test_delete() {
        let state_store = SqliteStateStore::open_in_memory().unwrap();
        let store = SqliteInsightStore::new(state_store.connection());

        let node = make_test_node();
        store.save(&node).await.unwrap();
        store.delete(node.id).await.unwrap();

        let all = store.list(50).await.unwrap();
        assert_eq!(all.len(), 0);
    }

    #[tokio::test]
    async fn test_search_text() {
        let state_store = SqliteStateStore::open_in_memory().unwrap();
        let store = SqliteInsightStore::new(state_store.connection());

        let node = make_test_node();
        store.save(&node).await.unwrap();

        let results = store.search_text("Frankfurt", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, node.id);

        let empty = store.search_text("quantum", 10).await.unwrap();
        assert_eq!(empty.len(), 0);
    }

    #[tokio::test]
    async fn test_adjacent_by_embedding() {
        let state_store = SqliteStateStore::open_in_memory().unwrap();
        let store = SqliteInsightStore::new(state_store.connection());

        let mut n1 = make_test_node();
        n1.embedding = Some(vec![1.0, 0.0, 0.0, 0.0, 0.0]);
        n1.source.article_title = Some("Frankfurt Cases".to_string());
        store.save(&n1).await.unwrap();

        let mut n2 = make_test_node();
        n2.embedding = Some(vec![0.9, 0.1, 0.0, 0.0, 0.0]);
        n2.source.article_title = Some("Moral Responsibility".to_string());
        store.save(&n2).await.unwrap();

        let mut n3 = make_test_node();
        n3.embedding = Some(vec![0.0, 0.0, 0.0, 0.0, 1.0]);
        n3.source.article_title = Some("Quantum Mechanics".to_string());
        store.save(&n3).await.unwrap();

        // Query with vector close to n1/n2
        let query = vec![0.95, 0.05, 0.0, 0.0, 0.0];
        let adjacent = store.adjacent_by_embedding(&query, 2).await.unwrap();
        assert_eq!(adjacent.len(), 2);
        // n1 should be most similar, then n2
        assert_eq!(
            adjacent[0].source.article_title,
            Some("Frankfurt Cases".to_string())
        );
        assert_eq!(
            adjacent[1].source.article_title,
            Some("Moral Responsibility".to_string())
        );
    }
}
