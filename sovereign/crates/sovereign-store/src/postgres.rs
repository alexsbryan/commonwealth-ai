use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use deadpool_postgres::{Config, Pool, Runtime};
use tokio_postgres::NoTls;

use sovereign_core::error::{Error, Result};
use sovereign_core::observer::{noop_observer, SharedStateStoreObserver};
use sovereign_core::traits::{
    BudgetStore, ConversationStore, CorpusStateStore, DocumentSessionStore,
    DocumentStore, HealthStore, MemoryStore, PermissionStore, RoutingStore,
    StateStore, TaskStore,
};
use sovereign_core::types::*;

pub struct PostgresStateStore {
    pool: Pool,
    /// Post-commit observer. Mirror of the field on `SqliteStateStore`.
    /// Uses `Arc<RwLock<_>>` so callers can install the observer
    /// after the store is Arc-wrapped — same invariant described on
    /// the SQLite store.
    observer: Arc<RwLock<SharedStateStoreObserver>>,
}

impl PostgresStateStore {
    /// Connect to PostgreSQL and run migrations.
    pub async fn connect(connection_url: &str) -> Result<Self> {
        let mut cfg = Config::new();
        cfg.url = Some(connection_url.to_string());

        let pool = cfg
            .create_pool(Some(Runtime::Tokio1), NoTls)
            .map_err(|e| Error::Storage(format!("Failed to create pool: {e}")))?;

        let store = Self {
            pool,
            observer: Arc::new(RwLock::new(noop_observer())),
        };
        store.run_migrations().await?;
        Ok(store)
    }

    /// Builder-style observer install. See
    /// [`crate::sqlite::SqliteStateStore::with_observer`] for
    /// semantics; this is the Postgres mirror.
    pub fn with_observer(self, observer: SharedStateStoreObserver) -> Self {
        self.set_observer(observer);
        self
    }

    /// Runtime observer swap through a shared reference. Mirrors
    /// [`crate::sqlite::SqliteStateStore::set_observer`].
    pub fn set_observer(&self, observer: SharedStateStoreObserver) {
        let mut guard = self
            .observer
            .write()
            .expect("PostgresStateStore observer RwLock poisoned");
        *guard = observer;
    }

    fn fire_observer<F>(&self, f: F)
    where
        F: FnOnce(&dyn sovereign_core::observer::StateStoreObserver),
        F: std::panic::UnwindSafe,
    {
        let observer = {
            let guard = self
                .observer
                .read()
                .expect("PostgresStateStore observer RwLock poisoned");
            Arc::clone(&*guard)
        };
        // Mirror of `SqliteStateStore::fire_observer` — see that
        // implementation for the rationale on catching panics here.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            f(observer.as_ref());
        }));
        if let Err(payload) = result {
            let msg = if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = payload.downcast_ref::<&'static str>() {
                s.to_string()
            } else {
                "<non-string panic payload>".to_string()
            };
            tracing::warn!(
                panic = %msg,
                "StateStoreObserver handler panicked; write already committed"
            );
        }
    }

    async fn run_migrations(&self) -> Result<()> {
        let client = self
            .pool
            .get()
            .await
            .map_err(|e| Error::Storage(format!("Pool error: {e}")))?;

        client
            .batch_execute(
                r#"
            CREATE TABLE IF NOT EXISTS conversations (
                id TEXT PRIMARY KEY,
                title TEXT,
                created_at BIGINT NOT NULL,
                updated_at BIGINT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at BIGINT NOT NULL,
                metadata TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_messages_convo ON messages(conversation_id, created_at);

            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                goal TEXT NOT NULL,
                plan TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT '{}',
                status TEXT NOT NULL DEFAULT 'running',
                created_at BIGINT NOT NULL,
                updated_at BIGINT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS memories (
                id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                source TEXT NOT NULL,
                confidence DOUBLE PRECISION NOT NULL DEFAULT 1.0,
                created_at BIGINT NOT NULL,
                last_used BIGINT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS documents (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                content TEXT NOT NULL,
                chunk_index INTEGER NOT NULL,
                embedding BYTEA,
                created_at BIGINT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS permissions (
                tool_id TEXT NOT NULL,
                scope TEXT NOT NULL,
                granted BOOLEAN NOT NULL,
                granted_at BIGINT NOT NULL,
                PRIMARY KEY (tool_id, scope)
            );

            CREATE TABLE IF NOT EXISTS routing_log (
                id SERIAL PRIMARY KEY,
                message_hash TEXT NOT NULL,
                classified_as TEXT NOT NULL,
                was_correct BOOLEAN,
                latency_ms BIGINT NOT NULL,
                oicp_match_quality TEXT,
                oicp_model_id TEXT,
                created_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT
            );

            CREATE TABLE IF NOT EXISTS corpus_state (
                corpus_id    TEXT PRIMARY KEY,
                installed_at BIGINT NOT NULL,
                source_date  TEXT NOT NULL,
                chunks_count BIGINT NOT NULL DEFAULT 0,
                index_size_mb BIGINT NOT NULL DEFAULT 0,
                last_updated BIGINT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS search_budget (
                backend         TEXT PRIMARY KEY,
                monthly_limit   INTEGER NOT NULL,
                used_this_month INTEGER NOT NULL DEFAULT 0,
                reset_date      BIGINT NOT NULL
            );

            ALTER TABLE documents ADD COLUMN IF NOT EXISTS source_type TEXT DEFAULT 'user';
            ALTER TABLE documents ADD COLUMN IF NOT EXISTS corpus_id TEXT;

            -- KnowledgeView v1 additive columns (mirror of the SQLite
            -- run_knowledge_view_migrations). Nullable so existing rows
            -- remain valid; populated on new writes.
            ALTER TABLE memories ADD COLUMN IF NOT EXISTS source_conversation_id TEXT;
            ALTER TABLE conversations ADD COLUMN IF NOT EXISTS skill_id TEXT;

            -- Inner-work memory wall (denormalized scope tag). Mirror
            -- of run_inner_work_memory_wall_migrations on SQLite.
            ALTER TABLE memories ADD COLUMN IF NOT EXISTS source_skill_id TEXT;

            -- Rolling-summary memory compaction (2026-05-23). Mirror
            -- of run_memory_compaction_migrations on SQLite. All three
            -- columns are nullable/defaulted so existing rows surface
            -- as Raw + empty source_memory_ids + non-superseded.
            ALTER TABLE memories ADD COLUMN IF NOT EXISTS kind TEXT NOT NULL DEFAULT 'raw';
            ALTER TABLE memories ADD COLUMN IF NOT EXISTS source_memory_ids TEXT NOT NULL DEFAULT '[]';
            ALTER TABLE memories ADD COLUMN IF NOT EXISTS superseded_by TEXT;
            CREATE INDEX IF NOT EXISTS idx_memories_superseded_by
                ON memories(superseded_by);
            CREATE INDEX IF NOT EXISTS idx_memories_conv_active
                ON memories(source_conversation_id)
                WHERE superseded_by IS NULL;

            -- Antifragile-routing signal columns (PR4). Mirror of
            -- migrations::run_antifragile_routing_migrations on the
            -- SQLite side. Captured when the user redirects away
            -- from a Propose-tier commit.
            ALTER TABLE routing_log ADD COLUMN IF NOT EXISTS was_redirected BOOLEAN NOT NULL DEFAULT FALSE;
            ALTER TABLE routing_log ADD COLUMN IF NOT EXISTS redirect_to TEXT;
            "#,
            )
            .await
            .map_err(|e| Error::Storage(format!("Migration failed: {e}")))?;

        Ok(())
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }
}

#[async_trait]
impl ConversationStore for PostgresStateStore {
    async fn save_message(&self, msg: &Message) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let now = Self::now();

        // Upsert conversation.
        client
            .execute(
                "INSERT INTO conversations (id, title, created_at, updated_at) \
                 VALUES ($1, NULL, $2, $2) \
                 ON CONFLICT (id) DO UPDATE SET updated_at = $2",
                &[&msg.conversation_id, &now],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;

        let role = msg.role_str();
        let metadata = msg.metadata.as_ref().map(|m| m.to_string());

        client
            .execute(
                "INSERT INTO messages (id, conversation_id, role, content, created_at, metadata) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (id) DO NOTHING",
                &[&msg.id, &msg.conversation_id, &role, &msg.content, &msg.created_at, &metadata],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;

        // Post-commit observer notification — mirror of the SQLite
        // path. Safe to fire after the client is returned to the pool;
        // the observer is documented as best-effort.
        self.fire_observer(|o| o.on_message_written(&msg.conversation_id));
        Ok(())
    }

    async fn get_conversation(&self, id: &str) -> Result<Conversation> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;

        let row = client
            .query_opt(
                "SELECT id, title, created_at, updated_at FROM conversations WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?
            .ok_or_else(|| Error::NotFound(format!("Conversation {id}")))?;

        let messages = client
            .query(
                "SELECT id, conversation_id, role, content, created_at, metadata \
                 FROM messages WHERE conversation_id = $1 ORDER BY created_at",
                &[&id],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;

        let msgs: Vec<Message> = messages
            .iter()
            .map(|r| {
                let role_str: String = r.get("role");
                Message {
                    id: r.get("id"),
                    conversation_id: r.get("conversation_id"),
                    role: if role_str == "user" { Role::User } else if role_str == "system" { Role::System } else { Role::Assistant },
                    content: r.get("content"),
                    created_at: r.get("created_at"),
                    metadata: r.get::<_, Option<String>>("metadata").and_then(|s| serde_json::from_str(&s).ok()),
                }
            })
            .collect();

        Ok(Conversation {
            id: row.get("id"),
            title: row.get("title"),
            messages: msgs,
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            version: 0,
            deleted_at: None,
            skill_id: None,
        })
    }

    async fn list_conversations(&self, limit: usize, offset: usize) -> Result<Vec<Conversation>> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;

        let rows = client
            .query(
                "SELECT id, title, created_at, updated_at, skill_id FROM conversations \
                 WHERE deleted_at IS NULL \
                   AND (skill_id IS NULL OR skill_id != 'inner-work') \
                 ORDER BY updated_at DESC LIMIT $1 OFFSET $2",
                &[&(limit as i64), &(offset as i64)],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;

        Ok(rows
            .iter()
            .map(|r| Conversation {
                id: r.get("id"),
                title: r.get("title"),
                messages: vec![],
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
                version: 0,
                deleted_at: None,
                skill_id: r.get("skill_id"),
            })
            .collect())
    }

    async fn search_messages(&self, query: &str) -> Result<Vec<Message>> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;

        // Use ILIKE for simple text search. For production, use tsvector.
        let pattern = format!("%{query}%");
        let rows = client
            .query(
                "SELECT id, conversation_id, role, content, created_at, metadata \
                 FROM messages WHERE content ILIKE $1 ORDER BY created_at DESC LIMIT 50",
                &[&pattern],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;

        Ok(rows
            .iter()
            .map(|r| {
                let role_str: String = r.get("role");
                Message {
                    id: r.get("id"),
                    conversation_id: r.get("conversation_id"),
                    role: if role_str == "user" { Role::User } else if role_str == "system" { Role::System } else { Role::Assistant },
                    content: r.get("content"),
                    created_at: r.get("created_at"),
                    metadata: r.get::<_, Option<String>>("metadata").and_then(|s| serde_json::from_str(&s).ok()),
                }
            })
            .collect())
    }

    async fn delete_conversation(&self, id: &str) -> Result<()> {
        {
            let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
            // CASCADE deletes messages.
            client
                .execute("DELETE FROM conversations WHERE id = $1", &[&id])
                .await
                .map_err(|e| Error::Storage(e.to_string()))?;
        }
        self.fire_observer(|o| o.on_conversation_deleted(id));
        Ok(())
    }

    async fn update_conversation_title(&self, id: &str, title: &str) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let ts = Self::now();
        let rows = client
            .execute(
                "UPDATE conversations SET title = $2, updated_at = $3 WHERE id = $1",
                &[&id, &title, &ts],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        if rows == 0 {
            return Err(Error::NotFound(format!("conversation {id}")));
        }
        Ok(())
    }

    async fn set_conversation_skill_if_unset(
        &self,
        conversation_id: &str,
        skill_id: &str,
    ) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        client
            .execute(
                "UPDATE conversations SET skill_id = $2 \
                 WHERE id = $1 AND skill_id IS NULL",
                &[&conversation_id, &skill_id],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl TaskStore for PostgresStateStore {
    async fn save_task(&self, task: &Task) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let plan = serde_json::to_string(&task.plan).unwrap_or_default();
        let state = serde_json::to_string(&task.completed_steps).unwrap_or_default();
        let status = format!("{:?}", task.status);
        let now = Self::now();

        client
            .execute(
                "INSERT INTO tasks (id, conversation_id, goal, plan, state, status, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $7) \
                 ON CONFLICT (id) DO UPDATE SET plan = $4, state = $5, status = $6, updated_at = $7",
                &[&task.id, &task.conversation_id, &task.goal, &plan, &state, &status, &now],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;

        Ok(())
    }

    async fn get_task(&self, id: &str) -> Result<Task> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let row = client
            .query_opt("SELECT * FROM tasks WHERE id = $1", &[&id])
            .await
            .map_err(|e| Error::Storage(e.to_string()))?
            .ok_or_else(|| Error::NotFound(format!("Task {id}")))?;

        let plan_str: String = row.get("plan");
        let state_str: String = row.get("state");

        Ok(Task {
            id: row.get("id"),
            conversation_id: row.get("conversation_id"),
            goal: row.get("goal"),
            plan: serde_json::from_str(&plan_str).unwrap_or_else(|_| Plan {
                id: String::new(),
                goal: String::new(),
                steps: vec![],
                edges: vec![],
            }),
            completed_steps: serde_json::from_str(&state_str).unwrap_or_default(),
            status: TaskStatus::Running,
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    }
}

#[async_trait]
impl MemoryStore for PostgresStateStore {
    async fn save_memory(&self, memory: &Memory) -> Result<()> {
        {
            let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
            let kind_str = match memory.kind {
                sovereign_core::types::MemoryKind::Raw => "raw",
                sovereign_core::types::MemoryKind::Summary => "summary",
            };
            let source_memory_ids_json = serde_json::to_string(&memory.source_memory_ids)
                .unwrap_or_else(|_| "[]".into());
            client
                .execute(
                    "INSERT INTO memories \
                       (id, content, source, confidence, created_at, last_used, \
                        kind, source_memory_ids, superseded_by) \
                     VALUES ($1, $2, $3, $4, $5, $5, $6, $7, $8) \
                     ON CONFLICT (id) DO UPDATE SET \
                       content = $2, confidence = $4, \
                       kind = $6, source_memory_ids = $7, superseded_by = $8",
                    &[
                        &memory.id,
                        &memory.content,
                        &memory.source,
                        &memory.confidence,
                        &memory.created_at,
                        &kind_str,
                        &source_memory_ids_json,
                        &memory.superseded_by,
                    ],
                )
                .await
                .map_err(|e| Error::Storage(e.to_string()))?;
        }
        self.fire_observer(|o| o.on_memory_written(&memory.id));
        Ok(())
    }

    async fn get_relevant_memories(&self, context: &str, limit: usize) -> Result<Vec<Memory>> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let pattern = format!("%{context}%");
        let rows = client
            .query(
                "SELECT id, content, source, confidence, created_at, last_used, \
                        kind, source_memory_ids, superseded_by \
                 FROM memories \
                 WHERE content ILIKE $1 \
                   AND confidence > 0.1 \
                   AND superseded_by IS NULL \
                 ORDER BY confidence DESC, last_used DESC LIMIT $2",
                &[&pattern, &(limit as i64)],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;

        Ok(rows.iter().map(pg_row_to_memory).collect())
    }

    async fn get_all_memories(&self) -> Result<Vec<Memory>> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let rows = client
            .query(
                "SELECT id, content, source, confidence, created_at, last_used, \
                        kind, source_memory_ids, superseded_by \
                 FROM memories \
                 WHERE superseded_by IS NULL \
                 ORDER BY created_at DESC",
                &[],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;

        Ok(rows.iter().map(pg_row_to_memory).collect())
    }

    async fn delete_memory(&self, id: &str) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        client.execute("DELETE FROM memories WHERE id = $1", &[&id]).await.map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    async fn update_memory_confidence(&self, id: &str, confidence: f64) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        client.execute("UPDATE memories SET confidence = $1 WHERE id = $2", &[&confidence, &id]).await.map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    async fn touch_memory(&self, id: &str, timestamp: i64) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        client.execute("UPDATE memories SET last_used = $1 WHERE id = $2", &[&timestamp, &id]).await.map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    async fn list_memories_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<Memory>> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let rows = client
            .query(
                "SELECT id, content, source, confidence, created_at, last_used, \
                        kind, source_memory_ids, superseded_by \
                 FROM memories \
                 WHERE source_conversation_id = $1 \
                   AND superseded_by IS NULL \
                 ORDER BY created_at ASC",
                &[&conversation_id],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(rows.iter().map(pg_row_to_memory).collect())
    }

    async fn mark_superseded(
        &self,
        memory_id: &str,
        summary_id: &str,
    ) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        client
            .execute(
                "UPDATE memories SET superseded_by = $1 WHERE id = $2",
                &[&summary_id, &memory_id],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }
}

fn pg_row_to_memory(r: &tokio_postgres::Row) -> Memory {
    let kind_str: Option<String> = r.try_get("kind").ok();
    let kind = match kind_str.as_deref() {
        Some("summary") => sovereign_core::types::MemoryKind::Summary,
        _ => sovereign_core::types::MemoryKind::Raw,
    };
    let source_memory_ids_json: Option<String> = r.try_get("source_memory_ids").ok();
    let source_memory_ids: Vec<String> = source_memory_ids_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let superseded_by: Option<String> = r.try_get("superseded_by").ok().flatten();
    Memory {
        id: r.get("id"),
        content: r.get("content"),
        source: r.get("source"),
        confidence: r.get("confidence"),
        created_at: r.get("created_at"),
        last_used: r.get("last_used"),
        version: 0,
        deleted_at: None,
        source_conversation_id: None,
        source_skill_id: None,
        kind,
        source_memory_ids,
        superseded_by,
    }
}

#[async_trait]
impl RoutingStore for PostgresStateStore {
    async fn log_routing(&self, message_hash: &str, classified_as: &str, latency_ms: i64) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let now = Self::now();
        client
            .execute(
                "INSERT INTO routing_log (message_hash, classified_as, latency_ms, created_at) VALUES ($1, $2, $3, $4)",
                &[&message_hash, &classified_as, &latency_ms, &now],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    async fn get_routing_corrections(&self, limit: usize) -> Result<Vec<RoutingCorrection>> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let rows = client
            .query(
                "SELECT message_hash, classified_as, was_correct, created_at FROM routing_log WHERE was_correct = false ORDER BY created_at DESC LIMIT $1",
                &[&(limit as i64)],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;

        Ok(rows.iter().map(|r| RoutingCorrection {
            message_hash: r.get("message_hash"),
            classified_as: r.get("classified_as"),
            was_correct: r.get::<_, Option<bool>>("was_correct").unwrap_or(false),
            created_at: r.get("created_at"),
        }).collect())
    }

    async fn mark_routing_correct(&self, message_hash: &str, was_correct: bool) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        client
            .execute(
                "UPDATE routing_log SET was_correct = $1 WHERE message_hash = $2",
                &[&was_correct, &message_hash],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    async fn mark_routing_redirected(
        &self,
        message_hash: &str,
        redirect_to: &str,
    ) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        client
            .execute(
                "UPDATE routing_log SET was_redirected = TRUE, redirect_to = $1 \
                 WHERE message_hash = $2",
                &[&redirect_to, &message_hash],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl DocumentStore for PostgresStateStore {
    async fn store_chunks(&self, chunks: &[DocumentChunk]) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let now = Self::now();
        for chunk in chunks {
            let embedding_bytes: Option<Vec<u8>> = chunk.embedding.as_ref().map(|e| {
                e.iter().flat_map(|f| f.to_le_bytes()).collect()
            });
            let (source_type_str, corpus_id) = chunk.source_type.to_db_columns();
            let corpus_id_owned = corpus_id.map(|s| s.to_string());
            client
                .execute(
                    "INSERT INTO documents (id, source, content, chunk_index, embedding, created_at, source_type, corpus_id) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT (id) DO NOTHING",
                    &[&chunk.id, &chunk.source, &chunk.content, &(chunk.chunk_index as i32), &embedding_bytes, &now, &source_type_str, &corpus_id_owned],
                )
                .await
                .map_err(|e| Error::Storage(e.to_string()))?;
        }
        Ok(())
    }

    async fn search_documents(&self, _query_embedding: &[f32], query_text: &str, limit: usize) -> Result<Vec<DocumentChunk>> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let pattern = format!("%{query_text}%");
        let rows = client
            .query(
                "SELECT id, source, content, chunk_index, source_type, corpus_id FROM documents WHERE content ILIKE $1 LIMIT $2",
                &[&pattern, &(limit as i64)],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;

        Ok(rows.iter().map(|r| {
            let idx: i32 = r.get("chunk_index");
            let st: Option<String> = r.get("source_type");
            let cid: Option<String> = r.get("corpus_id");
            DocumentChunk {
                id: r.get("id"),
                source: r.get("source"),
                content: r.get("content"),
                chunk_index: idx as usize,
                embedding: None,
                created_at: Self::now(),
                source_type: SourceType::from_db_columns(st.as_deref().unwrap_or("user"), cid.as_deref()),
            }
        }).collect())
    }

    async fn get_chunks_by_source(&self, source: &str) -> Result<Vec<DocumentChunk>> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let rows = client
            .query("SELECT id, source, content, chunk_index, source_type, corpus_id FROM documents WHERE source = $1 ORDER BY chunk_index", &[&source])
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;

        Ok(rows.iter().map(|r| {
            let idx: i32 = r.get("chunk_index");
            let st: Option<String> = r.get("source_type");
            let cid: Option<String> = r.get("corpus_id");
            DocumentChunk {
                id: r.get("id"),
                source: r.get("source"),
                content: r.get("content"),
                chunk_index: idx as usize,
                embedding: None,
                created_at: Self::now(),
                source_type: SourceType::from_db_columns(st.as_deref().unwrap_or("user"), cid.as_deref()),
            }
        }).collect())
    }

    async fn delete_chunks_by_corpus(&self, corpus_id: &str) -> Result<u64> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let count = client
            .execute("DELETE FROM documents WHERE corpus_id = $1", &[&corpus_id])
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(count)
    }

    async fn list_sources(&self) -> Result<Vec<String>> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let rows = client
            .query("SELECT DISTINCT source FROM documents ORDER BY source", &[])
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(rows.iter().map(|r| r.get("source")).collect())
    }
}

#[async_trait]
impl CorpusStateStore for PostgresStateStore {
    async fn save_corpus_state(&self, state: &CorpusState) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        client
            .execute(
                "INSERT INTO corpus_state (corpus_id, installed_at, source_date, chunks_count, index_size_mb, last_updated, vector_index_ready) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (corpus_id) DO UPDATE SET installed_at = $2, source_date = $3, chunks_count = $4, index_size_mb = $5, last_updated = $6, vector_index_ready = $7",
                &[&state.corpus_id, &state.installed_at, &state.source_date, &state.chunks_count, &state.index_size_mb, &state.last_updated, &(state.vector_index_ready as i64)],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    async fn get_corpus_state(&self, corpus_id: &str) -> Result<CorpusState> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let row = client
            .query_opt(
                "SELECT corpus_id, installed_at, source_date, chunks_count, index_size_mb, last_updated, COALESCE(vector_index_ready, 0) FROM corpus_state WHERE corpus_id = $1",
                &[&corpus_id],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;

        match row {
            Some(r) => Ok(CorpusState {
                corpus_id: r.get("corpus_id"),
                installed_at: r.get("installed_at"),
                source_date: r.get("source_date"),
                chunks_count: r.get("chunks_count"),
                index_size_mb: r.get("index_size_mb"),
                last_updated: r.get("last_updated"),
                version: 0,
                deleted_at: None,
                vector_index_ready: r.get::<_, i64>("vector_index_ready") != 0,
            }),
            None => Err(Error::NotFound(format!("Corpus {corpus_id}"))),
        }
    }

    async fn list_corpus_states(&self) -> Result<Vec<CorpusState>> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let rows = client
            .query("SELECT corpus_id, installed_at, source_date, chunks_count, index_size_mb, last_updated, COALESCE(vector_index_ready, 0) FROM corpus_state ORDER BY installed_at DESC", &[])
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;

        Ok(rows.iter().map(|r| CorpusState {
            corpus_id: r.get("corpus_id"),
            installed_at: r.get("installed_at"),
            source_date: r.get("source_date"),
            chunks_count: r.get("chunks_count"),
            index_size_mb: r.get("index_size_mb"),
            last_updated: r.get("last_updated"),
            version: 0,
            deleted_at: None,
            vector_index_ready: r.get::<_, i64>("vector_index_ready") != 0,
        }).collect())
    }

    async fn delete_corpus_state(&self, corpus_id: &str) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        client
            .execute("DELETE FROM corpus_state WHERE corpus_id = $1", &[&corpus_id])
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    async fn set_vector_index_ready(&self, corpus_id: &str, ready: bool) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        client
            .execute(
                "UPDATE corpus_state SET vector_index_ready = $1 WHERE corpus_id = $2",
                &[&(ready as i64), &corpus_id],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    async fn get_vector_index_ready(&self, corpus_id: &str) -> Result<bool> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let row = client
            .query_opt(
                "SELECT COALESCE(vector_index_ready, 0) FROM corpus_state WHERE corpus_id = $1",
                &[&corpus_id],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(row.map(|r| r.get::<_, i64>(0) != 0).unwrap_or(false))
    }
}

#[async_trait]
impl BudgetStore for PostgresStateStore {
    async fn get_search_budget(&self, backend: &str) -> Result<Option<SearchBudget>> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let row = client
            .query_opt(
                "SELECT backend, monthly_limit, used_this_month, reset_date FROM search_budget WHERE backend = $1",
                &[&backend],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;

        Ok(row.map(|r| SearchBudget {
            backend: r.get("backend"),
            monthly_limit: r.get::<_, i32>("monthly_limit") as u32,
            used_this_month: r.get::<_, i32>("used_this_month") as u32,
            reset_date: r.get("reset_date"),
        }))
    }

    async fn update_search_budget(&self, budget: &SearchBudget) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        client
            .execute(
                "INSERT INTO search_budget (backend, monthly_limit, used_this_month, reset_date) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (backend) DO UPDATE SET monthly_limit = $2, used_this_month = $3, reset_date = $4",
                &[&budget.backend, &(budget.monthly_limit as i32), &(budget.used_this_month as i32), &budget.reset_date],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl PermissionStore for PostgresStateStore {
    async fn get_permission(&self, tool_id: &str, scope: &str) -> Result<Option<bool>> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let row = client
            .query_opt(
                "SELECT granted FROM permissions WHERE tool_id = $1 AND scope = $2",
                &[&tool_id, &scope],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(row.map(|r| r.get("granted")))
    }

    async fn set_permission(&self, tool_id: &str, scope: &str, granted: bool) -> Result<()> {
        let client = self.pool.get().await.map_err(|e| Error::Storage(e.to_string()))?;
        let now = Self::now();
        client
            .execute(
                "INSERT INTO permissions (tool_id, scope, granted, granted_at) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (tool_id, scope) DO UPDATE SET granted = $3, granted_at = $4",
                &[&tool_id, &scope, &granted, &now],
            )
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl HealthStore for PostgresStateStore {}

#[async_trait]
impl DocumentSessionStore for PostgresStateStore {
    async fn create_document_session(&self, _session: &DocumentSession) -> Result<()> {
        Err(Error::NotImplemented("Postgres document sessions".into()))
    }
    async fn get_document_session(&self, _session_id: &str) -> Result<Option<DocumentSession>> {
        Ok(None)
    }
    async fn get_document_session_by_conversation(
        &self,
        _conversation_id: &str,
    ) -> Result<Option<DocumentSession>> {
        Ok(None)
    }
    async fn update_document_session(&self, _session: &DocumentSession) -> Result<()> {
        Err(Error::NotImplemented("Postgres document sessions".into()))
    }
}

impl StateStore for PostgresStateStore {}
