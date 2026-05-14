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
use crate::notes_schema::{
    MIGRATION_V1, MIGRATION_V2, MIGRATION_V3, MIGRATION_V4, MIGRATION_V5, MIGRATION_V6,
    MIGRATION_V7, SCHEMA_NEW,
};

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

/// Provenance dimension for notes (audit-hardening v6 schema).
///
/// `Agent` is the highest-confidence source — the agent explicitly
/// called the `note` tool. The other four record automated sources
/// the audit assembly ranks lower:
///
/// - `Committed` — harvested from a git commit message by the daemon
///   reindexer's git HEAD poll.
/// - `Extracted` — produced by an LLM pass over the session diff at
///   audit-assembly time.
/// - `Inferred` — regex-mined from agent response text in the
///   conversation transcript.
/// - `Observed` — derived from a tool-call pattern match (e.g.
///   `blast` → file write counts as "investigated impact before
///   modifying").
///
/// The audit floor is non-empty when at least one of these fires,
/// even if the agent never wrote an explicit note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteSource {
    Agent,
    Committed,
    Extracted,
    Inferred,
    Observed,
}

impl NoteSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Committed => "committed",
            Self::Extracted => "extracted",
            Self::Inferred => "inferred",
            Self::Observed => "observed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "agent" => Some(Self::Agent),
            "committed" => Some(Self::Committed),
            "extracted" => Some(Self::Extracted),
            "inferred" => Some(Self::Inferred),
            "observed" => Some(Self::Observed),
            _ => None,
        }
    }

    /// Audit-display priority. Higher number = higher priority.
    /// Used to sort decisions so agent-written notes appear above
    /// extracted/inferred/observed ones at the same date.
    pub fn priority(self) -> u8 {
        match self {
            Self::Agent => 4,
            Self::Committed => 3,
            Self::Extracted => 2,
            Self::Inferred => 1,
            Self::Observed => 0,
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
    /// Free-text entity name this note relates to — typically a
    /// `Person` / `Organization` name for `commitment` and
    /// `follow_up` kinds, an `Initiative` name for `goal` kind. Not
    /// a foreign key into the entity graph (the graph is rebuilt
    /// each enrichment cycle); the digest matches at query time.
    /// `None` when the note has no relational anchor (e.g. classic
    /// `decision` / `invariant` kinds).
    pub related_entity: Option<String>,
    /// Provenance of the note. One of:
    /// - `"agent"`     — explicit `note` tool call by an agent (highest signal).
    /// - `"committed"` — harvested from a git commit message.
    /// - `"extracted"` — extracted by an LLM pass over the session diff.
    /// - `"inferred"`  — regex-mined from agent response text.
    /// - `"observed"`  — derived from a tool-call pattern match.
    ///
    /// Pre-v6 rows default to `"agent"`. CHECK enforcement is at the
    /// application layer (in [`NoteStore::write_note_with_source`])
    /// rather than via a SQL constraint, so adding a new source is a
    /// one-line code change rather than a schema migration.
    pub source: String,
    /// Note id this note reverses. `None` for first-time decisions.
    /// Audit assembly uses this to render `↳ REVERSED` lines under the
    /// original decision. The referenced row is left intact — only the
    /// audit display treats this as a reversal.
    pub supersedes: Option<String>,
    /// Structured per-kind payload (v7+). Used by the recipe-author
    /// kinds (`decision` with a `decision_kind`, `research_finding`
    /// with `authority`, `recipe_issue` with category/count, etc.) so
    /// the dashboard / CLI can read fields without reparsing
    /// `content`. NULL for pre-v7 rows and for kinds that don't carry
    /// structured data.
    pub payload_json: Option<String>,
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

        // v4 → v5: Relational + Strategic Awareness changeset
        // (requirements §6). Three new kinds — `commitment`,
        // `follow_up`, `goal` — plus a `related_entity` text column
        // on every row. Rename-recreate again because the CHECK
        // constraint changes and SQLite can't ALTER one in place.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version < 5 {
            conn.execute_batch(MIGRATION_V5).map_err(|e| {
                Error::Io(std::io::Error::other(format!("NoteStore migrate v5: {e}")))
            })?;
        }

        // v5 → v6: Audit-hardening provenance fields. Two new columns
        // (`source`, `supersedes`) supporting the four extraction
        // streams (agent / committed / extracted / inferred / observed)
        // and decision-reversal display. Plain ADD COLUMN — no CHECK
        // constraint changes, so no rename-recreate.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version < 6 {
            conn.execute_batch(MIGRATION_V6).map_err(|e| {
                Error::Io(std::io::Error::other(format!("NoteStore migrate v6: {e}")))
            })?;
        }

        // v6 → v7: Recipe-author note kinds + structured payload column.
        // Six new kinds — `research_finding`, `capability_request`,
        // `recipe_issue`, `checkpoint`, `checkpoint_restored`,
        // `deferred_question` — plus a nullable `payload_json` TEXT
        // column for per-kind structured data (decision_kind on
        // `decision` rows, authority on `research_finding`, category
        // on `recipe_issue`, etc.). Rename-recreate because the CHECK
        // constraint changes; SQLite can't ALTER one in place.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version < 7 {
            conn.execute_batch(MIGRATION_V7).map_err(|e| {
                Error::Io(std::io::Error::other(format!("NoteStore migrate v7: {e}")))
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
        self.write_note_with_relation(
            kind, content, symbols, files, session_id, scope, feature_id, None,
        )
        .await
    }

    /// Persist a note with all of: explicit scope, optional feature
    /// id, and an optional `related_entity` anchor. The note is
    /// tagged `source = 'agent'` (the highest-confidence source).
    ///
    /// This is a back-compat wrapper over [`write_note_with_source`];
    /// new call sites that have a non-agent provenance (commit
    /// harvester, diff extractor, response miner, pattern matcher)
    /// should call `write_note_with_source` directly.
    ///
    /// The `related_entity` field is a free-text entity name —
    /// typically a Person / Organization name for `commitment` /
    /// `follow_up` kinds, an Initiative name for `goal`. It's not
    /// validated against the entity graph here (the graph is rebuilt
    /// each enrichment cycle, so a hard FK would be a write-time
    /// race); the Relational / Strategic digests match it at query
    /// time. `related_entity = None` matches the pre-v5 behaviour.
    #[allow(clippy::too_many_arguments)]
    pub async fn write_note_with_relation(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
    ) -> Result<String> {
        self.write_note_with_source(
            kind,
            content,
            symbols,
            files,
            session_id,
            scope,
            feature_id,
            related_entity,
            NoteSource::Agent,
            None,
        )
        .await
    }

    /// Full-fat write path with explicit provenance.
    ///
    /// `source` records where the note came from — agent (explicit
    /// `note` tool call), committed (commit-message harvest),
    /// extracted (LLM pass over session diff), inferred (regex over
    /// agent response text), or observed (tool-call pattern match).
    /// The audit assembly orders by source priority
    /// (agent > committed > extracted > inferred > observed) and
    /// renders attribution.
    ///
    /// `supersedes` carries the note id this note reverses, when
    /// applicable. NULL for first-time decisions. The audit display
    /// renders a `↳ REVERSED` line under the original on a match.
    ///
    /// CHECK enforcement on `source` is at the API layer (the
    /// [`NoteSource`] enum is the source of truth) rather than via
    /// SQL constraint — adding a new source becomes a one-line code
    /// change rather than a schema migration.
    ///
    /// Invariant: `scope == Feature` requires `feature_id.is_some()`;
    /// violators return [`Error::InvalidInput`].
    #[allow(clippy::too_many_arguments)]
    pub async fn write_note_with_source(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
        source: NoteSource,
        supersedes: Option<&str>,
    ) -> Result<String> {
        self.write_note_full(
            kind,
            content,
            symbols,
            files,
            session_id,
            scope,
            feature_id,
            related_entity,
            source,
            supersedes,
            None,
        )
        .await
    }

    /// Full-fat v7 write path that also accepts a structured
    /// `payload_json` blob for per-kind data (e.g. `decision_kind`
    /// on `decision` rows, `authority` on `research_finding`,
    /// `category`/`status` on `recipe_issue`). The string is stored
    /// verbatim — callers serialise their own JSON and own its
    /// schema. NULL is the valid "no payload" value and matches
    /// pre-v7 semantics.
    #[allow(clippy::too_many_arguments)]
    pub async fn write_note_full(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
        source: NoteSource,
        supersedes: Option<&str>,
        payload_json: Option<&str>,
    ) -> Result<String> {
        if scope == NoteScope::Feature && feature_id.is_none() {
            return Err(Error::InvalidInput(
                "write_note_full: scope='feature' requires feature_id".into(),
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = unix_now();
        let symbols_json = serde_json::to_string(&symbols).unwrap_or_else(|_| "[]".to_string());
        let files_json = serde_json::to_string(&files).unwrap_or_else(|_| "[]".to_string());

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO notes (id, kind, content, symbols, files, session_id, created_at, updated_at, scope, feature_id, related_entity, source, supersedes, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                id, kind, content, symbols_json, files_json, session_id, now,
                scope.as_str(), feature_id, related_entity, source.as_str(), supersedes, payload_json
            ],
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
                        scope, feature_id, promoted_from, related_entity,
                        source, supersedes, payload_json
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
                           n.scope, n.feature_id, n.promoted_from, n.related_entity,
                           n.source, n.supersedes, n.payload_json
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
                            scope, feature_id, promoted_from, related_entity,
                            source, supersedes, payload_json
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
                    scope, feature_id, promoted_from, related_entity,
                    source, supersedes, payload_json
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

    /// Return all active notes whose `related_entity` matches the given
    /// canonical name (case-sensitive match against the column value).
    /// Filters out retired notes and orders by `created_at DESC`. Used by
    /// the relational + strategic digests at splice time to find
    /// commitment / follow_up / goal notes anchored to an entity.
    ///
    /// `kinds`, when non-empty, restricts to a subset of note kinds —
    /// the digest passes `["commitment", "follow_up"]` for the
    /// relational block and `["goal"]` for the strategic block.
    pub async fn read_notes_by_related_entity(
        &self,
        related_entity: &str,
        kinds: &[&str],
    ) -> Result<Vec<NoteRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, content, symbols, files, session_id,
                        created_at, tool_name, retired_at, retired_by,
                        scope, feature_id, promoted_from, related_entity,
                        source, supersedes, payload_json
                 FROM notes
                 WHERE related_entity = ?1
                   AND retired_at IS NULL
                 ORDER BY created_at DESC",
            )
            .map_err(sqlite_err)?;
        let mapped = stmt
            .query_map(params![related_entity], map_note_row)
            .map_err(sqlite_err)?;
        let mut out = Vec::new();
        for row in mapped {
            let row = row.map_err(sqlite_err)?;
            if !kinds.is_empty() && !kinds.iter().any(|k| *k == row.kind) {
                continue;
            }
            out.push(row);
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
                        scope, feature_id, promoted_from, related_entity,
                        source, supersedes, payload_json
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

/// Full schema for brand-new databases. Sets `user_version = 1`; the
/// open path then steps the DB through migrations v1→v2→…→v5 so a
/// fresh install lands in the same final shape as an upgraded
/// install, with no schema drift between paths. Adding a new kind
/// or column means writing one new migration constant — never
/// editing this schema twice.
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
        related_entity: row.get(13)?,
        source: row.get(14)?,
        supersedes: row.get(15)?,
        payload_json: row.get(16)?,
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
        // Pre-v5 path: writes that don't go through
        // `write_note_with_relation` leave related_entity NULL.
        assert!(notes[0].related_entity.is_none());
    }

    #[tokio::test]
    async fn write_note_v5_kinds_round_trip() {
        // Each of the three new kinds must be admitted by the
        // CHECK constraint and round-trip through write → read.
        let store = make_store().await;
        for kind in ["commitment", "follow_up", "goal"] {
            let id = store
                .write_note(kind, &format!("test {kind}"), vec![], vec![], "s1")
                .await
                .unwrap_or_else(|e| panic!("kind {kind} should be accepted: {e}"));
            assert!(!id.is_empty());
        }
        let notes = store
            .read_notes(None, &[], &[], &[], 10, false)
            .await
            .unwrap();
        let kinds: std::collections::HashSet<_> =
            notes.iter().map(|n| n.kind.as_str()).collect();
        assert!(kinds.contains("commitment"));
        assert!(kinds.contains("follow_up"));
        assert!(kinds.contains("goal"));
    }

    #[tokio::test]
    async fn write_note_with_relation_persists_related_entity() {
        let store = make_store().await;
        let id = store
            .write_note_with_relation(
                "commitment",
                "send revised pricing to Sarah by Friday",
                vec![],
                vec![],
                "s1",
                NoteScope::Global,
                None,
                Some("Sarah Chen"),
            )
            .await
            .unwrap();
        let row = store.read_note_by_id(&id).await.unwrap().unwrap();
        assert_eq!(row.kind, "commitment");
        assert_eq!(row.related_entity.as_deref(), Some("Sarah Chen"));
    }

    #[tokio::test]
    async fn related_entity_filters_correctly_via_read_notes() {
        // The sql index `idx_notes_related_entity` is a partial
        // index — we don't query it here directly but we exercise
        // the post-filter path that production digests use.
        let store = make_store().await;
        store
            .write_note_with_relation(
                "commitment", "alpha", vec![], vec![], "s1",
                NoteScope::Global, None, Some("Sarah Chen"),
            )
            .await
            .unwrap();
        store
            .write_note_with_relation(
                "follow_up", "beta", vec![], vec![], "s1",
                NoteScope::Global, None, Some("Mike Torres"),
            )
            .await
            .unwrap();
        store
            .write_note_with_relation(
                "goal", "gamma", vec![], vec![], "s1",
                NoteScope::Global, None, None,
            )
            .await
            .unwrap();

        let all = store
            .read_notes(None, &[], &[], &[], 100, false)
            .await
            .unwrap();
        let with_entity: Vec<_> = all
            .iter()
            .filter(|n| n.related_entity.as_deref() == Some("Sarah Chen"))
            .collect();
        assert_eq!(with_entity.len(), 1);
        assert_eq!(with_entity[0].content, "alpha");
    }

    #[tokio::test]
    async fn read_notes_by_related_entity_returns_only_matching_active_notes() {
        // Three notes: two for Sarah (one commitment, one retired
        // commitment), one for Mike. The query must surface only the
        // active Sarah note — retired and unrelated rows are excluded.
        let store = make_store().await;
        let sarah_active = store
            .write_note_with_relation(
                "commitment", "send pricing", vec![], vec![], "s1",
                NoteScope::Global, None, Some("Sarah Chen"),
            )
            .await
            .unwrap();
        let sarah_retired = store
            .write_note_with_relation(
                "commitment", "old commitment", vec![], vec![], "s1",
                NoteScope::Global, None, Some("Sarah Chen"),
            )
            .await
            .unwrap();
        store
            .write_note_with_relation(
                "follow_up", "ping mike", vec![], vec![], "s1",
                NoteScope::Global, None, Some("Mike Torres"),
            )
            .await
            .unwrap();
        store
            .retire_by_id(&sarah_retired, "test")
            .await
            .unwrap();

        let all_for_sarah = store
            .read_notes_by_related_entity("Sarah Chen", &[])
            .await
            .unwrap();
        assert_eq!(all_for_sarah.len(), 1);
        assert_eq!(all_for_sarah[0].id, sarah_active);

        // Kind filter narrows the result.
        let goals_for_sarah = store
            .read_notes_by_related_entity("Sarah Chen", &["goal"])
            .await
            .unwrap();
        assert!(goals_for_sarah.is_empty());

        let commitments_for_sarah = store
            .read_notes_by_related_entity("Sarah Chen", &["commitment"])
            .await
            .unwrap();
        assert_eq!(commitments_for_sarah.len(), 1);

        // Unknown entity → empty.
        let none = store
            .read_notes_by_related_entity("Nobody", &[])
            .await
            .unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn write_note_rejects_unknown_kind_at_check_constraint() {
        let store = make_store().await;
        // The kind 'totally_invalid' is not in the CHECK list — the
        // SQLite layer must reject it. We don't validate kind in
        // the Rust API on purpose so new kinds can land in one PR
        // (schema migration) without source-side ceremony; the
        // CHECK constraint is the structural backstop.
        let r = store
            .write_note("totally_invalid", "x", vec![], vec![], "s1")
            .await;
        assert!(r.is_err(), "CHECK must reject unknown kind");
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

    // ── v5 → v6 migration (audit-hardening: source + supersedes) ──────────

    /// Build a v5 database by hand and confirm the v6 migration adds the
    /// two new columns + indexes without losing any rows. Pre-existing
    /// rows must default to `source = 'agent'` so the audit assembly
    /// continues to render them as the highest-priority source.
    #[tokio::test]
    async fn migrates_v5_to_v6_preserving_existing_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");

        // Build a v5 database manually: copy the schema we know v5 ends
        // with (no source/supersedes columns, kind CHECK admits the v5
        // set), insert one row, set user_version = 5.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE notes (
                     id            TEXT    PRIMARY KEY,
                     kind          TEXT    NOT NULL CHECK(kind IN (
                         'decision','attempt','invariant','todo','reflection',
                         'uncertainty','postmortem_pointer','redteam_finding',
                         'deviation','commitment','follow_up','goal'
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
                     promoted_from TEXT,
                     related_entity TEXT
                 );
                 CREATE VIRTUAL TABLE notes_fts USING fts5(
                     content, kind, content='notes', content_rowid='rowid'
                 );
                 CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
                     INSERT INTO notes_fts(rowid, content, kind)
                         VALUES (new.rowid, new.content, new.kind);
                 END;
                 CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
                     INSERT INTO notes_fts(notes_fts, rowid, content, kind)
                         VALUES ('delete', old.rowid, old.content, old.kind);
                 END;
                 CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
                     INSERT INTO notes_fts(notes_fts, rowid, content, kind)
                         VALUES ('delete', old.rowid, old.content, old.kind);
                     INSERT INTO notes_fts(rowid, content, kind)
                         VALUES (new.rowid, new.content, new.kind);
                 END;
                 CREATE TABLE meta_counters (key TEXT PRIMARY KEY, val INTEGER NOT NULL);
                 INSERT INTO meta_counters(key, val) VALUES ('notes_version', 0);
                 CREATE TABLE note_digest_cache (
                     scope_hash    TEXT    PRIMARY KEY,
                     digest_text   TEXT    NOT NULL,
                     notes_version INTEGER NOT NULL,
                     created_at    INTEGER NOT NULL
                 );
                 CREATE TABLE tool_call_log (
                     id         TEXT    PRIMARY KEY,
                     session_id TEXT    NOT NULL,
                     tool_name  TEXT    NOT NULL,
                     outcome    TEXT    NOT NULL,
                     called_at  INTEGER NOT NULL
                 );
                 INSERT INTO notes (
                     id, kind, content, session_id, created_at, updated_at,
                     scope
                 ) VALUES (
                     'pre-v6-row', 'decision', 'before migration',
                     'sess-1', 1000, 1000, 'global'
                 );
                 PRAGMA user_version = 5;
                 COMMIT;",
            )
            .unwrap();
        }

        // Now open through NoteStore — should run V6 and add columns.
        let store = NoteStore::open(&db_path).unwrap();

        // Pre-existing row gets source='agent', supersedes=NULL.
        let row = store
            .read_note_by_id("pre-v6-row")
            .await
            .unwrap()
            .expect("row preserved across v5→v6 migration");
        assert_eq!(row.kind, "decision");
        assert_eq!(row.content, "before migration");
        assert_eq!(row.source, "agent", "default source for pre-v6 rows");
        assert_eq!(row.supersedes, None, "no supersedes default");

        // user_version is at v6 or higher. Pinning the head version
        // here would force this test to be edited every schema bump
        // even though the v5→v6 invariants under test (source/
        // supersedes columns + indexes) don't change. Lower-bound
        // assertion captures the actual contract: opening a v5 db
        // must run v6's migration successfully.
        let conn = Connection::open(&db_path).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(v >= 6, "expected user_version >= 6 after migration, got {v}");

        // The two new indexes exist.
        let has_source_idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='index' AND name='idx_notes_source_created'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_source_idx, 1);

        let has_supersedes_idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='index' AND name='idx_notes_supersedes'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_supersedes_idx, 1);
    }

    // ── v6 → v7 migration (recipe-author kinds + payload_json) ────────────

    /// Build a v6 database by hand and confirm the v7 migration:
    ///
    /// 1. Adds the six new kinds to the CHECK constraint (verified
    ///    by writing a `research_finding` after the migration runs).
    /// 2. Adds the nullable `payload_json` column (verified by
    ///    reading back a written value).
    /// 3. Preserves every pre-v7 row, with `payload_json = NULL`.
    #[tokio::test]
    async fn migrates_v6_to_v7_preserving_existing_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");

        // Build a v6 database manually. v6 = v5 + (source, supersedes,
        // two indexes); we replicate enough to look like a real on-disk
        // v6 then bump user_version.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE notes (
                     id            TEXT    PRIMARY KEY,
                     kind          TEXT    NOT NULL CHECK(kind IN (
                         'decision','attempt','invariant','todo','reflection',
                         'uncertainty','postmortem_pointer','redteam_finding',
                         'deviation','commitment','follow_up','goal'
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
                     promoted_from TEXT,
                     related_entity TEXT,
                     source        TEXT    NOT NULL DEFAULT 'agent',
                     supersedes    TEXT
                 );
                 CREATE VIRTUAL TABLE notes_fts USING fts5(
                     content, kind, content='notes', content_rowid='rowid'
                 );
                 CREATE TRIGGER notes_fts_ai AFTER INSERT ON notes BEGIN
                     INSERT INTO notes_fts(rowid, content, kind)
                         VALUES (new.rowid, new.content, new.kind);
                 END;
                 CREATE TRIGGER notes_fts_ad BEFORE DELETE ON notes BEGIN
                     INSERT INTO notes_fts(notes_fts, rowid, content, kind)
                         VALUES ('delete', old.rowid, old.content, old.kind);
                 END;
                 CREATE TRIGGER notes_fts_au AFTER UPDATE ON notes BEGIN
                     INSERT INTO notes_fts(notes_fts, rowid, content, kind)
                         VALUES ('delete', old.rowid, old.content, old.kind);
                     INSERT INTO notes_fts(rowid, content, kind)
                         VALUES (new.rowid, new.content, new.kind);
                 END;
                 CREATE TABLE meta_counters (key TEXT PRIMARY KEY, val INTEGER NOT NULL);
                 INSERT INTO meta_counters(key, val) VALUES ('notes_version', 0);
                 CREATE TABLE note_digest_cache (
                     scope_hash    TEXT    PRIMARY KEY,
                     digest_text   TEXT    NOT NULL,
                     notes_version INTEGER NOT NULL,
                     created_at    INTEGER NOT NULL
                 );
                 CREATE TABLE tool_call_log (
                     id         TEXT    PRIMARY KEY,
                     session_id TEXT    NOT NULL,
                     tool_name  TEXT    NOT NULL,
                     outcome    TEXT    NOT NULL,
                     called_at  INTEGER NOT NULL
                 );
                 INSERT INTO notes (
                     id, kind, content, session_id, created_at, updated_at,
                     scope, source
                 ) VALUES (
                     'pre-v7-row', 'decision', 'before v7 migration',
                     'sess-1', 1000, 1000, 'global', 'agent'
                 );
                 PRAGMA user_version = 6;
                 COMMIT;",
            )
            .unwrap();
        }

        // Open through NoteStore — should run V7 and rebuild the table.
        let store = NoteStore::open(&db_path).unwrap();

        // Pre-existing row preserved with payload_json = NULL.
        let row = store
            .read_note_by_id("pre-v7-row")
            .await
            .unwrap()
            .expect("row preserved across v6→v7 migration");
        assert_eq!(row.kind, "decision");
        assert_eq!(row.payload_json, None, "pre-v7 rows default payload to NULL");

        // user_version is now 7.
        let conn = Connection::open(&db_path).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 7);

        // The new kinds are admitted by the rebuilt CHECK. Round-trip
        // a `research_finding` write through `write_note_full` to
        // confirm both the CHECK and the payload column work.
        let payload =
            r#"{"authority":"authoritative","host":"courtlistener.com"}"#;
        let id = store
            .write_note_full(
                "research_finding",
                "CourtListener documents API supports cursor pagination",
                vec![],
                vec![],
                "sess-1",
                NoteScope::Feature,
                Some("p1"),
                None,
                NoteSource::Agent,
                None,
                Some(payload),
            )
            .await
            .unwrap();
        let written = store
            .read_note_by_id(&id)
            .await
            .unwrap()
            .expect("research_finding row should round-trip");
        assert_eq!(written.kind, "research_finding");
        assert_eq!(written.payload_json.as_deref(), Some(payload));
    }

    /// New writes via `write_note_with_source` carry their explicit
    /// source through to the read path. Each of the five sources
    /// round-trips intact.
    #[tokio::test]
    async fn write_note_with_source_round_trips_each_source() {
        let store = make_store().await;

        for src in [
            NoteSource::Agent,
            NoteSource::Committed,
            NoteSource::Extracted,
            NoteSource::Inferred,
            NoteSource::Observed,
        ] {
            let id = store
                .write_note_with_source(
                    "decision",
                    &format!("from {}", src.as_str()),
                    vec![],
                    vec![],
                    "sess-source",
                    NoteScope::Global,
                    None,
                    None,
                    src,
                    None,
                )
                .await
                .unwrap();
            let row = store.read_note_by_id(&id).await.unwrap().unwrap();
            assert_eq!(row.source, src.as_str());
            assert_eq!(row.supersedes, None);
        }
    }

    /// `write_note_with_source` carries `supersedes` through, and the
    /// referenced original row is left untouched (the reversal is a
    /// display-time concept).
    #[tokio::test]
    async fn supersedes_threads_through_writes_without_mutating_original() {
        let store = make_store().await;
        let original = store
            .write_note(
                "decision",
                "BTreeMap for ordered iteration",
                vec![],
                vec![],
                "sess-rev",
            )
            .await
            .unwrap();

        let reversal = store
            .write_note_with_source(
                "decision",
                "HashMap — ordered iteration not actually needed",
                vec![],
                vec![],
                "sess-rev",
                NoteScope::Global,
                None,
                None,
                NoteSource::Extracted,
                Some(&original),
            )
            .await
            .unwrap();

        let original_row = store.read_note_by_id(&original).await.unwrap().unwrap();
        let reversal_row = store.read_note_by_id(&reversal).await.unwrap().unwrap();

        // Original is preserved verbatim — only the reversal carries
        // the link.
        assert_eq!(original_row.content, "BTreeMap for ordered iteration");
        assert_eq!(original_row.supersedes, None);
        assert_eq!(original_row.source, "agent");
        assert_eq!(reversal_row.supersedes.as_deref(), Some(original.as_str()));
        assert_eq!(reversal_row.source, "extracted");
    }

    /// `NoteSource::priority` orders the five sources from highest
    /// to lowest. The audit assembly relies on this order.
    #[test]
    fn note_source_priority_order_is_stable() {
        assert!(NoteSource::Agent.priority() > NoteSource::Committed.priority());
        assert!(NoteSource::Committed.priority() > NoteSource::Extracted.priority());
        assert!(NoteSource::Extracted.priority() > NoteSource::Inferred.priority());
        assert!(NoteSource::Inferred.priority() > NoteSource::Observed.priority());
    }
}
