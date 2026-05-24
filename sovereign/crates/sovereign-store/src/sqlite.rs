use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension};
use tokio::sync::Mutex;

use sovereign_core::error::{Error, Result};
use sovereign_core::observer::{noop_observer, SharedStateStoreObserver};
use sovereign_core::traits::{
    BudgetStore, ConversationStore, CorpusStateStore, DocumentAssetStore,
    DocumentSessionStore, DocumentStore, HealthStore, MemoryStore,
    PermissionStore, RoutingStore, StateStore, TaskStore,
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
    /// Observer fired after successful writes (post-commit). Defaults
    /// to a no-op; callers install a real observer via
    /// [`SqliteStateStore::with_observer`] (builder, pre-Arc) or
    /// [`SqliteStateStore::set_observer`] (runtime swap, works on
    /// `Arc<SqliteStateStore>`).
    ///
    /// The `RwLock` lets the server bootstrap create the store,
    /// `Arc`-wrap it for the router and corpus tools, build the
    /// `KnowledgeViewManager` (which needs the `CorpusEngine`), and
    /// *then* register the manager as the observer — without
    /// restructuring the initialization order.
    observer: Arc<RwLock<SharedStateStoreObserver>>,
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
        migrations::run_document_session_migration(&conn)
            .map_err(|e| Error::Storage(format!("Document session migration failed: {e}")))?;
        migrations::run_document_asset_migration(&conn)
            .map_err(|e| Error::Storage(format!("Document asset migration failed: {e}")))?;
        migrations::run_raptor_atlas_migration(&conn)
            .map_err(|e| Error::Storage(format!("RAPTOR atlas migration failed: {e}")))?;
        migrations::run_knowledge_view_migrations(&conn)
            .map_err(|e| Error::Storage(format!("KnowledgeView migration failed: {e}")))?;
        migrations::run_inner_work_memory_wall_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Inner-work memory wall migration failed: {e}")))?;
        migrations::run_memory_compaction_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Memory compaction migration failed: {e}")))?;
        migrations::run_antifragile_routing_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Antifragile routing migration failed: {e}")))?;
        migrations::run_conv_tiered_migration(&conn)
            .map_err(|e| Error::Storage(format!("Conv tiered migration failed: {e}")))?;
        migrations::run_chunk_entities_migration(&conn)
            .map_err(|e| Error::Storage(format!("Chunk entities migration failed: {e}")))?;
        migrations::run_vault_themes_migration(&conn)
            .map_err(|e| Error::Storage(format!("Vault themes migration failed: {e}")))?;
        migrations::run_surface_skill_backfill(&conn)
            .map_err(|e| Error::Storage(format!("Surface-skill backfill failed: {e}")))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            observer: Arc::new(RwLock::new(noop_observer())),
        })
    }

    /// Return a shared handle to the underlying connection.
    /// Used by `SqliteInsightStore` to share the same database connection.
    pub fn connection(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }

    /// Create an empty conversation row with an optional surface
    /// skill tag. Called by the desktop `create_conversation` Tauri
    /// command so the routing layer knows which workspace surface
    /// created the conversation BEFORE any messages are sent.
    ///
    /// `surface_skill_id` semantics:
    ///   - `Some("inner-work")` / `Some("recipe-author")` — conversation
    ///     belongs to that workspace surface. Routing reads this tag
    ///     at every turn and applies the workspace's intent policy.
    ///   - `None` — default chat conversation. Routing follows
    ///     intent-derived policy without a workspace override.
    ///
    /// Idempotent via `INSERT OR IGNORE` — calling twice with the
    /// same id is a no-op (the second create_conversation in a
    /// flaky-network retry won't blow up).
    pub async fn insert_empty_conversation(
        &self,
        id: &str,
        created_at: i64,
        surface_skill_id: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO conversations \
                (id, title, created_at, updated_at, skill_id) \
             VALUES (?1, NULL, ?2, ?2, ?3)",
            rusqlite::params![id, created_at, surface_skill_id],
        )
        .map_err(map_db)?;
        Ok(())
    }

    /// List conversations filtered by their surface tag. Drives the
    /// cross-surface visibility restriction: the default chat
    /// sidebar passes `None` (untagged conversations only); the
    /// Inner Work history drawer passes `Some("inner-work")`;
    /// Recipe Author passes `Some("recipe-author")`.
    ///
    /// The filter is exact-match: `None` returns conversations
    /// where `skill_id IS NULL`; `Some(id)` returns conversations
    /// where `skill_id = id`. There is no "all conversations"
    /// affordance — that would defeat the visibility restriction
    /// the surfaces are meant to enforce.
    pub async fn list_conversations_for_surface(
        &self,
        surface_skill_id: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Conversation>> {
        let conn = self.conn.lock().await;
        let limit_i = limit as i64;
        let offset_i = offset as i64;
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<Conversation> {
            Ok(Conversation {
                id: row.get(0)?,
                title: row.get(1)?,
                messages: Vec::new(),
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
                version: 0,
                deleted_at: None,
                skill_id: row.get(4)?,
            })
        };
        let rows: Vec<Conversation> = match surface_skill_id {
            Some(id) => {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, title, created_at, updated_at, skill_id \
                         FROM conversations \
                         WHERE skill_id = ?1 AND deleted_at IS NULL \
                         ORDER BY updated_at DESC LIMIT ?2 OFFSET ?3",
                    )
                    .map_err(map_db)?;
                let iter = stmt
                    .query_map(rusqlite::params![id, limit_i, offset_i], map_row)
                    .map_err(map_db)?;
                let collected: std::result::Result<Vec<_>, _> = iter.collect();
                collected.map_err(map_db)?
            }
            None => {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, title, created_at, updated_at, skill_id \
                         FROM conversations \
                         WHERE skill_id IS NULL AND deleted_at IS NULL \
                         ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2",
                    )
                    .map_err(map_db)?;
                let iter = stmt
                    .query_map(rusqlite::params![limit_i, offset_i], map_row)
                    .map_err(map_db)?;
                let collected: std::result::Result<Vec<_>, _> = iter.collect();
                collected.map_err(map_db)?
            }
        };
        Ok(rows)
    }

    /// Read the (was_redirected, redirect_to) fields for the most
    /// recent `routing_log` row matching `message_hash`. Returns
    /// `None` if no row is found. Exposed for PR4 integration
    /// tests + future calibration job introspection — the schema
    /// columns are otherwise private to the SQLite impl.
    pub async fn read_redirect_signal(
        &self,
        message_hash: &str,
    ) -> Option<(bool, Option<String>)> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT was_redirected, redirect_to FROM routing_log \
             WHERE message_hash = ?1 ORDER BY created_at DESC LIMIT 1",
            rusqlite::params![message_hash],
            |row| {
                let was_redirected: i64 = row.get(0)?;
                let redirect_to: Option<String> = row.get(1)?;
                Ok((was_redirected != 0, redirect_to))
            },
        )
        .ok()
    }

    /// Builder-style observer install. Equivalent to
    /// [`SqliteStateStore::set_observer`] but consumes `self` so
    /// callers can chain at construction.
    ///
    /// The observer fires **after** the write transaction commits, so a
    /// panicking observer never corrupts the store. Dropped updates are
    /// recoverable by the next scheduled full-enrichment pass.
    pub fn with_observer(self, observer: SharedStateStoreObserver) -> Self {
        self.set_observer(observer);
        self
    }

    /// Swap the post-commit observer at runtime. Works through
    /// `Arc<SqliteStateStore>` (shared reference) because the
    /// observer slot uses interior mutability. The server bootstrap
    /// uses this to install the `KnowledgeViewManager` once the
    /// manager has been built (which requires the CorpusEngine that
    /// is constructed *after* the store is Arc-wrapped).
    pub fn set_observer(&self, observer: SharedStateStoreObserver) {
        let mut guard = self
            .observer
            .write()
            .expect("SqliteStateStore observer RwLock poisoned");
        *guard = observer;
    }

    fn fire_observer<F>(&self, f: F)
    where
        F: FnOnce(&dyn sovereign_core::observer::StateStoreObserver),
        F: std::panic::UnwindSafe,
    {
        // Clone the `Arc<dyn ...>` out from under the lock so the
        // observer handler can run without holding the RwLock —
        // avoids re-entrancy deadlocks if the observer calls back
        // into the store.
        let observer = {
            let guard = self
                .observer
                .read()
                .expect("SqliteStateStore observer RwLock poisoned");
            Arc::clone(&*guard)
        };
        // Defensive panic isolation: a buggy observer handler must
        // not take the store's caller down. The write has already
        // committed by the time we fire; dropping a panic here is
        // strictly safer than letting it unwind through the async
        // task boundary. Missed notifications are recoverable by
        // the next Tier-3 sweep.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            f(observer.as_ref());
        }));
        if let Err(payload) = result {
            let msg = panic_message(&payload);
            tracing::warn!(
                panic = %msg,
                "StateStoreObserver handler panicked; write already committed"
            );
        }
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
        migrations::run_document_session_migration(&conn)
            .map_err(|e| Error::Storage(format!("Document session migration failed: {e}")))?;
        migrations::run_document_asset_migration(&conn)
            .map_err(|e| Error::Storage(format!("Document asset migration failed: {e}")))?;
        migrations::run_raptor_atlas_migration(&conn)
            .map_err(|e| Error::Storage(format!("RAPTOR atlas migration failed: {e}")))?;
        migrations::run_knowledge_view_migrations(&conn)
            .map_err(|e| Error::Storage(format!("KnowledgeView migration failed: {e}")))?;
        migrations::run_inner_work_memory_wall_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Inner-work memory wall migration failed: {e}")))?;
        migrations::run_memory_compaction_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Memory compaction migration failed: {e}")))?;
        migrations::run_antifragile_routing_migrations(&conn)
            .map_err(|e| Error::Storage(format!("Antifragile routing migration failed: {e}")))?;
        migrations::run_conv_tiered_migration(&conn)
            .map_err(|e| Error::Storage(format!("Conv tiered migration failed: {e}")))?;
        migrations::run_chunk_entities_migration(&conn)
            .map_err(|e| Error::Storage(format!("Chunk entities migration failed: {e}")))?;
        migrations::run_vault_themes_migration(&conn)
            .map_err(|e| Error::Storage(format!("Vault themes migration failed: {e}")))?;
        migrations::run_surface_skill_backfill(&conn)
            .map_err(|e| Error::Storage(format!("Surface-skill backfill failed: {e}")))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            observer: Arc::new(RwLock::new(noop_observer())),
        })
    }
}

fn map_db(e: rusqlite::Error) -> Error {
    Error::Storage(e.to_string())
}

fn map_json(e: serde_json::Error) -> Error {
    Error::Storage(format!("JSON error: {e}"))
}

/// Column list used by every `memories` SELECT that needs the full
/// Memory shape (including compaction fields). Kept as a constant so
/// the row-reading helper and the SQL strings stay in lockstep — if
/// you add a column to the projection, update [`row_to_memory_full`]
/// in the same edit.
const MEMORY_FULL_COLUMNS: &str =
    "id, content, source, confidence, created_at, last_used, \
     source_conversation_id, source_skill_id, \
     kind, source_memory_ids, superseded_by";

/// Read a row produced by a SELECT whose projection matches
/// [`MEMORY_FULL_COLUMNS`] (11 columns) into a `Memory`. Honors the
/// compaction-fields defaults (Raw / empty / None) when the row
/// predates the compaction migration — sqlite returns NULL for those
/// columns on unmigrated rows; `Option::get` collapses NULL to None
/// and we coerce to the documented defaults below.
fn row_to_memory_full(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    let kind_str: Option<String> = row.get(8)?;
    let kind = match kind_str.as_deref() {
        Some("summary") => sovereign_core::types::MemoryKind::Summary,
        _ => sovereign_core::types::MemoryKind::Raw,
    };
    let source_memory_ids_json: Option<String> = row.get(9)?;
    let source_memory_ids: Vec<String> = source_memory_ids_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    Ok(Memory {
        id: row.get(0)?,
        content: row.get(1)?,
        source: row.get(2)?,
        confidence: row.get(3)?,
        created_at: row.get(4)?,
        last_used: row.get(5)?,
        version: 0,
        deleted_at: None,
        source_conversation_id: row.get(6)?,
        source_skill_id: row.get(7)?,
        kind,
        source_memory_ids,
        superseded_by: row.get(10)?,
    })
}

/// Extract a human-readable message from a panic payload. Used by
/// the observer panic-isolation wrapper so `tracing::warn!` can log
/// a useful string instead of `Any`.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&'static str>() {
        s.to_string()
    } else {
        "<non-string panic payload>".to_string()
    }
}

#[async_trait]
impl ConversationStore for SqliteStateStore {
    async fn save_message(&self, msg: &Message) -> Result<()> {
        {
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
        }
        // Post-commit observer notification. Lock dropped above so the
        // observer cannot deadlock on a store read from inside its handler.
        self.fire_observer(|o| o.on_message_written(&msg.conversation_id));
        Ok(())
    }

    async fn get_conversation(&self, id: &str) -> Result<Conversation> {
        let conn = self.conn.lock().await;

        let (title, created_at, updated_at, skill_id) = conn
            .query_row(
                "SELECT title, created_at, updated_at, skill_id FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
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
            skill_id,
        })
    }

    async fn list_conversations(&self, limit: usize, offset: usize) -> Result<Vec<Conversation>> {
        let conn = self.conn.lock().await;

        let mut stmt = conn
            .prepare(
                "SELECT id, title, created_at, updated_at, skill_id
                 FROM conversations
                 WHERE deleted_at IS NULL
                   AND (skill_id IS NULL OR skill_id != 'inner-work')
                 ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2",
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
                    skill_id: row.get(4)?,
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
        {
            let conn = self.conn.lock().await;
            let ts = now();
            conn.execute(
                "UPDATE conversations SET deleted_at = ?2, version = ?2 WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![id, ts],
            )
            .map_err(map_db)?;
        }
        // Post-commit notification so the KnowledgeView conversational
        // acquirer can drop this conversation's chunks from its index.
        self.fire_observer(|o| o.on_conversation_deleted(id));
        Ok(())
    }

    async fn update_conversation_title(&self, id: &str, title: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        let ts = now();
        let rows = conn
            .execute(
                "UPDATE conversations SET title = ?2, updated_at = ?3, version = ?3 \
                 WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![id, title, ts],
            )
            .map_err(map_db)?;
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
        // `WHERE skill_id IS NULL` guarantees idempotence: a later
        // skill activation cannot overwrite the first-message tag.
        // Silently a no-op when the conversation doesn't exist or
        // was already tagged — the Runtime calls this on every
        // message write for simplicity, relying on the constraint
        // to keep the first-writer-wins semantic.
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE conversations SET skill_id = ?2 \
             WHERE id = ?1 AND skill_id IS NULL",
            rusqlite::params![conversation_id, skill_id],
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
        {
            let conn = self.conn.lock().await;
            let kind_str = match memory.kind {
                sovereign_core::types::MemoryKind::Raw => "raw",
                sovereign_core::types::MemoryKind::Summary => "summary",
            };
            let source_memory_ids_json = serde_json::to_string(&memory.source_memory_ids)
                .unwrap_or_else(|_| "[]".into());
            conn.execute(
                "INSERT OR REPLACE INTO memories
                   (id, content, source, confidence, created_at, last_used,
                    source_conversation_id, source_skill_id,
                    kind, source_memory_ids, superseded_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    memory.id,
                    memory.content,
                    memory.source,
                    memory.confidence,
                    memory.created_at,
                    memory.last_used,
                    memory.source_conversation_id,
                    memory.source_skill_id,
                    kind_str,
                    source_memory_ids_json,
                    memory.superseded_by,
                ],
            )
            .map_err(map_db)?;
        }
        // Post-commit observer notification. Lock dropped above so the
        // observer cannot deadlock on a store read from inside its handler.
        self.fire_observer(|o| o.on_memory_written(&memory.id));
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
        tracing::debug!(
            input_chars = context.len(),
            fts_query = %fts_context,
            "memory:fts_match query"
        );

        let sql = format!(
            "SELECT {cols} \
             FROM memories m \
             JOIN memories_fts fts ON m.rowid = fts.rowid \
             WHERE memories_fts MATCH ?1 \
               AND m.deleted_at IS NULL \
               AND m.superseded_by IS NULL \
             LIMIT ?2",
            cols = MEMORY_FULL_COLUMNS
                .split(", ")
                .map(|c| format!("m.{c}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
        let mut stmt = conn.prepare(&sql).map_err(map_db)?;

        let memories: Vec<Memory> = stmt
            .query_map(rusqlite::params![fts_context, (limit * 3) as i64], row_to_memory_full)
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

    async fn get_all_memories_for_scope(
        &self,
        scope: &sovereign_core::MemoryScope,
    ) -> Result<Vec<Memory>> {
        let conn = self.conn.lock().await;
        // Filter at the SQL layer — the inner-work wall is a privacy
        // contract; in-process filtering would still load scoped rows
        // through the observer hooks and any future replication
        // transport. This route ensures we never even read scoped
        // bytes when serving a general query.
        let (where_clause, scope_param): (&str, Option<String>) = match scope {
            sovereign_core::MemoryScope::General => (
                "WHERE deleted_at IS NULL \
                   AND superseded_by IS NULL \
                   AND source_skill_id IS NULL",
                None,
            ),
            sovereign_core::MemoryScope::Scoped(id) => (
                "WHERE deleted_at IS NULL \
                   AND superseded_by IS NULL \
                   AND source_skill_id = ?1",
                Some(id.clone()),
            ),
        };
        let sql = format!(
            "SELECT {cols} FROM memories {where_clause}",
            cols = MEMORY_FULL_COLUMNS,
        );
        let mut stmt = conn.prepare(&sql).map_err(map_db)?;
        let memories: Vec<Memory> = if let Some(id) = scope_param {
            stmt.query_map(rusqlite::params![id], row_to_memory_full)
                .map_err(map_db)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(map_db)?
        } else {
            stmt.query_map([], row_to_memory_full)
                .map_err(map_db)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(map_db)?
        };
        Ok(memories)
    }

    async fn get_relevant_memories_for_scope(
        &self,
        scope: &sovereign_core::MemoryScope,
        context_query: &str,
        limit: usize,
    ) -> Result<Vec<Memory>> {
        if context_query.is_empty() {
            return Ok(Vec::new());
        }
        let fts_context = sanitize_fts5_query(context_query);
        if fts_context.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().await;
        let current_time = now();

        // Same SQL-level wall as get_all_memories_for_scope — the FTS
        // path has to honor the same invariant or scoped memories
        // could leak through the keyword fallback.
        let (scope_clause, scope_param): (&str, Option<String>) = match scope {
            sovereign_core::MemoryScope::General => (
                "AND m.source_skill_id IS NULL",
                None,
            ),
            sovereign_core::MemoryScope::Scoped(id) => (
                "AND m.source_skill_id = ?3",
                Some(id.clone()),
            ),
        };
        let cols = MEMORY_FULL_COLUMNS
            .split(", ")
            .map(|c| format!("m.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {cols} \
             FROM memories m \
             JOIN memories_fts fts ON m.rowid = fts.rowid \
             WHERE memories_fts MATCH ?1 \
               AND m.deleted_at IS NULL \
               AND m.superseded_by IS NULL \
               {scope_clause} \
             LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_db)?;
        let raw: Vec<Memory> = if let Some(id) = scope_param {
            stmt.query_map(
                rusqlite::params![fts_context, (limit * 3) as i64, id],
                row_to_memory_full,
            )
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap_or_default()
        } else {
            stmt.query_map(
                rusqlite::params![fts_context, (limit * 3) as i64],
                row_to_memory_full,
            )
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap_or_default()
        };

        // Same confidence-decay floor + last-used touch as the
        // unscoped path so callers can swap freely.
        let mut scored: Vec<(f64, Memory)> = raw
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
        let sql = format!(
            "SELECT {cols} \
             FROM memories \
             WHERE deleted_at IS NULL AND superseded_by IS NULL",
            cols = MEMORY_FULL_COLUMNS,
        );
        let mut stmt = conn.prepare(&sql).map_err(map_db)?;

        let memories: Vec<Memory> = stmt
            .query_map([], row_to_memory_full)
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

    async fn list_memories_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<Memory>> {
        let conn = self.conn.lock().await;
        let sql = format!(
            "SELECT {cols} \
             FROM memories \
             WHERE source_conversation_id = ?1 \
               AND deleted_at IS NULL \
               AND superseded_by IS NULL \
             ORDER BY created_at ASC",
            cols = MEMORY_FULL_COLUMNS,
        );
        let mut stmt = conn.prepare(&sql).map_err(map_db)?;
        let memories: Vec<Memory> = stmt
            .query_map(rusqlite::params![conversation_id], row_to_memory_full)
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;
        Ok(memories)
    }

    async fn mark_superseded(
        &self,
        memory_id: &str,
        summary_id: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE memories SET superseded_by = ?2 WHERE id = ?1",
            rusqlite::params![memory_id, summary_id],
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

    async fn mark_routing_redirected(
        &self,
        message_hash: &str,
        redirect_to: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE routing_log SET was_redirected = 1, redirect_to = ?2 \
             WHERE message_hash = ?1",
            rusqlite::params![message_hash, redirect_to],
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

    // Split on every non-alphanumeric character, INCLUDING dashes.
    // FTS5 parses `foo-bar` as `foo NOT bar` (the `-` is the NOT
    // operator), so a single hyphenated token in the query corrupts
    // the OR semantics of the surrounding clause and silently
    // returns zero rows. Splitting `6-month` into `6` + `month`
    // means the length-1 token drops out and the meaningful token
    // contributes a clean OR clause — recall behaves as intended.
    // Voice-eval scenario 07 ("…6-month growth roadmap…") was the
    // canonical reproduction: seed memories were saved correctly,
    // the witness path was wired correctly, but FTS returned 0
    // rows because the query string itself was malformed.
    let words: Vec<&str> = query
        .split(|c: char| !c.is_alphanumeric())
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

// ─── DocumentAssetStore ──────────────────────────────────────

#[async_trait]
impl DocumentAssetStore for SqliteStateStore {
    async fn save_document_asset(&self, asset: &DocumentAsset) -> Result<()> {
        let conn = self.conn.lock().await;
        let state_json =
            serde_json::to_string(&asset.state).map_err(|e| Error::Storage(e.to_string()))?;
        let skeleton_json = asset
            .skeleton
            .as_ref()
            .map(|s| serde_json::to_string(s))
            .transpose()
            .map_err(|e| Error::Storage(e.to_string()))?;
        let ingested_ts = asset.ingested_at.timestamp();
        let doc_type = serde_json::to_string(&asset.document_type)
            .map_err(|e| Error::Storage(e.to_string()))?;

        conn.execute(
            "INSERT OR REPLACE INTO document_assets
             (id, title, filename, file_size_mb, word_count, chunk_count,
              document_type, ingested_at, index_id, state_json, skeleton_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                asset.id,
                asset.title,
                asset.filename,
                asset.file_size_mb,
                asset.word_count as i64,
                asset.chunk_count as i64,
                doc_type,
                ingested_ts,
                asset.index_id,
                state_json,
                skeleton_json,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn update_asset_state(&self, id: &str, state: &AssetState) -> Result<()> {
        let conn = self.conn.lock().await;
        let state_json =
            serde_json::to_string(state).map_err(|e| Error::Storage(e.to_string()))?;
        conn.execute(
            "UPDATE document_assets SET state_json = ?1 WHERE id = ?2",
            rusqlite::params![state_json, id],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn save_asset_skeleton(
        &self,
        id: &str,
        skeleton: &DocumentSkeleton,
        document_type: &DocumentTypeTag,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let skeleton_json =
            serde_json::to_string(skeleton).map_err(|e| Error::Storage(e.to_string()))?;
        let doc_type_json =
            serde_json::to_string(document_type).map_err(|e| Error::Storage(e.to_string()))?;
        conn.execute(
            "UPDATE document_assets SET skeleton_json = ?1, document_type = ?2 WHERE id = ?3",
            rusqlite::params![skeleton_json, doc_type_json, id],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn get_document_asset(&self, id: &str) -> Result<Option<DocumentAsset>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, title, filename, file_size_mb, word_count, chunk_count,
                    document_type, ingested_at, index_id, state_json, skeleton_json
             FROM document_assets WHERE id = ?1",
            rusqlite::params![id],
            |row| Ok(row_to_document_asset(row)),
        )
        .optional()
        .map_err(map_db)
    }

    async fn list_document_assets(&self) -> Result<Vec<DocumentAsset>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, title, filename, file_size_mb, word_count, chunk_count,
                        document_type, ingested_at, index_id, state_json, skeleton_json
                 FROM document_assets
                 ORDER BY ingested_at DESC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map([], |row| Ok(row_to_document_asset(row)))
            .map_err(map_db)?;
        let mut assets = Vec::new();
        for row in rows {
            assets.push(row.map_err(map_db)?);
        }
        Ok(assets)
    }

    async fn delete_document_asset(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        // Cascade: document_conversations has ON DELETE CASCADE.
        // document_operations doesn't — clean up explicitly.
        conn.execute(
            "DELETE FROM document_operations WHERE asset_id = ?1",
            rusqlite::params![id],
        )
        .map_err(map_db)?;
        conn.execute(
            "DELETE FROM document_assets WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn save_document_operation(
        &self,
        message_id: &str,
        asset_id: &str,
        operation: &DocumentAssetOperation,
        duration_ms: u64,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let operation_json =
            serde_json::to_string(operation).map_err(|e| Error::Storage(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO document_operations
             (message_id, asset_id, operation_json, duration_ms)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![message_id, asset_id, operation_json, duration_ms as i64],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn save_raptor_nodes(
        &self,
        asset_id: &str,
        nodes: &[RaptorNode],
    ) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        tx.execute(
            "DELETE FROM raptor_nodes WHERE asset_id = ?1",
            rusqlite::params![asset_id],
        )
        .map_err(map_db)?;
        for node in nodes {
            let children = serde_json::to_string(&node.children_node_ids)
                .map_err(|e| Error::Storage(e.to_string()))?;
            let direct_members: Option<String> = if node.direct_member_chunk_ids.is_empty() {
                None
            } else {
                Some(
                    serde_json::to_string(&node.direct_member_chunk_ids)
                        .map_err(|e| Error::Storage(e.to_string()))?,
                )
            };
            let evidence = serde_json::to_string(&node.evidence_chunk_ids)
                .map_err(|e| Error::Storage(e.to_string()))?;
            let quotes = serde_json::to_string(&node.quote_spans)
                .map_err(|e| Error::Storage(e.to_string()))?;
            let entities = serde_json::to_string(&node.primary_entities)
                .map_err(|e| Error::Storage(e.to_string()))?;
            tx.execute(
                "INSERT INTO raptor_nodes
                 (node_id, asset_id, level, summary,
                  summary_embedding, centroid_embedding,
                  children_node_ids, direct_member_chunk_ids,
                  evidence_chunk_ids, quote_spans, primary_entities,
                  cluster_coherence, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                rusqlite::params![
                    node.node_id,
                    asset_id,
                    node.level as i64,
                    node.summary,
                    encode_f32_vec(&node.summary_embedding),
                    encode_f32_vec(&node.centroid_embedding),
                    children,
                    direct_members,
                    evidence,
                    quotes,
                    entities,
                    node.cluster_coherence as f64,
                    node.created_at.timestamp(),
                ],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    async fn list_raptor_nodes(&self, asset_id: &str) -> Result<Vec<RaptorNode>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT node_id, level, summary, summary_embedding, centroid_embedding,
                        children_node_ids, direct_member_chunk_ids, evidence_chunk_ids,
                        quote_spans, primary_entities, cluster_coherence, created_at
                 FROM raptor_nodes
                 WHERE asset_id = ?1
                 ORDER BY level ASC, node_id ASC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![asset_id], |row| {
                Ok(row_to_raptor_node(row))
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)??);
        }
        Ok(out)
    }

    async fn get_raptor_node(&self, node_id: &str) -> Result<Option<RaptorNode>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT node_id, level, summary, summary_embedding, centroid_embedding,
                        children_node_ids, direct_member_chunk_ids, evidence_chunk_ids,
                        quote_spans, primary_entities, cluster_coherence, created_at
                 FROM raptor_nodes
                 WHERE node_id = ?1",
            )
            .map_err(map_db)?;
        let mut rows = stmt
            .query_map(rusqlite::params![node_id], |row| {
                Ok(row_to_raptor_node(row))
            })
            .map_err(map_db)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(map_db)??)),
            None => Ok(None),
        }
    }

    async fn save_asset_motifs(
        &self,
        asset_id: &str,
        motifs: &[AssetMotif],
    ) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        tx.execute(
            "DELETE FROM asset_motifs WHERE asset_id = ?1",
            rusqlite::params![asset_id],
        )
        .map_err(map_db)?;
        for motif in motifs {
            let occurrences = serde_json::to_string(&motif.occurrence_chunk_ids)
                .map_err(|e| Error::Storage(e.to_string()))?;
            tx.execute(
                "INSERT INTO asset_motifs
                 (asset_id, term, tf_idf_score, occurrence_chunk_ids, is_distinctive)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    asset_id,
                    motif.term,
                    motif.tf_idf_score as f64,
                    occurrences,
                    if motif.is_distinctive { 1 } else { 0 },
                ],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    async fn list_asset_motifs(&self, asset_id: &str) -> Result<Vec<AssetMotif>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT term, tf_idf_score, occurrence_chunk_ids, is_distinctive
                 FROM asset_motifs
                 WHERE asset_id = ?1
                 ORDER BY is_distinctive DESC, tf_idf_score DESC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![asset_id], |row| {
                let term: String = row.get(0)?;
                let tf_idf_score: f64 = row.get(1)?;
                let occurrences: String = row.get(2)?;
                let is_distinctive: i64 = row.get(3)?;
                Ok((term, tf_idf_score, occurrences, is_distinctive))
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            let (term, score, occurrences_json, is_distinctive) = row.map_err(map_db)?;
            let occurrence_chunk_ids: Vec<u32> =
                serde_json::from_str(&occurrences_json).map_err(|e| Error::Storage(e.to_string()))?;
            out.push(AssetMotif {
                term,
                tf_idf_score: score as f32,
                occurrence_chunk_ids,
                is_distinctive: is_distinctive != 0,
            });
        }
        Ok(out)
    }
}

/// Encode a vector of f32s as little-endian bytes for BLOB storage.
fn encode_f32_vec(v: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(v.len() * 4);
    for x in v {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    bytes
}

/// Decode a little-endian f32 BLOB into a vector. Trailing bytes that
/// don't form a complete f32 are silently dropped — embeddings are
/// fixed-width per model so this only triggers on a corrupted row.
fn decode_f32_vec(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn row_to_raptor_node(row: &rusqlite::Row) -> Result<RaptorNode> {
    let node_id: String = row.get(0).map_err(map_db)?;
    let level: i64 = row.get(1).map_err(map_db)?;
    let summary: String = row.get(2).map_err(map_db)?;
    let summary_embedding_blob: Vec<u8> = row.get(3).map_err(map_db)?;
    let centroid_embedding_blob: Vec<u8> = row.get(4).map_err(map_db)?;
    let children_json: String = row.get(5).map_err(map_db)?;
    let direct_members_json: Option<String> = row.get(6).map_err(map_db)?;
    let evidence_json: String = row.get(7).map_err(map_db)?;
    let quotes_json: String = row.get(8).map_err(map_db)?;
    let entities_json: String = row.get(9).map_err(map_db)?;
    let cluster_coherence: f64 = row.get(10).map_err(map_db)?;
    let created_at_unix: i64 = row.get(11).map_err(map_db)?;

    let children_node_ids: Vec<String> = serde_json::from_str(&children_json)
        .map_err(|e| Error::Storage(format!("raptor_nodes.children_node_ids: {e}")))?;
    let direct_member_chunk_ids: Vec<u32> = match direct_members_json {
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| Error::Storage(format!("raptor_nodes.direct_member_chunk_ids: {e}")))?,
        None => Vec::new(),
    };
    let evidence_chunk_ids: Vec<u32> = serde_json::from_str(&evidence_json)
        .map_err(|e| Error::Storage(format!("raptor_nodes.evidence_chunk_ids: {e}")))?;
    let quote_spans: Vec<QuoteSpan> = serde_json::from_str(&quotes_json)
        .map_err(|e| Error::Storage(format!("raptor_nodes.quote_spans: {e}")))?;
    let primary_entities: Vec<String> = serde_json::from_str(&entities_json)
        .map_err(|e| Error::Storage(format!("raptor_nodes.primary_entities: {e}")))?;

    let created_at = chrono::DateTime::<chrono::Utc>::from_timestamp(created_at_unix, 0)
        .unwrap_or_else(chrono::Utc::now);

    Ok(RaptorNode {
        node_id,
        level: level as u8,
        summary,
        summary_embedding: decode_f32_vec(&summary_embedding_blob),
        centroid_embedding: decode_f32_vec(&centroid_embedding_blob),
        children_node_ids,
        direct_member_chunk_ids,
        evidence_chunk_ids,
        quote_spans,
        primary_entities,
        cluster_coherence: cluster_coherence as f32,
        created_at,
    })
}

fn row_to_document_asset(row: &rusqlite::Row) -> DocumentAsset {
    let state_json: String = row.get(9).unwrap_or_else(|_| r#""Pending""#.to_string());
    let skeleton_json: Option<String> = row.get(10).ok().flatten();
    let doc_type_str: String = row.get(6).unwrap_or_else(|_| r#""Unknown""#.to_string());
    let ingested_ts: i64 = row.get(7).unwrap_or(0);

    DocumentAsset {
        id: row.get(0).unwrap_or_default(),
        title: row.get(1).unwrap_or_default(),
        filename: row.get(2).unwrap_or_default(),
        file_size_mb: row.get(3).unwrap_or(0.0),
        word_count: row.get::<_, i64>(4).unwrap_or(0) as usize,
        chunk_count: row.get::<_, i64>(5).unwrap_or(0) as usize,
        document_type: serde_json::from_str(&doc_type_str).unwrap_or(DocumentTypeTag::Unknown),
        ingested_at: chrono::DateTime::from_timestamp(ingested_ts, 0)
            .unwrap_or_else(|| chrono::Utc::now()),
        index_id: row.get(8).unwrap_or_default(),
        skeleton: skeleton_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok()),
        state: serde_json::from_str(&state_json).unwrap_or(AssetState::Pending),
    }
}

fn row_to_document_session(row: &rusqlite::Row) -> DocumentSession {
    let history_json: String = row.get(11).unwrap_or_else(|_| "[]".to_string());
    let history: Vec<DocumentOperation> =
        serde_json::from_str(&history_json).unwrap_or_default();
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

// ── Conversation tiered-retrieval port (spec CONV_TIERED_PORT.md) ──
//
// Row structs + reader trait live in `sovereign-core::conv_tiered` to
// avoid a sovereign-core → sovereign-store cycle (sovereign-store
// already depends on sovereign-core for `StateStore`/`Error`). Impl
// of the reader trait on `SqliteStateStore` lives below; the row
// re-exports below let existing call sites keep their import paths.

pub use sovereign_core::conv_tiered::{
    ConvMotifRow, ConvRaptorNodeRow, ConvSkeletonRow, ConvTieredReader, ConvTieredState,
};

#[async_trait::async_trait]
impl ConvTieredReader for SqliteStateStore {
    async fn list_conv_skeletons_for_corpus(
        &self,
        corpus_id: &str,
        conv_uuids: &[String],
    ) -> sovereign_core::error::Result<Vec<ConvSkeletonRow>> {
        SqliteStateStore::list_conv_skeletons_for_corpus(self, corpus_id, conv_uuids).await
    }

    async fn list_conv_raptor_nodes(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> sovereign_core::error::Result<Vec<ConvRaptorNodeRow>> {
        SqliteStateStore::list_conv_raptor_nodes(self, corpus_id, conv_uuid).await
    }

    async fn list_chunk_entities_for_conv(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> sovereign_core::error::Result<Vec<sovereign_core::conv_tiered::ChunkEntityRow>> {
        SqliteStateStore::list_chunk_entities_for_conv(self, corpus_id, conv_uuid).await
    }

    async fn get_chunk_entity_progress(
        &self,
        corpus_id: &str,
    ) -> sovereign_core::error::Result<
        Option<sovereign_core::conv_tiered::ChunkEntityProgressRow>,
    > {
        SqliteStateStore::get_chunk_entity_progress(self, corpus_id).await
    }

    async fn list_vault_themes_for_corpus(
        &self,
        corpus_id: &str,
    ) -> sovereign_core::error::Result<
        Vec<sovereign_core::conv_tiered::VaultThemeRow>,
    > {
        SqliteStateStore::list_vault_themes_for_corpus(self, corpus_id).await
    }
}

//
// Persistence surface for the per-conversation T2/T3 enrichment
// output. The `TieredEnrichmentProvider` impl in `sovereign-tools`
// holds an `Arc<SqliteStateStore>` and writes through these methods;
// corpus-engine never touches the store directly (no dep on
// sovereign-store).

impl SqliteStateStore {
    /// Upsert the per-conv skeleton row. `state` is one of
    /// `ConvTieredState::as_str()`; future-proofed to bare string so
    /// the provider can write a custom error sub-state without a
    /// schema change.
    pub async fn save_conv_skeleton(&self, row: &ConvSkeletonRow) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO conv_skeletons
                (corpus_id, conv_uuid, state, skeleton_json, overview,
                 segments_json, chunk_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(corpus_id, conv_uuid) DO UPDATE SET
                state         = excluded.state,
                skeleton_json = excluded.skeleton_json,
                overview      = excluded.overview,
                segments_json = excluded.segments_json,
                chunk_count   = excluded.chunk_count,
                updated_at    = excluded.updated_at",
            rusqlite::params![
                row.corpus_id,
                row.conv_uuid,
                row.state,
                row.skeleton_json,
                row.overview,
                row.segments_json,
                row.chunk_count,
                row.updated_at,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    /// Read the state row for one conversation. Returns `None` if the
    /// tiered pass has never run for `(corpus_id, conv_uuid)`.
    pub async fn get_conv_skeleton(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> Result<Option<ConvSkeletonRow>> {
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT corpus_id, conv_uuid, state, skeleton_json, overview,
                        segments_json, chunk_count, updated_at
                 FROM conv_skeletons
                 WHERE corpus_id = ?1 AND conv_uuid = ?2",
                rusqlite::params![corpus_id, conv_uuid],
                |r| {
                    Ok(ConvSkeletonRow {
                        corpus_id: r.get(0)?,
                        conv_uuid: r.get(1)?,
                        state: r.get(2)?,
                        skeleton_json: r.get(3)?,
                        overview: r.get(4)?,
                        segments_json: r.get(5)?,
                        chunk_count: r.get(6)?,
                        updated_at: r.get(7)?,
                    })
                },
            )
            .ok();
        Ok(row)
    }

    /// Replace the RAPTOR node set for one conversation. Atomic
    /// delete + insert in one transaction — mirrors the attached-doc
    /// `save_raptor_nodes` semantics so a partial provider crash
    /// doesn't leave a half-built tree on disk.
    pub async fn save_conv_raptor_nodes(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        nodes: &[ConvRaptorNodeRow],
    ) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        tx.execute(
            "DELETE FROM conv_raptor_nodes WHERE corpus_id = ?1 AND conv_uuid = ?2",
            rusqlite::params![corpus_id, conv_uuid],
        )
        .map_err(map_db)?;
        for node in nodes {
            tx.execute(
                "INSERT INTO conv_raptor_nodes
                    (node_id, corpus_id, conv_uuid, level, summary,
                     summary_embedding, centroid_embedding,
                     children_node_ids, direct_member_chunk_ids,
                     evidence_chunk_ids, quote_spans, primary_entities,
                     cluster_coherence, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                rusqlite::params![
                    node.node_id,
                    node.corpus_id,
                    node.conv_uuid,
                    node.level,
                    node.summary,
                    encode_f32_vec(&node.summary_embedding),
                    encode_f32_vec(&node.centroid_embedding),
                    node.children_node_ids_json,
                    node.direct_member_chunk_ids_json,
                    node.evidence_chunk_ids_json,
                    node.quote_spans_json,
                    node.primary_entities_json,
                    node.cluster_coherence,
                    node.created_at,
                ],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    /// Replace the motif set for one conversation. Same atomicity
    /// rationale as `save_conv_raptor_nodes`.
    pub async fn save_conv_motifs(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        motifs: &[ConvMotifRow],
    ) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        tx.execute(
            "DELETE FROM conv_motifs WHERE corpus_id = ?1 AND conv_uuid = ?2",
            rusqlite::params![corpus_id, conv_uuid],
        )
        .map_err(map_db)?;
        for motif in motifs {
            tx.execute(
                "INSERT INTO conv_motifs
                    (corpus_id, conv_uuid, term, tf_idf_score,
                     occurrence_chunk_ids, is_distinctive)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    motif.corpus_id,
                    motif.conv_uuid,
                    motif.term,
                    motif.tf_idf_score,
                    motif.occurrence_chunk_ids_json,
                    if motif.is_distinctive { 1i64 } else { 0i64 },
                ],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    /// Read every RAPTOR node for one conversation, ordered by level
    /// descending then by `node_id` so the briefing layer sees root
    /// summaries first (the top-of-tree paraphrase that anchors
    /// reading order) and leaf clusters last. Level-0 leaves carry
    /// `direct_member_chunk_ids`; higher levels carry only
    /// `evidence_chunk_ids` (the transitive subtree union).
    pub async fn list_conv_raptor_nodes(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> Result<Vec<ConvRaptorNodeRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT node_id, corpus_id, conv_uuid, level, summary,
                        summary_embedding, centroid_embedding,
                        children_node_ids, direct_member_chunk_ids,
                        evidence_chunk_ids, quote_spans, primary_entities,
                        cluster_coherence, created_at
                 FROM conv_raptor_nodes
                 WHERE corpus_id = ?1 AND conv_uuid = ?2
                 ORDER BY level DESC, node_id ASC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![corpus_id, conv_uuid], |r| {
                Ok(ConvRaptorNodeRow {
                    node_id: r.get(0)?,
                    corpus_id: r.get(1)?,
                    conv_uuid: r.get(2)?,
                    level: r.get(3)?,
                    summary: r.get(4)?,
                    summary_embedding: decode_f32_vec(r.get::<_, Vec<u8>>(5)?.as_slice()),
                    centroid_embedding: decode_f32_vec(r.get::<_, Vec<u8>>(6)?.as_slice()),
                    children_node_ids_json: r.get(7)?,
                    direct_member_chunk_ids_json: r.get(8)?,
                    evidence_chunk_ids_json: r.get(9)?,
                    quote_spans_json: r.get(10)?,
                    primary_entities_json: r.get(11)?,
                    cluster_coherence: r.get(12)?,
                    created_at: r.get(13)?,
                })
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)?);
        }
        Ok(out)
    }

    /// Wipe every RAPTOR node for a single source_doc inside a
    /// corpus, without touching the rest of the vault. Used by the
    /// incremental sweeper: when a note's chunk set changes, the
    /// caller wants to drop the stale RAPTOR before
    /// `save_conv_raptor_nodes` re-writes it. Returns the number of
    /// rows actually deleted so the caller can log a skipped-doc
    /// short-circuit.
    pub async fn delete_conv_raptor_nodes_for_source(
        &self,
        corpus_id: &str,
        source_doc_id: &str,
    ) -> Result<usize> {
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM conv_raptor_nodes
                 WHERE corpus_id = ?1 AND conv_uuid = ?2",
                rusqlite::params![corpus_id, source_doc_id],
            )
            .map_err(map_db)?;
        Ok(deleted)
    }

    /// All `conv_uuid`s for a corpus whose `conv_skeletons.state` is
    /// `'Ready'`. Used by the vault-wide synthesis pass to enumerate
    /// the per-note RAPTOR trees that should feed the cross-note
    /// theme clustering. Returns deterministically-ordered uuids
    /// (`ORDER BY conv_uuid ASC`) so the synthesis input is stable
    /// across re-runs.
    pub async fn list_ready_source_doc_ids_for_corpus(
        &self,
        corpus_id: &str,
    ) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT conv_uuid
                 FROM conv_skeletons
                 WHERE corpus_id = ?1 AND state = 'Ready'
                 ORDER BY conv_uuid ASC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![corpus_id], |r| r.get::<_, String>(0))
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)?);
        }
        Ok(out)
    }

    /// Atomically replace the vault-wide synthesis themes for one
    /// corpus. Like `save_conv_raptor_nodes`, the entire prior theme
    /// set is deleted in the same transaction so a re-synthesis pass
    /// observably swaps the briefing's "Vault themes" block as one
    /// commit — never partial.
    pub async fn save_vault_themes(
        &self,
        corpus_id: &str,
        themes: &[sovereign_core::conv_tiered::VaultThemeRow],
    ) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        tx.execute(
            "DELETE FROM vault_themes WHERE corpus_id = ?1",
            rusqlite::params![corpus_id],
        )
        .map_err(map_db)?;
        for theme in themes {
            tx.execute(
                "INSERT INTO vault_themes
                    (corpus_id, theme_id, summary, summary_embedding,
                     member_source_doc_ids_json, cluster_coherence,
                     created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    theme.corpus_id,
                    theme.theme_id,
                    theme.summary,
                    encode_f32_vec(&theme.summary_embedding),
                    theme.member_source_doc_ids_json,
                    theme.cluster_coherence,
                    theme.created_at,
                ],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    /// All vault-wide synthesis themes for one corpus, ordered by
    /// `cluster_coherence DESC`. Empty when the synthesis pass has
    /// not run yet — caller (the briefing layer) treats empty as
    /// "no vault-wide block, fall through to per-note signposts".
    pub async fn list_vault_themes_for_corpus(
        &self,
        corpus_id: &str,
    ) -> Result<Vec<sovereign_core::conv_tiered::VaultThemeRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT corpus_id, theme_id, summary, summary_embedding,
                        member_source_doc_ids_json, cluster_coherence,
                        created_at
                 FROM vault_themes
                 WHERE corpus_id = ?1
                 ORDER BY cluster_coherence DESC, theme_id ASC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![corpus_id], |r| {
                Ok(sovereign_core::conv_tiered::VaultThemeRow {
                    corpus_id: r.get(0)?,
                    theme_id: r.get(1)?,
                    summary: r.get(2)?,
                    summary_embedding: decode_f32_vec(
                        r.get::<_, Vec<u8>>(3)?.as_slice(),
                    ),
                    member_source_doc_ids_json: r.get(4)?,
                    cluster_coherence: r.get(5)?,
                    created_at: r.get(6)?,
                })
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)?);
        }
        Ok(out)
    }

    /// Wipe every vault-wide theme for a corpus. Called by the
    /// disable-enrichment teardown path so the briefing stops
    /// referencing themes that no longer reflect the current vault
    /// state.
    pub async fn delete_vault_themes_for_corpus(
        &self,
        corpus_id: &str,
    ) -> Result<usize> {
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM vault_themes WHERE corpus_id = ?1",
                rusqlite::params![corpus_id],
            )
            .map_err(map_db)?;
        Ok(deleted)
    }

    /// Bulk read for `(state, overview, chunk_count)` triples across
    /// many conversations. Avoids a per-conv round-trip when the
    /// briefing builder is selecting which convs to surface. Drops
    /// conv_uuids that have no row (briefing layer treats those as
    /// "no tiered enrichment yet").
    pub async fn list_conv_skeletons_for_corpus(
        &self,
        corpus_id: &str,
        conv_uuids: &[String],
    ) -> Result<Vec<ConvSkeletonRow>> {
        if conv_uuids.is_empty() {
            return Ok(Vec::new());
        }
        // SQLite has a default 999 parameter limit; cap the IN-list
        // size to stay well under. Briefing layer caps at top-8 anyway,
        // so this floor is purely defensive.
        let max_in_list = conv_uuids.len().min(500);
        let placeholders: Vec<&str> = (0..max_in_list).map(|_| "?").collect();
        let mut placeholder_list = String::from("?,");
        placeholder_list.push_str(&placeholders.join(","));
        let sql = format!(
            "SELECT corpus_id, conv_uuid, state, skeleton_json, overview,
                    segments_json, chunk_count, updated_at
             FROM conv_skeletons
             WHERE corpus_id = ?1 AND conv_uuid IN ({})",
            placeholders.join(",")
        );
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql).map_err(map_db)?;
        let mut params: Vec<&dyn rusqlite::ToSql> =
            Vec::with_capacity(max_in_list + 1);
        params.push(&corpus_id);
        for uuid in conv_uuids.iter().take(max_in_list) {
            params.push(uuid);
        }
        let rows = stmt
            .query_map(params.as_slice(), |r| {
                Ok(ConvSkeletonRow {
                    corpus_id: r.get(0)?,
                    conv_uuid: r.get(1)?,
                    state: r.get(2)?,
                    skeleton_json: r.get(3)?,
                    overview: r.get(4)?,
                    segments_json: r.get(5)?,
                    chunk_count: r.get(6)?,
                    updated_at: r.get(7)?,
                })
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)?);
        }
        Ok(out)
    }

    /// Enumerate every conv corpus that has at least one row in
    /// `conv_skeletons`, together with per-state counts + max
    /// `updated_at`. Used by the desktop Atlas index
    /// (`atlas_list_conv_corpora`) to render the "Conversations"
    /// group alongside atoms.json-backed corpora.
    ///
    /// Returns one tuple per corpus_id: `(corpus_id, total,
    /// max_updated_at, per_state)`. Empty when no tiered enrichment
    /// has ever run.
    pub async fn list_conv_corpora_with_state_buckets(
        &self,
    ) -> Result<Vec<(String, u64, i64, Vec<(String, u64)>)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT corpus_id, state, COUNT(*) as n, MAX(updated_at) as max_ts
                 FROM conv_skeletons
                 GROUP BY corpus_id, state
                 ORDER BY corpus_id ASC, state ASC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .map_err(map_db)?;
        let mut by_corpus: std::collections::BTreeMap<String, (Vec<(String, u64)>, i64)> =
            std::collections::BTreeMap::new();
        for row in rows {
            let (corpus_id, state, n, ts) = row.map_err(map_db)?;
            let entry = by_corpus.entry(corpus_id).or_default();
            entry.0.push((state, n as u64));
            entry.1 = entry.1.max(ts);
        }
        Ok(by_corpus
            .into_iter()
            .map(|(corpus_id, (per_state, max_ts))| {
                let total: u64 = per_state.iter().map(|(_, n)| *n).sum();
                (corpus_id, total, max_ts, per_state)
            })
            .collect())
    }

    /// Paginated list of conversations in one corpus, optionally
    /// filtered by case-insensitive substring on `overview`. Returns
    /// the page slice + total matching count.
    pub async fn list_conversations_paginated(
        &self,
        corpus_id: &str,
        filter: Option<&str>,
        offset: u64,
        limit: u64,
    ) -> Result<(Vec<ConvSkeletonRow>, u64)> {
        let conn = self.conn.lock().await;
        let filter_clause = if filter.is_some() {
            "AND COALESCE(overview, '') LIKE ?2"
        } else {
            ""
        };
        // Total count first.
        let count_sql = format!(
            "SELECT COUNT(*) FROM conv_skeletons WHERE corpus_id = ?1 {filter_clause}"
        );
        let total: i64 = if let Some(f) = filter {
            let needle = format!("%{}%", f.replace('%', "\\%").replace('_', "\\_"));
            conn.query_row(&count_sql, rusqlite::params![corpus_id, needle], |r| r.get(0))
                .map_err(map_db)?
        } else {
            conn.query_row(&count_sql, rusqlite::params![corpus_id], |r| r.get(0))
                .map_err(map_db)?
        };

        // Page itself, ordered by updated_at DESC then conv_uuid for
        // stable pagination. SQLite supports OFFSET on indexed sorts,
        // but for very large corpora a keyset cursor (last-seen
        // updated_at, conv_uuid) would be preferable; deferred until
        // anyone hits a 100k+ conv corpus.
        let page_sql = format!(
            "SELECT corpus_id, conv_uuid, state, skeleton_json, overview,
                    segments_json, chunk_count, updated_at
             FROM conv_skeletons
             WHERE corpus_id = ?1 {filter_clause}
             ORDER BY updated_at DESC, conv_uuid ASC
             LIMIT ?{} OFFSET ?{}",
            if filter.is_some() { 3 } else { 2 },
            if filter.is_some() { 4 } else { 3 },
        );
        let mut stmt = conn.prepare(&page_sql).map_err(map_db)?;
        let map_row = |r: &rusqlite::Row<'_>| {
            Ok(ConvSkeletonRow {
                corpus_id: r.get(0)?,
                conv_uuid: r.get(1)?,
                state: r.get(2)?,
                skeleton_json: r.get(3)?,
                overview: r.get(4)?,
                segments_json: r.get(5)?,
                chunk_count: r.get(6)?,
                updated_at: r.get(7)?,
            })
        };
        let rows_result = if let Some(f) = filter {
            let needle = format!("%{}%", f.replace('%', "\\%").replace('_', "\\_"));
            stmt.query_map(
                rusqlite::params![corpus_id, needle, limit as i64, offset as i64],
                map_row,
            )
        } else {
            stmt.query_map(
                rusqlite::params![corpus_id, limit as i64, offset as i64],
                map_row,
            )
        }
        .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows_result {
            out.push(row.map_err(map_db)?);
        }
        Ok((out, total as u64))
    }

    /// Replace all chunk_entities rows for one conversation. Writes
    /// inside a transaction so concurrent reads see either the prior
    /// or the new set, never a half-applied state. `rows.len()` is
    /// also the natural progress increment for the batch CLI.
    pub async fn save_chunk_entities_for_conv(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        rows: &[sovereign_core::conv_tiered::ChunkEntityRow],
    ) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        tx.execute(
            "DELETE FROM chunk_entities WHERE corpus_id = ?1 AND conv_uuid = ?2",
            rusqlite::params![corpus_id, conv_uuid],
        )
        .map_err(map_db)?;
        for row in rows {
            tx.execute(
                "INSERT OR REPLACE INTO chunk_entities
                    (corpus_id, chunk_id, text, label, char_start,
                     char_end, score, conv_uuid, extracted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    row.corpus_id,
                    row.chunk_id as i64,
                    row.text,
                    row.label,
                    row.char_start,
                    row.char_end,
                    row.score,
                    row.conv_uuid,
                    row.extracted_at,
                ],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    /// Tear down all tiered-enrichment rows for one corpus. Used by
    /// `LocalCorpusManager::disable_enrichment` to clean up
    /// `conv_raptor_nodes` / `conv_motifs` / `conv_skeletons` /
    /// `chunk_entities` / `chunk_entity_progress` so re-enabling on
    /// the same corpus starts from a clean slate.
    ///
    /// One transaction so partial teardown isn't possible — either
    /// the corpus has tiered data or it doesn't.
    pub async fn delete_tiered_for_corpus(&self, corpus_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        tx.execute(
            "DELETE FROM conv_raptor_nodes WHERE corpus_id = ?1",
            rusqlite::params![corpus_id],
        )
        .map_err(map_db)?;
        tx.execute(
            "DELETE FROM conv_motifs WHERE corpus_id = ?1",
            rusqlite::params![corpus_id],
        )
        .map_err(map_db)?;
        tx.execute(
            "DELETE FROM conv_skeletons WHERE corpus_id = ?1",
            rusqlite::params![corpus_id],
        )
        .map_err(map_db)?;
        tx.execute(
            "DELETE FROM chunk_entities WHERE corpus_id = ?1",
            rusqlite::params![corpus_id],
        )
        .map_err(map_db)?;
        tx.execute(
            "DELETE FROM chunk_entity_progress WHERE corpus_id = ?1",
            rusqlite::params![corpus_id],
        )
        .map_err(map_db)?;
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    /// Bulk-write chunk_entities rows scoped by chunk_id (no conv
    /// grouping). Idempotent via PRIMARY KEY collision → REPLACE.
    /// Used by non-conv corpora that don't have a `conv_uuid` to
    /// group on.
    pub async fn save_chunk_entities(
        &self,
        rows: &[sovereign_core::conv_tiered::ChunkEntityRow],
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().map_err(map_db)?;
        for row in rows {
            tx.execute(
                "INSERT OR REPLACE INTO chunk_entities
                    (corpus_id, chunk_id, text, label, char_start,
                     char_end, score, conv_uuid, extracted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    row.corpus_id,
                    row.chunk_id as i64,
                    row.text,
                    row.label,
                    row.char_start,
                    row.char_end,
                    row.score,
                    row.conv_uuid,
                    row.extracted_at,
                ],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(())
    }

    /// Distinct `chunk_id` values already present in `chunk_entities`
    /// for a corpus, returned as a `HashSet` so the Phase B
    /// incremental hook can compute the delta against the current
    /// Lance chunk set in one membership-test pass. Empty set when no
    /// extraction has run yet for the corpus.
    pub async fn list_extracted_chunk_ids_for_corpus(
        &self,
        corpus_id: &str,
    ) -> Result<std::collections::HashSet<u64>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT chunk_id FROM chunk_entities WHERE corpus_id = ?1",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![corpus_id], |r| r.get::<_, i64>(0))
            .map_err(map_db)?;
        let mut out = std::collections::HashSet::new();
        for row in rows {
            out.insert(row.map_err(map_db)? as u64);
        }
        Ok(out)
    }

    /// Aggregate one entity's footprint inside a corpus. Drives the
    /// desktop's Atlas-view entity drawer. Match is case-insensitive
    /// on `text` (so "Borges" + "borges" + "BORGES" fold into one
    /// row); per-label breakdown surfaces homonyms ("Swift"
    /// Person vs "SWIFT" Organization) without merging.
    ///
    /// `co_limit` caps co-occurring entities; `conv_limit` caps the
    /// top-conv list. Pass small values (~20) — the drawer shows the
    /// head only; the full list is reserved for the "expand" tail.
    pub async fn aggregate_entity(
        &self,
        corpus_id: &str,
        text: &str,
        co_limit: usize,
        conv_limit: usize,
    ) -> Result<sovereign_core::conv_tiered::EntityAggregateRow> {
        use sovereign_core::conv_tiered::{
            CoOccurringEntity, EntityAggregateRow, EntityConvHit, EntityLabelCount,
        };
        let conn = self.conn.lock().await;

        // Canonical display form: pick the most-common surface-form
        // variant inside the corpus. Ties broken by alphabetical so
        // the answer is deterministic across re-queries.
        let canonical: String = conn
            .query_row(
                "SELECT text FROM chunk_entities
                 WHERE corpus_id = ?1 AND text = ?2 COLLATE NOCASE
                 GROUP BY text
                 ORDER BY COUNT(*) DESC, text ASC
                 LIMIT 1",
                rusqlite::params![corpus_id, text],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(map_db)?
            .unwrap_or_else(|| text.to_string());

        // Label breakdown.
        let mut labels_stmt = conn
            .prepare(
                "SELECT label, COUNT(*) AS n
                 FROM chunk_entities
                 WHERE corpus_id = ?1 AND text = ?2 COLLATE NOCASE
                 GROUP BY label
                 ORDER BY n DESC, label ASC",
            )
            .map_err(map_db)?;
        let labels: Vec<EntityLabelCount> = labels_stmt
            .query_map(rusqlite::params![corpus_id, text], |r| {
                Ok(EntityLabelCount {
                    label: r.get::<_, String>(0)?,
                    count: r.get::<_, i64>(1)?,
                })
            })
            .map_err(map_db)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_db)?;
        drop(labels_stmt);

        // Scalar counts in one round-trip.
        let (mention_count, conv_count, chunk_count): (i64, i64, i64) = conn
            .query_row(
                "SELECT
                    COUNT(*),
                    COUNT(DISTINCT conv_uuid),
                    COUNT(DISTINCT chunk_id)
                 FROM chunk_entities
                 WHERE corpus_id = ?1 AND text = ?2 COLLATE NOCASE",
                rusqlite::params![corpus_id, text],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(map_db)?;

        // Top convs by mention count. NULL conv_uuid rows (non-conv
        // corpora) are filtered out here so the drawer's "where it
        // appears" list never tries to link to a missing conv.
        let mut conv_stmt = conn
            .prepare(
                "SELECT conv_uuid, COUNT(*) AS n
                 FROM chunk_entities
                 WHERE corpus_id = ?1
                   AND text = ?2 COLLATE NOCASE
                   AND conv_uuid IS NOT NULL
                 GROUP BY conv_uuid
                 ORDER BY n DESC, conv_uuid ASC
                 LIMIT ?3",
            )
            .map_err(map_db)?;
        let top_convs: Vec<EntityConvHit> = conv_stmt
            .query_map(
                rusqlite::params![corpus_id, text, conv_limit as i64],
                |r| {
                    Ok(EntityConvHit {
                        conv_uuid: r.get::<_, String>(0)?,
                        mention_count: r.get::<_, i64>(1)?,
                    })
                },
            )
            .map_err(map_db)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_db)?;
        drop(conv_stmt);

        // Co-occurring entities: chunks that contain the seed
        // entity, with every OTHER entity in those chunks bucketed
        // by `(text, label)`. The self-join keys on chunk_id so
        // intra-chunk neighbours are counted, not inter-chunk
        // collisions.
        let mut co_stmt = conn
            .prepare(
                "SELECT other.text, other.label, COUNT(DISTINCT other.chunk_id) AS shared
                 FROM chunk_entities AS seed
                 JOIN chunk_entities AS other
                   ON other.corpus_id = seed.corpus_id
                  AND other.chunk_id = seed.chunk_id
                 WHERE seed.corpus_id = ?1
                   AND seed.text = ?2 COLLATE NOCASE
                   AND NOT (other.text = ?2 COLLATE NOCASE)
                 GROUP BY other.text, other.label
                 ORDER BY shared DESC, other.text ASC
                 LIMIT ?3",
            )
            .map_err(map_db)?;
        let co_occurring: Vec<CoOccurringEntity> = co_stmt
            .query_map(
                rusqlite::params![corpus_id, text, co_limit as i64],
                |r| {
                    Ok(CoOccurringEntity {
                        text: r.get::<_, String>(0)?,
                        label: r.get::<_, String>(1)?,
                        shared_chunk_count: r.get::<_, i64>(2)?,
                    })
                },
            )
            .map_err(map_db)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_db)?;

        Ok(EntityAggregateRow {
            corpus_id: corpus_id.to_string(),
            text: canonical,
            labels,
            mention_count,
            conv_count,
            chunk_count,
            top_convs,
            co_occurring,
        })
    }

    /// Read every `chunk_entities` row for one conversation.
    /// Returned in `(chunk_id ASC, char_start ASC)` order.
    pub async fn list_chunk_entities_for_conv(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> Result<Vec<sovereign_core::conv_tiered::ChunkEntityRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT corpus_id, chunk_id, text, label, char_start,
                        char_end, score, conv_uuid, extracted_at
                 FROM chunk_entities
                 WHERE corpus_id = ?1 AND conv_uuid = ?2
                 ORDER BY chunk_id ASC, char_start ASC",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![corpus_id, conv_uuid], |r| {
                Ok(sovereign_core::conv_tiered::ChunkEntityRow {
                    corpus_id: r.get(0)?,
                    chunk_id: r.get::<_, i64>(1)? as u64,
                    text: r.get(2)?,
                    label: r.get(3)?,
                    char_start: r.get(4)?,
                    char_end: r.get(5)?,
                    score: r.get(6)?,
                    conv_uuid: r.get(7)?,
                    extracted_at: r.get(8)?,
                })
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)?);
        }
        Ok(out)
    }

    /// Upsert per-corpus extraction progress. Drives the CLI's
    /// progress bar + the desktop's "entity extraction running"
    /// badge.
    pub async fn upsert_chunk_entity_progress(
        &self,
        row: &sovereign_core::conv_tiered::ChunkEntityProgressRow,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO chunk_entity_progress
                (corpus_id, chunks_processed, chunks_total,
                 mentions_extracted, last_chunk_id, started_at,
                 updated_at, finished_at, state, model_id, threshold,
                 labels_json, error_msg)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(corpus_id) DO UPDATE SET
                chunks_processed = excluded.chunks_processed,
                chunks_total = excluded.chunks_total,
                mentions_extracted = excluded.mentions_extracted,
                last_chunk_id = excluded.last_chunk_id,
                updated_at = excluded.updated_at,
                finished_at = excluded.finished_at,
                state = excluded.state,
                model_id = excluded.model_id,
                threshold = excluded.threshold,
                labels_json = excluded.labels_json,
                error_msg = excluded.error_msg",
            rusqlite::params![
                row.corpus_id,
                row.chunks_processed,
                row.chunks_total,
                row.mentions_extracted,
                row.last_chunk_id,
                row.started_at,
                row.updated_at,
                row.finished_at,
                row.state,
                row.model_id,
                row.threshold,
                row.labels_json,
                row.error_msg,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    pub async fn get_chunk_entity_progress(
        &self,
        corpus_id: &str,
    ) -> Result<Option<sovereign_core::conv_tiered::ChunkEntityProgressRow>> {
        let conn = self.conn.lock().await;
        Ok(conn
            .query_row(
                "SELECT corpus_id, chunks_processed, chunks_total,
                        mentions_extracted, last_chunk_id, started_at,
                        updated_at, finished_at, state, model_id,
                        threshold, labels_json, error_msg
                 FROM chunk_entity_progress
                 WHERE corpus_id = ?1",
                rusqlite::params![corpus_id],
                |r| {
                    Ok(sovereign_core::conv_tiered::ChunkEntityProgressRow {
                        corpus_id: r.get(0)?,
                        chunks_processed: r.get(1)?,
                        chunks_total: r.get(2)?,
                        mentions_extracted: r.get(3)?,
                        last_chunk_id: r.get(4)?,
                        started_at: r.get(5)?,
                        updated_at: r.get(6)?,
                        finished_at: r.get(7)?,
                        state: r.get(8)?,
                        model_id: r.get(9)?,
                        threshold: r.get(10)?,
                        labels_json: r.get(11)?,
                        error_msg: r.get(12)?,
                    })
                },
            )
            .ok())
    }

    /// Inventory of conv states for a corpus — used by ops tools to
    /// answer "how far has the tiered pass progressed across this
    /// import?". Returns `(state, count)` pairs.
    pub async fn count_conv_skeletons_by_state(
        &self,
        corpus_id: &str,
    ) -> Result<Vec<(String, i64)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT state, COUNT(*) FROM conv_skeletons
                 WHERE corpus_id = ?1 GROUP BY state ORDER BY state",
            )
            .map_err(map_db)?;
        let rows = stmt
            .query_map(rusqlite::params![corpus_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(map_db)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_db)?);
        }
        Ok(out)
    }
}
