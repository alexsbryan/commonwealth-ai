//! Working notes store — persistent, searchable scratchpad for agents.
//!
//! Notes survive across sessions and are retrieved by full-text search,
//! symbol name, file path, or kind filter. Unlike test/lint stores (which
//! are overwritten on every run), notes are only deleted explicitly via
//! [`NoteStore::delete_note`].
//!
//! ## Kinds
//!
//! - `"decision"` — architectural or implementation choices made
//! - `"attempt"` — approaches tried and abandoned (so future sessions don't repeat)
//! - `"invariant"` — constraints that must never be violated
//! - `"todo"` — follow-up work for a future session
//! - `"reflection"` — post-task structured feedback on tool quality
//!
//! ## Schema
//!
//! - **`notes`** — one row per note with JSON arrays for `symbols` and `files`.
//!   Three nullable columns (`tool_name`, `retired_at`, `retired_by`) support
//!   the reflection lifecycle: write → surface → fix → retire.
//! - **`notes_fts`** — FTS5 virtual table backed by `notes`, kept in sync by
//!   three triggers (after insert, before delete, after update).
//! - **`tool_call_log`** — ring buffer (10,000 rows) of MCP tool invocations.
//!   Records tool names and outcomes only — no parameters, no content.
//!
//! ## Threading
//!
//! `NoteStore` wraps a synchronous `rusqlite::Connection` in a
//! `tokio::sync::Mutex`. All operations are microsecond-fast; no
//! `spawn_blocking` is needed.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use tokio::sync::Mutex;

use crate::error::{Error, Result};

// ─── Types ────────────────────────────────────────────────────────────────────

/// Scope dimension for ATOS notes.
///
/// - `Global`: architectural invariants that outlive any one feature.
/// - `Feature`: decisions/attempts/invariants tied to a single feature id.
/// - `Session`: ephemeral scratch within one agent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteScope {
    Global,
    Feature,
    Session,
}

impl NoteScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Feature => "feature",
            Self::Session => "session",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "global" => Some(Self::Global),
            "feature" => Some(Self::Feature),
            "session" => Some(Self::Session),
            _ => None,
        }
    }
}

/// A single note row returned from [`NoteStore::read_notes`].
#[derive(Debug, Clone)]
pub struct NoteRow {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub symbols: Vec<String>,
    pub files: Vec<String>,
    pub session_id: String,
    /// RFC 3339 timestamp string.
    pub created_at: String,
    /// Primary tool this note concerns (reflections only; `None` for other kinds).
    pub tool_name: Option<String>,
    /// Unix timestamp when this note was retired; `None` means active.
    pub retired_at: Option<i64>,
    /// Human-readable reason for retirement (e.g. "fixed in PR #88").
    pub retired_by: Option<String>,
    /// Scope dimension: `"global"` | `"feature"` | `"session"`.
    pub scope: String,
    /// ATOS feature id when `scope == "feature"`. `None` otherwise.
    pub feature_id: Option<String>,
    /// Origin note id when this row was created by `promote_note`. `None` for
    /// native writes.
    pub promoted_from: Option<String>,
}

/// Retrieval filter for scope/feature combinations.
///
/// Use `ScopeFilter::default()` to preserve the legacy behavior of reading
/// all notes regardless of scope.
#[derive(Debug, Clone, Default)]
pub struct ScopeFilter {
    /// When non-empty, results are restricted to rows with `scope` in this list.
    pub scopes: Vec<NoteScope>,
    /// When `Some`, applies `feature_id = ?` as an additional predicate. Only
    /// meaningful when `scopes` includes `NoteScope::Feature`.
    pub feature_id: Option<String>,
}

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

// ─── Store ────────────────────────────────────────────────────────────────────

/// SQLite + FTS5 store for agent working notes.
pub struct NoteStore {
    conn: Arc<Mutex<Connection>>,
}

impl NoteStore {
    /// Open or create the database at `db_path`. Schema migrations are
    /// idempotent — safe to call on an existing database.
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let conn = Connection::open(db_path).map_err(|e| {
            Error::Io(std::io::Error::other(format!(
                "NoteStore::open {}: {e}",
                db_path.display()
            )))
        })?;

        // WAL mode is always desirable — idempotent, safe to repeat.
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| Error::Io(std::io::Error::other(format!("NoteStore WAL: {e}"))))?;

        // Determine whether this is a fresh DB or an existing one, then
        // run the appropriate setup (full schema vs. incremental migration).
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);

        if version == 0 {
            let table_exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='notes'",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);

            if table_exists > 0 {
                // Existing database on old schema — apply migration.
                conn.execute_batch(MIGRATION_V1).map_err(|e| {
                    Error::Io(std::io::Error::other(format!("NoteStore migrate v1: {e}")))
                })?;
            } else {
                // Brand-new database — create full schema and mark it current.
                conn.execute_batch(SCHEMA_NEW).map_err(|e| {
                    Error::Io(std::io::Error::other(format!("NoteStore schema: {e}")))
                })?;
            }
        }

        // Re-read after any v0→v1 work above, then apply v1→v2 if needed.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version < 2 {
            conn.execute_batch(MIGRATION_V2).map_err(|e| {
                Error::Io(std::io::Error::other(format!("NoteStore migrate v2: {e}")))
            })?;
        }

        // v2 → v3: expand the `kind` CHECK constraint to admit three
        // new ATOS note kinds. SQLite can't alter a CHECK in-place, so
        // we follow MIGRATION_V1's rename-recreate-copy pattern. The
        // FTS5 virtual table and triggers must also be rebuilt because
        // they reference the `notes` table by name.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version < 3 {
            conn.execute_batch(MIGRATION_V3).map_err(|e| {
                Error::Io(std::io::Error::other(format!("NoteStore migrate v3: {e}")))
            })?;
        }

        // v3 → v4: ATOS M4 adds the `deviation` kind for automatic
        // spec-drift notes written by the approval-gate middleware.
        // Same rename-recreate pattern as V3 — SQLite cannot ALTER a
        // CHECK constraint in place.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version < 4 {
            conn.execute_batch(MIGRATION_V4).map_err(|e| {
                Error::Io(std::io::Error::other(format!("NoteStore migrate v4: {e}")))
            })?;
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    // ── Note writes ────────────────────────────────────────────────────────

    /// Persist a new note at global scope. Back-compat wrapper over
    /// [`write_note_scoped`]; new call sites should prefer the scoped API.
    pub async fn write_note(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
    ) -> Result<String> {
        self.write_note_scoped(
            kind,
            content,
            symbols,
            files,
            session_id,
            NoteScope::Global,
            None,
        )
        .await
    }

    /// Persist a new note with an explicit scope. Returns the generated id.
    ///
    /// `kind` must be one of `"decision"`, `"attempt"`, `"invariant"`, `"todo"`.
    /// Use [`write_reflection_scoped`] for `kind = "reflection"`.
    ///
    /// Invariant: `scope == Feature` requires `feature_id.is_some()`; violators
    /// return [`Error::InvalidInput`].
    pub async fn write_note_scoped(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
    ) -> Result<String> {
        if scope == NoteScope::Feature && feature_id.is_none() {
            return Err(Error::InvalidInput(
                "write_note_scoped: scope='feature' requires feature_id".into(),
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = unix_now();
        let symbols_json = serde_json::to_string(&symbols).unwrap_or_else(|_| "[]".to_string());
        let files_json = serde_json::to_string(&files).unwrap_or_else(|_| "[]".to_string());

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO notes (id, kind, content, symbols, files, session_id, created_at, updated_at, scope, feature_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9)",
            params![id, kind, content, symbols_json, files_json, session_id, now, scope.as_str(), feature_id],
        )
        .map_err(sqlite_err)?;
        bump_notes_version(&conn)?;

        Ok(id)
    }

    /// Persist a reflection note at global scope. Back-compat wrapper.
    pub async fn write_reflection(
        &self,
        content: &str,
        tool_name: Option<&str>,
        session_id: &str,
    ) -> Result<String> {
        self.write_reflection_scoped(content, tool_name, session_id, NoteScope::Global, None)
            .await
    }

    /// Persist a reflection note with an explicit scope.
    pub async fn write_reflection_scoped(
        &self,
        content: &str,
        tool_name: Option<&str>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
    ) -> Result<String> {
        if scope == NoteScope::Feature && feature_id.is_none() {
            return Err(Error::InvalidInput(
                "write_reflection_scoped: scope='feature' requires feature_id".into(),
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = unix_now();

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO notes (id, kind, content, symbols, files, session_id, created_at, updated_at, tool_name, scope, feature_id)
             VALUES (?1, 'reflection', ?2, '[]', '[]', ?3, ?4, ?4, ?5, ?6, ?7)",
            params![id, content, session_id, now, tool_name, scope.as_str(), feature_id],
        )
        .map_err(sqlite_err)?;
        bump_notes_version(&conn)?;

        Ok(id)
    }

    /// Rewrite `id`'s scope (and optional `feature_id`) to match a promotion.
    ///
    /// Returns the newly inserted promoted note id (a fresh row is created;
    /// the source row is left intact for audit). The new row carries
    /// `promoted_from = <source id>`.
    pub async fn promote_note(
        &self,
        source_id: &str,
        new_scope: NoteScope,
        new_feature_id: Option<&str>,
        new_content: Option<&str>,
    ) -> Result<String> {
        if new_scope == NoteScope::Feature && new_feature_id.is_none() {
            return Err(Error::InvalidInput(
                "promote_note: scope='feature' requires feature_id".into(),
            ));
        }

        let new_id = uuid::Uuid::new_v4().to_string();
        let now = unix_now();

        let conn = self.conn.lock().await;
        let (kind, content, symbols, files, session_id, tool_name): (
            String,
            String,
            String,
            String,
            String,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT kind, content, symbols, files, session_id, tool_name
                 FROM notes WHERE id = ?",
                params![source_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Error::InvalidInput(format!(
                    "promote_note: source id not found: {source_id}"
                )),
                other => sqlite_err(other),
            })?;

        let final_content = new_content.unwrap_or(&content);
        conn.execute(
            "INSERT INTO notes (id, kind, content, symbols, files, session_id, created_at, updated_at, tool_name, scope, feature_id, promoted_from)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9, ?10, ?11)",
            params![
                new_id,
                kind,
                final_content,
                symbols,
                files,
                session_id,
                now,
                tool_name,
                new_scope.as_str(),
                new_feature_id,
                source_id,
            ],
        )
        .map_err(sqlite_err)?;
        bump_notes_version(&conn)?;

        Ok(new_id)
    }

    /// Look up a single note by id, or return `None` when not found.
    ///
    /// Used by compaction-recovery paths: a digest references notes by id
    /// (`[note:abc-123]`), and the agent calls this to fetch the full row
    /// only for those it needs.
    pub async fn read_note_by_id(&self, id: &str) -> Result<Option<NoteRow>> {
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT id, kind, content, symbols, files, session_id,
                        created_at, tool_name, retired_at, retired_by,
                        scope, feature_id, promoted_from
                 FROM notes WHERE id = ?",
                params![id],
                map_note_row,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(sqlite_err(other)),
            })?;
        Ok(row)
    }

    /// Returns the current monotonic counter that increments on every note
    /// write / delete / retire. Used by the digest cache in M1.4 to key
    /// cached digests without invalidating on every call.
    pub async fn notes_version(&self) -> Result<i64> {
        let conn = self.conn.lock().await;
        let v: i64 = conn
            .query_row(
                "SELECT val FROM meta_counters WHERE key = 'notes_version'",
                [],
                |r| r.get(0),
            )
            .map_err(sqlite_err)?;
        Ok(v)
    }

    /// Look up a cached digest by `(scope_hash, notes_version)`.
    ///
    /// Returns `None` if no matching row exists — the caller
    /// (`ReadNoteDigestTool`) should regenerate via the Fast slot and
    /// write back with [`digest_cache_put`](Self::digest_cache_put).
    pub async fn digest_cache_get(
        &self,
        scope_hash: &str,
        notes_version: i64,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        let row: rusqlite::Result<String> = conn.query_row(
            "SELECT digest_md FROM note_digest_cache
             WHERE scope_hash = ?1 AND notes_version = ?2",
            params![scope_hash, notes_version],
            |r| r.get(0),
        );
        match row {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(sqlite_err(e)),
        }
    }

    /// Store a digest for `(scope_hash, notes_version)`. `INSERT OR
    /// REPLACE` semantics — racing regens (two callers computed the
    /// same digest at the same version) converge on the later write
    /// without erroring.
    pub async fn digest_cache_put(
        &self,
        scope_hash: &str,
        notes_version: i64,
        digest_md: &str,
        token_count: i64,
    ) -> Result<()> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO note_digest_cache
                (scope_hash, notes_version, digest_md, token_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![scope_hash, notes_version, digest_md, token_count, now],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    // ── Note reads ─────────────────────────────────────────────────────────

    /// Query notes. Filters compose with AND.
    ///
    /// - `query` — FTS5 full-text search, ordered by BM25 relevance.
    ///   When `None`, results are ordered by recency (newest first).
    /// - `symbols` — retain notes that mention any of these symbol names.
    /// - `files` — retain notes that mention any of these file paths.
    /// - `kinds` — retain notes whose `kind` is in this list.
    /// - `limit` — maximum number of results (capped at 100 internally).
    /// - `include_retired` — when `false` (default for agents), retired reflections
    ///   are filtered out. Pass `true` for developer history views.
    pub async fn read_notes(
        &self,
        query: Option<&str>,
        symbols: &[String],
        files: &[String],
        kinds: &[String],
        limit: usize,
        include_retired: bool,
    ) -> Result<Vec<NoteRow>> {
        self.read_notes_scoped(
            query,
            symbols,
            files,
            kinds,
            limit,
            include_retired,
            &ScopeFilter::default(),
        )
        .await
    }

    /// Query notes with an additional scope predicate.
    ///
    /// All filters from [`read_notes`] apply. Scope filtering is a post-match
    /// step (like `kinds`) because the FTS5 index is not aware of the `scope`
    /// column — see `idx_notes_scope_feature` for the recency path.
    pub async fn read_notes_scoped(
        &self,
        query: Option<&str>,
        symbols: &[String],
        files: &[String],
        kinds: &[String],
        limit: usize,
        include_retired: bool,
        scope_filter: &ScopeFilter,
    ) -> Result<Vec<NoteRow>> {
        let cap = limit.min(100);
        // Over-fetch when FTS is active to leave room for post-filtering.
        let fetch_limit = if query.is_some() { cap * 10 } else { cap };

        let retired_clause = if include_retired {
            ""
        } else {
            "AND retired_at IS NULL"
        };

        let rows: Vec<NoteRow> = {
            let conn = self.conn.lock().await;
            if let Some(q) = query.filter(|s| !s.is_empty()) {
                let sql = format!(
                    "WITH ranked AS (
                        SELECT rowid, bm25(notes_fts) AS rank
                        FROM notes_fts
                        WHERE notes_fts MATCH ?
                        LIMIT {fetch_limit}
                    )
                    SELECT n.id, n.kind, n.content, n.symbols, n.files, n.session_id,
                           n.created_at, n.tool_name, n.retired_at, n.retired_by,
                           n.scope, n.feature_id, n.promoted_from
                    FROM notes n
                    JOIN ranked r ON r.rowid = n.rowid
                    WHERE 1=1 {retired_clause}
                    ORDER BY r.rank"
                );
                let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;
                let mapped = stmt.query_map(params![q], map_note_row).map_err(sqlite_err)?;
                let mut out = Vec::new();
                for row in mapped {
                    out.push(row.map_err(sqlite_err)?);
                }
                out
            } else {
                let sql = format!(
                    "SELECT id, kind, content, symbols, files, session_id,
                            created_at, tool_name, retired_at, retired_by,
                            scope, feature_id, promoted_from
                     FROM notes
                     WHERE 1=1 {retired_clause}
                     ORDER BY created_at DESC
                     LIMIT ?"
                );
                let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;
                let mapped = stmt
                    .query_map(params![fetch_limit as i64], map_note_row)
                    .map_err(sqlite_err)?;
                let mut out = Vec::new();
                for row in mapped {
                    out.push(row.map_err(sqlite_err)?);
                }
                out
            }
        };

        let mut out: Vec<NoteRow> = rows
            .into_iter()
            .filter(|n| kinds.is_empty() || kinds.iter().any(|k| k == &n.kind))
            .filter(|n| symbols.is_empty() || symbols.iter().any(|s| n.symbols.contains(s)))
            .filter(|n| files.is_empty() || files.iter().any(|f| n.files.contains(f)))
            .filter(|n| scope_matches(n, scope_filter))
            .collect();

        out.truncate(cap);
        Ok(out)
    }

    /// Return reflection notes for the developer-facing `sovereign reflect` command.
    ///
    /// - `since` — unix timestamp lower bound (0 = all time)
    /// - `tool_filter` — restrict to notes with this `tool_name`
    /// - `include_retired` — include retired reflections (for `--history`)
    pub async fn read_reflections(
        &self,
        since: i64,
        tool_filter: Option<&str>,
        include_retired: bool,
    ) -> Result<Vec<NoteRow>> {
        let retired_clause = if include_retired {
            ""
        } else {
            "AND retired_at IS NULL"
        };
        let tool_clause = if tool_filter.is_some() {
            "AND tool_name = ?"
        } else {
            ""
        };

        let sql = format!(
            "SELECT id, kind, content, symbols, files, session_id,
                    created_at, tool_name, retired_at, retired_by,
                    scope, feature_id, promoted_from
             FROM notes
             WHERE kind = 'reflection'
               AND created_at >= ?
               {retired_clause}
               {tool_clause}
             ORDER BY created_at DESC
             LIMIT 1000"
        );

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;

        let mapped = if let Some(tool) = tool_filter {
            stmt.query_map(params![since, tool], map_note_row)
                .map_err(sqlite_err)?
        } else {
            stmt.query_map(params![since], map_note_row)
                .map_err(sqlite_err)?
        };

        let mut out = Vec::new();
        for row in mapped {
            out.push(row.map_err(sqlite_err)?);
        }
        Ok(out)
    }

    // ── Note deletion / retirement ─────────────────────────────────────────

    /// Delete a note by ID. Returns `true` if a row was removed, `false` if
    /// the ID was not found.
    pub async fn delete_note(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let affected = conn
            .execute("DELETE FROM notes WHERE id = ?", params![id])
            .map_err(sqlite_err)?;
        if affected > 0 {
            bump_notes_version(&conn)?;
        }
        Ok(affected > 0)
    }

    /// Mark all active reflections for `tool_name` as retired.
    ///
    /// Returns the IDs of the notes that were retired. Returns an empty vec
    /// if no matching active reflections exist.
    pub async fn retire_by_tool(&self, tool_name: &str, reason: &str) -> Result<Vec<String>> {
        let now = unix_now();
        let conn = self.conn.lock().await;

        // Collect IDs first so we can return them.
        let mut stmt = conn
            .prepare(
                "SELECT id FROM notes WHERE tool_name = ? AND kind = 'reflection' AND retired_at IS NULL",
            )
            .map_err(sqlite_err)?;
        let ids: Vec<String> = stmt
            .query_map(params![tool_name], |r| r.get(0))
            .map_err(sqlite_err)?
            .filter_map(|r| r.ok())
            .collect();

        if ids.is_empty() {
            return Ok(ids);
        }

        conn.execute(
            "UPDATE notes SET retired_at = ?1, retired_by = ?2
             WHERE tool_name = ?3 AND kind = 'reflection' AND retired_at IS NULL",
            params![now, reason, tool_name],
        )
        .map_err(sqlite_err)?;
        bump_notes_version(&conn)?;

        Ok(ids)
    }

    /// Mark a single reflection as retired by its ID.
    ///
    /// Returns `true` if the note existed and was not already retired,
    /// `false` otherwise.
    pub async fn retire_by_id(&self, id: &str, reason: &str) -> Result<bool> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE notes SET retired_at = ?1, retired_by = ?2
                 WHERE id = ?3 AND retired_at IS NULL",
                params![now, reason, id],
            )
            .map_err(sqlite_err)?;
        if affected > 0 {
            bump_notes_version(&conn)?;
        }
        Ok(affected > 0)
    }

    // ── Todo summary ───────────────────────────────────────────────────────

    /// Return the most recent open `todo` notes (for the startup summary).
    pub async fn open_todos(&self, limit: usize) -> Result<Vec<NoteRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, content, symbols, files, session_id,
                        created_at, tool_name, retired_at, retired_by,
                        scope, feature_id, promoted_from
                 FROM notes
                 WHERE kind = 'todo' AND retired_at IS NULL
                 ORDER BY created_at DESC
                 LIMIT ?",
            )
            .map_err(sqlite_err)?;
        let mapped = stmt
            .query_map(params![limit as i64], map_note_row)
            .map_err(sqlite_err)?;
        let mut out = Vec::new();
        for row in mapped {
            out.push(row.map_err(sqlite_err)?);
        }
        Ok(out)
    }

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

// ─── Schema (new databases) ───────────────────────────────────────────────────

/// Full schema for brand-new databases. Includes reflection support and
/// tool_call_log from the start.
const SCHEMA_NEW: &str = "
PRAGMA journal_mode=WAL;

CREATE TABLE IF NOT EXISTS notes (
    id         TEXT    PRIMARY KEY,
    kind       TEXT    NOT NULL CHECK(kind IN (
        'decision','attempt','invariant','todo','reflection',
        'uncertainty','postmortem_pointer','redteam_finding',
        'deviation'
    )),
    content    TEXT    NOT NULL,
    symbols    TEXT    NOT NULL DEFAULT '[]',
    files      TEXT    NOT NULL DEFAULT '[]',
    session_id TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    tool_name  TEXT,
    retired_at INTEGER,
    retired_by TEXT
);
CREATE INDEX IF NOT EXISTS idx_notes_kind       ON notes(kind);
CREATE INDEX IF NOT EXISTS idx_notes_created    ON notes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_tool_name  ON notes(tool_name);
CREATE INDEX IF NOT EXISTS idx_notes_retired_at ON notes(retired_at);

CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
    content, kind,
    content='notes',
    content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TRIGGER IF NOT EXISTS notes_fts_ad BEFORE DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
END;

CREATE TRIGGER IF NOT EXISTS notes_fts_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TABLE IF NOT EXISTS tool_call_log (
    id         TEXT    PRIMARY KEY,
    session_id TEXT    NOT NULL,
    tool_name  TEXT    NOT NULL,
    outcome    TEXT    NOT NULL,
    called_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_log_session ON tool_call_log(session_id);
CREATE INDEX IF NOT EXISTS idx_log_tool    ON tool_call_log(tool_name);
CREATE INDEX IF NOT EXISTS idx_log_called  ON tool_call_log(called_at DESC);

PRAGMA user_version = 1;
";

// ─── Schema migration v3 → v4 (ATOS M4 deviation note kind) ─────────────────

/// Applied to databases at `user_version = 3`. Expands the
/// `notes.kind` CHECK constraint to admit `deviation`, the kind the
/// M4 approval-gate middleware writes when it detects spec-hash
/// drift. Rename-recreate pattern; rebuilds FTS5.
const MIGRATION_V4: &str = "
BEGIN;

ALTER TABLE notes RENAME TO notes_v3;

CREATE TABLE notes (
    id            TEXT    PRIMARY KEY,
    kind          TEXT    NOT NULL CHECK(kind IN (
        'decision','attempt','invariant','todo','reflection',
        'uncertainty','postmortem_pointer','redteam_finding',
        'deviation'
    )),
    content       TEXT    NOT NULL,
    symbols       TEXT    NOT NULL DEFAULT '[]',
    files         TEXT    NOT NULL DEFAULT '[]',
    session_id    TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    tool_name     TEXT,
    retired_at    INTEGER,
    retired_by    TEXT,
    scope         TEXT    NOT NULL DEFAULT 'global'
                  CHECK(scope IN ('global','feature','session')),
    feature_id    TEXT,
    promoted_from TEXT
);

INSERT INTO notes (
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from
)
SELECT
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from
FROM notes_v3;

DROP TABLE notes_v3;

CREATE INDEX IF NOT EXISTS idx_notes_kind           ON notes(kind);
CREATE INDEX IF NOT EXISTS idx_notes_created        ON notes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_tool_name      ON notes(tool_name);
CREATE INDEX IF NOT EXISTS idx_notes_retired_at     ON notes(retired_at);
CREATE INDEX IF NOT EXISTS idx_notes_scope_feature  ON notes(scope, feature_id);
CREATE INDEX IF NOT EXISTS idx_notes_feature
    ON notes(feature_id) WHERE feature_id IS NOT NULL;

DROP TABLE IF EXISTS notes_fts;
CREATE VIRTUAL TABLE notes_fts USING fts5(
    content, kind,
    content='notes',
    content_rowid='rowid'
);
INSERT INTO notes_fts(notes_fts) VALUES('rebuild');

DROP TRIGGER IF EXISTS notes_fts_ai;
DROP TRIGGER IF EXISTS notes_fts_ad;
DROP TRIGGER IF EXISTS notes_fts_au;

CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
END;

CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

PRAGMA user_version = 4;

COMMIT;
";

// ─── Schema migration v2 → v3 (ATOS note kinds: uncertainty,
//     postmortem_pointer, redteam_finding) ─────────────────────────────────

/// Applied to databases at `user_version = 2`. Expands the `notes.kind`
/// CHECK constraint. SQLite cannot alter a CHECK in-place, so we
/// rename-copy-rebuild — same pattern as `MIGRATION_V1`. The FTS5
/// virtual table and triggers are rebuilt because they reference the
/// `notes` table by name; the rebuild trigger repopulates the index
/// from the copied rows.
///
/// Idempotent across a single install: gated by `PRAGMA user_version <
/// 3` in `NoteStore::open`.
const MIGRATION_V3: &str = "
BEGIN;

ALTER TABLE notes RENAME TO notes_v2;

CREATE TABLE notes (
    id            TEXT    PRIMARY KEY,
    kind          TEXT    NOT NULL CHECK(kind IN (
        'decision','attempt','invariant','todo','reflection',
        'uncertainty','postmortem_pointer','redteam_finding'
    )),
    content       TEXT    NOT NULL,
    symbols       TEXT    NOT NULL DEFAULT '[]',
    files         TEXT    NOT NULL DEFAULT '[]',
    session_id    TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    tool_name     TEXT,
    retired_at    INTEGER,
    retired_by    TEXT,
    scope         TEXT    NOT NULL DEFAULT 'global'
                  CHECK(scope IN ('global','feature','session')),
    feature_id    TEXT,
    promoted_from TEXT
);

INSERT INTO notes (
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from
)
SELECT
    id, kind, content, symbols, files, session_id, created_at, updated_at,
    tool_name, retired_at, retired_by, scope, feature_id, promoted_from
FROM notes_v2;

DROP TABLE notes_v2;

CREATE INDEX IF NOT EXISTS idx_notes_kind           ON notes(kind);
CREATE INDEX IF NOT EXISTS idx_notes_created        ON notes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_tool_name      ON notes(tool_name);
CREATE INDEX IF NOT EXISTS idx_notes_retired_at     ON notes(retired_at);
CREATE INDEX IF NOT EXISTS idx_notes_scope_feature  ON notes(scope, feature_id);
CREATE INDEX IF NOT EXISTS idx_notes_feature
    ON notes(feature_id) WHERE feature_id IS NOT NULL;

-- Rebuild FTS5 + triggers. The old `notes_v2` has been dropped, so
-- any pre-existing rowid mappings in `notes_fts` are stale — blow it
-- away and repopulate from the current `notes` table.
DROP TABLE IF EXISTS notes_fts;
CREATE VIRTUAL TABLE notes_fts USING fts5(
    content, kind,
    content='notes',
    content_rowid='rowid'
);
INSERT INTO notes_fts(notes_fts) VALUES('rebuild');

DROP TRIGGER IF EXISTS notes_fts_ai;
DROP TRIGGER IF EXISTS notes_fts_ad;
DROP TRIGGER IF EXISTS notes_fts_au;

CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
END;

CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

PRAGMA user_version = 3;

COMMIT;
";

// ─── Schema migration v1 → v2 (ATOS scoping) ─────────────────────────────────

/// Applied to databases at `user_version = 1`. Adds ATOS scope/feature_id
/// columns to `notes`, creates the `meta_counters` singleton for the
/// `notes_version` clock, and provisions the `note_digest_cache` table
/// used by the Fast-slot digest in M1.4.
///
/// Idempotent across a single install: the migration is gated by
/// `PRAGMA user_version < 2` in `NoteStore::open`.
const MIGRATION_V2: &str = "
BEGIN;

ALTER TABLE notes ADD COLUMN scope         TEXT NOT NULL DEFAULT 'global';
ALTER TABLE notes ADD COLUMN feature_id    TEXT;
ALTER TABLE notes ADD COLUMN promoted_from TEXT;

CREATE INDEX IF NOT EXISTS idx_notes_scope_feature ON notes(scope, feature_id);
CREATE INDEX IF NOT EXISTS idx_notes_feature
    ON notes(feature_id) WHERE feature_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS meta_counters (
    key TEXT PRIMARY KEY,
    val INTEGER NOT NULL
);
INSERT OR IGNORE INTO meta_counters(key, val) VALUES ('notes_version', 0);

CREATE TABLE IF NOT EXISTS note_digest_cache (
    scope_hash    TEXT    NOT NULL,
    notes_version INTEGER NOT NULL,
    digest_md     TEXT    NOT NULL,
    token_count   INTEGER NOT NULL,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY(scope_hash, notes_version)
);
CREATE INDEX IF NOT EXISTS idx_digest_created
    ON note_digest_cache(created_at DESC);

PRAGMA user_version = 2;

COMMIT;
";

// ─── Schema migration v0 → v1 ────────────────────────────────────────────────

/// Applied to existing databases whose `user_version = 0`. Uses SQLite's
/// standard rename-recreate-copy-drop pattern to update the CHECK constraint
/// and add three new nullable columns. Rebuilds FTS5 and triggers.
const MIGRATION_V1: &str = "
BEGIN;

ALTER TABLE notes RENAME TO notes_v0;

CREATE TABLE notes (
    id         TEXT    PRIMARY KEY,
    kind       TEXT    NOT NULL CHECK(kind IN ('decision','attempt','invariant','todo','reflection')),
    content    TEXT    NOT NULL,
    symbols    TEXT    NOT NULL DEFAULT '[]',
    files      TEXT    NOT NULL DEFAULT '[]',
    session_id TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    tool_name  TEXT,
    retired_at INTEGER,
    retired_by TEXT
);

INSERT INTO notes (id, kind, content, symbols, files, session_id, created_at, updated_at)
SELECT id, kind, content, symbols, files, session_id, created_at, updated_at FROM notes_v0;

DROP TABLE notes_v0;

CREATE INDEX IF NOT EXISTS idx_notes_kind       ON notes(kind);
CREATE INDEX IF NOT EXISTS idx_notes_created    ON notes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notes_tool_name  ON notes(tool_name);
CREATE INDEX IF NOT EXISTS idx_notes_retired_at ON notes(retired_at);

DROP TABLE IF EXISTS notes_fts;
CREATE VIRTUAL TABLE notes_fts USING fts5(
    content, kind,
    content='notes',
    content_rowid='rowid'
);
INSERT INTO notes_fts(notes_fts) VALUES('rebuild');

DROP TRIGGER IF EXISTS notes_fts_ai;
DROP TRIGGER IF EXISTS notes_fts_ad;
DROP TRIGGER IF EXISTS notes_fts_au;

CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
END;

CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TABLE IF NOT EXISTS tool_call_log (
    id         TEXT    PRIMARY KEY,
    session_id TEXT    NOT NULL,
    tool_name  TEXT    NOT NULL,
    outcome    TEXT    NOT NULL,
    called_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_log_session ON tool_call_log(session_id);
CREATE INDEX IF NOT EXISTS idx_log_tool    ON tool_call_log(tool_name);
CREATE INDEX IF NOT EXISTS idx_log_called  ON tool_call_log(called_at DESC);

PRAGMA user_version = 1;

COMMIT;
";

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn sqlite_err(e: rusqlite::Error) -> Error {
    Error::Io(std::io::Error::other(format!("NoteStore sqlite: {e}")))
}

/// Returns `true` when the note matches the caller's scope predicate.
///
/// Default `ScopeFilter` (no scopes, no feature_id) always matches — the
/// legacy [`NoteStore::read_notes`] wrapper uses this to preserve behavior.
fn scope_matches(note: &NoteRow, filter: &ScopeFilter) -> bool {
    if filter.scopes.is_empty() && filter.feature_id.is_none() {
        return true;
    }

    if !filter.scopes.is_empty() {
        let ok = filter.scopes.iter().any(|s| s.as_str() == note.scope);
        if !ok {
            return false;
        }
    }

    if let Some(fid) = &filter.feature_id {
        // Feature_id predicate only applies to feature-scoped rows. Global /
        // session rows pass through regardless so a `scopes = [global,
        // feature]` + `feature_id = X` query returns globals + one feature.
        if note.scope == "feature" && note.feature_id.as_deref() != Some(fid.as_str()) {
            return false;
        }
    }

    true
}

/// Monotonic counter bumped on every note mutation. Callers must hold the
/// NoteStore lock so the bump is effectively atomic with the mutation.
fn bump_notes_version(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE meta_counters SET val = val + 1 WHERE key = 'notes_version'",
        [],
    )
    .map_err(sqlite_err)?;
    Ok(())
}

fn map_note_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteRow> {
    let symbols_json: String = row.get(3)?;
    let files_json: String = row.get(4)?;
    let created_at_secs: i64 = row.get(6)?;

    let symbols: Vec<String> = serde_json::from_str(&symbols_json).unwrap_or_default();
    let files: Vec<String> = serde_json::from_str(&files_json).unwrap_or_default();

    // Convert unix timestamp to RFC 3339.
    let created_at = chrono::DateTime::<chrono::Utc>::from_timestamp(created_at_secs, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| created_at_secs.to_string());

    Ok(NoteRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        content: row.get(2)?,
        symbols,
        files,
        session_id: row.get(5)?,
        created_at,
        tool_name: row.get(7)?,
        retired_at: row.get(8)?,
        retired_by: row.get(9)?,
        scope: row.get(10)?,
        feature_id: row.get(11)?,
        promoted_from: row.get(12)?,
    })
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_store() -> NoteStore {
        let dir = tempfile::tempdir().unwrap();
        NoteStore::open(&dir.path().join("notes.db")).unwrap()
    }

    // ── Existing note tests (must continue to pass) ──────────────────────

    #[tokio::test]
    async fn write_note_roundtrip() {
        let store = make_store().await;
        let id = store
            .write_note(
                "decision",
                "Use BFS for blast radius",
                vec!["blast_radius".into()],
                vec!["src/lib.rs".into()],
                "test-session",
            )
            .await
            .unwrap();
        assert!(!id.is_empty());

        let notes = store
            .read_notes(None, &[], &[], &[], 10, false)
            .await
            .unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].kind, "decision");
        assert_eq!(notes[0].content, "Use BFS for blast radius");
        assert_eq!(notes[0].symbols, vec!["blast_radius"]);
        assert!(notes[0].tool_name.is_none());
        assert!(notes[0].retired_at.is_none());
    }

    #[tokio::test]
    async fn read_notes_fts_search() {
        let store = make_store().await;
        store
            .write_note("decision", "Use BFS for blast radius traversal", vec![], vec![], "s1")
            .await
            .unwrap();
        store
            .write_note("todo", "Unrelated note about caching", vec![], vec![], "s1")
            .await
            .unwrap();

        let results = store
            .read_notes(Some("blast radius"), &[], &[], &[], 10, false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("blast radius"));
    }

    #[tokio::test]
    async fn read_notes_symbol_filter() {
        let store = make_store().await;
        store
            .write_note("attempt", "tried foo", vec!["foo_fn".into()], vec![], "s1")
            .await
            .unwrap();
        store
            .write_note("attempt", "tried bar", vec!["bar_fn".into()], vec![], "s1")
            .await
            .unwrap();

        let results = store
            .read_notes(None, &["foo_fn".to_string()], &[], &[], 10, false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("foo"));
    }

    #[tokio::test]
    async fn read_notes_kind_filter() {
        let store = make_store().await;
        store
            .write_note("decision", "keep this", vec![], vec![], "s1")
            .await
            .unwrap();
        store
            .write_note("todo", "do this later", vec![], vec![], "s1")
            .await
            .unwrap();

        let results = store
            .read_notes(None, &[], &[], &["todo".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "todo");
    }

    #[tokio::test]
    async fn delete_note_removes() {
        let store = make_store().await;
        let id = store
            .write_note("invariant", "never call this twice", vec![], vec![], "s1")
            .await
            .unwrap();

        let deleted = store.delete_note(&id).await.unwrap();
        assert!(deleted);

        let notes = store.read_notes(None, &[], &[], &[], 10, false).await.unwrap();
        assert!(notes.is_empty());

        // Deleting again returns false.
        let deleted_again = store.delete_note(&id).await.unwrap();
        assert!(!deleted_again);
    }

    #[tokio::test]
    async fn open_todos_returns_only_todos() {
        let store = make_store().await;
        store
            .write_note("todo", "fix the thing", vec![], vec![], "s1")
            .await
            .unwrap();
        store
            .write_note("decision", "keep it", vec![], vec![], "s1")
            .await
            .unwrap();

        let todos = store.open_todos(10).await.unwrap();
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].content, "fix the thing");
    }

    // ── Reflection tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn reflection_stored_with_kind() {
        let store = make_store().await;
        let id = store
            .write_reflection(
                r#"{"task_summary":"Refactored EmbedFn"}"#,
                Some("blast_radius"),
                "s1",
            )
            .await
            .unwrap();
        assert!(!id.is_empty());

        let results = store
            .read_notes(None, &[], &[], &["reflection".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "reflection");
        assert_eq!(results[0].tool_name.as_deref(), Some("blast_radius"));
    }

    #[tokio::test]
    async fn reflection_active_by_default() {
        let store = make_store().await;
        let id = store
            .write_reflection(r#"{"task_summary":"test"}"#, None, "s1")
            .await
            .unwrap();

        let notes = store.read_notes(None, &[], &[], &[], 10, false).await.unwrap();
        let note = notes.iter().find(|n| n.id == id).unwrap();
        assert!(note.retired_at.is_none());
        assert!(note.retired_by.is_none());
    }

    #[tokio::test]
    async fn retired_reflection_hidden_by_default() {
        let store = make_store().await;
        let id = store
            .write_reflection(r#"{"task_summary":"test"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();

        let retired = store.retire_by_id(&id, "fixed in PR #1").await.unwrap();
        assert!(retired);

        // Default read (include_retired=false) should not return it.
        let notes = store.read_notes(None, &[], &[], &[], 10, false).await.unwrap();
        assert!(notes.is_empty());

        // But read_notes with include_retired=true should.
        let all = store.read_notes(None, &[], &[], &[], 10, true).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].retired_by.as_deref(), Some("fixed in PR #1"));
        assert!(all[0].retired_at.is_some());
    }

    #[tokio::test]
    async fn retired_reflection_visible_in_history() {
        let store = make_store().await;
        let id = store
            .write_reflection(r#"{"task_summary":"test"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();
        store.retire_by_id(&id, "v0.4.2").await.unwrap();

        let reflections = store.read_reflections(0, None, true).await.unwrap();
        assert_eq!(reflections.len(), 1);
        assert_eq!(reflections[0].retired_by.as_deref(), Some("v0.4.2"));
    }

    #[tokio::test]
    async fn retire_by_tool_matches_all() {
        let store = make_store().await;
        for _ in 0..3 {
            store
                .write_reflection(r#"{"task_summary":"blast radius miss"}"#, Some("blast_radius"), "s1")
                .await
                .unwrap();
        }
        // Unrelated reflection — should not be retired.
        store
            .write_reflection(r#"{"task_summary":"project context miss"}"#, Some("project_context"), "s1")
            .await
            .unwrap();

        let retired_ids = store.retire_by_tool("blast_radius", "macro support added").await.unwrap();
        assert_eq!(retired_ids.len(), 3);

        // blast_radius reflections gone from default read.
        let active = store
            .read_notes(None, &[], &[], &["reflection".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].tool_name.as_deref(), Some("project_context"));
    }

    #[tokio::test]
    async fn retire_by_id_leaves_others_active() {
        let store = make_store().await;
        let id1 = store
            .write_reflection(r#"{"task_summary":"a"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();
        let _id2 = store
            .write_reflection(r#"{"task_summary":"b"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();

        let retired = store.retire_by_id(&id1, "fixed").await.unwrap();
        assert!(retired);

        let active = store
            .read_notes(None, &[], &[], &["reflection".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, _id2);
    }

    #[tokio::test]
    async fn retire_by_id_already_retired_returns_false() {
        let store = make_store().await;
        let id = store
            .write_reflection(r#"{"task_summary":"a"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();
        store.retire_by_id(&id, "first").await.unwrap();
        let second = store.retire_by_id(&id, "second").await.unwrap();
        assert!(!second);
    }

    // ── tool_call_log tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn tool_call_log_records_outcome() {
        let store = make_store().await;
        store.log_tool_call("sess-1", "lint_status", "success").await.unwrap();
        store.log_tool_call("sess-1", "blast_radius", "error").await.unwrap();

        let rows = store.tool_call_log_rows(0, 100).await.unwrap();
        assert_eq!(rows.len(), 2);
        // Most recent first.
        assert_eq!(rows[0].tool_name, "blast_radius");
        assert_eq!(rows[0].outcome, "error");
        assert_eq!(rows[1].tool_name, "lint_status");
        assert_eq!(rows[1].outcome, "success");
    }

    #[tokio::test]
    async fn tool_call_log_ring_buffer() {
        let store = make_store().await;
        for i in 0..10_001usize {
            store
                .log_tool_call("sess", "lint_status", "success")
                .await
                .unwrap();
            let _ = i; // suppress unused warning
        }

        let rows = store.tool_call_log_rows(0, 20_000).await.unwrap();
        assert_eq!(rows.len(), 10_000);
    }

    // ── Migration test ────────────────────────────────────────────────────

    #[tokio::test]
    async fn migration_v0_to_v1_preserves_data_and_enables_reflections() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");

        // Simulate an old-schema database (no tool_name/retired_at/retired_by,
        // restricted CHECK constraint, no tool_call_log).
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE notes (
                     id TEXT PRIMARY KEY,
                     kind TEXT NOT NULL CHECK(kind IN ('decision','attempt','invariant','todo')),
                     content TEXT NOT NULL,
                     symbols TEXT NOT NULL DEFAULT '[]',
                     files TEXT NOT NULL DEFAULT '[]',
                     session_id TEXT NOT NULL,
                     created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 CREATE VIRTUAL TABLE notes_fts USING fts5(content, kind, content='notes', content_rowid='rowid');
                 INSERT INTO notes VALUES ('id-1','todo','old note','[]','[]','s0',1000,1000);",
            )
            .unwrap();
            // user_version stays 0 (default).
        }

        // Open with new NoteStore — migration should run.
        let store = NoteStore::open(&db_path).unwrap();

        // Old note is preserved.
        let notes = store.read_notes(None, &[], &[], &[], 10, false).await.unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].content, "old note");

        // Reflection kind is now accepted.
        let id = store
            .write_reflection(r#"{"task_summary":"post-migration"}"#, Some("blast_radius"), "s1")
            .await
            .unwrap();
        assert!(!id.is_empty());

        // tool_call_log is available.
        store.log_tool_call("sess", "lint_status", "success").await.unwrap();
        let rows = store.tool_call_log_rows(0, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
    }

    // ── ATOS scope tests (M1.1) ──────────────────────────────────────────

    #[tokio::test]
    async fn scoped_note_persists_scope_and_feature_id() {
        let store = make_store().await;
        let id = store
            .write_note_scoped(
                "decision",
                "prefer UNION over sequential queries",
                vec![],
                vec![],
                "s1",
                NoteScope::Feature,
                Some("atos-version-flag"),
            )
            .await
            .unwrap();

        let notes = store.read_notes(None, &[], &[], &[], 10, false).await.unwrap();
        let note = notes.iter().find(|n| n.id == id).unwrap();
        assert_eq!(note.scope, "feature");
        assert_eq!(note.feature_id.as_deref(), Some("atos-version-flag"));
        assert!(note.promoted_from.is_none());
    }

    #[tokio::test]
    async fn feature_scope_requires_feature_id() {
        let store = make_store().await;
        let result = store
            .write_note_scoped(
                "decision",
                "bad",
                vec![],
                vec![],
                "s1",
                NoteScope::Feature,
                None,
            )
            .await;
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[tokio::test]
    async fn legacy_write_note_defaults_to_global_scope() {
        let store = make_store().await;
        let id = store
            .write_note("invariant", "never panic", vec![], vec![], "s1")
            .await
            .unwrap();
        let notes = store.read_notes(None, &[], &[], &[], 10, false).await.unwrap();
        let note = notes.iter().find(|n| n.id == id).unwrap();
        assert_eq!(note.scope, "global");
        assert!(note.feature_id.is_none());
    }

    #[tokio::test]
    async fn scope_filter_selects_feature_and_global() {
        let store = make_store().await;

        // A global invariant visible to every feature.
        store
            .write_note_scoped(
                "invariant",
                "global rule",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
            )
            .await
            .unwrap();
        // Two features' worth of feature-scoped notes.
        store
            .write_note_scoped(
                "decision",
                "feat-A decision",
                vec![],
                vec![],
                "s1",
                NoteScope::Feature,
                Some("feat-a"),
            )
            .await
            .unwrap();
        store
            .write_note_scoped(
                "decision",
                "feat-B decision",
                vec![],
                vec![],
                "s1",
                NoteScope::Feature,
                Some("feat-b"),
            )
            .await
            .unwrap();

        let filter = ScopeFilter {
            scopes: vec![NoteScope::Global, NoteScope::Feature],
            feature_id: Some("feat-a".into()),
        };
        let notes = store
            .read_notes_scoped(None, &[], &[], &[], 10, false, &filter)
            .await
            .unwrap();

        let contents: Vec<_> = notes.iter().map(|n| n.content.as_str()).collect();
        assert!(contents.contains(&"global rule"));
        assert!(contents.contains(&"feat-A decision"));
        assert!(!contents.contains(&"feat-B decision"));
        assert_eq!(notes.len(), 2);
    }

    #[tokio::test]
    async fn notes_version_counter_increments_on_writes() {
        let store = make_store().await;
        let v0 = store.notes_version().await.unwrap();

        store
            .write_note("decision", "a", vec![], vec![], "s1")
            .await
            .unwrap();
        let v1 = store.notes_version().await.unwrap();
        assert_eq!(v1, v0 + 1);

        let id = store
            .write_note_scoped(
                "decision",
                "b",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
            )
            .await
            .unwrap();
        let v2 = store.notes_version().await.unwrap();
        assert_eq!(v2, v1 + 1);

        store.delete_note(&id).await.unwrap();
        let v3 = store.notes_version().await.unwrap();
        assert_eq!(v3, v2 + 1);

        // No-op delete should not bump.
        store.delete_note("nonexistent").await.unwrap();
        let v4 = store.notes_version().await.unwrap();
        assert_eq!(v4, v3);
    }

    #[tokio::test]
    async fn promote_note_creates_new_row_and_tags_origin() {
        let store = make_store().await;
        let src = store
            .write_note_scoped(
                "decision",
                "feature-local decision",
                vec!["Foo".into()],
                vec![],
                "s1",
                NoteScope::Feature,
                Some("feat-a"),
            )
            .await
            .unwrap();

        let promoted_id = store
            .promote_note(&src, NoteScope::Global, None, Some("rewritten as global rule"))
            .await
            .unwrap();

        let notes = store.read_notes(None, &[], &[], &[], 10, false).await.unwrap();
        let promoted = notes.iter().find(|n| n.id == promoted_id).unwrap();
        assert_eq!(promoted.scope, "global");
        assert_eq!(promoted.content, "rewritten as global rule");
        assert_eq!(promoted.promoted_from.as_deref(), Some(src.as_str()));
        // Source row still exists at feature scope.
        let source = notes.iter().find(|n| n.id == src).unwrap();
        assert_eq!(source.scope, "feature");
    }

    // ── v3 kinds round-trip ──────────────────────────────────────────────

    #[tokio::test]
    async fn write_uncertainty_kind_round_trip() {
        let store = make_store().await;
        let id = store
            .write_note_scoped(
                "uncertainty",
                "Deep collection nesting in Zotero exports — flatten to nearest ancestor.",
                vec![],
                vec!["acquirers/zotero.rs".into()],
                "s1",
                NoteScope::Feature,
                Some("zotero-acquirer"),
            )
            .await
            .unwrap();
        let notes = store.read_notes(None, &[], &[], &[], 10, false).await.unwrap();
        let n = notes.iter().find(|n| n.id == id).unwrap();
        assert_eq!(n.kind, "uncertainty");
        assert_eq!(n.feature_id.as_deref(), Some("zotero-acquirer"));
    }

    #[tokio::test]
    async fn write_postmortem_pointer_kind_round_trip() {
        let store = make_store().await;
        let id = store
            .write_note_scoped(
                "postmortem_pointer",
                "zotero_rdf.rs::parse_item — RDF boundary detection is the most complex path.",
                vec!["parse_item".into()],
                vec!["extractors/zotero_rdf.rs".into()],
                "s1",
                NoteScope::Feature,
                Some("zotero-acquirer"),
            )
            .await
            .unwrap();
        let notes = store
            .read_notes(None, &[], &[], &["postmortem_pointer".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, id);
    }

    #[tokio::test]
    async fn write_redteam_finding_kind_round_trip() {
        let store = make_store().await;
        let id = store
            .write_note_scoped(
                "redteam_finding",
                "ZoteroLibrary factory does not explicitly set scope=Local.",
                vec![],
                vec![],
                "s1",
                NoteScope::Feature,
                Some("zotero-acquirer"),
            )
            .await
            .unwrap();
        let notes = store.read_notes_scoped(
            None, &[], &[], &["redteam_finding".to_string()], 10, false,
            &ScopeFilter { scopes: vec![NoteScope::Feature], feature_id: Some("zotero-acquirer".into()) },
        ).await.unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, id);
    }

    #[tokio::test]
    async fn write_deviation_kind_round_trip() {
        let store = make_store().await;
        let id = store
            .write_note_scoped(
                "deviation",
                "Spec content hash changed since approval (a3f1 → 8b7c).",
                vec![],
                vec![".sovereign/features/fx/spec.md".into()],
                "atos-middleware",
                NoteScope::Feature,
                Some("fx"),
            )
            .await
            .unwrap();
        let notes = store
            .read_notes(None, &[], &[], &["deviation".to_string()], 10, false)
            .await
            .unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, id);
        assert!(notes[0].content.contains("Spec content hash"));
    }

    #[tokio::test]
    async fn migration_v3_to_v4_preserves_data_and_enables_deviation() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");

        // Build a v3 database by running SCHEMA_NEW + V2 + V3 and
        // then seeding a row. We can't simulate a "v3 without V4's
        // kind" from a clean DB because SCHEMA_NEW already has the
        // expanded kind list after M4.2's edit — so we seed the
        // note BEFORE V4 runs by opening and closing the store once
        // at v3, then running V4 and checking behavior.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(SCHEMA_NEW).unwrap();
            conn.execute_batch(MIGRATION_V2).unwrap();
            conn.execute_batch(MIGRATION_V3).unwrap();
            conn.execute(
                "INSERT INTO notes (id, kind, content, symbols, files, session_id,
                    created_at, updated_at, scope)
                 VALUES ('pre-v4','decision','survived','[]','[]','s0',1000,1000,'global')",
                [],
            )
            .unwrap();
            let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
            assert_eq!(v, 3, "baseline should be v3");
        }

        // Reopen — MIGRATION_V4 runs.
        let store = NoteStore::open(&db_path).unwrap();

        let old = store.read_note_by_id("pre-v4").await.unwrap().unwrap();
        assert_eq!(old.content, "survived");

        let new_id = store
            .write_note_scoped(
                "deviation",
                "post-migration",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
            )
            .await
            .unwrap();
        assert!(!new_id.is_empty());

        // FTS5 still works after the rebuild.
        let hits = store
            .read_notes(Some("survived"), &[], &[], &[], 5, false)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn unknown_kind_still_rejected() {
        let store = make_store().await;
        let err = store
            .write_note("not_a_kind", "hi", vec![], vec![], "s1")
            .await
            .unwrap_err();
        // CHECK constraint violation surfaces via rusqlite → Error::Io.
        let msg = format!("{err}");
        assert!(msg.to_lowercase().contains("check") || msg.to_lowercase().contains("constraint"),
                "unexpected error: {msg}");
    }

    #[tokio::test]
    async fn migration_v2_to_v3_preserves_data_and_enables_new_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");

        // Build a v2 database manually — run SCHEMA_NEW (v1) + MIGRATION_V2 (v2),
        // then stop short of MIGRATION_V3. Insert a pre-v3 row so we can
        // prove it survives the rebuild.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(SCHEMA_NEW).unwrap();
            conn.execute_batch(MIGRATION_V2).unwrap();
            // Drop the post-migration CHECK back down to the v2 set so
            // we're actually simulating a v2 DB (SCHEMA_NEW already has
            // the expanded list after this file's M3.2 edit, but a
            // real-world v2 DB on disk won't).
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, 2, "baseline should be v2");
            conn.execute(
                "INSERT INTO notes (id, kind, content, symbols, files, session_id,
                    created_at, updated_at, scope)
                 VALUES ('pre-v3','decision','survives migration','[]','[]','s0',1000,1000,'global')",
                [],
            )
            .unwrap();
        }

        // Reopen — MIGRATION_V3 runs.
        let store = NoteStore::open(&db_path).unwrap();

        // Old row preserved.
        let old = store.read_note_by_id("pre-v3").await.unwrap().unwrap();
        assert_eq!(old.content, "survives migration");
        assert_eq!(old.scope, "global");

        // New kinds accepted.
        let id = store
            .write_note_scoped(
                "uncertainty",
                "post-migration",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
            )
            .await
            .unwrap();
        assert!(!id.is_empty());

        // FTS5 still works (was rebuilt during the migration).
        let hits = store
            .read_notes(Some("survives"), &[], &[], &[], 5, false)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "pre-v3");
    }

    #[tokio::test]
    async fn digest_cache_round_trip() {
        let store = make_store().await;
        let v = store.notes_version().await.unwrap();

        // Miss on an empty cache.
        assert!(store.digest_cache_get("abc", v).await.unwrap().is_none());

        // Put and hit.
        store
            .digest_cache_put("abc", v, "## Digest\n\n[note:xyz] invariant", 8)
            .await
            .unwrap();
        let hit = store.digest_cache_get("abc", v).await.unwrap();
        assert_eq!(hit.as_deref(), Some("## Digest\n\n[note:xyz] invariant"));

        // Same scope_hash, different version → miss. The cache is
        // versioned precisely so a post-write read doesn't serve
        // stale content.
        assert!(store.digest_cache_get("abc", v + 1).await.unwrap().is_none());

        // Put with replace at same key.
        store
            .digest_cache_put("abc", v, "## Digest v2", 3)
            .await
            .unwrap();
        let replaced = store.digest_cache_get("abc", v).await.unwrap();
        assert_eq!(replaced.as_deref(), Some("## Digest v2"));
    }

    #[tokio::test]
    async fn migration_v1_to_v2_adds_scope_columns_and_counter() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");

        // Build a v1 database manually (stops short of MIGRATION_V2).
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(SCHEMA_NEW).unwrap();
            conn.execute(
                "INSERT INTO notes (id, kind, content, symbols, files, session_id, created_at, updated_at)
                 VALUES ('old-1','decision','pre-ATOS note','[]','[]','s0',1000,1000)",
                [],
            )
            .unwrap();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, 1, "baseline should be at v1");
        }

        // Reopen — MIGRATION_V2 runs.
        let store = NoteStore::open(&db_path).unwrap();

        // Old row preserved; gains scope='global', feature_id=NULL via column default.
        let notes = store.read_notes(None, &[], &[], &[], 10, false).await.unwrap();
        let old = notes.iter().find(|n| n.id == "old-1").unwrap();
        assert_eq!(old.scope, "global");
        assert!(old.feature_id.is_none());

        // notes_version counter is available.
        let v = store.notes_version().await.unwrap();
        assert!(v >= 0);

        // note_digest_cache table exists (query returns 0 rows, no error).
        let conn = Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM note_digest_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
