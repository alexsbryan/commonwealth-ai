use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rusqlite::Connection;
use tokio::sync::Mutex;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{
    BudgetStore, ConversationStore, CorpusStateStore, DocumentStore, HealthStore,
    MemoryStore, PermissionStore, RoutingStore, StateStore, TaskStore,
};
use sovereign_core::types::*;

use crate::migrations;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub struct SqliteStateStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteStateStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Storage(format!("Failed to create data dir: {e}")))?;
        }

        let conn = Connection::open(path)
            .map_err(|e| Error::Storage(format!("Failed to open database: {e}")))?;

        migrations::run_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Migration failed: {e}")))?;
        migrations::run_column_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Column migration failed: {e}")))?;
        migrations::run_sync_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Sync migration failed: {e}")))?;
        migrations::run_metacognition_log_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Metacognition log migration failed: {e}")))?;
        migrations::run_index_readiness_migration(&conn)
            .map_err(|e| Error::Storage(format!("Index readiness migration failed: {e}")))?;
        migrations::run_insight_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Insight migration failed: {e}")))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Return a shared handle to the underlying connection.
    /// Used by `SqliteInsightStore` to share the same database connection.
    pub fn connection(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }

    /// Run `PRAGMA integrity_check` and return the first result row.
    /// Returns `"ok"` when the database is clean.
    pub async fn integrity_check(&self) -> Result<String> {
        let conn = self.conn.lock().await;
        let result: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(map_db)?;
        Ok(result)
    }

    /// Run `PRAGMA wal_checkpoint(TRUNCATE)` to shrink the WAL file.
    pub async fn wal_checkpoint(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").map_err(map_db)?;
        Ok(())
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Storage(format!("Failed to open in-memory db: {e}")))?;

        migrations::run_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Migration failed: {e}")))?;
        migrations::run_column_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Column migration failed: {e}")))?;
        migrations::run_sync_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Sync migration failed: {e}")))?;
        migrations::run_metacognition_log_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Metacognition log migration failed: {e}")))?;
        migrations::run_index_readiness_migration(&conn)
            .map_err(|e| Error::Storage(format!("Index readiness migration failed: {e}")))?;
        migrations::run_insight_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Insight migration failed: {e}")))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
}

fn map_db(e: rusqlite::Error) -> Error {
    Error::Storage(e.to_string())
}

fn map_json(e: serde_json::Error) -> Error {
    Error::Storage(format!("JSON error: {e}"))
}

#[async_trait]
impl ConversationStore for SqliteStateStore {
    async fn save_message(&self, msg: &Message) -> Result<()> {
        let conn = self.conn.lock().await;

        // Upsert conversation.
        conn.execute(
            "INSERT INTO conversations (id, title, created_at, updated_at)
             VALUES (?1, NULL, ?2, ?2)
             ON CONFLICT(id) DO UPDATE SET updated_at = ?2",
            rusqlite::params![msg.conversation_id, now()],
        )
        .map_err(map_db)?;

        let metadata_json = msg
            .metadata
            .as_ref()
            .map(|m| serde_json::to_string(m).unwrap_or_default());

        let role_str = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        };

        conn.execute(
            "INSERT OR REPLACE INTO messages (id, conversation_id, role, content, created_at, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                msg.id,
                msg.conversation_id,
                role_str,
                msg.content,
                msg.created_at,
                metadata_json,
            ],
        )
        .map_err(map_db)?;

        Ok(())
    }

    async fn get_conversation(&self, id: &str) -> Result<Conversation> {
        let conn = self.conn.lock().await;

        let (title, created_at, updated_at) = conn
            .query_row(
                "SELECT title, created_at, updated_at FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::NotFound(format!("Conversation {id}"))
                }
                other => map_db(other),
            })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, conversation_id, role, content, created_at, metadata
                 FROM messages WHERE conversation_id = ?1 ORDER BY created_at ASC",
            )
            .map_err(map_db)?;

        let messages: Vec<Message> = stmt
            .query_map(rusqlite::params![id], |row| {
                let role_str: String = row.get(2)?;
                let metadata_str: Option<String> = row.get(5)?;

                Ok(Message {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    role: match role_str.as_str() {
                        "assistant" => Role::Assistant,
                        "system" => Role::System,
                        _ => Role::User,
                    },
                    content: row.get(3)?,
                    created_at: row.get(4)?,
                    metadata: metadata_str
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    version: 0,
                })
            })
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(Conversation {
            id: id.to_string(),
            title,
            messages,
            created_at,
            updated_at,
            version: 0,
            deleted_at: None,
        })
    }

    async fn list_conversations(&self, limit: usize, offset: usize) -> Result<Vec<Conversation>> {
        let conn = self.conn.lock().await;

        let mut stmt = conn
            .prepare(
                "SELECT id, title, created_at, updated_at
                 FROM conversations WHERE deleted_at IS NULL ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2",
            )
            .map_err(map_db)?;

        let convos: Vec<Conversation> = stmt
            .query_map(rusqlite::params![limit as i64, offset as i64], |row| {
                Ok(Conversation {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    messages: Vec::new(), // Not loading messages for list view.
                    created_at: row.get(2)?,
                    updated_at: row.get(3)?,
                    version: 0,
                    deleted_at: None,
                })
            })
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(convos)
    }

    async fn search_messages(&self, query: &str) -> Result<Vec<Message>> {
        let conn = self.conn.lock().await;

        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.conversation_id, m.role, m.content, m.created_at, m.metadata
                 FROM messages m
                 JOIN messages_fts fts ON m.rowid = fts.rowid
                 WHERE messages_fts MATCH ?1
                 ORDER BY m.created_at DESC
                 LIMIT 50",
            )
            .map_err(map_db)?;

        let messages: Vec<Message> = stmt
            .query_map(rusqlite::params![query], |row| {
                let role_str: String = row.get(2)?;
                let metadata_str: Option<String> = row.get(5)?;

                Ok(Message {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    role: match role_str.as_str() {
                        "assistant" => Role::Assistant,
                        "system" => Role::System,
                        _ => Role::User,
                    },
                    content: row.get(3)?,
                    created_at: row.get(4)?,
                    metadata: metadata_str
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    version: 0,
                })
            })
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(messages)
    }

    async fn delete_conversation(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        let ts = now();
        conn.execute(
            "UPDATE conversations SET deleted_at = ?2, version = ?2 WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![id, ts],
        )
        .map_err(map_db)?;
        Ok(())
    }
}

#[async_trait]
impl TaskStore for SqliteStateStore {
    async fn save_task(&self, task: &Task) -> Result<()> {
        let conn = self.conn.lock().await;
        let plan_json =
            serde_json::to_string(&task.plan).map_err(|e| Error::Serialization(e.to_string()))?;
        let state_json = serde_json::to_string(&task.completed_steps)
            .map_err(|e| Error::Serialization(e.to_string()))?;
        let status_str = match task.status {
            TaskStatus::Running => "running",
            TaskStatus::Paused => "paused",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
        };

        conn.execute(
            "INSERT OR REPLACE INTO tasks (id, conversation_id, goal, plan, state, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                task.id,
                task.conversation_id,
                task.goal,
                plan_json,
                state_json,
                status_str,
                task.created_at,
                task.updated_at,
            ],
        )
        .map_err(map_db)?;

        Ok(())
    }

    async fn get_task(&self, id: &str) -> Result<Task> {
        let conn = self.conn.lock().await;

        conn.query_row(
            "SELECT id, conversation_id, goal, plan, state, status, created_at, updated_at
             FROM tasks WHERE id = ?1",
            rusqlite::params![id],
            |row| {
                let plan_json: String = row.get(3)?;
                let state_json: String = row.get(4)?;
                let status_str: String = row.get(5)?;

                Ok(Task {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    goal: row.get(2)?,
                    plan: serde_json::from_str(&plan_json).unwrap_or_else(|_| Plan {
                        id: String::new(),
                        goal: String::new(),
                        steps: Vec::new(),
                        edges: Vec::new(),
                    }),
                    completed_steps: serde_json::from_str(&state_json).unwrap_or_default(),
                    status: match status_str.as_str() {
                        "running" => TaskStatus::Running,
                        "paused" => TaskStatus::Paused,
                        "completed" => TaskStatus::Completed,
                        _ => TaskStatus::Failed,
                    },
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                    version: 0,
                })
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("Task {id}")),
            other => map_db(other),
        })
    }
}

#[async_trait]
impl MemoryStore for SqliteStateStore {
    async fn save_memory(&self, memory: &Memory) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO memories (id, content, source, confidence, created_at, last_used)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                memory.id,
                memory.content,
                memory.source,
                memory.confidence,
                memory.created_at,
                memory.last_used,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn get_relevant_memories(&self, context: &str, limit: usize) -> Result<Vec<Memory>> {
        if context.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.conn.lock().await;
        let current_time = now();

        let fts_context = sanitize_fts5_query(context);
        if fts_context.is_empty() {
            return Ok(Vec::new());
        }

        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.content, m.source, m.confidence, m.created_at, m.last_used
                 FROM memories m
                 JOIN memories_fts fts ON m.rowid = fts.rowid
                 WHERE memories_fts MATCH ?1 AND m.deleted_at IS NULL
                 LIMIT ?2",
            )
            .map_err(map_db)?;

        let memories: Vec<Memory> = stmt
            .query_map(rusqlite::params![fts_context, (limit * 3) as i64], |row| {
                Ok(Memory {
                    id: row.get(0)?,
                    content: row.get(1)?,
                    source: row.get(2)?,
                    confidence: row.get(3)?,
                    created_at: row.get(4)?,
                    last_used: row.get(5)?,
                    version: 0,
                    deleted_at: None,
                })
            })
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap_or_default();

        // Apply confidence decay and filter.
        let mut scored: Vec<(f64, Memory)> = memories
            .into_iter()
            .filter_map(|m| {
                let months = (current_time - m.last_used) as f64 / (30.0 * 86400.0);
                let decayed = m.confidence * 0.9_f64.powf(months);
                if decayed >= 0.2 {
                    Some((decayed, m))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        // Touch returned memories.
        for (_, mem) in &scored {
            let _ = conn.execute(
                "UPDATE memories SET last_used = ?2 WHERE id = ?1",
                rusqlite::params![mem.id, current_time],
            );
        }

        Ok(scored.into_iter().map(|(_, m)| m).collect())
    }

    async fn get_all_memories(&self) -> Result<Vec<Memory>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT id, content, source, confidence, created_at, last_used FROM memories WHERE deleted_at IS NULL")
            .map_err(map_db)?;

        let memories: Vec<Memory> = stmt
            .query_map([], |row| {
                Ok(Memory {
                    id: row.get(0)?,
                    content: row.get(1)?,
                    source: row.get(2)?,
                    confidence: row.get(3)?,
                    created_at: row.get(4)?,
                    last_used: row.get(5)?,
                    version: 0,
                    deleted_at: None,
                })
            })
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(memories)
    }

    async fn delete_memory(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        let ts = now();
        conn.execute(
            "UPDATE memories SET deleted_at = ?2, version = ?2 WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![id, ts],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn update_memory_confidence(&self, id: &str, confidence: f64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE memories SET confidence = ?2 WHERE id = ?1",
            rusqlite::params![id, confidence],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn touch_memory(&self, id: &str, timestamp: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE memories SET last_used = ?2 WHERE id = ?1",
            rusqlite::params![id, timestamp],
        )
        .map_err(map_db)?;
        Ok(())
    }
}

#[async_trait]
impl RoutingStore for SqliteStateStore {
    async fn log_routing(
        &self,
        message_hash: &str,
        classified_as: &str,
        latency_ms: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO routing_log (message_hash, classified_as, latency_ms, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![message_hash, classified_as, latency_ms, now()],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn log_routing_meta(
        &self,
        message_hash: &str,
        coarse_intent: &str,
        self_assessment: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE routing_log SET coarse_intent = ?1, self_assessment = ?2
             WHERE id = (
                 SELECT id FROM routing_log WHERE message_hash = ?3
                 ORDER BY created_at DESC LIMIT 1
             )",
            rusqlite::params![coarse_intent, self_assessment, message_hash],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn get_routing_corrections(&self, limit: usize) -> Result<Vec<RoutingCorrection>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT message_hash, classified_as, was_correct, created_at
                 FROM routing_log WHERE was_correct = 0
                 ORDER BY created_at DESC LIMIT ?1",
            )
            .map_err(map_db)?;

        let corrections: Vec<RoutingCorrection> = stmt
            .query_map(rusqlite::params![limit as i64], |row| {
                Ok(RoutingCorrection {
                    message_hash: row.get(0)?,
                    classified_as: row.get(1)?,
                    was_correct: row.get::<_, bool>(2)?,
                    created_at: row.get(3)?,
                })
            })
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(corrections)
    }

    async fn mark_routing_correct(&self, message_hash: &str, was_correct: bool) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE routing_log SET was_correct = ?2 WHERE message_hash = ?1",
            rusqlite::params![message_hash, was_correct],
        )
        .map_err(map_db)?;
        Ok(())
    }
}

#[async_trait]
impl DocumentStore for SqliteStateStore {
    async fn store_chunks(&self, chunks: &[DocumentChunk]) -> Result<()> {
        let conn = self.conn.lock().await;
        for chunk in chunks {
            let embedding_blob = chunk.embedding.as_ref().map(|v| {
                v.iter()
                    .flat_map(|f| f.to_le_bytes())
                    .collect::<Vec<u8>>()
            });
            let (source_type_str, corpus_id) = chunk.source_type.to_db_columns();

            conn.execute(
                "INSERT OR REPLACE INTO documents (id, source, content, chunk_index, embedding, created_at, source_type, corpus_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    chunk.id,
                    chunk.source,
                    chunk.content,
                    chunk.chunk_index as i64,
                    embedding_blob,
                    chunk.created_at,
                    source_type_str,
                    corpus_id,
                ],
            )
            .map_err(map_db)?;
        }
        Ok(())
    }

    async fn search_documents(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<DocumentChunk>> {
        let conn = self.conn.lock().await;

        // Hybrid search: combine FTS5 text search with cosine similarity.
        // Collect results from both, deduplicate by ID, return top N.

        let mut results: std::collections::HashMap<String, (f32, DocumentChunk)> =
            std::collections::HashMap::new();

        // 1. FTS5 text search (always available, no embeddings needed).
        // Sanitize query into FTS5-safe keywords.
        let fts_query = sanitize_fts5_query(query_text);
        if !fts_query.is_empty() {
            let mut fts_stmt = conn
                .prepare(
                    "SELECT d.id, d.source, d.content, d.chunk_index, d.embedding, d.created_at, d.source_type, d.corpus_id
                     FROM documents d
                     JOIN documents_fts fts ON d.rowid = fts.rowid
                     WHERE documents_fts MATCH ?1 AND d.deleted_at IS NULL
                     LIMIT ?2",
                )
                .map_err(map_db)?;

            let fts_results: Vec<DocumentChunk> = fts_stmt
                .query_map(rusqlite::params![fts_query, (limit * 2) as i64], |row| {
                    let embedding_blob: Option<Vec<u8>> = row.get(4)?;
                    let embedding = embedding_blob.map(|blob| {
                        blob.chunks(4)
                            .map(|c| {
                                let mut bytes = [0u8; 4];
                                bytes.copy_from_slice(c);
                                f32::from_le_bytes(bytes)
                            })
                            .collect::<Vec<f32>>()
                    });
                    let st: String = row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "user".to_string());
                    let cid: Option<String> = row.get(7)?;
                    Ok(DocumentChunk {
                        id: row.get(0)?,
                        source: row.get(1)?,
                        content: row.get(2)?,
                        chunk_index: row.get::<_, i64>(3)? as usize,
                        embedding,
                        created_at: row.get(5)?,
                        source_type: SourceType::from_db_columns(&st, cid.as_deref()),
                        version: 0,
                        deleted_at: None,
                    })
                })
                .map_err(map_db)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap_or_default();

            for (i, chunk) in fts_results.into_iter().enumerate() {
                // FTS5 results get a score based on rank position (1.0 → 0.5).
                let score = 1.0 - (i as f32 * 0.05).min(0.5);
                results.insert(chunk.id.clone(), (score, chunk));
            }
        }

        // 2. Vector similarity search (if embeddings are available).
        if !query_embedding.is_empty() {
            let mut vec_stmt = conn
                .prepare(
                    "SELECT id, source, content, chunk_index, embedding, created_at, source_type, corpus_id
                     FROM documents WHERE embedding IS NOT NULL AND deleted_at IS NULL",
                )
                .map_err(map_db)?;

            let vector_results: Vec<(String, f32, DocumentChunk)> = vec_stmt
                .query_map([], |row| {
                    let embedding_blob: Option<Vec<u8>> = row.get(4)?;
                    let embedding = embedding_blob.map(|blob| {
                        blob.chunks(4)
                            .map(|c| {
                                let mut bytes = [0u8; 4];
                                bytes.copy_from_slice(c);
                                f32::from_le_bytes(bytes)
                            })
                            .collect::<Vec<f32>>()
                    });
                    let id: String = row.get(0)?;
                    let st: String = row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "user".to_string());
                    let cid: Option<String> = row.get(7)?;
                    Ok((
                        id.clone(),
                        embedding.clone(),
                        DocumentChunk {
                            id,
                            source: row.get(1)?,
                            content: row.get(2)?,
                            chunk_index: row.get::<_, i64>(3)? as usize,
                            embedding,
                            created_at: row.get(5)?,
                            source_type: SourceType::from_db_columns(&st, cid.as_deref()),
                            version: 0,
                            deleted_at: None,
                        },
                    ))
                })
                .map_err(map_db)?
                .filter_map(|r| r.ok())
                .filter_map(|(id, emb, chunk)| {
                    emb.map(|e| {
                        let sim = cosine_similarity(query_embedding, &e);
                        (id, sim, chunk)
                    })
                })
                .collect();

            for (id, sim, chunk) in vector_results {
                results
                    .entry(id)
                    .and_modify(|(score, _)| {
                        // Boost score if found by both methods.
                        *score = (*score + sim) / 2.0 + 0.1;
                    })
                    .or_insert((sim, chunk));
            }
        }

        // Sort by score descending, return top N.
        let mut sorted: Vec<(f32, DocumentChunk)> = results.into_values().collect();
        sorted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        Ok(sorted.into_iter().take(limit).map(|(_, c)| c).collect())
    }

    async fn search_documents_scored(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<ScoredChunk>> {
        // Reuse the same hybrid search logic but preserve scores.
        let conn = self.conn.lock().await;
        let mut results: std::collections::HashMap<String, (f32, DocumentChunk)> =
            std::collections::HashMap::new();

        let fts_query_scored = sanitize_fts5_query(query_text);
        if !fts_query_scored.is_empty() {
            let mut fts_stmt = conn
                .prepare(
                    "SELECT d.id, d.source, d.content, d.chunk_index, d.embedding, d.created_at, d.source_type, d.corpus_id
                     FROM documents d
                     JOIN documents_fts fts ON d.rowid = fts.rowid
                     WHERE documents_fts MATCH ?1 AND d.deleted_at IS NULL
                     LIMIT ?2",
                )
                .map_err(map_db)?;

            let fts_results: Vec<DocumentChunk> = fts_stmt
                .query_map(rusqlite::params![fts_query_scored, (limit * 2) as i64], |row| {
                    let embedding_blob: Option<Vec<u8>> = row.get(4)?;
                    let embedding = embedding_blob.map(|blob| {
                        blob.chunks(4)
                            .map(|c| {
                                let mut bytes = [0u8; 4];
                                bytes.copy_from_slice(c);
                                f32::from_le_bytes(bytes)
                            })
                            .collect::<Vec<f32>>()
                    });
                    let st: String = row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "user".to_string());
                    let cid: Option<String> = row.get(7)?;
                    Ok(DocumentChunk {
                        id: row.get(0)?,
                        source: row.get(1)?,
                        content: row.get(2)?,
                        chunk_index: row.get::<_, i64>(3)? as usize,
                        embedding,
                        created_at: row.get(5)?,
                        source_type: SourceType::from_db_columns(&st, cid.as_deref()),
                        version: 0,
                        deleted_at: None,
                    })
                })
                .map_err(map_db)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap_or_default();

            for (i, chunk) in fts_results.into_iter().enumerate() {
                let score = 1.0 - (i as f32 * 0.05).min(0.5);
                results.insert(chunk.id.clone(), (score, chunk));
            }
        }

        if !query_embedding.is_empty() {
            let mut vec_stmt = conn
                .prepare(
                    "SELECT id, source, content, chunk_index, embedding, created_at, source_type, corpus_id
                     FROM documents WHERE embedding IS NOT NULL AND deleted_at IS NULL",
                )
                .map_err(map_db)?;

            let vector_results: Vec<(String, f32, DocumentChunk)> = vec_stmt
                .query_map([], |row| {
                    let embedding_blob: Option<Vec<u8>> = row.get(4)?;
                    let embedding = embedding_blob.map(|blob| {
                        blob.chunks(4)
                            .map(|c| {
                                let mut bytes = [0u8; 4];
                                bytes.copy_from_slice(c);
                                f32::from_le_bytes(bytes)
                            })
                            .collect::<Vec<f32>>()
                    });
                    let id: String = row.get(0)?;
                    let st: String = row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "user".to_string());
                    let cid: Option<String> = row.get(7)?;
                    Ok((
                        id.clone(),
                        embedding.clone(),
                        DocumentChunk {
                            id,
                            source: row.get(1)?,
                            content: row.get(2)?,
                            chunk_index: row.get::<_, i64>(3)? as usize,
                            embedding,
                            created_at: row.get(5)?,
                            source_type: SourceType::from_db_columns(&st, cid.as_deref()),
                            version: 0,
                            deleted_at: None,
                        },
                    ))
                })
                .map_err(map_db)?
                .filter_map(|r| r.ok())
                .filter_map(|(id, emb, chunk)| {
                    emb.map(|e| {
                        let sim = cosine_similarity(query_embedding, &e);
                        (id, sim, chunk)
                    })
                })
                .collect();

            for (id, sim, chunk) in vector_results {
                results
                    .entry(id)
                    .and_modify(|(score, _)| {
                        *score = (*score + sim) / 2.0 + 0.1;
                    })
                    .or_insert((sim, chunk));
            }
        }

        let mut sorted: Vec<(f32, DocumentChunk)> = results.into_values().collect();
        sorted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        Ok(sorted
            .into_iter()
            .take(limit)
            .map(|(score, chunk)| ScoredChunk { chunk, score })
            .collect())
    }

    async fn get_chunks_by_source(&self, source: &str) -> Result<Vec<DocumentChunk>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, source, content, chunk_index, embedding, created_at, source_type, corpus_id
                 FROM documents WHERE source = ?1 AND deleted_at IS NULL ORDER BY chunk_index ASC",
            )
            .map_err(map_db)?;

        let chunks: Vec<DocumentChunk> = stmt
            .query_map(rusqlite::params![source], |row| {
                let embedding_blob: Option<Vec<u8>> = row.get(4)?;
                let embedding = embedding_blob.map(|blob| {
                    blob.chunks(4)
                        .map(|c| {
                            let mut bytes = [0u8; 4];
                            bytes.copy_from_slice(c);
                            f32::from_le_bytes(bytes)
                        })
                        .collect::<Vec<f32>>()
                });
                let st: String = row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "user".to_string());
                let cid: Option<String> = row.get(7)?;
                Ok(DocumentChunk {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    content: row.get(2)?,
                    chunk_index: row.get::<_, i64>(3)? as usize,
                    embedding,
                    created_at: row.get(5)?,
                    source_type: SourceType::from_db_columns(&st, cid.as_deref()),
                    version: 0,
                    deleted_at: None,
                })
            })
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(chunks)
    }

    async fn delete_chunks_by_corpus(&self, corpus_id: &str) -> Result<u64> {
        let conn = self.conn.lock().await;
        let ts = now();
        let count = conn
            .execute(
                "UPDATE documents SET deleted_at = ?2, version = ?2 WHERE corpus_id = ?1 AND deleted_at IS NULL",
                rusqlite::params![corpus_id, ts],
            )
            .map_err(map_db)?;
        Ok(count as u64)
    }

    async fn list_sources(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT DISTINCT source FROM documents WHERE deleted_at IS NULL ORDER BY source")
            .map_err(map_db)?;

        let sources: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(sources)
    }
}

#[async_trait]
impl CorpusStateStore for SqliteStateStore {
    async fn save_corpus_state(&self, state: &CorpusState) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO corpus_state (corpus_id, installed_at, source_date, chunks_count, index_size_mb, last_updated, vector_index_ready)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                state.corpus_id,
                state.installed_at,
                state.source_date,
                state.chunks_count,
                state.index_size_mb,
                state.last_updated,
                state.vector_index_ready as i64,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn get_corpus_state(&self, corpus_id: &str) -> Result<CorpusState> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT corpus_id, installed_at, source_date, chunks_count, index_size_mb, last_updated, COALESCE(vector_index_ready, 0)
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
                "SELECT corpus_id, installed_at, source_date, chunks_count, index_size_mb, last_updated, COALESCE(vector_index_ready, 0)
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

#[async_trait]
impl BudgetStore for SqliteStateStore {
    async fn get_search_budget(&self, backend: &str) -> Result<Option<SearchBudget>> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT backend, monthly_limit, used_this_month, reset_date
             FROM search_budget WHERE backend = ?1",
            rusqlite::params![backend],
            |row| {
                Ok(SearchBudget {
                    backend: row.get(0)?,
                    monthly_limit: row.get(1)?,
                    used_this_month: row.get(2)?,
                    reset_date: row.get(3)?,
                    version: 0,
                })
            },
        );

        match result {
            Ok(budget) => Ok(Some(budget)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(map_db(e)),
        }
    }

    async fn update_search_budget(&self, budget: &SearchBudget) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO search_budget (backend, monthly_limit, used_this_month, reset_date)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                budget.backend,
                budget.monthly_limit,
                budget.used_this_month,
                budget.reset_date,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }
}

#[async_trait]
impl PermissionStore for SqliteStateStore {
    async fn get_permission(&self, tool_id: &str, scope: &str) -> Result<Option<bool>> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT granted FROM permissions WHERE tool_id = ?1 AND scope = ?2",
            rusqlite::params![tool_id, scope],
            |row| row.get::<_, bool>(0),
        );

        match result {
            Ok(granted) => Ok(Some(granted)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(map_db(e)),
        }
    }

    async fn set_permission(&self, tool_id: &str, scope: &str, granted: bool) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO permissions (tool_id, scope, granted, granted_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![tool_id, scope, granted, now()],
        )
        .map_err(map_db)?;
        Ok(())
    }
}

#[async_trait]
impl HealthStore for SqliteStateStore {
    async fn save_health_report(
        &self,
        report: &sovereign_core::health::HealthReport,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let component = serde_json::to_string(&report.component).map_err(map_json)?;
        let status = serde_json::to_string(&report.status).map_err(map_json)?;
        let issues_json = serde_json::to_string(&report.issues).map_err(map_json)?;
        conn.execute(
            "INSERT INTO health_reports (component, status, issues_json, summary, measured_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                component,
                status,
                issues_json,
                report.summary,
                report.measured_at
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn save_pending_decision(
        &self,
        d: &sovereign_core::health::PendingDecision,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let component = serde_json::to_string(&d.component).map_err(map_json)?;
        let issue_json = serde_json::to_string(&d.issue).map_err(map_json)?;
        let options_json = serde_json::to_string(&d.options).map_err(map_json)?;
        conn.execute(
            "INSERT INTO pending_health_decisions
             (component, issue_json, question, options_json, consequence, surfaced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                component,
                issue_json,
                d.question,
                options_json,
                d.consequence,
                d.surfaced_at_secs
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn list_pending_decisions(
        &self,
    ) -> Result<Vec<sovereign_core::health::PendingDecision>> {
        use std::time::{Duration, UNIX_EPOCH};
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, component, issue_json, question, options_json, consequence, surfaced_at
                 FROM pending_health_decisions WHERE resolved_at IS NULL",
            )
            .map_err(map_db)?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(map_db)?;

        let mut out = Vec::new();
        for row in rows {
            let (id, component_json, issue_json, question, options_json, consequence, surfaced_at_secs) =
                row.map_err(map_db)?;
            let component: sovereign_core::health::Component =
                serde_json::from_str(&component_json).map_err(map_json)?;
            let issue: sovereign_core::health::HealthIssue =
                serde_json::from_str(&issue_json).map_err(map_json)?;
            let options: Vec<sovereign_core::health::UserOption> =
                serde_json::from_str(&options_json).map_err(map_json)?;
            let surfaced_at = UNIX_EPOCH
                .checked_add(Duration::from_secs(surfaced_at_secs.max(0) as u64));
            out.push(sovereign_core::health::PendingDecision {
                id: Some(id),
                component,
                issue,
                question,
                options,
                consequence,
                surfaced_at_secs,
                surfaced_at,
            });
        }
        Ok(out)
    }

    async fn resolve_pending_decision(
        &self,
        id: i64,
        chosen: sovereign_core::health::RepairKind,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let chosen_json = serde_json::to_string(&chosen).map_err(map_json)?;
        conn.execute(
            "UPDATE pending_health_decisions SET resolved_at = ?1 WHERE id = ?2",
            rusqlite::params![now(), id],
        )
        .map_err(map_db)?;
        let _ = chosen_json; // stored in resolved_at for now; extend schema if needed
        Ok(())
    }
}

impl StateStore for SqliteStateStore {}

/// Sanitize a natural language query into FTS5-safe keywords.
/// Strips punctuation, stopwords, and joins remaining terms with OR
/// for broader matching.
fn sanitize_fts5_query(query: &str) -> String {
    const STOPWORDS: &[&str] = &[
        "a", "an", "the", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "could",
        "should", "may", "might", "shall", "can", "need", "dare", "ought",
        "to", "of", "in", "for", "on", "with", "at", "by", "from", "as",
        "into", "through", "during", "before", "after", "above", "below",
        "between", "out", "off", "over", "under", "again", "further", "then",
        "once", "here", "there", "when", "where", "why", "how", "all", "both",
        "each", "few", "more", "most", "other", "some", "such", "no", "nor",
        "not", "only", "own", "same", "so", "than", "too", "very", "just",
        "because", "but", "and", "or", "if", "while", "about", "what", "which",
        "who", "whom", "this", "that", "these", "those", "am", "it", "its",
        "he", "she", "his", "her", "they", "them", "their", "we", "us", "our",
        "you", "your", "i", "me", "my",
    ];

    let words: Vec<&str> = query
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|w| !w.is_empty())
        .filter(|w| w.len() > 1)
        .filter(|w| !STOPWORDS.contains(&w.to_lowercase().as_str()))
        .collect();

    if words.is_empty() {
        return String::new();
    }

    // Use OR to match any keyword (broader recall).
    words.join(" OR ")
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}
