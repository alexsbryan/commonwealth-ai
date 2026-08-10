// SPDX-License-Identifier: AGPL-3.0-or-later
use std::path::Path;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension};
use tokio::sync::Mutex;

use sovereign_core::error::{Error, Result};
use sovereign_core::observer::{noop_observer, SharedStateStoreObserver};
use sovereign_core::traits::{
    BudgetStore, ConversationStore, CorpusStateStore, DocumentAssetStore, DocumentSessionStore,
    DocumentStore, HealthStore, MemoryStore, PermissionStore, RoutingStore, StateStore,
    StepExecutionStore, TaskStore,
};
use sovereign_core::types::*;

use crate::migrations;

// One module per StateStore sub-trait concern (ARCH §3.1 split of the
// former single-file trait-impl hotel). Shared helpers — `now`,
// `map_db`/`map_json`, `sanitize_fts5_query`, the f32 BLOB codecs —
// stay in this parent module; children reach them via `use super::*`.
mod budget;
mod chat_activity;
mod conv_tiered;
mod conversation;
mod corpus_state;
mod document;
mod document_asset;
mod document_session;
mod health;
mod memory;
mod permission;
mod routing;
mod step_execution;
mod task;

use sovereign_core::time::unix_now as now;

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

        // Cross-process SQLITE_BUSY retry window: a CLI/tool process can
        // open this db while the daemon holds it — retry briefly instead
        // of erroring the caller outright (DAEMON_RESILIENCE.md P3.4).
        // In-process access is serialized by the store's own Mutex.
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));

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
        migrations::run_corpus_visibility_migration(&conn)
            .map_err(|e| Error::Storage(format!("Corpus visibility migration failed: {e}")))?;
        migrations::run_document_owner_migration(&conn)
            .map_err(|e| Error::Storage(format!("Document owner migration failed: {e}")))?;
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
        migrations::run_memory_embedding_migration(&conn)
            .map_err(|e| Error::Storage(format!("Memory embedding migration failed: {e}")))?;
        migrations::run_mem_raptor_migration(&conn)
            .map_err(|e| Error::Storage(format!("Memory raptor migration failed: {e}")))?;
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
        migrations::run_corpus_filter_migration(&conn)
            .map_err(|e| Error::Storage(format!("Corpus filter migration failed: {e}")))?;
        migrations::run_searched_sources_migration(&conn)
            .map_err(|e| Error::Storage(format!("Searched sources migration failed: {e}")))?;
        migrations::run_conversation_frame_migration(&conn)
            .map_err(|e| Error::Storage(format!("Conversation frame migration failed: {e}")))?;

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
            let enabled_corpora_json: Option<String> = row.get(5)?;
            let searched_sources_json: Option<String> = row.get(6)?;
            Ok(Conversation {
                id: row.get(0)?,
                title: row.get(1)?,
                messages: Vec::new(),
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
                version: 0,
                deleted_at: None,
                skill_id: row.get(4)?,
                enabled_corpora: enabled_corpora_json
                    .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok()),
                searched_sources: searched_sources_json.and_then(|s| {
                    serde_json::from_str::<Vec<sovereign_core::types::SearchedSourceEntry>>(&s).ok()
                }),
            })
        };
        let rows: Vec<Conversation> = match surface_skill_id {
            Some(id) => {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, title, created_at, updated_at, skill_id, enabled_corpora, searched_sources \
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
                        "SELECT id, title, created_at, updated_at, skill_id, enabled_corpora, searched_sources \
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

    /// List the conversations explicitly scoped to one corpus (notebook),
    /// newest first — a notebook's Ask-tab history. A conversation
    /// matches when its `enabled_corpora` allow-list *contains*
    /// `corpus_id`. "Everything" conversations (`enabled_corpora IS NULL`)
    /// are deliberately excluded, so a notebook only shows threads the
    /// user actually had while scoped to it. Default-chat surface only
    /// (`skill_id IS NULL`) — these are the conversations a notebook's
    /// Ask tab mints. Mirrors `list_conversations_for_surface`'s row map.
    pub async fn list_conversations_for_corpus(
        &self,
        corpus_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Conversation>> {
        let conn = self.conn.lock().await;
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<Conversation> {
            let enabled_corpora_json: Option<String> = row.get(5)?;
            let searched_sources_json: Option<String> = row.get(6)?;
            Ok(Conversation {
                id: row.get(0)?,
                title: row.get(1)?,
                messages: Vec::new(),
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
                version: 0,
                deleted_at: None,
                skill_id: row.get(4)?,
                enabled_corpora: enabled_corpora_json
                    .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok()),
                searched_sources: searched_sources_json.and_then(|s| {
                    serde_json::from_str::<Vec<sovereign_core::types::SearchedSourceEntry>>(&s).ok()
                }),
            })
        };
        // `enabled_corpora IS NOT NULL` guards `json_each` against a NULL
        // (and is exactly the "exclude everything-scoped" rule). `json_each`
        // ships with SQLite ≥ 3.38.
        let mut stmt = conn
            .prepare(
                "SELECT id, title, created_at, updated_at, skill_id, enabled_corpora, searched_sources \
                 FROM conversations \
                 WHERE skill_id IS NULL AND deleted_at IS NULL \
                   AND enabled_corpora IS NOT NULL \
                   AND EXISTS (SELECT 1 FROM json_each(conversations.enabled_corpora) WHERE value = ?1) \
                 ORDER BY updated_at DESC LIMIT ?2 OFFSET ?3",
            )
            .map_err(map_db)?;
        let iter = stmt
            .query_map(
                rusqlite::params![corpus_id, limit as i64, offset as i64],
                map_row,
            )
            .map_err(map_db)?;
        let collected: std::result::Result<Vec<_>, _> = iter.collect();
        collected.map_err(map_db)
    }

    /// Read the (was_redirected, redirect_to) fields for the most
    /// recent `routing_log` row matching `message_hash`. Returns
    /// `None` if no row is found. Exposed for PR4 integration
    /// tests + future calibration job introspection — the schema
    /// columns are otherwise private to the SQLite impl.
    pub async fn read_redirect_signal(&self, message_hash: &str) -> Option<(bool, Option<String>)> {
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
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .map_err(map_db)?;
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
        migrations::run_corpus_visibility_migration(&conn)
            .map_err(|e| Error::Storage(format!("Corpus visibility migration failed: {e}")))?;
        migrations::run_document_owner_migration(&conn)
            .map_err(|e| Error::Storage(format!("Document owner migration failed: {e}")))?;
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
        migrations::run_memory_embedding_migration(&conn)
            .map_err(|e| Error::Storage(format!("Memory embedding migration failed: {e}")))?;
        migrations::run_mem_raptor_migration(&conn)
            .map_err(|e| Error::Storage(format!("Memory raptor migration failed: {e}")))?;
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
        migrations::run_corpus_filter_migration(&conn)
            .map_err(|e| Error::Storage(format!("Corpus filter migration failed: {e}")))?;
        migrations::run_searched_sources_migration(&conn)
            .map_err(|e| Error::Storage(format!("Searched sources migration failed: {e}")))?;
        migrations::run_conversation_frame_migration(&conn)
            .map_err(|e| Error::Storage(format!("Conversation frame migration failed: {e}")))?;

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

impl StateStore for SqliteStateStore {}

/// Sanitize a natural language query into FTS5-safe keywords.
/// Strips punctuation, stopwords, and joins remaining terms with OR
/// for broader matching.
fn sanitize_fts5_query(query: &str) -> String {
    const STOPWORDS: &[&str] = &[
        "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
        "do", "does", "did", "will", "would", "could", "should", "may", "might", "shall", "can",
        "need", "dare", "ought", "to", "of", "in", "for", "on", "with", "at", "by", "from", "as",
        "into", "through", "during", "before", "after", "above", "below", "between", "out", "off",
        "over", "under", "again", "further", "then", "once", "here", "there", "when", "where",
        "why", "how", "all", "both", "each", "few", "more", "most", "other", "some", "such", "no",
        "nor", "not", "only", "own", "same", "so", "than", "too", "very", "just", "because", "but",
        "and", "or", "if", "while", "about", "what", "which", "who", "whom", "this", "that",
        "these", "those", "am", "it", "its", "he", "she", "his", "her", "they", "them", "their",
        "we", "us", "our", "you", "your", "i", "me", "my",
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

/// Encode a vector of f32s as little-endian bytes for BLOB storage.
pub(crate) fn encode_f32_vec(v: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(v.len() * 4);
    for x in v {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    bytes
}

/// Decode a little-endian f32 BLOB into a vector. Trailing bytes that
/// don't form a complete f32 are silently dropped — embeddings are
/// fixed-width per model so this only triggers on a corrupted row.
pub(crate) fn decode_f32_vec(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

// ── Conversation tiered-retrieval port (spec CONV_TIERED_PORT.md) ──
//
// Row structs + reader trait live in `sovereign-core::conv_tiered` to
// avoid a sovereign-core → sovereign-store cycle (sovereign-store
// already depends on sovereign-core for `StateStore`/`Error`). Impl
// of the reader trait on `SqliteStateStore` lives below; the row
// re-exports below let existing call sites keep their import paths.

pub use sovereign_core::conv_tiered::{
    ConvRaptorNodeRow, ConvSkeletonRow, ConvTieredReader, ConvTieredState, SummaryCorrectionRow,
};

/// Per-corpus chunk-retrieval rollup for the chat activity surface.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatCorpusUsage {
    pub origin: String,
    pub chunks: u64,
    /// True when these chunks came from a mesh peer (the provenance
    /// `SourceSummary.from_peer` was set).
    pub from_peer: bool,
}

/// Per-model turn + token rollup for the chat activity surface.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatModelUsage {
    pub model: String,
    pub turns: u64,
    pub tokens_generated: u64,
}

/// Read-side rollup of the user's own chat usage, derived entirely
/// from the `ResponseProvenance` already persisted under
/// `metadata["provenance"]` on each assistant message. There is no new
/// write path: chat runs in the in-process Runtime (it never crosses a
/// daemon HTTP boundary, so the daemon's Activity ledger can't see it),
/// but every turn already records tokens + retrieved sources, so the
/// summary is *derived* rather than separately recorded — the data is
/// durable because the messages are.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatActivitySummary {
    pub window_days: u32,
    pub turns: u64,
    pub tokens_generated: u64,
    pub chunks_retrieved: u64,
    pub by_corpus: Vec<ChatCorpusUsage>,
    pub by_model: Vec<ChatModelUsage>,
}
