//! SQLite-backed store for lint runner results.
//!
//! Mirrors [`crate::test_results`] but adds `line`/`col` columns for
//! precise error location, a `warn` kind alongside `pass`/`fail`, and a
//! tighter truncation limit (500 chars vs 4096 for tests — lint errors are
//! much shorter).
//!
//! Written by [`crate::update::lint_watcher::LintWatcher`]; read by the
//! `lint_status` and `get_lint_output` MCP tools.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use tokio::sync::Mutex;

use crate::error::{Error, Result};

// ─── Result types ─────────────────────────────────────────────────────────────

/// Summary of a single completed lint run.
#[derive(Debug, Clone)]
pub struct LintRunSummary {
    pub run_id: i64,
    pub pass_count: u32,
    pub fail_count: u32,
    pub warn_count: u32,
    pub exit_code: i32,
    pub elapsed_ms: Option<u64>,
    pub finished_at: SystemTime,
}

impl LintRunSummary {
    pub fn passed(&self) -> bool {
        self.exit_code == 0 && self.fail_count == 0
    }
}

/// A single lint result from a run.
#[derive(Debug, Clone)]
pub struct LintResult {
    pub run_id: i64,
    pub kind: LintResultKind,
    pub file: String,
    /// Output truncated to [`OUTPUT_TRUNCATE_CHARS`] if `output_truncated`.
    pub output: Option<String>,
    pub output_truncated: bool,
    /// Source line (1-indexed) if provided by the linter.
    pub line: Option<u32>,
    /// Source column (1-indexed) if provided by the linter.
    pub col: Option<u32>,
}

/// Result kind for a lint event.
#[derive(Debug, Clone, PartialEq)]
pub enum LintResultKind {
    Pass,
    Fail,
    Warn,
}

impl LintResultKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Warn => "warn",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "pass" => Self::Pass,
            "warn" => Self::Warn,
            _ => Self::Fail,
        }
    }
}

/// Maximum characters stored per lint error output.
/// Lint errors are compact (one-liners + context), so 500 is generous.
pub const OUTPUT_TRUNCATE_CHARS: usize = 500;

// ─── Store ────────────────────────────────────────────────────────────────────

/// SQLite store for lint runner results.
pub struct LintResultStore {
    conn: Arc<Mutex<Connection>>,
}

impl LintResultStore {
    /// Open or create the database at `db_path`.
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let conn = Connection::open(db_path).map_err(|e| {
            Error::Io(std::io::Error::other(format!(
                "LintResultStore::open {}: {e}",
                db_path.display()
            )))
        })?;
        conn.execute_batch(SCHEMA).map_err(|e| {
            Error::Io(std::io::Error::other(format!("schema migration: {e}")))
        })?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Start a new run and return its id.
    pub async fn begin_run(&self) -> Result<i64> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO lint_runs (started_at, pass_count, fail_count, warn_count) VALUES (?1, 0, 0, 0)",
            params![now],
        )
        .map_err(sqlite_err)?;
        Ok(conn.last_insert_rowid())
    }

    /// Record a single lint result for `run_id`.
    pub async fn record_result(
        &self,
        run_id: i64,
        kind: LintResultKind,
        file: &str,
        output: Option<&str>,
        line: Option<u32>,
        col: Option<u32>,
    ) -> Result<()> {
        let (stored_output, truncated) = match output {
            None => (None, false),
            Some(s) if s.len() <= OUTPUT_TRUNCATE_CHARS => (Some(s.to_string()), false),
            Some(s) => (Some(s[..OUTPUT_TRUNCATE_CHARS].to_string()), true),
        };

        let counter_col = match kind {
            LintResultKind::Pass => "pass_count",
            LintResultKind::Fail => "fail_count",
            LintResultKind::Warn => "warn_count",
        };

        let conn = self.conn.lock().await;
        conn.execute(
            &format!("UPDATE lint_runs SET {counter_col} = {counter_col} + 1 WHERE id = ?1"),
            params![run_id],
        )
        .map_err(sqlite_err)?;

        conn.execute(
            "INSERT INTO lint_results (run_id, kind, file, output, output_truncated, line, col) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run_id,
                kind.as_str(),
                file,
                stored_output,
                truncated as i32,
                line.map(|v| v as i64),
                col.map(|v| v as i64),
            ],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    /// Mark a run as finished.
    pub async fn finish_run(&self, run_id: i64, exit_code: i32, elapsed_ms: u64) -> Result<()> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE lint_runs SET finished_at = ?1, exit_code = ?2, elapsed_ms = ?3 WHERE id = ?4",
            params![now, exit_code, elapsed_ms as i64, run_id],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    /// Store full raw output for `get_lint_output`.
    pub async fn store_raw_output(&self, run_id: i64, raw: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE lint_runs SET raw_output = ?1 WHERE id = ?2",
            params![raw, run_id],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    /// Return the most recently finished run summary.
    pub async fn latest_run(&self) -> Result<Option<LintRunSummary>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, pass_count, fail_count, warn_count, exit_code, elapsed_ms, finished_at \
                 FROM lint_runs WHERE finished_at IS NOT NULL ORDER BY id DESC LIMIT 1",
            )
            .map_err(sqlite_err)?;
        let row = stmt
            .query_row([], |row| {
                Ok(LintRunSummary {
                    run_id: row.get(0)?,
                    pass_count: row.get::<_, i64>(1)? as u32,
                    fail_count: row.get::<_, i64>(2)? as u32,
                    warn_count: row.get::<_, i64>(3)? as u32,
                    exit_code: row.get(4)?,
                    elapsed_ms: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                    finished_at: unix_to_system_time(row.get::<_, i64>(6)?),
                })
            })
            .optional()
            .map_err(sqlite_err)?;
        Ok(row)
    }

    /// Return all failures from the most recent run (grouped by file for
    /// the `lint_status` tool response).
    pub async fn latest_failures(&self, limit: usize) -> Result<Vec<LintResult>> {
        self.latest_by_kind("fail", limit).await
    }

    /// Return all warnings from the most recent run.
    pub async fn latest_warnings(&self, limit: usize) -> Result<Vec<LintResult>> {
        self.latest_by_kind("warn", limit).await
    }

    async fn latest_by_kind(&self, kind: &str, limit: usize) -> Result<Vec<LintResult>> {
        let conn = self.conn.lock().await;
        let run_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM lint_runs WHERE finished_at IS NOT NULL ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_err)?;

        let Some(run_id) = run_id else {
            return Ok(vec![]);
        };

        let mut stmt = conn
            .prepare(
                "SELECT run_id, kind, file, output, output_truncated, line, col \
                 FROM lint_results WHERE run_id = ?1 AND kind = ?2 LIMIT ?3",
            )
            .map_err(sqlite_err)?;

        let rows = stmt
            .query_map(params![run_id, kind, limit as i64], |row| {
                let kind_str: String = row.get(1)?;
                Ok(LintResult {
                    run_id: row.get(0)?,
                    kind: LintResultKind::from_str(&kind_str),
                    file: row.get(2)?,
                    output: row.get(3)?,
                    output_truncated: row.get::<_, i32>(4)? != 0,
                    line: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                    col: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                })
            })
            .map_err(sqlite_err)?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(sqlite_err)?);
        }
        Ok(out)
    }

    /// Return full raw output for `get_lint_output`.
    pub async fn raw_output(&self, run_id: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT raw_output FROM lint_runs WHERE id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_err)
    }

    /// Mark files as stale.
    pub async fn mark_stale(&self, paths: &[PathBuf]) -> Result<()> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        for path in paths {
            conn.execute(
                "INSERT OR REPLACE INTO lint_stale_files (path, marked_at) VALUES (?1, ?2)",
                params![path.to_string_lossy().as_ref(), now],
            )
            .map_err(sqlite_err)?;
        }
        Ok(())
    }

    /// Clear the stale files table.
    pub async fn clear_stale(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute("DELETE FROM lint_stale_files", [])
            .map_err(sqlite_err)?;
        Ok(())
    }

    /// Return all stale paths.
    pub async fn stale_files_since_last_run(&self) -> Result<Vec<PathBuf>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT path FROM lint_stale_files ORDER BY marked_at ASC")
            .map_err(sqlite_err)?;
        let paths = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(sqlite_err)?;
        let mut out = Vec::new();
        for p in paths {
            out.push(PathBuf::from(p.map_err(sqlite_err)?));
        }
        Ok(out)
    }

    /// Returns true if any stale files exist.
    pub async fn has_stale_files(&self) -> Result<bool> {
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM lint_stale_files", [], |r| r.get(0))
            .map_err(sqlite_err)?;
        Ok(count > 0)
    }

    /// Returns true if the most recent run is still in progress.
    ///
    /// Checks only the latest row — abandoned runs (task aborted before
    /// `finish_run` was called) leave orphaned NULL rows that would otherwise
    /// make this return true forever.
    pub async fn run_in_progress(&self) -> Result<bool> {
        let conn = self.conn.lock().await;
        // None → no runs at all; Some(None) → latest run not finished; Some(Some(_)) → finished.
        let finished_at: Option<Option<i64>> = conn
            .query_row(
                "SELECT finished_at FROM lint_runs ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_err)?;
        Ok(matches!(finished_at, Some(None)))
    }
}

// ─── Schema ───────────────────────────────────────────────────────────────────

const SCHEMA: &str = "
PRAGMA journal_mode=WAL;

CREATE TABLE IF NOT EXISTS lint_runs (
    id          INTEGER PRIMARY KEY,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    exit_code   INTEGER,
    elapsed_ms  INTEGER,
    pass_count  INTEGER NOT NULL DEFAULT 0,
    fail_count  INTEGER NOT NULL DEFAULT 0,
    warn_count  INTEGER NOT NULL DEFAULT 0,
    raw_output  TEXT
);

CREATE TABLE IF NOT EXISTS lint_results (
    id               INTEGER PRIMARY KEY,
    run_id           INTEGER NOT NULL REFERENCES lint_runs(id) ON DELETE CASCADE,
    kind             TEXT    NOT NULL CHECK(kind IN ('pass','fail','warn')),
    file             TEXT    NOT NULL,
    output           TEXT,
    output_truncated INTEGER NOT NULL DEFAULT 0,
    line             INTEGER,
    col              INTEGER
);
CREATE INDEX IF NOT EXISTS idx_lint_results_run ON lint_results(run_id);

CREATE TABLE IF NOT EXISTS lint_stale_files (
    path       TEXT    PRIMARY KEY,
    marked_at  INTEGER NOT NULL
);
";

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn unix_to_system_time(secs: i64) -> SystemTime {
    UNIX_EPOCH + std::time::Duration::from_secs(secs as u64)
}

fn sqlite_err(e: rusqlite::Error) -> Error {
    Error::Io(std::io::Error::other(format!("sqlite: {e}")))
}

trait OptionalExt<T> {
    fn optional(self) -> rusqlite::Result<Option<T>>;
}

impl<T> OptionalExt<T> for rusqlite::Result<T> {
    fn optional(self) -> rusqlite::Result<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_store() -> LintResultStore {
        let dir = tempfile::tempdir().unwrap();
        LintResultStore::open(&dir.path().join("lint.db")).unwrap()
    }

    #[tokio::test]
    async fn lint_tier2_warn_not_failure() {
        // Warnings should NOT affect exit_code == 0 → passed() == true.
        let store = make_store().await;
        let run_id = store.begin_run().await.unwrap();
        store
            .record_result(run_id, LintResultKind::Warn, "src/foo.rs", Some("unused var"), Some(12), Some(9))
            .await
            .unwrap();
        store.finish_run(run_id, 0, 100).await.unwrap();

        let summary = store.latest_run().await.unwrap().unwrap();
        assert_eq!(summary.warn_count, 1);
        assert_eq!(summary.fail_count, 0);
        assert!(summary.passed(), "exit_code=0 with only warnings should be passing");
    }

    #[tokio::test]
    async fn lint_line_col_stored() {
        let store = make_store().await;
        let run_id = store.begin_run().await.unwrap();
        store
            .record_result(run_id, LintResultKind::Fail, "src/bar.rs", Some("type error"), Some(34), Some(5))
            .await
            .unwrap();
        store.finish_run(run_id, 1, 200).await.unwrap();

        let failures = store.latest_failures(1).await.unwrap();
        assert_eq!(failures[0].line, Some(34));
        assert_eq!(failures[0].col, Some(5));
    }

    #[tokio::test]
    async fn lint_output_truncated_at_500() {
        let store = make_store().await;
        let run_id = store.begin_run().await.unwrap();
        let long = "e".repeat(OUTPUT_TRUNCATE_CHARS + 200);
        store
            .record_result(run_id, LintResultKind::Fail, "src/baz.rs", Some(&long), None, None)
            .await
            .unwrap();
        store.finish_run(run_id, 1, 50).await.unwrap();

        let failures = store.latest_failures(1).await.unwrap();
        assert!(failures[0].output_truncated);
        assert_eq!(failures[0].output.as_ref().unwrap().len(), OUTPUT_TRUNCATE_CHARS);
    }

    #[tokio::test]
    async fn lint_stale_roundtrip() {
        let store = make_store().await;
        store
            .mark_stale(&[PathBuf::from("src/a.rs")])
            .await
            .unwrap();
        assert!(store.has_stale_files().await.unwrap());
        store.clear_stale().await.unwrap();
        assert!(!store.has_stale_files().await.unwrap());
    }
}
