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
use std::path::Path;
use std::sync::Arc;

use rusqlite::{params, Connection};
use tokio::sync::{Mutex, RwLock};

use crate::error::{Error, Result};

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
    LanguageNotIndexed { language: String, install_hint: String },
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
    pub name: String,
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
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(e))?;
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
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS symbols (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                file_path TEXT NOT NULL,
                line_start INTEGER NOT NULL,
                line_end INTEGER NOT NULL,
                language TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
            CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_path);

            CREATE TABLE IF NOT EXISTS refs (
                id INTEGER PRIMARY KEY,
                caller_symbol TEXT NOT NULL,
                callee_symbol TEXT NOT NULL,
                file_path TEXT NOT NULL,
                line INTEGER NOT NULL,
                ref_kind TEXT NOT NULL DEFAULT 'direct'
            );
            CREATE INDEX IF NOT EXISTS idx_refs_caller ON refs(caller_symbol);
            CREATE INDEX IF NOT EXISTS idx_refs_callee ON refs(callee_symbol);

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
        self.stale_files
            .write()
            .await
            .insert(file_path.to_string());

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
        let csv = stale
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(",");
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
        Some(age.num_hours().max(0) as u64)
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

    /// Find all symbols that the given symbol calls.
    pub async fn find_callees(
        &self,
        symbol_name: &str,
    ) -> Result<(Vec<Callee>, StalenessCaution)> {
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

        let result_files: Vec<String> = callees.iter().map(|c| c.file_path.clone()).collect();
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

        let depth = depth.min(2).max(1);
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

    /// Bulk insert symbols and references. Used by the SCIP exporter and
    /// by tests to populate the graph directly.
    pub async fn ingest_symbols_and_refs(
        &self,
        symbols: Vec<ScipSymbolRecord>,
        refs: Vec<ScipRefRecord>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;

        // Use a transaction for performance.
        conn.execute_batch("BEGIN TRANSACTION")
            .map_err(|e| Error::Database(format!("begin: {e}")))?;

        for sym in &symbols {
            conn.execute(
                "INSERT INTO symbols (name, kind, file_path, line_start, line_end, language)
                 VALUES (?, ?, ?, ?, ?, ?)",
                params![
                    sym.name,
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
                "INSERT INTO refs (caller_symbol, callee_symbol, file_path, line, ref_kind)
                 VALUES (?, ?, ?, ?, ?)",
                params![
                    r.caller_symbol,
                    r.callee_symbol,
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

    /// Number of files currently in the stale set.
    pub async fn stale_file_count(&self) -> usize {
        self.stale_files.read().await.len()
    }

    /// Number of symbols in the graph.
    pub async fn symbol_count(&self) -> usize {
        let conn = self.conn.lock().await;
        conn.query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get::<_, usize>(0))
            .unwrap_or(0)
    }

    /// Number of references in the graph.
    pub async fn ref_count(&self) -> usize {
        let conn = self.conn.lock().await;
        conn.query_row("SELECT COUNT(*) FROM refs", [], |row| row.get::<_, usize>(0))
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

    /// Import all symbols and references from another ScipGraph database
    /// file into this graph. Used to build a merged view across multiple
    /// per-project graphs (e.g. for a multi-project MCP server).
    ///
    /// Returns `(symbols_imported, refs_imported)`.
    pub async fn import_from_path(&self, other_path: &Path) -> Result<(usize, usize)> {
        if !other_path.exists() {
            return Ok((0, 0));
        }

        let other_conn = Connection::open(other_path)
            .map_err(|e| Error::Database(format!("import open: {e}")))?;

        // Read symbols.
        let mut symbols = Vec::new();
        {
            let mut stmt = other_conn
                .prepare("SELECT name, kind, file_path, line_start, line_end, language FROM symbols")
                .map_err(|e| Error::Database(format!("import read symbols: {e}")))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(ScipSymbolRecord {
                        name: row.get(0)?,
                        kind: row.get(1)?,
                        file_path: row.get(2)?,
                        line_start: row.get(3)?,
                        line_end: row.get(4)?,
                        language: row.get(5)?,
                    })
                })
                .map_err(|e| Error::Database(format!("import query symbols: {e}")))?;
            for row in rows {
                if let Ok(sym) = row {
                    symbols.push(sym);
                }
            }
        }

        // Read refs.
        let mut refs = Vec::new();
        {
            let mut stmt = other_conn
                .prepare(
                    "SELECT caller_symbol, callee_symbol, file_path, line, ref_kind FROM refs",
                )
                .map_err(|e| Error::Database(format!("import read refs: {e}")))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(ScipRefRecord {
                        caller_symbol: row.get(0)?,
                        callee_symbol: row.get(1)?,
                        file_path: row.get(2)?,
                        line: row.get(3)?,
                        ref_kind: row.get(4)?,
                    })
                })
                .map_err(|e| Error::Database(format!("import query refs: {e}")))?;
            for row in rows {
                if let Ok(r) = row {
                    refs.push(r);
                }
            }
        }

        let sym_count = symbols.len();
        let ref_count = refs.len();

        if !symbols.is_empty() || !refs.is_empty() {
            self.ingest_symbols_and_refs(symbols, refs).await?;
        }

        Ok((sym_count, ref_count))
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

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_symbols() -> Vec<ScipSymbolRecord> {
        vec![
            ScipSymbolRecord {
                name: "auth_middleware".into(),
                kind: "function".into(),
                file_path: "src/middleware/auth.rs".into(),
                line_start: 1,
                line_end: 15,
                language: "rust".into(),
            },
            ScipSymbolRecord {
                name: "validate_access_token".into(),
                kind: "function".into(),
                file_path: "src/auth/tokens.rs".into(),
                line_start: 1,
                line_end: 10,
                language: "rust".into(),
            },
            ScipSymbolRecord {
                name: "extract_bearer_token".into(),
                kind: "function".into(),
                file_path: "src/middleware/auth.rs".into(),
                line_start: 17,
                line_end: 25,
                language: "rust".into(),
            },
            ScipSymbolRecord {
                name: "login_handler".into(),
                kind: "function".into(),
                file_path: "src/routes/auth.rs".into(),
                line_start: 1,
                line_end: 10,
                language: "rust".into(),
            },
            ScipSymbolRecord {
                name: "refresh_handler".into(),
                kind: "function".into(),
                file_path: "src/routes/auth.rs".into(),
                line_start: 12,
                line_end: 20,
                language: "rust".into(),
            },
            ScipSymbolRecord {
                name: "issue_token_pair".into(),
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
                file_path: "src/middleware/auth.rs".into(),
                line: 5,
                ref_kind: "direct".into(),
            },
            ScipRefRecord {
                caller_symbol: "auth_middleware".into(),
                callee_symbol: "validate_access_token".into(),
                file_path: "src/middleware/auth.rs".into(),
                line: 6,
                ref_kind: "direct".into(),
            },
            // login_handler and refresh_handler call issue_token_pair
            ScipRefRecord {
                caller_symbol: "login_handler".into(),
                callee_symbol: "issue_token_pair".into(),
                file_path: "src/routes/auth.rs".into(),
                line: 5,
                ref_kind: "direct".into(),
            },
            ScipRefRecord {
                caller_symbol: "refresh_handler".into(),
                callee_symbol: "issue_token_pair".into(),
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
                kind: "function".into(),
                file_path: "a.rs".into(),
                line_start: 1,
                line_end: 5,
                language: "rust".into(),
            },
            ScipSymbolRecord {
                name: "b".into(),
                kind: "function".into(),
                file_path: "b.rs".into(),
                line_start: 1,
                line_end: 5,
                language: "rust".into(),
            },
            ScipSymbolRecord {
                name: "c".into(),
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
                file_path: "a.rs".into(),
                line: 3,
                ref_kind: "direct".into(),
            },
            ScipRefRecord {
                caller_symbol: "b".into(),
                callee_symbol: "c".into(),
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
}
