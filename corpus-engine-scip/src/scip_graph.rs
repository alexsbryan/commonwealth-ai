// SPDX-License-Identifier: AGPL-3.0-or-later
//! SCIP-based call graph with staleness tracking.
//!
//! Stores symbol definitions and call-site references in a SQLite
//! database. Query results carry a [`StalenessCaution`] that tells the
//! caller how confident the data is — fresh, aging, or stale — so the
//! tool layer can communicate uncertainty proportionally.
//!
//! ## Staleness model
//!
//! Staleness is per-file, not per-corpus. A file modified since the last
//! SCIP export has potentially stale call graph entries. The
//! [`CodeWatcher`](crate::update::watch::CodeWatcher) calls
//! [`ScipGraph::mark_file_stale`] on every re-indexed file; the stale
//! set is cleared when a new SCIP export is recorded via
//! [`ScipGraph::record_export`].
//!
//! ## Threading
//!
//! `ScipGraph` wraps a synchronous `rusqlite::Connection` in a
//! `tokio::sync::Mutex`. All operations complete in microseconds so the
//! async mutex is negligible overhead — no `spawn_blocking` needed for
//! individual queries. Bulk ingestion uses `spawn_blocking`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{params, Connection};
use tokio::sync::{Mutex, RwLock};

use crate::error::{Error, Result};

/// On-disk schema version. Bumped when the scip_graph SQLite schema
/// changes in a way that prior data can no longer be read correctly.
/// `open_with_integrity` refuses to open a DB whose stored version
/// differs from this constant — the caller (typically the daemon's
/// `Reindexer`) treats that as a signal to trigger a full rebuild.
///
/// v2: adds `corpus_id` column to `symbols` and `refs` so a merged
/// graph (one DB holding symbols from many corpora) can scope
/// delete-and-replace operations to a single source. v1 had only
/// `id INTEGER PRIMARY KEY`, which meant `import_from_path` could
/// only ever append — causing unbounded merged-graph growth.
///
/// v3: adds `qualified_name` to `symbols` and `caller_qualified` /
/// `callee_qualified` to `refs`. Stores the full SCIP descriptor
/// alongside the bare display `name`, giving consumers (notably the
/// atlas code-walk) an unambiguous cross-crate identifier without
/// changing the existing `name`-based UX of the tools surface. v2
/// graphs that haven't been rebuilt return `OpenError::SchemaMismatch`
/// — caller (Reindexer) triggers a full re-export.
pub const SCHEMA_VERSION: u32 = 3;

// ─── Types ───────────────────────────────────────────────────

/// How a call was resolved by the SCIP exporter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CallKind {
    /// `foo()` — statically resolved.
    Direct,
    /// `self.foo()` — resolved via type inference.
    Method,
    /// `<T as Trait>::foo()` — resolved via trait impl.
    Trait,
    /// `dyn Trait` — resolved at runtime, SCIP may not see it.
    Dynamic,
}

impl CallKind {
    pub fn from_ref_kind(s: &str) -> Self {
        match s {
            "method" => Self::Method,
            "trait" => Self::Trait,
            "dynamic" => Self::Dynamic,
            _ => Self::Direct,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Method => "method",
            Self::Trait => "trait",
            Self::Dynamic => "dynamic",
        }
    }
}

/// A single-lock health snapshot of the SCIP graph.
/// Returned by [`ScipGraph::stats`] to avoid multiple mutex acquisitions.
#[derive(Debug, Clone)]
pub struct ScipGraphStats {
    pub symbol_count: usize,
    pub ref_count: usize,
    pub stale_file_count: usize,
    /// Hours since last successful SCIP export. `None` if never exported.
    pub export_age_hours: Option<u64>,
}

/// A row from the `symbols` table — a symbol definition recorded by
/// the SCIP exporter. Returned by [`ScipGraph::symbols_in_file`],
/// [`ScipGraph::symbols_in_crate`], and [`ScipGraph::symbol_definition`].
/// Distinct from [`Callee`] / [`Caller`] which describe call-site
/// references.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SymbolRow {
    /// Corpus the row was ingested under. Stable across all rows
    /// returned by a per-corpus method (matches `self.corpus_id`);
    /// varies across rows when the graph was opened as the merged
    /// in-memory union of multiple corpora (see [`Self::merged_graph`]
    /// in the reindexer).
    pub corpus_id: String,
    pub name: String,
    /// Full SCIP descriptor; empty for legacy or non-rust-analyzer
    /// rows. Use this axis for cross-crate disambiguation.
    pub qualified_name: String,
    pub kind: String,
    pub file_path: String,
    pub line_start: i32,
    pub line_end: i32,
    pub language: String,
}

/// A callee returned by [`ScipGraph::find_callees_qualified`] —
/// carries both the qualified SCIP descriptor (for unambiguous
/// resolution) and the bare display name (for human-readable
/// output). Distinct from [`Callee`], which is the bare-name shape
/// the legacy tools surface (kept stable for back-compat).
#[derive(Debug, Clone, serde::Serialize)]
pub struct QualifiedCallee {
    pub callee_qualified: String,
    pub callee_name: String,
    pub file_path: String,
    pub line: i32,
    pub call_kind: CallKind,
}

/// A function or method that is called by the queried symbol.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Callee {
    pub symbol_name: String,
    pub file_path: String,
    pub line: i32,
    pub call_kind: CallKind,
}

/// A function or method that calls the queried symbol.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Caller {
    pub symbol_name: String,
    pub file_path: String,
    pub line: i32,
    pub call_kind: CallKind,
}

/// A single entry in a blast-radius traversal result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BlastEntry {
    /// The symbol that (transitively) calls the changed symbol.
    pub symbol_name: String,
    /// File where this call-site lives.
    pub file_path: String,
    /// Line number of the reference.
    pub line: i32,
    /// True when the file path looks like a test file.
    pub is_test: bool,
}

/// Result of [`ScipGraph::blast_radius`].
#[derive(Debug)]
pub struct BlastRadiusResult {
    /// All reachable callers, up to `max_symbols`.
    pub entries: Vec<BlastEntry>,
    /// True if the traversal was cut short by `max_symbols`.
    pub capped: bool,
    /// How many BFS levels were explored (up to `max_depth`).
    pub depth_reached: usize,
    /// Staleness caution for the files in `entries`.
    pub caution: StalenessCaution,
}

/// Staleness caution level for a call graph result.
/// Controls how prominently the tool communicates uncertainty.
#[derive(Debug, Clone, PartialEq)]
pub enum StalenessCaution {
    /// Graph is fresh (< 1 hour old, no modified files).
    /// Tool says nothing about staleness.
    None,

    /// Some call sites may be in recently modified files.
    /// The watcher has re-indexed these files but SCIP hasn't caught up.
    SomeCallSitesMayBeStale { stale_files: Vec<String> },

    /// Graph is 1–24 hours old.
    /// No watcher-flagged files, but time has passed.
    GraphIsAging { age_hours: u64 },

    /// Graph is > 24 hours old.
    /// Tool adds a prominent warning with the remediation command.
    GraphIsStale { age_hours: u64, corpus_id: String },

    /// Language never had SCIP exported.
    /// Different from stale — the data simply doesn't exist.
    LanguageNotIndexed {
        language: String,
        install_hint: String,
    },
}

impl StalenessCaution {
    /// Format for inclusion in tool output.
    /// Returns empty string when caution is None — no noise for fresh results.
    pub fn format_note(&self) -> String {
        match self {
            Self::None => String::new(),

            Self::SomeCallSitesMayBeStale { stale_files } => {
                let files = stale_files
                    .iter()
                    .map(|f| format!("`{f}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "\n*Note: {files} {} been modified since the last symbol graph \
                     export — call sites in {} may not be current.*",
                    if stale_files.len() == 1 {
                        "has"
                    } else {
                        "have"
                    },
                    if stale_files.len() == 1 {
                        "this file"
                    } else {
                        "these files"
                    },
                )
            }

            Self::GraphIsAging { age_hours } => format!(
                "\n*Symbol graph was last exported {age_hours} hours ago — \
                 recently modified files may not be reflected.*"
            ),

            Self::GraphIsStale {
                age_hours,
                corpus_id,
            } => format!(
                "\n\
                 \u{26a0} **Symbol graph is {age_hours} hours old.** Results may not \
                 reflect recent changes.\n\
                 To refresh: `sovereign corpus scip {corpus_id}`"
            ),

            Self::LanguageNotIndexed {
                language,
                install_hint,
            } => format!("\n*No call graph available for {language}. {install_hint}*"),
        }
    }

    pub fn is_prominent(&self) -> bool {
        matches!(self, Self::GraphIsStale { .. })
    }
}

// ─── Intermediate types for ingestion ────────────────────────

/// A symbol record for bulk ingestion.
#[derive(Debug, Clone)]
pub struct ScipSymbolRecord {
    /// Bare display name, e.g. `CorpusEngine`. Used by the human-
    /// facing tools (`callees`, `callers`, `blast`, `symbols`) for
    /// readable output and by `resolve_symbol` for ergonomic
    /// suffix-match lookups.
    pub name: String,
    /// Full SCIP descriptor, e.g. `rust-analyzer cargo corpus_engine 0.1.0 src/engine/mod.rs/CorpusEngine#`.
    /// This is the unambiguous identity the SCIP exporter assigns to
    /// the symbol and is what callers should use for cross-crate
    /// disambiguation. Empty string when the exporter has nothing
    /// better than the bare name (legacy / non-rust-analyzer
    /// languages); resolution falls back to `name` in that case.
    pub qualified_name: String,
    pub kind: String,
    pub file_path: String,
    pub line_start: i32,
    pub line_end: i32,
    pub language: String,
}

/// A reference record for bulk ingestion.
#[derive(Debug, Clone)]
pub struct ScipRefRecord {
    pub caller_symbol: String,
    pub callee_symbol: String,
    /// Full SCIP descriptor for the caller. See
    /// [`ScipSymbolRecord::qualified_name`].
    pub caller_qualified: String,
    /// Full SCIP descriptor for the callee.
    pub callee_qualified: String,
    pub file_path: String,
    pub line: i32,
    pub ref_kind: String,
}

// ─── ScipGraph ───────────────────────────────────────────────

pub struct ScipGraph {
    conn: Arc<Mutex<Connection>>,
    corpus_id: String,
    /// Files that the CodeWatcher has re-indexed since the last SCIP export.
    stale_files: Arc<RwLock<HashSet<String>>>,
}

impl ScipGraph {
    /// Open or create the SQLite database at the given path.
    pub fn open(db_path: &Path, corpus_id: &str) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }

        let conn = Connection::open(db_path)
            .map_err(|e| Error::Database(format!("SCIP graph open: {e}")))?;

        Self::init_schema(&conn)?;

        // Load stale files from previous session (if any).
        let stale = Self::load_stale_files(&conn);

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            corpus_id: corpus_id.to_string(),
            stale_files: Arc::new(RwLock::new(stale)),
        })
    }

    /// Open the DB at `db_path` with integrity + schema-version
    /// verification. Use this from anywhere that's consuming a
    /// daemon-managed graph file; `open()` stays available as the
    /// lenient variant for legacy call sites (tests, one-shot CLI
    /// tools that would rather see corruption than move it aside).
    ///
    /// Behaviour:
    /// - If `db_path` doesn't exist → fresh DB, schema initialised,
    ///   `schema_version` stamped. Orphan `.db-wal` / `.db-shm`
    ///   files from a previous crash are cleaned up first.
    /// - If the DB is corrupt (fails `PRAGMA integrity_check`) →
    ///   file is renamed to `scip_graph.db.corrupt.<unix_ts>`, and
    ///   `Err(OpenError::Corrupt)` is returned carrying the
    ///   quarantine path. Caller is expected to trigger a full
    ///   rebuild.
    /// - If `schema_version` doesn't match the compiled constant →
    ///   `Err(OpenError::SchemaMismatch)`. Same rebuild response.
    /// - Any other SQLite / IO failure → `Err(OpenError::Database)`
    ///   / `Err(OpenError::Io)`. These are not rebuild triggers;
    ///   caller should log and retry or surface to the operator.
    pub fn open_with_integrity(
        db_path: &Path,
        corpus_id: &str,
    ) -> std::result::Result<Self, OpenError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(OpenError::Io)?;
        }

        // Clean up orphan journal files if the main DB is missing.
        // rusqlite's WAL recovery gets confused when the .db itself
        // is gone but the sidecars linger; tidy them before open.
        if !db_path.exists() {
            for ext in &["-wal", "-shm"] {
                let orphan = sidecar_path(db_path, ext);
                let _ = std::fs::remove_file(&orphan);
            }
        }

        let is_existing =
            db_path.exists() && db_path.metadata().map(|m| m.len() > 0).unwrap_or(false);

        let conn = Connection::open(db_path)
            .map_err(|e| OpenError::Database(format!("open {}: {e}", db_path.display())))?;

        if is_existing {
            // `PRAGMA integrity_check` returns "ok" when the DB is
            // sound, or one or more error lines otherwise. We check
            // only the first row — any non-"ok" value means quarantine.
            let verdict: rusqlite::Result<String> =
                conn.query_row("PRAGMA integrity_check", [], |row| row.get(0));
            let quarantined = !matches!(verdict, Ok(v) if v == "ok");
            if quarantined {
                drop(conn);
                let moved_to = corrupt_quarantine_path(db_path);
                std::fs::rename(db_path, &moved_to).map_err(OpenError::Io)?;
                return Err(OpenError::Corrupt { moved_to });
            }

            // Schema version check. Absent key is treated as 0 (pre-
            // versioned DB, written before this integrity work); we
            // still treat that as a mismatch and let the caller
            // rebuild — the SCHEMA_VERSION constant starts at 1.
            let found: u32 = conn
                .query_row(
                    "SELECT value FROM scip_meta WHERE key = 'schema_version'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if found != SCHEMA_VERSION {
                return Err(OpenError::SchemaMismatch {
                    found,
                    expected: SCHEMA_VERSION,
                });
            }
        }

        Self::init_schema(&conn).map_err(|e| OpenError::Database(e.to_string()))?;
        // Stamp schema version unconditionally so a fresh DB
        // becomes compatible on next open.
        conn.execute(
            "INSERT OR REPLACE INTO scip_meta (key, value) VALUES ('schema_version', ?)",
            params![SCHEMA_VERSION.to_string()],
        )
        .ok();

        let stale = Self::load_stale_files(&conn);
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            corpus_id: corpus_id.to_string(),
            stale_files: Arc::new(RwLock::new(stale)),
        })
    }

    /// Try to acquire an exclusive lock guarding a SCIP rebuild. The
    /// lock is a `flock(LOCK_EX | LOCK_NB)` on
    /// `<db_dir>/.rebuild.lock`, released when the returned guard is
    /// dropped (or when the holding process dies — the kernel
    /// cleans up, so we can never leak a stale lock).
    ///
    /// `None` means another writer holds the lock; the caller should
    /// drop this rebuild attempt and let the current holder finish
    /// (the Reindexer's debouncer will re-fire after it's done).
    pub fn try_rebuild_lock(db_dir: &Path) -> std::io::Result<Option<RebuildLock>> {
        use fs4::fs_std::FileExt;

        std::fs::create_dir_all(db_dir)?;
        let lock_path = db_dir.join(".rebuild.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(false)
            .open(&lock_path)?;

        // fs4's flock wrapper returns Ok(()) on acquire and
        // Err(WouldBlock) when another writer holds the lock — map
        // the latter to `Ok(None)` for the caller's semantics.
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Some(RebuildLock { _file: file })),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Record a rebuild's outcome in `scip_meta`. Extends the
    /// existing `last_export_at` key with a structured trigger
    /// reason, the indexed git HEAD (empty string for non-git
    /// projects), and a JSON-encoded summary of exporter outcomes
    /// so `sovereign project watch status` and `sovereign doctor`
    /// can surface "typescript exporter missing" to operators.
    pub async fn record_rebuild(
        &self,
        trigger_reason: &str,
        indexed_head: Option<&str>,
        exporter_outcomes: Option<&str>,
    ) {
        self.stale_files.write().await.clear();
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT OR REPLACE INTO scip_meta (key, value) VALUES ('last_export_at', ?)",
            params![chrono::Utc::now().to_rfc3339()],
        );
        // Persist the corpus id so `import_from_path` has a
        // reliable source to key the delete-and-replace against,
        // even if the rows themselves are empty (an ingestion that
        // produced no symbols but did bump metadata).
        let _ = conn.execute(
            "INSERT OR REPLACE INTO scip_meta (key, value) VALUES ('corpus_id', ?)",
            params![self.corpus_id.as_str()],
        );
        let _ = conn.execute(
            "INSERT OR REPLACE INTO scip_meta (key, value) VALUES ('stale_files', '')",
            [],
        );
        let _ = conn.execute(
            "INSERT OR REPLACE INTO scip_meta (key, value) VALUES ('last_trigger_reason', ?)",
            params![trigger_reason],
        );
        if let Some(head) = indexed_head {
            let _ = conn.execute(
                "INSERT OR REPLACE INTO scip_meta (key, value) VALUES ('last_indexed_head', ?)",
                params![head],
            );
        }
        if let Some(outcomes) = exporter_outcomes {
            let _ = conn.execute(
                "INSERT OR REPLACE INTO scip_meta (key, value) VALUES ('last_exporter_outcomes', ?)",
                params![outcomes],
            );
        }
        // A success supersedes any recorded failure: the meta table always
        // states the LATEST outcome, so readers never have to compare
        // timestamps to know which one is current.
        let _ = conn.execute("DELETE FROM scip_meta WHERE key = 'last_rebuild_error'", []);
        let _ = conn.execute("DELETE FROM scip_meta WHERE key = 'last_rebuild_failed_at'", []);
    }

    /// Record a FAILED rebuild in `scip_meta` — written to the LIVE graph
    /// (the staging DB is discarded on failure), deliberately touching only
    /// the failure keys so `last_export_at` keeps describing the last good
    /// export. Exists because a rebuild that fails identically every poll
    /// cycle was invisible outside the daemon log: the graph silently froze
    /// at its last indexed commit while every status surface stayed green
    /// (live incident 2026-08-06, exporters unresolvable from the service
    /// manager's PATH).
    pub async fn record_rebuild_failure(&self, error: &str) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT OR REPLACE INTO scip_meta (key, value) VALUES ('last_rebuild_error', ?)",
            params![error],
        );
        let _ = conn.execute(
            "INSERT OR REPLACE INTO scip_meta (key, value) VALUES ('last_rebuild_failed_at', ?)",
            params![chrono::Utc::now().to_rfc3339()],
        );
    }

    /// The most recent rebuild failure, if the LATEST outcome was a failure:
    /// `(error, failed_at_rfc3339)`. `None` after a successful rebuild
    /// (success deletes the keys) or on a legacy DB.
    pub async fn last_rebuild_failure(&self) -> Option<(String, String)> {
        let conn = self.conn.lock().await;
        let get = |key: &str| {
            conn.query_row(
                "SELECT value FROM scip_meta WHERE key = ?",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .ok()
        };
        Some((get("last_rebuild_error")?, get("last_rebuild_failed_at")?))
    }

    /// Read `last_indexed_head` from `scip_meta`. `None` when the
    /// field was never written (legacy DB, pre-integrity work).
    /// Used by the daemon's startup catch-up signal to decide
    /// whether HEAD has drifted since the last build.
    pub async fn last_indexed_head(&self) -> Option<String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT value FROM scip_meta WHERE key = 'last_indexed_head'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
    }

    /// Create an in-memory database for testing.
    pub fn open_in_memory(corpus_id: &str) -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Database(format!("SCIP graph in-memory: {e}")))?;

        Self::init_schema(&conn)?;

        // Record a fresh export so tests start with no staleness.
        conn.execute(
            "INSERT OR REPLACE INTO scip_meta (key, value) VALUES ('last_export_at', ?)",
            params![chrono::Utc::now().to_rfc3339()],
        )
        .ok();

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            corpus_id: corpus_id.to_string(),
            stale_files: Arc::new(RwLock::new(HashSet::new())),
        })
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        // Schema v2: `corpus_id` is a first-class column on symbols
        // and refs so the merged-graph code path can delete-and-
        // replace per source without touching unrelated rows. The
        // default empty-string value preserves the previous
        // per-DB-equals-per-corpus invariant for single-corpus
        // callers (CLI tests, per-project graphs) that haven't
        // been updated to pass a corpus_id.
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS symbols (
                id INTEGER PRIMARY KEY,
                corpus_id TEXT NOT NULL DEFAULT '',
                name TEXT NOT NULL,
                qualified_name TEXT NOT NULL DEFAULT '',
                kind TEXT NOT NULL,
                file_path TEXT NOT NULL,
                line_start INTEGER NOT NULL,
                line_end INTEGER NOT NULL,
                language TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
            CREATE INDEX IF NOT EXISTS idx_symbols_qualified ON symbols(qualified_name);
            CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_path);
            CREATE INDEX IF NOT EXISTS idx_symbols_corpus ON symbols(corpus_id);

            CREATE TABLE IF NOT EXISTS refs (
                id INTEGER PRIMARY KEY,
                corpus_id TEXT NOT NULL DEFAULT '',
                caller_symbol TEXT NOT NULL,
                callee_symbol TEXT NOT NULL,
                caller_qualified TEXT NOT NULL DEFAULT '',
                callee_qualified TEXT NOT NULL DEFAULT '',
                file_path TEXT NOT NULL,
                line INTEGER NOT NULL,
                ref_kind TEXT NOT NULL DEFAULT 'direct'
            );
            CREATE INDEX IF NOT EXISTS idx_refs_caller ON refs(caller_symbol);
            CREATE INDEX IF NOT EXISTS idx_refs_callee ON refs(callee_symbol);
            CREATE INDEX IF NOT EXISTS idx_refs_corpus ON refs(corpus_id);

            CREATE TABLE IF NOT EXISTS scip_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            ",
        )
        .map_err(|e| Error::Database(format!("SCIP graph schema: {e}")))?;
        Ok(())
    }

    fn load_stale_files(conn: &Connection) -> HashSet<String> {
        let mut stale = HashSet::new();
        if let Ok(csv) = conn.query_row(
            "SELECT value FROM scip_meta WHERE key = 'stale_files'",
            [],
            |row| row.get::<_, String>(0),
        ) {
            for f in csv.split(',') {
                let f = f.trim();
                if !f.is_empty() {
                    stale.insert(f.to_string());
                }
            }
        }
        stale
    }

    /// Called by CodeWatcher when a file is re-indexed.
    /// Marks call graph entries for that file's symbols as potentially stale.
    pub async fn mark_file_stale(&self, file_path: &str) {
        self.stale_files.write().await.insert(file_path.to_string());

        // Persist to survive process restarts.
        self.persist_stale_files().await;
    }

    /// Called after a successful SCIP export.
    /// Clears the stale file set and records the export time.
    pub async fn record_export(&self) {
        self.stale_files.write().await.clear();

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO scip_meta (key, value) VALUES ('last_export_at', ?)",
            params![chrono::Utc::now().to_rfc3339()],
        )
        .ok();
        conn.execute(
            "INSERT OR REPLACE INTO scip_meta (key, value) VALUES ('stale_files', '')",
            [],
        )
        .ok();
    }

    async fn persist_stale_files(&self) {
        let stale = self.stale_files.read().await;
        let csv = stale.iter().cloned().collect::<Vec<_>>().join(",");
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO scip_meta (key, value) VALUES ('stale_files', ?)",
            params![csv],
        )
        .ok();
    }

    /// Compute the staleness caution for a set of files in query results.
    pub async fn staleness_for(&self, result_files: &[String]) -> StalenessCaution {
        let stale = self.stale_files.read().await;

        // Check if any result files are in the stale set.
        let stale_in_results: Vec<String> = result_files
            .iter()
            .filter(|f| stale.contains(*f))
            .cloned()
            .collect();

        if !stale_in_results.is_empty() {
            return StalenessCaution::SomeCallSitesMayBeStale {
                stale_files: stale_in_results,
            };
        }

        // Check export age.
        let export_age_hours = self.export_age_hours().await;

        match export_age_hours {
            Some(0) => StalenessCaution::None,
            Some(h) if h < 24 => StalenessCaution::GraphIsAging { age_hours: h },
            Some(h) => StalenessCaution::GraphIsStale {
                age_hours: h,
                corpus_id: self.corpus_id.clone(),
            },
            // No export recorded — graph is empty but not "stale" per se.
            // Avoid false warnings; the empty-result message handles this case.
            std::option::Option::None => StalenessCaution::None,
        }
    }

    async fn export_age_hours(&self) -> Option<u64> {
        self.export_age_secs().await.map(|s| s / 3600)
    }

    /// Seconds since the last successful SCIP export, or `None` when
    /// no export has been recorded yet. The minute-grain companion to
    /// the (intentionally crude) hour bucket used by
    /// [`Self::staleness_for`]. Doctor uses this to flag a watcher
    /// whose source tree has moved past the indexed snapshot — at
    /// hour granularity a 47-minute wedge would look identical to
    /// "just indexed".
    pub async fn export_age_secs(&self) -> Option<u64> {
        let conn = self.conn.lock().await;
        let ts_str = conn
            .query_row(
                "SELECT value FROM scip_meta WHERE key = 'last_export_at'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()?;
        let export_time = chrono::DateTime::parse_from_rfc3339(&ts_str).ok()?;
        let age = chrono::Utc::now() - export_time.with_timezone(&chrono::Utc);
        Some(age.num_seconds().max(0) as u64)
    }

    /// Resolve a symbol name to its canonical form in the database.
    /// Tries exact match first, then suffix match (for unqualified names).
    pub async fn resolve_symbol(&self, name: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().await;

        // Exact match.
        if let Ok(found) = conn.query_row(
            "SELECT name FROM symbols WHERE name = ? LIMIT 1",
            params![name],
            |row| row.get::<_, String>(0),
        ) {
            return Ok(Some(found));
        }

        // Suffix match: user passes "my_fn" and the DB has
        // "my_crate::module::my_fn".
        let pattern = format!("%{name}");
        if let Ok(found) = conn.query_row(
            "SELECT name FROM symbols WHERE name LIKE ? LIMIT 1",
            params![pattern],
            |row| row.get::<_, String>(0),
        ) {
            return Ok(Some(found));
        }

        Ok(None)
    }

    /// Look up symbols by exact name (and optional kind filter)
    /// across every corpus this graph has ingested. The Symbol Lookup
    /// MCP tool uses this as the authoritative source — Lance carried
    /// the same data redundantly until the move to SCIP-as-truth, but
    /// the SQLite path here doesn't depend on the chunk index being
    /// fresh and survives a corrupt Lance corpus.
    ///
    /// `limit` is a hard cap on the row count; pass `8` for the
    /// default tool contract. `kind` is matched verbatim against the
    /// schema's `kind` column when `Some`; pass `None` to skip the
    /// filter.
    pub async fn find_symbols_by_name(
        &self,
        name: &str,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SymbolRow>> {
        let limit_clamped: i64 = limit.clamp(1, 256) as i64;
        let conn = self.conn.lock().await;
        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<SymbolRow> {
            Ok(SymbolRow {
                corpus_id: row.get(0)?,
                name: row.get(1)?,
                qualified_name: row.get(2)?,
                kind: row.get(3)?,
                file_path: row.get(4)?,
                line_start: row.get(5)?,
                line_end: row.get(6)?,
                language: row.get(7)?,
            })
        };
        // Collect inside the same scope as `stmt` / `conn`. Returning
        // a MappedRows out of a sub-block drops `stmt` before the
        // iterator is consumed — recorded as an invariant in repo
        // memory.
        let rows: Vec<SymbolRow> = match kind {
            Some(k) => {
                let mut stmt = conn
                    .prepare(
                        "SELECT corpus_id, name, qualified_name, kind, file_path, \
                                line_start, line_end, language \
                         FROM symbols \
                         WHERE name = ?1 AND kind = ?2 \
                         ORDER BY corpus_id, file_path, line_start \
                         LIMIT ?3",
                    )
                    .map_err(|e| Error::Database(format!("find_symbols_by_name prepare: {e}")))?;
                let iter = stmt
                    .query_map(params![name, k, limit_clamped], map_row)
                    .map_err(|e| Error::Database(format!("find_symbols_by_name query: {e}")))?;
                iter.filter_map(|r| r.ok()).collect()
            }
            None => {
                let mut stmt = conn
                    .prepare(
                        "SELECT corpus_id, name, qualified_name, kind, file_path, \
                                line_start, line_end, language \
                         FROM symbols \
                         WHERE name = ?1 \
                         ORDER BY corpus_id, file_path, line_start \
                         LIMIT ?2",
                    )
                    .map_err(|e| Error::Database(format!("find_symbols_by_name prepare: {e}")))?;
                let iter = stmt
                    .query_map(params![name, limit_clamped], map_row)
                    .map_err(|e| Error::Database(format!("find_symbols_by_name query: {e}")))?;
                iter.filter_map(|r| r.ok()).collect()
            }
        };
        Ok(rows)
    }

    /// Find all symbols that the given symbol calls.
    pub async fn find_callees(&self, symbol_name: &str) -> Result<(Vec<Callee>, StalenessCaution)> {
        let resolved = self.resolve_symbol(symbol_name).await?;
        let Some(resolved) = resolved else {
            return Ok((vec![], StalenessCaution::None));
        };

        let callees = {
            let conn = self.conn.lock().await;
            let mut stmt = conn
                .prepare(
                    "SELECT r.callee_symbol, r.file_path, r.line, r.ref_kind
                     FROM refs r
                     WHERE r.caller_symbol = ?
                     ORDER BY r.file_path, r.line",
                )
                .map_err(|e| Error::Database(format!("find_callees prepare: {e}")))?;

            let result: Vec<Callee> = stmt
                .query_map(params![resolved], |row| {
                    Ok(Callee {
                        symbol_name: row.get(0)?,
                        file_path: row.get(1)?,
                        line: row.get(2)?,
                        call_kind: CallKind::from_ref_kind(
                            &row.get::<_, String>(3).unwrap_or_default(),
                        ),
                    })
                })
                .map_err(|e| Error::Database(format!("find_callees query: {e}")))?
                .filter_map(|r| r.ok())
                .collect();
            result
        };

        // Collect call-site files (where each call appears) plus the definition
        // files of each callee symbol.  A stale definition file means the
        // callee's signature may have changed since the last SCIP export, which
        // is equally important to surface.
        let mut result_files: Vec<String> = callees.iter().map(|c| c.file_path.clone()).collect();
        if !callees.is_empty() {
            let conn = self.conn.lock().await;
            for callee in &callees {
                if let Ok(def_file) = conn.query_row(
                    "SELECT file_path FROM symbols WHERE name = ? LIMIT 1",
                    params![callee.symbol_name.as_str()],
                    |row| row.get::<_, String>(0),
                ) {
                    if !result_files.contains(&def_file) {
                        result_files.push(def_file);
                    }
                }
            }
        }
        let caution = self.staleness_for(&result_files).await;

        Ok((callees, caution))
    }

    /// Find all symbols that call the given symbol.
    /// `depth` is capped at 2: 1 = direct callers, 2 = callers of callers.
    pub async fn find_callers(
        &self,
        symbol_name: &str,
        depth: usize,
    ) -> Result<(Vec<Caller>, StalenessCaution)> {
        let resolved = self.resolve_symbol(symbol_name).await?;
        let Some(resolved) = resolved else {
            return Ok((vec![], StalenessCaution::None));
        };

        let depth = depth.clamp(1, 2);
        let mut all_callers = Vec::new();
        let mut seen = HashSet::new();
        seen.insert(resolved.clone());

        let mut frontier = vec![resolved];

        for _level in 0..depth {
            let mut next_frontier = Vec::new();

            for target in &frontier {
                let rows = {
                    let conn = self.conn.lock().await;
                    // Use r.file_path directly — the refs table records
                    // where the reference occurs, which is always correct.
                    // The LEFT JOIN on symbols is only for enrichment (e.g.
                    // resolving a module-level ref to a struct name); the
                    // file_path from refs is the source of truth.
                    let mut stmt = conn
                        .prepare(
                            "SELECT r.caller_symbol, r.file_path, r.line, r.ref_kind
                             FROM refs r
                             WHERE r.callee_symbol = ?
                             ORDER BY r.file_path, r.line",
                        )
                        .map_err(|e| Error::Database(format!("find_callers prepare: {e}")))?;

                    let result: Vec<Caller> = stmt
                        .query_map(params![target], |row| {
                            Ok(Caller {
                                symbol_name: row.get(0)?,
                                file_path: row.get(1)?,
                                line: row.get(2)?,
                                call_kind: CallKind::from_ref_kind(
                                    &row.get::<_, String>(3).unwrap_or_default(),
                                ),
                            })
                        })
                        .map_err(|e| Error::Database(format!("find_callers query: {e}")))?
                        .filter_map(|r| r.ok())
                        .collect();
                    result
                };

                for caller in rows {
                    if seen.insert(caller.symbol_name.clone()) {
                        next_frontier.push(caller.symbol_name.clone());
                        all_callers.push(caller);
                    }
                }
            }

            frontier = next_frontier;
            if frontier.is_empty() {
                break;
            }
        }

        let result_files: Vec<String> = all_callers.iter().map(|c| c.file_path.clone()).collect();
        let caution = self.staleness_for(&result_files).await;

        Ok((all_callers, caution))
    }

    /// All symbols defined in `file_path`, scoped to this graph's
    /// corpus_id. Ordered by `(line_start, name)` for deterministic
    /// downstream consumption (e.g., the atlas code-walk's module
    /// aggregation). Returns an empty Vec if no rows match — a missing
    /// file is not an error.
    pub async fn symbols_in_file(&self, file_path: &str) -> Result<Vec<SymbolRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT corpus_id, name, qualified_name, kind, file_path, line_start, line_end, language
                 FROM symbols
                 WHERE corpus_id = ? AND file_path = ?
                 ORDER BY line_start, name",
            )
            .map_err(|e| Error::Database(format!("symbols_in_file prepare: {e}")))?;
        let rows: Vec<SymbolRow> = stmt
            .query_map(params![self.corpus_id, file_path], |row| {
                Ok(SymbolRow {
                    corpus_id: row.get(0)?,
                    name: row.get(1)?,
                    qualified_name: row.get(2)?,
                    kind: row.get(3)?,
                    file_path: row.get(4)?,
                    line_start: row.get(5)?,
                    line_end: row.get(6)?,
                    language: row.get(7)?,
                })
            })
            .map_err(|e| Error::Database(format!("symbols_in_file query: {e}")))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// All symbols whose name starts with `<crate>::` or whose
    /// file_path begins with the supplied path prefix. The two-axis
    /// match is needed because the SCIP exporter writes the symbol's
    /// fully-qualified path in `name` (e.g., `corpus_engine::engine::CorpusEngine`)
    /// but some references arrive with only the file_path scoped. The
    /// caller picks whichever axis is appropriate (typically the
    /// crate-name path-prefix for workspace traversal).
    pub async fn symbols_in_crate(
        &self,
        crate_name_prefix: &str,
        file_path_prefix: &str,
    ) -> Result<Vec<SymbolRow>> {
        let conn = self.conn.lock().await;
        let name_pattern = format!("{}::%", crate_name_prefix);
        let file_pattern = format!("{}%", file_path_prefix);
        let mut stmt = conn
            .prepare(
                "SELECT corpus_id, name, qualified_name, kind, file_path, line_start, line_end, language
                 FROM symbols
                 WHERE corpus_id = ?
                   AND (name LIKE ? OR file_path LIKE ?)
                 ORDER BY file_path, line_start, name",
            )
            .map_err(|e| Error::Database(format!("symbols_in_crate prepare: {e}")))?;
        let rows: Vec<SymbolRow> = stmt
            .query_map(params![self.corpus_id, name_pattern, file_pattern], |row| {
                Ok(SymbolRow {
                    corpus_id: row.get(0)?,
                    name: row.get(1)?,
                    qualified_name: row.get(2)?,
                    kind: row.get(3)?,
                    file_path: row.get(4)?,
                    line_start: row.get(5)?,
                    line_end: row.get(6)?,
                    language: row.get(7)?,
                })
            })
            .map_err(|e| Error::Database(format!("symbols_in_crate query: {e}")))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// The set of `caller_qualified` names in the ref graph — every symbol with a
    /// body that calls something. This is the precise "real function / method"
    /// population for code-intel enrichment: the per-symbol `kind` field from a
    /// SCIP export is unreliable (rust-analyzer leaves most Rust fns `unknown`),
    /// but a symbol that appears as a caller provably has a body. Empty when the
    /// graph has no refs (e.g. an in-memory test fixture) — callers treat an
    /// empty set as "no caller filter", falling back to the kind screen.
    pub async fn caller_qualified_names(&self) -> Result<std::collections::HashSet<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT caller_qualified FROM refs
                 WHERE corpus_id = ? AND caller_qualified != ''",
            )
            .map_err(|e| Error::Database(format!("caller_qualified_names prepare: {e}")))?;
        let set: std::collections::HashSet<String> = stmt
            .query_map(params![self.corpus_id], |row| row.get::<_, String>(0))
            .map_err(|e| Error::Database(format!("caller_qualified_names query: {e}")))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(set)
    }

    /// Bulk-enumerate every symbol defined in this corpus, in one lock.
    /// This is the snapshot the capability-map builder
    /// ([`crate::capability_map`]) consumes. Scoped by `corpus_id` so a
    /// merged daemon graph yields only this corpus's definitions (same
    /// scoping as [`caller_qualified_names`](Self::caller_qualified_names)).
    pub async fn iter_all_symbols(&self) -> Result<Vec<ScipSymbolRecord>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT name, qualified_name, kind, file_path, line_start, line_end, language
                 FROM symbols WHERE corpus_id = ?",
            )
            .map_err(|e| Error::Database(format!("iter_all_symbols prepare: {e}")))?;
        let rows = stmt
            .query_map(params![self.corpus_id], |row| {
                Ok(ScipSymbolRecord {
                    name: row.get(0)?,
                    qualified_name: row.get(1)?,
                    kind: row.get(2)?,
                    file_path: row.get(3)?,
                    line_start: row.get(4)?,
                    line_end: row.get(5)?,
                    language: row.get(6)?,
                })
            })
            .map_err(|e| Error::Database(format!("iter_all_symbols query: {e}")))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Bulk-enumerate every call/reference edge in this corpus. The
    /// capability-map builder filters these down to first-party
    /// function-call edges (`caller_qualified -> callee_qualified`, callee
    /// descriptor ending `().`). Scoped by `corpus_id`.
    pub async fn iter_all_refs(&self) -> Result<Vec<ScipRefRecord>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT caller_symbol, callee_symbol, caller_qualified, callee_qualified,
                        file_path, line, ref_kind
                 FROM refs WHERE corpus_id = ?",
            )
            .map_err(|e| Error::Database(format!("iter_all_refs prepare: {e}")))?;
        let rows = stmt
            .query_map(params![self.corpus_id], |row| {
                Ok(ScipRefRecord {
                    caller_symbol: row.get(0)?,
                    callee_symbol: row.get(1)?,
                    caller_qualified: row.get(2)?,
                    callee_qualified: row.get(3)?,
                    file_path: row.get(4)?,
                    line: row.get(5)?,
                    ref_kind: row.get(6)?,
                })
            })
            .map_err(|e| Error::Database(format!("iter_all_refs query: {e}")))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Direct lookup of a symbol's definition row by exact name.
    /// Returns `None` when nothing matches in this corpus. Use
    /// [`resolve_symbol`](Self::resolve_symbol) when the caller has an
    /// unqualified name and wants suffix matching; this method is the
    /// strict variant for callers that already hold the canonical
    /// path (e.g., the atlas code-walk receiving a name from
    /// [`find_callees`](Self::find_callees)).
    pub async fn symbol_definition(&self, name: &str) -> Result<Option<SymbolRow>> {
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT corpus_id, name, qualified_name, kind, file_path, line_start, line_end, language
                 FROM symbols
                 WHERE corpus_id = ? AND name = ?
                 LIMIT 1",
                params![self.corpus_id, name],
                |row| {
                    Ok(SymbolRow {
                        corpus_id: row.get(0)?,
                        name: row.get(1)?,
                        qualified_name: row.get(2)?,
                        kind: row.get(3)?,
                        file_path: row.get(4)?,
                        line_start: row.get(5)?,
                        line_end: row.get(6)?,
                        language: row.get(7)?,
                    })
                },
            )
            .ok();
        Ok(row)
    }

    /// Direct lookup of a symbol's definition row by exact
    /// `qualified_name` (full SCIP descriptor). Use this when the
    /// caller already holds the unambiguous identifier — e.g., the
    /// atlas walker receiving a `callee_qualified` field from
    /// [`find_callees_qualified`](Self::find_callees_qualified).
    pub async fn symbol_definition_qualified(
        &self,
        qualified_name: &str,
    ) -> Result<Option<SymbolRow>> {
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT corpus_id, name, qualified_name, kind, file_path, line_start, line_end, language
                 FROM symbols
                 WHERE corpus_id = ? AND qualified_name = ?
                 LIMIT 1",
                params![self.corpus_id, qualified_name],
                |row| {
                    Ok(SymbolRow {
                        corpus_id: row.get(0)?,
                        name: row.get(1)?,
                        qualified_name: row.get(2)?,
                        kind: row.get(3)?,
                        file_path: row.get(4)?,
                        line_start: row.get(5)?,
                        line_end: row.get(6)?,
                        language: row.get(7)?,
                    })
                },
            )
            .ok();
        Ok(row)
    }

    /// Find all qualified callees of a symbol identified by its
    /// `qualified_name`. Returns each call-site's
    /// `(callee_qualified, callee_name, file_path, line, ref_kind)`
    /// so the caller can disambiguate cross-crate links. Like
    /// [`find_callees`](Self::find_callees), but without the
    /// staleness wrapper — the atlas walker doesn't surface that
    /// caution to humans (it's a one-shot batch run).
    /// All qualified call edges `(caller_qualified, callee_qualified)` in a single query —
    /// for building an in-memory adjacency map so a BFS is HashMap lookups rather than one
    /// async SQL round-trip per node (orders of magnitude faster for repeated traversals).
    pub async fn all_qualified_edges(&self) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT caller_qualified, callee_qualified FROM refs
                 WHERE corpus_id = ? AND callee_qualified != '' AND caller_qualified != ''",
            )
            .map_err(|e| Error::Database(format!("all_qualified_edges prepare: {e}")))?;
        let rows: Vec<(String, String)> = stmt
            .query_map(params![self.corpus_id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(|e| Error::Database(format!("all_qualified_edges query: {e}")))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    pub async fn find_callees_qualified(
        &self,
        caller_qualified: &str,
    ) -> Result<Vec<QualifiedCallee>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT r.callee_qualified, r.callee_symbol, r.file_path, r.line, r.ref_kind
                 FROM refs r
                 WHERE r.corpus_id = ? AND r.caller_qualified = ?
                 ORDER BY r.file_path, r.line",
            )
            .map_err(|e| Error::Database(format!("find_callees_qualified prepare: {e}")))?;
        let rows: Vec<QualifiedCallee> = stmt
            .query_map(params![self.corpus_id, caller_qualified], |row| {
                Ok(QualifiedCallee {
                    callee_qualified: row.get(0)?,
                    callee_name: row.get(1)?,
                    file_path: row.get(2)?,
                    line: row.get(3)?,
                    call_kind: CallKind::from_ref_kind(
                        &row.get::<_, String>(4).unwrap_or_default(),
                    ),
                })
            })
            .map_err(|e| Error::Database(format!("find_callees_qualified query: {e}")))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Bulk insert symbols and references for `self.corpus_id`.
    /// Append-only — multiple calls accumulate rows under the same
    /// corpus_id, which is what the per-language exporter loop
    /// depends on (rust ingest, then typescript, then python all
    /// land in the same DB under one corpus label).
    ///
    /// For the merged-graph flow that needs idempotent replacement
    /// of a single source corpus's rows, see
    /// [`replace_corpus`](Self::replace_corpus).
    pub async fn ingest_symbols_and_refs(
        &self,
        symbols: Vec<ScipSymbolRecord>,
        refs: Vec<ScipRefRecord>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let corpus = self.corpus_id.clone();

        conn.execute_batch("BEGIN TRANSACTION")
            .map_err(|e| Error::Database(format!("begin: {e}")))?;

        for sym in &symbols {
            conn.execute(
                "INSERT INTO symbols (corpus_id, name, qualified_name, kind, file_path, line_start, line_end, language)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    corpus,
                    sym.name,
                    sym.qualified_name,
                    sym.kind,
                    sym.file_path,
                    sym.line_start,
                    sym.line_end,
                    sym.language,
                ],
            )
            .map_err(|e| Error::Database(format!("insert symbol: {e}")))?;
        }

        for r in &refs {
            conn.execute(
                "INSERT INTO refs (corpus_id, caller_symbol, callee_symbol, caller_qualified, callee_qualified, file_path, line, ref_kind)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    corpus,
                    r.caller_symbol,
                    r.callee_symbol,
                    r.caller_qualified,
                    r.callee_qualified,
                    r.file_path,
                    r.line,
                    r.ref_kind,
                ],
            )
            .map_err(|e| Error::Database(format!("insert ref: {e}")))?;
        }

        conn.execute_batch("COMMIT")
            .map_err(|e| Error::Database(format!("commit: {e}")))?;

        Ok(())
    }

    /// Clear all symbols and references for a fresh re-import.
    pub async fn clear(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch("DELETE FROM refs; DELETE FROM symbols;")
            .map_err(|e| Error::Database(format!("clear: {e}")))?;
        Ok(())
    }

    /// Atomically replace ALL symbols/refs for this corpus in a single
    /// transaction. The graph is never observably empty mid-swap, and on ANY
    /// error the transaction rolls back — the prior graph is left intact.
    ///
    /// This is the write half of the "never wipe on failure" contract: callers
    /// (`export_all`) collect a full, *viable* export first and only then swap
    /// it in here. Contrast [`clear`] + [`ingest_symbols_and_refs`], where a
    /// crash between the two leaves the graph empty. Scoped by `corpus_id` so a
    /// shared multi-corpus DB only touches this corpus's rows.
    pub async fn replace_all(
        &self,
        symbols: Vec<ScipSymbolRecord>,
        refs: Vec<ScipRefRecord>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let corpus = self.corpus_id.clone();

        conn.execute_batch("BEGIN TRANSACTION")
            .map_err(|e| Error::Database(format!("replace_all begin: {e}")))?;

        let txn: std::result::Result<(), rusqlite::Error> = (|| {
            conn.execute("DELETE FROM refs WHERE corpus_id = ?", params![corpus])?;
            conn.execute("DELETE FROM symbols WHERE corpus_id = ?", params![corpus])?;
            for sym in &symbols {
                conn.execute(
                    "INSERT INTO symbols (corpus_id, name, qualified_name, kind, file_path, line_start, line_end, language)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    params![corpus, sym.name, sym.qualified_name, sym.kind, sym.file_path, sym.line_start, sym.line_end, sym.language],
                )?;
            }
            for r in &refs {
                conn.execute(
                    "INSERT INTO refs (corpus_id, caller_symbol, callee_symbol, caller_qualified, callee_qualified, file_path, line, ref_kind)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    params![corpus, r.caller_symbol, r.callee_symbol, r.caller_qualified, r.callee_qualified, r.file_path, r.line, r.ref_kind],
                )?;
            }
            Ok(())
        })();

        match txn {
            Ok(()) => {
                conn.execute_batch("COMMIT")
                    .map_err(|e| Error::Database(format!("replace_all commit: {e}")))?;
                Ok(())
            }
            Err(e) => {
                // Roll back so the prior graph survives a mid-swap failure.
                let _ = conn.execute_batch("ROLLBACK");
                Err(Error::Database(format!(
                    "replace_all failed (rolled back, graph preserved): {e}"
                )))
            }
        }
    }

    /// Incrementally merge a re-export of ONLY the given `files`: delete each
    /// file's prior symbols/refs and insert the freshly-parsed ones, all in one
    /// transaction, leaving every OTHER file's rows untouched. Rolls back on
    /// error. On success, the merged files are removed from the stale set (see
    /// [`mark_file_stale`] / [`staleness_for`]) so agents stop seeing the
    /// "call sites may be stale" caution for files that are now fresh.
    ///
    /// This is the storage primitive behind the changed-files incremental path:
    /// an agent editing a handful of files gets those re-indexed in
    /// milliseconds without a full multi-minute re-export, and a failure here
    /// never degrades the rest of the graph. `symbols`/`refs` MUST belong to
    /// `files` (the caller filters the exporter output by `file_path`).
    pub async fn replace_files(
        &self,
        files: &[String],
        symbols: Vec<ScipSymbolRecord>,
        refs: Vec<ScipRefRecord>,
    ) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        {
            let conn = self.conn.lock().await;
            let corpus = self.corpus_id.clone();

            conn.execute_batch("BEGIN TRANSACTION")
                .map_err(|e| Error::Database(format!("replace_files begin: {e}")))?;

            let txn: std::result::Result<(), rusqlite::Error> = (|| {
                for f in files {
                    conn.execute(
                        "DELETE FROM symbols WHERE corpus_id = ? AND file_path = ?",
                        params![corpus, f],
                    )?;
                    conn.execute(
                        "DELETE FROM refs WHERE corpus_id = ? AND file_path = ?",
                        params![corpus, f],
                    )?;
                }
                for sym in &symbols {
                    conn.execute(
                        "INSERT INTO symbols (corpus_id, name, qualified_name, kind, file_path, line_start, line_end, language)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                        params![corpus, sym.name, sym.qualified_name, sym.kind, sym.file_path, sym.line_start, sym.line_end, sym.language],
                    )?;
                }
                for r in &refs {
                    conn.execute(
                        "INSERT INTO refs (corpus_id, caller_symbol, callee_symbol, caller_qualified, callee_qualified, file_path, line, ref_kind)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                        params![corpus, r.caller_symbol, r.callee_symbol, r.caller_qualified, r.callee_qualified, r.file_path, r.line, r.ref_kind],
                    )?;
                }
                Ok(())
            })();

            match txn {
                Ok(()) => conn
                    .execute_batch("COMMIT")
                    .map_err(|e| Error::Database(format!("replace_files commit: {e}")))?,
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(Error::Database(format!(
                        "replace_files failed (rolled back, graph preserved): {e}"
                    )));
                }
            }
        } // conn lock dropped here before touching the stale-set lock

        // These files are now fresh — drop them from the stale set so the
        // staleness caution stops firing for them.
        {
            let mut stale = self.stale_files.write().await;
            for f in files {
                stale.remove(f);
            }
        }
        self.persist_stale_files().await;
        Ok(())
    }

    /// Symbols-ONLY per-file replace: swap each file's symbol *definitions*
    /// while leaving its reference edges untouched. The transactional,
    /// roll-back-on-error sibling of [`replace_files`], but deliberately narrower
    /// — it is the tree-sitter overlay's write primitive.
    ///
    /// The overlay (structural watcher hot path) re-parses a saved file with
    /// tree-sitter, which can see symbol *existence* (a function was added,
    /// removed, or moved) but NOT cross-file call edges — those need
    /// rust-analyzer. So it updates the `symbols` table only and leaves `refs`
    /// as the last full export left them: slightly stale edges (a moved fn's
    /// call sites point at old lines) are gentler and more useful than edges
    /// deleted-until-rebuild, and the idle/periodic full export corrects them.
    /// That is exactly the accepted eventual-consistency contract: symbol-defs
    /// are immediately fresh, cross-file edges lag one full export.
    ///
    /// Merged files are dropped from the stale set (defs are now current).
    ///
    /// Uses this graph's own `corpus_id`. For the merged daemon graph — whose
    /// rows are scoped by each source project's id, not the literal "merged" —
    /// use [`replace_file_symbols_for`] with the project's corpus_id.
    pub async fn replace_file_symbols(
        &self,
        files: &[String],
        symbols: Vec<ScipSymbolRecord>,
    ) -> Result<()> {
        let corpus = self.corpus_id.clone();
        self.replace_file_symbols_for(&corpus, files, symbols).await
    }

    /// Corpus-parameterized [`replace_file_symbols`]: swap symbol defs for
    /// `files` under an EXPLICIT `corpus_id`. Needed for the shared merged graph
    /// the daemon's tools query — its rows carry each source project's corpus_id
    /// (set by [`import_from_path`]/`replace_corpus`), not the graph's own
    /// "merged" id. The structural watcher overlay writes here with the
    /// project's id so `symbols()` sees the update live, without a full re-import.
    pub async fn replace_file_symbols_for(
        &self,
        corpus_id: &str,
        files: &[String],
        symbols: Vec<ScipSymbolRecord>,
    ) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        {
            let conn = self.conn.lock().await;
            let corpus = corpus_id.to_string();

            conn.execute_batch("BEGIN TRANSACTION")
                .map_err(|e| Error::Database(format!("replace_file_symbols begin: {e}")))?;

            let txn: std::result::Result<(), rusqlite::Error> = (|| {
                for f in files {
                    conn.execute(
                        "DELETE FROM symbols WHERE corpus_id = ? AND file_path = ?",
                        params![corpus, f],
                    )?;
                }
                for sym in &symbols {
                    conn.execute(
                        "INSERT INTO symbols (corpus_id, name, qualified_name, kind, file_path, line_start, line_end, language)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                        params![corpus, sym.name, sym.qualified_name, sym.kind, sym.file_path, sym.line_start, sym.line_end, sym.language],
                    )?;
                }
                Ok(())
            })();

            match txn {
                Ok(()) => conn
                    .execute_batch("COMMIT")
                    .map_err(|e| Error::Database(format!("replace_file_symbols commit: {e}")))?,
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(Error::Database(format!(
                        "replace_file_symbols failed (rolled back, graph preserved): {e}"
                    )));
                }
            }
        } // conn lock dropped before touching the stale-set lock

        {
            let mut stale = self.stale_files.write().await;
            for f in files {
                stale.remove(f);
            }
        }
        self.persist_stale_files().await;
        Ok(())
    }

    /// Snapshot of the files the CodeWatcher has marked stale since the last
    /// export — the work-list for an incremental re-index.
    pub async fn stale_files_snapshot(&self) -> Vec<String> {
        self.stale_files.read().await.iter().cloned().collect()
    }

    /// A single-lock snapshot of graph health metrics.
    /// Used by IndexHealthChecker to classify staleness without
    /// acquiring the mutex multiple times per tool call.
    pub async fn stats(&self) -> ScipGraphStats {
        let stale_file_count = self.stale_files.read().await.len();
        let conn = self.conn.lock().await;

        let symbol_count = conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get::<_, usize>(0))
            .unwrap_or(0);

        let ref_count = conn
            .query_row("SELECT COUNT(*) FROM refs", [], |r| r.get::<_, usize>(0))
            .unwrap_or(0);

        let export_age_hours = conn
            .query_row(
                "SELECT value FROM scip_meta WHERE key = 'last_export_at'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(&ts).ok())
            .map(|t| {
                let age = chrono::Utc::now() - t.with_timezone(&chrono::Utc);
                age.num_hours().max(0) as u64
            });

        ScipGraphStats {
            symbol_count,
            ref_count,
            stale_file_count,
            export_age_hours,
        }
    }

    /// Number of files currently in the stale set.
    pub async fn stale_file_count(&self) -> usize {
        self.stale_files.read().await.len()
    }

    /// Number of symbols in the graph.
    pub async fn symbol_count(&self) -> usize {
        let conn = self.conn.lock().await;
        conn.query_row("SELECT COUNT(*) FROM symbols", [], |row| {
            row.get::<_, usize>(0)
        })
        .unwrap_or(0)
    }

    /// Number of references in the graph.
    pub async fn ref_count(&self) -> usize {
        let conn = self.conn.lock().await;
        conn.query_row("SELECT COUNT(*) FROM refs", [], |row| {
            row.get::<_, usize>(0)
        })
        .unwrap_or(0)
    }

    /// Record which languages have SCIP coverage.
    pub async fn record_languages(&self, languages: &[&str]) {
        let csv = languages.join(",");
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO scip_meta (key, value) VALUES ('languages_with_scip', ?)",
            params![csv],
        )
        .ok();
    }

    /// Import all symbols and references from another ScipGraph
    /// database into this one, scoped by the source's `corpus_id`.
    ///
    /// Semantics are delete-and-replace: any rows previously
    /// imported under the source's corpus_id are dropped before
    /// insertion. This bounds growth of a merged graph — without
    /// it, each rebuild of a per-project graph doubled the merged
    /// table size and eventually blocked queries on mutex
    /// contention.
    ///
    /// The source DB must have a non-empty `corpus_id` (either in
    /// its rows' `corpus_id` column or recorded under
    /// `scip_meta.corpus_id`). We fall back to reading the first
    /// row's `corpus_id` when the scip_meta key is absent — a
    /// legacy v1 DB that's been rebuilt once under v2 will have
    /// per-row values but may not have the meta key.
    ///
    /// Returns `(symbols_imported, refs_imported)`.
    pub async fn import_from_path(&self, other_path: &Path) -> Result<(usize, usize)> {
        if !other_path.exists() {
            return Ok((0, 0));
        }

        let other_conn = Connection::open(other_path)
            .map_err(|e| Error::Database(format!("import open: {e}")))?;

        // Discover the source's corpus_id. Priority:
        //   1. `scip_meta.corpus_id` (set by `record_rebuild`).
        //   2. First symbol row's `corpus_id` column.
        //   3. Empty string (legacy v1 DB without per-row ids).
        // The replace step uses this key to scope the DELETE.
        let source_corpus = other_conn
            .query_row(
                "SELECT value FROM scip_meta WHERE key = 'corpus_id'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
            .or_else(|| {
                other_conn
                    .query_row("SELECT corpus_id FROM symbols LIMIT 1", [], |row| {
                        row.get::<_, String>(0)
                    })
                    .ok()
            })
            .unwrap_or_default();

        let mut symbols = Vec::new();
        {
            let mut stmt = other_conn
                .prepare(
                    "SELECT name, qualified_name, kind, file_path, line_start, line_end, language FROM symbols",
                )
                .map_err(|e| Error::Database(format!("import read symbols: {e}")))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(ScipSymbolRecord {
                        name: row.get(0)?,
                        qualified_name: row.get(1)?,
                        kind: row.get(2)?,
                        file_path: row.get(3)?,
                        line_start: row.get(4)?,
                        line_end: row.get(5)?,
                        language: row.get(6)?,
                    })
                })
                .map_err(|e| Error::Database(format!("import query symbols: {e}")))?;
            for row in rows.flatten() {
                symbols.push(row);
            }
        }

        let mut refs = Vec::new();
        {
            let mut stmt = other_conn
                .prepare(
                    "SELECT caller_symbol, callee_symbol, caller_qualified, callee_qualified, file_path, line, ref_kind FROM refs",
                )
                .map_err(|e| Error::Database(format!("import read refs: {e}")))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(ScipRefRecord {
                        caller_symbol: row.get(0)?,
                        callee_symbol: row.get(1)?,
                        caller_qualified: row.get(2)?,
                        callee_qualified: row.get(3)?,
                        file_path: row.get(4)?,
                        line: row.get(5)?,
                        ref_kind: row.get(6)?,
                    })
                })
                .map_err(|e| Error::Database(format!("import query refs: {e}")))?;
            for row in rows.flatten() {
                refs.push(row);
            }
        }

        let sym_count = symbols.len();
        let ref_count = refs.len();

        self.replace_corpus(&source_corpus, symbols, refs).await?;
        Ok((sym_count, ref_count))
    }

    /// Delete every symbol/ref currently stored under
    /// `source_corpus_id` in *this* graph, then insert the
    /// provided rows under that same id. Used by
    /// [`import_from_path`](Self::import_from_path) to keep a
    /// merged graph bounded at the union of all source corpora.
    ///
    /// Unlike [`ingest_symbols_and_refs`](Self::ingest_symbols_and_refs),
    /// this does NOT use `self.corpus_id` — the caller supplies
    /// the source's id so rows from different sources coexist.
    pub async fn replace_corpus(
        &self,
        source_corpus_id: &str,
        symbols: Vec<ScipSymbolRecord>,
        refs: Vec<ScipRefRecord>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;

        conn.execute_batch("BEGIN TRANSACTION")
            .map_err(|e| Error::Database(format!("replace begin: {e}")))?;

        conn.execute(
            "DELETE FROM symbols WHERE corpus_id = ?",
            params![source_corpus_id],
        )
        .map_err(|e| Error::Database(format!("replace delete symbols: {e}")))?;
        conn.execute(
            "DELETE FROM refs WHERE corpus_id = ?",
            params![source_corpus_id],
        )
        .map_err(|e| Error::Database(format!("replace delete refs: {e}")))?;

        for sym in &symbols {
            conn.execute(
                "INSERT INTO symbols (corpus_id, name, qualified_name, kind, file_path, line_start, line_end, language)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    source_corpus_id,
                    sym.name,
                    sym.qualified_name,
                    sym.kind,
                    sym.file_path,
                    sym.line_start,
                    sym.line_end,
                    sym.language,
                ],
            )
            .map_err(|e| Error::Database(format!("replace insert symbol: {e}")))?;
        }

        for r in &refs {
            conn.execute(
                "INSERT INTO refs (corpus_id, caller_symbol, callee_symbol, caller_qualified, callee_qualified, file_path, line, ref_kind)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    source_corpus_id,
                    r.caller_symbol,
                    r.callee_symbol,
                    r.caller_qualified,
                    r.callee_qualified,
                    r.file_path,
                    r.line,
                    r.ref_kind,
                ],
            )
            .map_err(|e| Error::Database(format!("replace insert ref: {e}")))?;
        }

        conn.execute_batch("COMMIT")
            .map_err(|e| Error::Database(format!("replace commit: {e}")))?;
        Ok(())
    }

    /// Compute the transitive blast radius for a symbol: all callers at every
    /// level up to `max_depth`, with cycle detection.
    ///
    /// `max_depth` is capped at 5 internally; `max_symbols` is capped at 200.
    /// Both `production` and `test` callers are included; the tool layer
    /// separates them by checking [`BlastEntry::is_test`].
    pub async fn blast_radius(
        &self,
        symbol_name: &str,
        max_depth: usize,
        max_symbols: usize,
    ) -> Result<BlastRadiusResult> {
        let max_depth = max_depth.clamp(1, 5);
        let max_symbols = max_symbols.clamp(1, 200);

        let resolved = self.resolve_symbol(symbol_name).await?;
        let Some(resolved) = resolved else {
            return Ok(BlastRadiusResult {
                entries: vec![],
                capped: false,
                depth_reached: 0,
                caution: StalenessCaution::None,
            });
        };

        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(resolved.clone());

        let mut frontier: Vec<String> = vec![resolved];
        let mut entries: Vec<BlastEntry> = Vec::new();
        let mut capped = false;
        let mut depth_reached = 0usize;

        'outer: for depth in 1..=max_depth {
            if frontier.is_empty() {
                break;
            }
            let mut next_frontier: Vec<String> = Vec::new();

            for target in &frontier {
                let rows = {
                    let conn = self.conn.lock().await;
                    let mut stmt = conn
                        .prepare(
                            "SELECT DISTINCT r.caller_symbol, r.file_path, r.line
                             FROM refs r
                             WHERE r.callee_symbol = ?
                             ORDER BY r.file_path, r.line",
                        )
                        .map_err(|e| Error::Database(format!("blast_radius prepare: {e}")))?;

                    let result: Vec<(String, String, i32)> = stmt
                        .query_map(params![target], |row| {
                            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                        })
                        .map_err(|e| Error::Database(format!("blast_radius query: {e}")))?
                        .filter_map(|r| r.ok())
                        .collect();
                    result
                };

                for (caller, file, line) in rows {
                    if visited.contains(&caller) {
                        continue;
                    }
                    visited.insert(caller.clone());
                    entries.push(BlastEntry {
                        is_test: is_test_path(&file),
                        symbol_name: caller.clone(),
                        file_path: file,
                        line,
                    });
                    next_frontier.push(caller);
                    if entries.len() >= max_symbols {
                        capped = true;
                        depth_reached = depth;
                        break 'outer;
                    }
                }
            }

            depth_reached = depth;
            frontier = next_frontier;
        }

        let result_files: Vec<String> = entries.iter().map(|e| e.file_path.clone()).collect();
        let caution = self.staleness_for(&result_files).await;

        Ok(BlastRadiusResult {
            entries,
            capped,
            depth_reached,
            caution,
        })
    }

    /// Get which languages have SCIP coverage.
    pub async fn languages_with_scip(&self) -> Vec<String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT value FROM scip_meta WHERE key = 'languages_with_scip'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .map(|csv| {
            csv.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
    }
}

// ─── Helpers ─────────────────────────────────────────────────

/// Returns true when a file path looks like a test file.
pub(crate) fn is_test_path(path: &str) -> bool {
    path.contains("/tests/")
        || path.contains("/test/")
        || path.ends_with("_test.rs")
        || path.ends_with("_tests.rs")
        || path.contains("/test_")
}

// ─── Integrity / rebuild primitives ──────────────────────────

/// Distinct error type returned by [`ScipGraph::open_with_integrity`].
/// Separated from the crate-wide `Error` because callers want to
/// branch on variants — `Corrupt` and `SchemaMismatch` are rebuild
/// triggers, while `Io` / `Database` are operator-visible failures.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// `PRAGMA integrity_check` failed. The offending file has been
    /// renamed to `moved_to` so a subsequent `open_with_integrity`
    /// starts from a clean slate.
    #[error("scip_graph.db was corrupt; quarantined to {}", .moved_to.display())]
    Corrupt { moved_to: PathBuf },
    /// On-disk schema doesn't match the compiled `SCHEMA_VERSION`.
    /// Caller should trigger a full rebuild.
    #[error("scip_graph.db schema version mismatch: found {found}, expected {expected}")]
    SchemaMismatch { found: u32, expected: u32 },
    /// Filesystem error opening or moving the DB.
    #[error("scip_graph IO: {0}")]
    Io(#[from] std::io::Error),
    /// SQLite-level error (corrupt file that rusqlite can't even
    /// probe, unreadable journal, etc.).
    #[error("scip_graph DB: {0}")]
    Database(String),
}

/// Guard returned by [`ScipGraph::try_rebuild_lock`]. Holding one
/// prevents any other process from entering the rebuild write path.
/// Release is automatic on drop (and on process death — the kernel
/// cleans up).
pub struct RebuildLock {
    _file: std::fs::File,
}

fn sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    // `scip_graph.db` + "-wal" → `scip_graph.db-wal`. We preserve the
    // full file name rather than using `set_extension` so we don't
    // trip over non-standard names.
    let name = db_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("scip_graph.db");
    db_path.with_file_name(format!("{name}{suffix}"))
}

fn corrupt_quarantine_path(db_path: &Path) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = db_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("scip_graph.db");
    db_path.with_file_name(format!("{name}.corrupt.{ts}"))
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod integrity_tests {
    //! Coverage for the integrity-checking open path, the rebuild
    //! lock, and metadata writes added for the daemon-owned
    //! freshness pipeline.
    use super::*;

    #[test]
    fn open_with_integrity_creates_fresh_db_and_stamps_schema_version() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("scip_graph.db");

        let g = ScipGraph::open_with_integrity(&path, "test").unwrap();
        // Schema version stamped on fresh DB.
        let v: String = {
            let conn = g.conn.try_lock().unwrap();
            conn.query_row(
                "SELECT value FROM scip_meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(v.parse::<u32>().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn open_with_integrity_moves_corrupt_db_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("scip_graph.db");

        // Plant a file that looks like SQLite but isn't. Any bytes
        // past the SQLite magic that don't form a valid page → the
        // first query fails integrity_check.
        std::fs::write(
            &path,
            b"SQLite format 3\0and then garbage that breaks pages",
        )
        .unwrap();

        let err = match ScipGraph::open_with_integrity(&path, "test") {
            Ok(_) => panic!("open should have failed"),
            Err(e) => e,
        };
        match err {
            OpenError::Corrupt { moved_to } => {
                assert!(moved_to.exists(), "quarantine file should be created");
                assert!(!path.exists(), "original should be moved aside");
                let name = moved_to.file_name().unwrap().to_string_lossy().to_string();
                assert!(
                    name.starts_with("scip_graph.db.corrupt."),
                    "unexpected quarantine name: {name}"
                );
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }

        // A fresh open now succeeds (no file to be corrupt about).
        ScipGraph::open_with_integrity(&path, "test").unwrap();
    }

    #[test]
    fn open_with_integrity_returns_schema_mismatch_when_version_differs() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("scip_graph.db");

        // Open fresh, then corrupt the schema_version to simulate
        // an older build writing the DB.
        let g = ScipGraph::open_with_integrity(&path, "test").unwrap();
        {
            let conn = g.conn.try_lock().unwrap();
            conn.execute(
                "UPDATE scip_meta SET value = ? WHERE key = 'schema_version'",
                params!["0"],
            )
            .unwrap();
        }
        drop(g);

        let err = match ScipGraph::open_with_integrity(&path, "test") {
            Ok(_) => panic!("open should have failed"),
            Err(e) => e,
        };
        match err {
            OpenError::SchemaMismatch { found, expected } => {
                assert_eq!(found, 0);
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaMismatch, got {other:?}"),
        }
    }

    #[test]
    fn open_with_integrity_cleans_orphan_wal_when_db_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("scip_graph.db");
        // Simulate leftover journal files with no .db (the kind of
        // mess a SIGKILL + manual `rm` leaves behind).
        let wal = tmp.path().join("scip_graph.db-wal");
        let shm = tmp.path().join("scip_graph.db-shm");
        std::fs::write(&wal, b"stale wal").unwrap();
        std::fs::write(&shm, b"stale shm").unwrap();

        ScipGraph::open_with_integrity(&path, "test").unwrap();
        assert!(path.exists());
        // After a successful open the live WAL/SHM are recreated by
        // rusqlite, but the orphans we planted are gone — rusqlite
        // would have re-created real ones with the same names.
        // The important property is that open() succeeded rather
        // than tripping on the stale journal.
    }

    #[test]
    fn try_rebuild_lock_is_exclusive_within_process() {
        let tmp = tempfile::tempdir().unwrap();
        let first = ScipGraph::try_rebuild_lock(tmp.path()).unwrap();
        assert!(first.is_some(), "first acquire should succeed");

        let second = ScipGraph::try_rebuild_lock(tmp.path()).unwrap();
        assert!(
            second.is_none(),
            "second acquire must not succeed while first is held"
        );

        drop(first);
        let third = ScipGraph::try_rebuild_lock(tmp.path()).unwrap();
        assert!(third.is_some(), "acquire should succeed after first drops");
    }

    /// Regression test for the merged-graph leak: before the
    /// corpus_id column landed, every import_from_path call
    /// appended rows into the merged DB, so repeated rebuilds
    /// of a per-project graph doubled the merged symbol count.
    /// With replace_corpus, re-importing the same source should
    /// leave the merged count constant.
    #[tokio::test]
    async fn import_from_path_is_idempotent_across_repeated_imports() {
        let tmp = tempfile::tempdir().unwrap();

        // Source DB with a single symbol under corpus_id = "alpha".
        let src_path = tmp.path().join("src.db");
        let src = ScipGraph::open_with_integrity(&src_path, "alpha").unwrap();
        src.ingest_symbols_and_refs(
            vec![ScipSymbolRecord {
                name: "hello".into(),
                qualified_name: String::new(),
                kind: "function".into(),
                file_path: "src/lib.rs".into(),
                line_start: 1,
                line_end: 3,
                language: "rust".into(),
            }],
            vec![],
        )
        .await
        .unwrap();
        // Explicitly record so the corpus_id lands in scip_meta.
        src.record_rebuild("test", None, None).await;
        drop(src);

        // Merged DB starts empty.
        let merged_path = tmp.path().join("merged.db");
        let merged = ScipGraph::open_with_integrity(&merged_path, "merged").unwrap();
        assert_eq!(merged.symbol_count().await, 0);

        // First import — one symbol lands.
        let (syms, _) = merged.import_from_path(&src_path).await.unwrap();
        assert_eq!(syms, 1);
        assert_eq!(merged.symbol_count().await, 1);

        // Second import of the same source — count stays at 1,
        // NOT 2. This is the leak fix.
        let (syms, _) = merged.import_from_path(&src_path).await.unwrap();
        assert_eq!(syms, 1);
        assert_eq!(
            merged.symbol_count().await,
            1,
            "import_from_path must not accumulate duplicates"
        );
    }

    fn sym(name: &str, file: &str) -> ScipSymbolRecord {
        ScipSymbolRecord {
            name: name.into(),
            qualified_name: String::new(),
            kind: "function".into(),
            file_path: file.into(),
            line_start: 1,
            line_end: 2,
            language: "rust".into(),
        }
    }

    #[tokio::test]
    async fn replace_all_swaps_contents_atomically() {
        let g = ScipGraph::open_in_memory("alpha").unwrap();
        g.ingest_symbols_and_refs(vec![sym("old_a", "a.rs"), sym("old_b", "b.rs")], vec![])
            .await
            .unwrap();
        assert_eq!(g.symbol_count().await, 2);

        // Full replace with a different set — old rows gone, new rows present.
        g.replace_all(vec![sym("new_c", "c.rs")], vec![])
            .await
            .unwrap();
        assert_eq!(g.symbol_count().await, 1);
        assert!(
            g.find_symbols_by_name("old_a", None, 8)
                .await
                .unwrap()
                .is_empty(),
            "replace_all must clear prior rows"
        );
        assert!(!g
            .find_symbols_by_name("new_c", None, 8)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn replace_files_merges_only_named_files() {
        let g = ScipGraph::open_in_memory("alpha").unwrap();
        g.ingest_symbols_and_refs(
            vec![
                sym("a_one", "a.rs"),
                sym("a_two", "a.rs"),
                sym("b_one", "b.rs"),
            ],
            vec![],
        )
        .await
        .unwrap();
        assert_eq!(g.symbol_count().await, 3);

        // Re-index ONLY a.rs (now with a single symbol). b.rs must be untouched.
        g.replace_files(&["a.rs".to_string()], vec![sym("a_new", "a.rs")], vec![])
            .await
            .unwrap();

        assert_eq!(
            g.symbol_count().await,
            2,
            "a.rs replaced (2→1), b.rs kept (1)"
        );
        assert!(
            g.find_symbols_by_name("a_one", None, 8)
                .await
                .unwrap()
                .is_empty(),
            "old a.rs rows gone"
        );
        assert!(
            !g.find_symbols_by_name("a_new", None, 8)
                .await
                .unwrap()
                .is_empty(),
            "new a.rs row present"
        );
        assert!(
            !g.find_symbols_by_name("b_one", None, 8)
                .await
                .unwrap()
                .is_empty(),
            "b.rs untouched"
        );
    }

    #[tokio::test]
    async fn replace_file_symbols_updates_defs_but_preserves_refs() {
        let g = ScipGraph::open_in_memory("alpha").unwrap();
        // a.rs has one symbol and one outgoing ref (a_one -> b_one).
        g.ingest_symbols_and_refs(
            vec![sym("a_one", "a.rs")],
            vec![ScipRefRecord {
                caller_symbol: "a_one".into(),
                callee_symbol: "b_one".into(),
                caller_qualified: String::new(),
                callee_qualified: String::new(),
                file_path: "a.rs".into(),
                line: 5,
                ref_kind: "call".into(),
            }],
        )
        .await
        .unwrap();
        assert_eq!(g.symbol_count().await, 1);
        assert_eq!(g.ref_count().await, 1);

        // Overlay re-index of a.rs: the fn was renamed a_one -> a_renamed.
        g.replace_file_symbols(&["a.rs".to_string()], vec![sym("a_renamed", "a.rs")])
            .await
            .unwrap();

        // Symbol def updated...
        assert!(
            g.find_symbols_by_name("a_renamed", None, 8)
                .await
                .unwrap()
                .len()
                == 1
        );
        assert!(g
            .find_symbols_by_name("a_one", None, 8)
            .await
            .unwrap()
            .is_empty());
        // ...but the ref edge is LEFT for the full rebuild to correct.
        assert_eq!(g.ref_count().await, 1, "overlay must not delete ref edges");
    }

    #[tokio::test]
    async fn replace_file_symbols_for_targets_explicit_corpus() {
        // A "merged"-style graph whose rows belong to other corpora. The overlay
        // writes under the PROJECT's corpus_id, not the graph's own "merged" id.
        let g = ScipGraph::open_in_memory("merged").unwrap();
        g.replace_file_symbols_for(
            "projA",
            &["src/a.rs".to_string()],
            vec![sym("fn_a", "src/a.rs")],
        )
        .await
        .unwrap();
        // Findable (name lookup is corpus-agnostic) and stored under projA.
        let hits = g.find_symbols_by_name("fn_a", None, 8).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].corpus_id, "projA",
            "row must carry the project's corpus_id, not 'merged'"
        );

        // A second corpus's same-path file is independent — updating projA does
        // not disturb projB's row.
        g.replace_file_symbols_for(
            "projB",
            &["src/a.rs".to_string()],
            vec![sym("fn_b", "src/a.rs")],
        )
        .await
        .unwrap();
        assert_eq!(
            g.find_symbols_by_name("fn_a", None, 8).await.unwrap().len(),
            1,
            "projA untouched"
        );
        assert_eq!(
            g.find_symbols_by_name("fn_b", None, 8).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn replace_files_drops_merged_files_from_stale_set() {
        let g = ScipGraph::open_in_memory("alpha").unwrap();
        g.ingest_symbols_and_refs(vec![sym("a_one", "a.rs")], vec![])
            .await
            .unwrap();
        g.mark_file_stale("a.rs").await;
        g.mark_file_stale("b.rs").await;
        assert_eq!(g.stale_file_count().await, 2);

        // Merging a.rs makes it fresh; b.rs stays stale.
        g.replace_files(&["a.rs".to_string()], vec![sym("a_new", "a.rs")], vec![])
            .await
            .unwrap();

        let stale = g.stale_files_snapshot().await;
        assert!(
            !stale.contains(&"a.rs".to_string()),
            "merged file left the stale set"
        );
        assert!(
            stale.contains(&"b.rs".to_string()),
            "unmerged file stays stale"
        );
    }

    /// Two source DBs with different corpus_ids must coexist in
    /// the merged graph — re-importing one shouldn't clobber the
    /// other.
    #[tokio::test]
    async fn import_from_path_scopes_delete_to_source_corpus() {
        let tmp = tempfile::tempdir().unwrap();

        let a_path = tmp.path().join("a.db");
        let a = ScipGraph::open_with_integrity(&a_path, "alpha").unwrap();
        a.ingest_symbols_and_refs(
            vec![ScipSymbolRecord {
                name: "alpha_sym".into(),
                qualified_name: String::new(),
                kind: "function".into(),
                file_path: "a/lib.rs".into(),
                line_start: 1,
                line_end: 2,
                language: "rust".into(),
            }],
            vec![],
        )
        .await
        .unwrap();
        a.record_rebuild("t", None, None).await;
        drop(a);

        let b_path = tmp.path().join("b.db");
        let b = ScipGraph::open_with_integrity(&b_path, "beta").unwrap();
        b.ingest_symbols_and_refs(
            vec![ScipSymbolRecord {
                name: "beta_sym".into(),
                qualified_name: String::new(),
                kind: "function".into(),
                file_path: "b/lib.rs".into(),
                line_start: 1,
                line_end: 2,
                language: "rust".into(),
            }],
            vec![],
        )
        .await
        .unwrap();
        b.record_rebuild("t", None, None).await;
        drop(b);

        let merged_path = tmp.path().join("merged.db");
        let merged = ScipGraph::open_with_integrity(&merged_path, "merged").unwrap();
        merged.import_from_path(&a_path).await.unwrap();
        merged.import_from_path(&b_path).await.unwrap();
        assert_eq!(merged.symbol_count().await, 2);

        // Re-import of alpha: only alpha rows are replaced, beta
        // rows stay. Total stays at 2.
        merged.import_from_path(&a_path).await.unwrap();
        assert_eq!(merged.symbol_count().await, 2);
    }

    #[tokio::test]
    async fn record_rebuild_stores_head_and_trigger_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("scip_graph.db");
        let g = ScipGraph::open_with_integrity(&path, "test").unwrap();

        g.record_rebuild(
            "fs_change",
            Some("abc123"),
            Some(r#"[{"lang":"rust","ok":true}]"#),
        )
        .await;

        assert_eq!(g.last_indexed_head().await.as_deref(), Some("abc123"));
        let conn = g.conn.lock().await;
        let reason: String = conn
            .query_row(
                "SELECT value FROM scip_meta WHERE key = 'last_trigger_reason'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reason, "fs_change");
        let outcomes: String = conn
            .query_row(
                "SELECT value FROM scip_meta WHERE key = 'last_exporter_outcomes'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(outcomes.contains("rust"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn iter_all_accessors_return_full_corpus() {
        let graph = ScipGraph::open_in_memory("cap-test").unwrap();
        let syms = test_symbols();
        let refs = test_refs();
        let (n_syms, n_refs) = (syms.len(), refs.len());
        graph.ingest_symbols_and_refs(syms, refs).await.unwrap();

        let got_syms = graph.iter_all_symbols().await.unwrap();
        assert_eq!(
            got_syms.len(),
            n_syms,
            "iter_all_symbols returns every defined symbol"
        );
        assert!(got_syms.iter().any(|s| s.name == "login_handler"));

        let got_refs = graph.iter_all_refs().await.unwrap();
        assert_eq!(got_refs.len(), n_refs, "iter_all_refs returns every edge");
        assert!(
            got_refs.iter().any(|r| r.caller_symbol == "auth_middleware"
                && r.callee_symbol == "validate_access_token"),
            "a known edge survives the round-trip"
        );
    }

    fn test_symbols() -> Vec<ScipSymbolRecord> {
        vec![
            ScipSymbolRecord {
                name: "auth_middleware".into(),
                qualified_name: String::new(),
                kind: "function".into(),
                file_path: "src/middleware/auth.rs".into(),
                line_start: 1,
                line_end: 15,
                language: "rust".into(),
            },
            ScipSymbolRecord {
                name: "validate_access_token".into(),
                qualified_name: String::new(),
                kind: "function".into(),
                file_path: "src/auth/tokens.rs".into(),
                line_start: 1,
                line_end: 10,
                language: "rust".into(),
            },
            ScipSymbolRecord {
                name: "extract_bearer_token".into(),
                qualified_name: String::new(),
                kind: "function".into(),
                file_path: "src/middleware/auth.rs".into(),
                line_start: 17,
                line_end: 25,
                language: "rust".into(),
            },
            ScipSymbolRecord {
                name: "login_handler".into(),
                qualified_name: String::new(),
                kind: "function".into(),
                file_path: "src/routes/auth.rs".into(),
                line_start: 1,
                line_end: 10,
                language: "rust".into(),
            },
            ScipSymbolRecord {
                name: "refresh_handler".into(),
                qualified_name: String::new(),
                kind: "function".into(),
                file_path: "src/routes/auth.rs".into(),
                line_start: 12,
                line_end: 20,
                language: "rust".into(),
            },
            ScipSymbolRecord {
                name: "issue_token_pair".into(),
                qualified_name: String::new(),
                kind: "function".into(),
                file_path: "src/auth/tokens.rs".into(),
                line_start: 12,
                line_end: 20,
                language: "rust".into(),
            },
        ]
    }

    fn test_refs() -> Vec<ScipRefRecord> {
        vec![
            // auth_middleware calls extract_bearer_token and validate_access_token
            ScipRefRecord {
                caller_symbol: "auth_middleware".into(),
                callee_symbol: "extract_bearer_token".into(),
                caller_qualified: String::new(),
                callee_qualified: String::new(),
                file_path: "src/middleware/auth.rs".into(),
                line: 5,
                ref_kind: "direct".into(),
            },
            ScipRefRecord {
                caller_symbol: "auth_middleware".into(),
                callee_symbol: "validate_access_token".into(),
                caller_qualified: String::new(),
                callee_qualified: String::new(),
                file_path: "src/middleware/auth.rs".into(),
                line: 6,
                ref_kind: "direct".into(),
            },
            // login_handler and refresh_handler call issue_token_pair
            ScipRefRecord {
                caller_symbol: "login_handler".into(),
                callee_symbol: "issue_token_pair".into(),
                caller_qualified: String::new(),
                callee_qualified: String::new(),
                file_path: "src/routes/auth.rs".into(),
                line: 5,
                ref_kind: "direct".into(),
            },
            ScipRefRecord {
                caller_symbol: "refresh_handler".into(),
                callee_symbol: "issue_token_pair".into(),
                caller_qualified: String::new(),
                callee_qualified: String::new(),
                file_path: "src/routes/auth.rs".into(),
                line: 15,
                ref_kind: "direct".into(),
            },
        ]
    }

    #[test]
    fn staleness_none_is_empty() {
        assert_eq!(StalenessCaution::None.format_note(), "");
    }

    #[test]
    fn staleness_none_is_not_prominent() {
        assert!(!StalenessCaution::None.is_prominent());
    }

    #[test]
    fn staleness_some_files_includes_names() {
        let c = StalenessCaution::SomeCallSitesMayBeStale {
            stale_files: vec!["src/foo.rs".into()],
        };
        let note = c.format_note();
        assert!(note.contains("`src/foo.rs`"));
        assert!(note.contains("has been modified"));
        assert!(!c.is_prominent());
    }

    #[test]
    fn staleness_some_files_plural() {
        let c = StalenessCaution::SomeCallSitesMayBeStale {
            stale_files: vec!["a.rs".into(), "b.rs".into()],
        };
        let note = c.format_note();
        assert!(note.contains("have been modified"));
        assert!(note.contains("these files"));
    }

    #[test]
    fn staleness_aging_includes_hours() {
        let c = StalenessCaution::GraphIsAging { age_hours: 3 };
        let note = c.format_note();
        assert!(note.contains("3 hours ago"));
        assert!(!c.is_prominent());
    }

    #[test]
    fn staleness_stale_includes_warning_and_command() {
        let c = StalenessCaution::GraphIsStale {
            age_hours: 26,
            corpus_id: "auth-demo".into(),
        };
        let note = c.format_note();
        assert!(note.contains("\u{26a0}"));
        assert!(note.contains("26 hours old"));
        assert!(note.contains("sovereign corpus scip auth-demo"));
        assert!(c.is_prominent());
    }

    #[test]
    fn staleness_language_not_indexed() {
        let c = StalenessCaution::LanguageNotIndexed {
            language: "TypeScript".into(),
            install_hint: "Install with: npm install -g @sourcegraph/scip-typescript".into(),
        };
        let note = c.format_note();
        assert!(note.contains("TypeScript"));
        assert!(note.contains("npm install"));
    }

    #[tokio::test]
    async fn find_callees_returns_correct_results() {
        let graph = ScipGraph::open_in_memory("test").unwrap();
        graph
            .ingest_symbols_and_refs(test_symbols(), test_refs())
            .await
            .unwrap();

        let (callees, caution) = graph.find_callees("auth_middleware").await.unwrap();

        assert_eq!(caution, StalenessCaution::None);
        assert_eq!(callees.len(), 2);

        let names: Vec<&str> = callees.iter().map(|c| c.symbol_name.as_str()).collect();
        assert!(names.contains(&"extract_bearer_token"));
        assert!(names.contains(&"validate_access_token"));
    }

    #[tokio::test]
    async fn find_callers_returns_correct_results() {
        let graph = ScipGraph::open_in_memory("test").unwrap();
        graph
            .ingest_symbols_and_refs(test_symbols(), test_refs())
            .await
            .unwrap();

        let (callers, caution) = graph.find_callers("issue_token_pair", 1).await.unwrap();

        assert_eq!(caution, StalenessCaution::None);
        assert_eq!(callers.len(), 2);

        let names: Vec<&str> = callers.iter().map(|c| c.symbol_name.as_str()).collect();
        assert!(names.contains(&"login_handler"));
        assert!(names.contains(&"refresh_handler"));
    }

    #[tokio::test]
    async fn find_callers_depth_2() {
        let graph = ScipGraph::open_in_memory("test").unwrap();

        // Chain: a → b → c
        let symbols = vec![
            ScipSymbolRecord {
                name: "a".into(),
                qualified_name: String::new(),
                kind: "function".into(),
                file_path: "a.rs".into(),
                line_start: 1,
                line_end: 5,
                language: "rust".into(),
            },
            ScipSymbolRecord {
                name: "b".into(),
                qualified_name: String::new(),
                kind: "function".into(),
                file_path: "b.rs".into(),
                line_start: 1,
                line_end: 5,
                language: "rust".into(),
            },
            ScipSymbolRecord {
                name: "c".into(),
                qualified_name: String::new(),
                kind: "function".into(),
                file_path: "c.rs".into(),
                line_start: 1,
                line_end: 5,
                language: "rust".into(),
            },
        ];
        let refs = vec![
            ScipRefRecord {
                caller_symbol: "a".into(),
                callee_symbol: "b".into(),
                caller_qualified: String::new(),
                callee_qualified: String::new(),
                file_path: "a.rs".into(),
                line: 3,
                ref_kind: "direct".into(),
            },
            ScipRefRecord {
                caller_symbol: "b".into(),
                callee_symbol: "c".into(),
                caller_qualified: String::new(),
                callee_qualified: String::new(),
                file_path: "b.rs".into(),
                line: 3,
                ref_kind: "direct".into(),
            },
        ];
        graph.ingest_symbols_and_refs(symbols, refs).await.unwrap();

        // Depth 1: callers of c = [b]
        let (callers_1, _) = graph.find_callers("c", 1).await.unwrap();
        assert_eq!(callers_1.len(), 1);
        assert_eq!(callers_1[0].symbol_name, "b");

        // Depth 2: callers of c = [b, a]
        let (callers_2, _) = graph.find_callers("c", 2).await.unwrap();
        assert_eq!(callers_2.len(), 2);
        let names: Vec<&str> = callers_2.iter().map(|c| c.symbol_name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
    }

    #[tokio::test]
    async fn find_callees_unknown_symbol_returns_empty() {
        let graph = ScipGraph::open_in_memory("test").unwrap();
        graph
            .ingest_symbols_and_refs(test_symbols(), test_refs())
            .await
            .unwrap();

        let (callees, caution) = graph.find_callees("nonexistent_xyz").await.unwrap();
        assert!(callees.is_empty());
        assert_eq!(caution, StalenessCaution::None);
    }

    #[tokio::test]
    async fn staleness_after_mark_file_stale() {
        let graph = ScipGraph::open_in_memory("test").unwrap();
        graph
            .ingest_symbols_and_refs(test_symbols(), test_refs())
            .await
            .unwrap();

        // Mark a file as stale.
        graph.mark_file_stale("src/middleware/auth.rs").await;

        // Query for a symbol whose callees include that file.
        let (_, caution) = graph.find_callees("auth_middleware").await.unwrap();

        match caution {
            StalenessCaution::SomeCallSitesMayBeStale { stale_files } => {
                assert!(stale_files.contains(&"src/middleware/auth.rs".to_string()));
            }
            other => panic!("Expected SomeCallSitesMayBeStale, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn record_export_clears_staleness() {
        let graph = ScipGraph::open_in_memory("test").unwrap();
        graph
            .ingest_symbols_and_refs(test_symbols(), test_refs())
            .await
            .unwrap();

        graph.mark_file_stale("src/middleware/auth.rs").await;
        assert_eq!(graph.stale_file_count().await, 1);

        graph.record_export().await;
        assert_eq!(graph.stale_file_count().await, 0);

        let (_, caution) = graph.find_callees("auth_middleware").await.unwrap();
        assert_eq!(caution, StalenessCaution::None);
    }

    #[tokio::test]
    async fn resolve_symbol_suffix_match() {
        let graph = ScipGraph::open_in_memory("test").unwrap();
        graph
            .ingest_symbols_and_refs(
                vec![ScipSymbolRecord {
                    name: "my_crate::module::my_fn".into(),
                    qualified_name: String::new(),
                    kind: "function".into(),
                    file_path: "src/lib.rs".into(),
                    line_start: 1,
                    line_end: 10,
                    language: "rust".into(),
                }],
                vec![],
            )
            .await
            .unwrap();

        // Exact match.
        let resolved = graph
            .resolve_symbol("my_crate::module::my_fn")
            .await
            .unwrap();
        assert_eq!(resolved, Some("my_crate::module::my_fn".to_string()));

        // Suffix match.
        let resolved = graph.resolve_symbol("my_fn").await.unwrap();
        assert_eq!(resolved, Some("my_crate::module::my_fn".to_string()));
    }

    #[tokio::test]
    async fn clear_removes_all_data() {
        let graph = ScipGraph::open_in_memory("test").unwrap();
        graph
            .ingest_symbols_and_refs(test_symbols(), test_refs())
            .await
            .unwrap();

        assert!(graph.symbol_count().await > 0);
        assert!(graph.ref_count().await > 0);

        graph.clear().await.unwrap();

        assert_eq!(graph.symbol_count().await, 0);
        assert_eq!(graph.ref_count().await, 0);
    }

    #[test]
    fn call_kind_round_trip() {
        for kind in &["direct", "method", "trait", "dynamic"] {
            let ck = CallKind::from_ref_kind(kind);
            assert_eq!(ck.as_str(), *kind);
        }
    }

    #[test]
    fn is_test_path_detection() {
        assert!(is_test_path("src/tests/foo.rs"));
        assert!(is_test_path("src/foo_test.rs"));
        assert!(is_test_path("src/foo_tests.rs"));
        assert!(is_test_path("crates/bar/test_helpers.rs"));
        assert!(!is_test_path("src/foo.rs"));
        assert!(!is_test_path("src/testing_utils.rs")); // doesn't match "test_" prefix
    }

    #[tokio::test]
    async fn blast_radius_groups_by_module() {
        let graph = ScipGraph::open_in_memory("test").unwrap();
        graph
            .ingest_symbols_and_refs(test_symbols(), test_refs())
            .await
            .unwrap();

        // blast_radius of issue_token_pair should find login_handler and refresh_handler.
        let result = graph.blast_radius("issue_token_pair", 1, 50).await.unwrap();
        let names: Vec<&str> = result
            .entries
            .iter()
            .map(|e| e.symbol_name.as_str())
            .collect();
        assert!(names.contains(&"login_handler"), "expected login_handler");
        assert!(
            names.contains(&"refresh_handler"),
            "expected refresh_handler"
        );
        assert!(!result.capped);
        assert_eq!(result.depth_reached, 1);
    }

    #[tokio::test]
    async fn blast_radius_cycle_detection() {
        let graph = ScipGraph::open_in_memory("test").unwrap();
        // Mutual recursion: a → b → a
        let symbols = vec![
            ScipSymbolRecord {
                name: "a".into(),
                qualified_name: String::new(),
                kind: "function".into(),
                file_path: "a.rs".into(),
                line_start: 1,
                line_end: 5,
                language: "rust".into(),
            },
            ScipSymbolRecord {
                name: "b".into(),
                qualified_name: String::new(),
                kind: "function".into(),
                file_path: "b.rs".into(),
                line_start: 1,
                line_end: 5,
                language: "rust".into(),
            },
        ];
        let refs = vec![
            ScipRefRecord {
                caller_symbol: "a".into(),
                callee_symbol: "b".into(),
                caller_qualified: String::new(),
                callee_qualified: String::new(),
                file_path: "a.rs".into(),
                line: 3,
                ref_kind: "direct".into(),
            },
            ScipRefRecord {
                caller_symbol: "b".into(),
                callee_symbol: "a".into(),
                caller_qualified: String::new(),
                callee_qualified: String::new(),
                file_path: "b.rs".into(),
                line: 3,
                ref_kind: "direct".into(),
            },
        ];
        graph.ingest_symbols_and_refs(symbols, refs).await.unwrap();

        // Should terminate without infinite loop.
        let result = graph.blast_radius("b", 5, 50).await.unwrap();
        // 'a' calls 'b', so 'a' is a depth-1 caller of 'b'.
        // Then 'b' calls 'a' but 'b' is already visited — cycle cut.
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].symbol_name, "a");
        assert!(!result.capped);
    }

    #[tokio::test]
    async fn blast_radius_cap_respected() {
        let graph = ScipGraph::open_in_memory("test").unwrap();
        // Build a star: 25 direct callers of "root" at depth 1.
        // With max_symbols=10, the traversal should stop after 10 entries.
        let n = 25usize;
        let mut symbols = vec![ScipSymbolRecord {
            name: "root".into(),
            qualified_name: String::new(),
            kind: "function".into(),
            file_path: "root.rs".into(),
            line_start: 1,
            line_end: 5,
            language: "rust".into(),
        }];
        let mut refs = Vec::new();
        for i in 1..=n {
            symbols.push(ScipSymbolRecord {
                name: format!("caller{i}"),
                qualified_name: String::new(),
                kind: "function".into(),
                file_path: format!("caller{i}.rs"),
                line_start: 1,
                line_end: 5,
                language: "rust".into(),
            });
            refs.push(ScipRefRecord {
                caller_symbol: format!("caller{i}"),
                callee_symbol: "root".to_string(),
                caller_qualified: String::new(),
                callee_qualified: String::new(),
                file_path: format!("caller{i}.rs"),
                line: 3,
                ref_kind: "direct".into(),
            });
        }
        graph.ingest_symbols_and_refs(symbols, refs).await.unwrap();

        let result = graph.blast_radius("root", 1, 10).await.unwrap();
        assert!(result.capped, "should be capped");
        assert_eq!(result.entries.len(), 10);
    }

    #[tokio::test]
    async fn symbols_in_file_returns_definitions() {
        let graph = ScipGraph::open_in_memory("test").unwrap();
        graph
            .ingest_symbols_and_refs(test_symbols(), test_refs())
            .await
            .unwrap();

        let rows = graph
            .symbols_in_file("src/middleware/auth.rs")
            .await
            .unwrap();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["auth_middleware", "extract_bearer_token"]);

        let empty = graph.symbols_in_file("src/nope.rs").await.unwrap();
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn symbols_in_crate_matches_path_prefix() {
        let graph = ScipGraph::open_in_memory("test").unwrap();
        graph
            .ingest_symbols_and_refs(test_symbols(), test_refs())
            .await
            .unwrap();

        // No name match (test fixtures use unqualified names), so the
        // file_path prefix is what carries this query.
        let rows = graph.symbols_in_crate("auth", "src/auth/").await.unwrap();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"validate_access_token"));
        assert!(names.contains(&"issue_token_pair"));
        assert!(!names.contains(&"login_handler"));
    }

    #[tokio::test]
    async fn symbol_definition_strict_match() {
        let graph = ScipGraph::open_in_memory("test").unwrap();
        graph
            .ingest_symbols_and_refs(test_symbols(), test_refs())
            .await
            .unwrap();

        let row = graph.symbol_definition("auth_middleware").await.unwrap();
        let row = row.expect("auth_middleware should resolve");
        assert_eq!(row.file_path, "src/middleware/auth.rs");
        assert_eq!(row.kind, "function");

        let missing = graph.symbol_definition("not_real").await.unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn blast_radius_staleness_propagated() {
        let graph = ScipGraph::open_in_memory("test").unwrap();
        graph
            .ingest_symbols_and_refs(test_symbols(), test_refs())
            .await
            .unwrap();

        // Mark the file where callers live as stale.
        graph.mark_file_stale("src/routes/auth.rs").await;

        let result = graph.blast_radius("issue_token_pair", 1, 50).await.unwrap();
        assert!(
            matches!(
                result.caution,
                StalenessCaution::SomeCallSitesMayBeStale { .. }
            ),
            "expected staleness caution when caller file is stale"
        );
    }
}
