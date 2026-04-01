use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rusqlite::Connection;
use tokio::sync::Mutex;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::StateStore;
use sovereign_core::types::*;

use crate::migrations;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub struct SqliteStateStore {
    conn: Mutex<Connection>,
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

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Storage(format!("Failed to open in-memory db: {e}")))?;

        migrations::run_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Migration failed: {e}")))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

fn map_db(e: rusqlite::Error) -> Error {
    Error::Storage(e.to_string())
}

#[async_trait]
impl StateStore for SqliteStateStore {
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
                "SELECT title, created_at, updated_at FROM conversations WHERE id = ?1",
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
        })
    }

    async fn list_conversations(&self, limit: usize, offset: usize) -> Result<Vec<Conversation>> {
        let conn = self.conn.lock().await;

        let mut stmt = conn
            .prepare(
                "SELECT id, title, created_at, updated_at
                 FROM conversations ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2",
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
                })
            })
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(messages)
    }

    async fn delete_conversation(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        // CASCADE deletes messages too.
        conn.execute(
            "DELETE FROM conversations WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(map_db)?;
        Ok(())
    }

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
                })
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("Task {id}")),
            other => map_db(other),
        })
    }

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

    async fn get_relevant_memories(&self, _context: &str, _limit: usize) -> Result<Vec<Memory>> {
        // Requires embeddings (Phase 9). Return empty for now.
        Ok(Vec::new())
    }

    async fn store_chunks(&self, chunks: &[DocumentChunk]) -> Result<()> {
        let conn = self.conn.lock().await;
        for chunk in chunks {
            let embedding_blob = chunk.embedding.as_ref().map(|v| {
                v.iter()
                    .flat_map(|f| f.to_le_bytes())
                    .collect::<Vec<u8>>()
            });

            conn.execute(
                "INSERT OR REPLACE INTO documents (id, source, content, chunk_index, embedding, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    chunk.id,
                    chunk.source,
                    chunk.content,
                    chunk.chunk_index as i64,
                    embedding_blob,
                    chunk.created_at,
                ],
            )
            .map_err(map_db)?;
        }
        Ok(())
    }

    async fn search_documents(
        &self,
        _query_embedding: &[f32],
        _query_text: &str,
        _limit: usize,
    ) -> Result<Vec<DocumentChunk>> {
        // Requires sqlite-vec (Phase 6). Return empty for now.
        Ok(Vec::new())
    }

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
