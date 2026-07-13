// SPDX-License-Identifier: AGPL-3.0-or-later
//! SQLite-backed store for test runner results.
//!
//! Records are written by [`crate::test_watcher::TestWatcher`] as
//! subprocess output arrives, and read by the `test_status` / `get_run_output`
//! MCP tools.
//!
//! ## Schema
//!
//! Three tables:
//! - **`test_runs`** — one row per subprocess invocation (started → finished).
//! - **`test_results`** — one row per Tier 2 `pass`/`fail` event in a run.
//! - **`test_stale_files`** — paths that changed since the last completed run.
//!
//! The stale-files table is the source of truth for `WatcherStatus::Stale`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use tokio::sync::Mutex;

use crate::error::{Error, Result};

// ─── Result types ─────────────────────────────────────────────────────────────

/// Summary of a single completed test run.
#[derive(Debug, Clone)]
pub struct RunSummary {
    pub run_id: i64,
    pub pass_count: u32,
    pub fail_count: u32,
    pub exit_code: i32,
    pub finished_at: SystemTime,
}

impl RunSummary {
    pub fn passed(&self) -> bool {
        self.exit_code == 0 && self.fail_count == 0
    }
}

/// A single test result from a run.
#[derive(Debug, Clone)]
pub struct TestResult {
    pub run_id: i64,
    pub kind: TestResultKind,
    pub name: String,
    /// Output truncated to [`OUTPUT_TRUNCATE_CHARS`] if `output_truncated` is true.
    pub output: Option<String>,
    pub output_truncated: bool,
}

/// Whether the result was a pass or failure.
#[derive(Debug, Clone, PartialEq)]
pub enum TestResultKind {
    Pass,
    Fail,
}

impl TestResultKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

/// Maximum characters stored per test failure output. Full output is stored in
/// `test_runs.raw_output` and retrievable via `get_run_output`.
pub const OUTPUT_TRUNCATE_CHARS: usize = 4096;

// ─── Store ────────────────────────────────────────────────────────────────────

/// SQLite store for test runner results.
///
/// Thread-safe: wraps the `rusqlite::Connection` in a `tokio::sync::Mutex`.
/// Individual operations are fast (microseconds) so we do not need
/// `spawn_blocking`.
pub struct TestResultStore {
    conn: Arc<Mutex<Connection>>,
}

impl TestResultStore {
    /// Open or create the database at `db_path`. Runs schema migrations on
    /// every open — idempotent, cheap.
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let conn = Connection::open(db_path).map_err(|e| {
            Error::Io(std::io::Error::other(format!(
                "TestResultStore::open {}: {e}",
                db_path.display()
            )))
        })?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| Error::Io(std::io::Error::other(format!("schema migration: {e}"))))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Insert a new run row and return its generated id.
    pub async fn begin_run(&self) -> Result<i64> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO test_runs (started_at, pass_count, fail_count) VALUES (?1, 0, 0)",
            params![now],
        )
        .map_err(sqlite_err)?;
        Ok(conn.last_insert_rowid())
    }

    /// Record a single pass/fail result for `run_id`. Truncates `output` at
    /// [`OUTPUT_TRUNCATE_CHARS`] characters and sets `output_truncated`.
    pub async fn record_result(
        &self,
        run_id: i64,
        kind: TestResultKind,
        name: &str,
        output: Option<&str>,
    ) -> Result<()> {
        let (stored_output, truncated) = match output {
            None => (None, false),
            Some(s) if s.len() <= OUTPUT_TRUNCATE_CHARS => (Some(s.to_string()), false),
            Some(s) => (Some(s[..OUTPUT_TRUNCATE_CHARS].to_string()), true),
        };

        // Increment the appropriate counter atomically.
        let counter_col = match kind {
            TestResultKind::Pass => "pass_count",
            TestResultKind::Fail => "fail_count",
        };

        let conn = self.conn.lock().await;
        conn.execute(
            &format!("UPDATE test_runs SET {counter_col} = {counter_col} + 1 WHERE id = ?1"),
            params![run_id],
        )
        .map_err(sqlite_err)?;

        conn.execute(
            "INSERT INTO test_results (run_id, kind, name, output, output_truncated) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![run_id, kind.as_str(), name, stored_output, truncated as i32],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    /// Mark a run as finished. Updates `finished_at` and `exit_code`.
    pub async fn finish_run(&self, run_id: i64, exit_code: i32) -> Result<()> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE test_runs SET finished_at = ?1, exit_code = ?2 WHERE id = ?3",
            params![now, exit_code, run_id],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    /// Store the full raw output for the run (Tier 1 output, unstructured).
    pub async fn store_raw_output(&self, run_id: i64, raw: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE test_runs SET raw_output = ?1 WHERE id = ?2",
            params![raw, run_id],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    /// Return the summary of the most recently *finished* run, if any.
    pub async fn latest_run(&self) -> Result<Option<RunSummary>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, pass_count, fail_count, exit_code, finished_at \
                 FROM test_runs \
                 WHERE finished_at IS NOT NULL \
                 ORDER BY id DESC LIMIT 1",
            )
            .map_err(sqlite_err)?;

        let row = stmt
            .query_row([], |row| {
                Ok(RunSummary {
                    run_id: row.get(0)?,
                    pass_count: row.get::<_, i64>(1)? as u32,
                    fail_count: row.get::<_, i64>(2)? as u32,
                    exit_code: row.get(3)?,
                    finished_at: unix_to_system_time(row.get::<_, i64>(4)?),
                })
            })
            .optional()
            .map_err(sqlite_err)?;

        Ok(row)
    }

    /// Return the failures from the most recently finished run (up to `limit`).
    pub async fn latest_failures(&self, limit: usize) -> Result<Vec<TestResult>> {
        let conn = self.conn.lock().await;
        let run_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM test_runs WHERE finished_at IS NOT NULL ORDER BY id DESC LIMIT 1",
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
                "SELECT run_id, kind, name, output, output_truncated \
                 FROM test_results \
                 WHERE run_id = ?1 AND kind = 'fail' \
                 LIMIT ?2",
            )
            .map_err(sqlite_err)?;

        let rows = stmt
            .query_map(params![run_id, limit as i64], |row| {
                let kind_str: String = row.get(1)?;
                Ok(TestResult {
                    run_id: row.get(0)?,
                    kind: if kind_str == "pass" {
                        TestResultKind::Pass
                    } else {
                        TestResultKind::Fail
                    },
                    name: row.get(2)?,
                    output: row.get(3)?,
                    output_truncated: row.get::<_, i32>(4)? != 0,
                })
            })
            .map_err(sqlite_err)?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(sqlite_err)?);
        }
        Ok(out)
    }

    /// Return the full raw output for a run (for `get_run_output` tool).
    pub async fn raw_output(&self, run_id: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT raw_output FROM test_runs WHERE id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_err)
    }

    /// Mark files as stale (changed since last completed run). Called by
    /// `TestWatcher::on_files_changed`.
    pub async fn mark_stale(&self, paths: &[PathBuf]) -> Result<()> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        for path in paths {
            conn.execute(
                "INSERT OR REPLACE INTO test_stale_files (path, marked_at) VALUES (?1, ?2)",
                params![path.to_string_lossy().as_ref(), now],
            )
            .map_err(sqlite_err)?;
        }
        Ok(())
    }

    /// Clear the stale-files table. Called when a new run starts.
    pub async fn clear_stale(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute("DELETE FROM test_stale_files", [])
            .map_err(sqlite_err)?;
        Ok(())
    }

    /// Return all paths that have been marked stale (changed since last run).
    pub async fn stale_files_since_last_run(&self) -> Result<Vec<PathBuf>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT path FROM test_stale_files ORDER BY marked_at ASC")
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

    /// Returns true if there are any stale files (run results may be outdated).
    pub async fn has_stale_files(&self) -> Result<bool> {
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM test_stale_files", [], |r| r.get(0))
            .map_err(sqlite_err)?;
        Ok(count > 0)
    }

    /// Delete rows where `finished_at IS NULL` — these are the orphans
    /// left when a watcher process was killed mid-run (SIGKILL, panic,
    /// machine sleep with the daemon's tokio task aborted before
    /// `finish_run` could fire). Without this, [`run_in_progress`]
    /// returns `true` indefinitely against a stale row, and
    /// `test_status` reports `running` forever.
    ///
    /// Idempotent. Returns the number of orphans purged. Safe to call
    /// at watcher startup before any new runs begin.
    pub async fn clear_orphan_runs(&self) -> Result<usize> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute("DELETE FROM test_runs WHERE finished_at IS NULL", [])
            .map_err(sqlite_err)?;
        Ok(n)
    }

    /// Returns true if the most recent run is still in progress.
    ///
    /// Checks only the latest row — abandoned runs (task aborted before
    /// `finish_run` was called) leave orphaned NULL rows that would otherwise
    /// make this return true forever. Watcher startup should call
    /// [`clear_orphan_runs`] to wipe such rows from a previous process.
    pub async fn run_in_progress(&self) -> Result<bool> {
        let conn = self.conn.lock().await;
        // None → no runs at all; Some(None) → latest run not finished; Some(Some(_)) → finished.
        let finished_at: Option<Option<i64>> = conn
            .query_row(
                "SELECT finished_at FROM test_runs ORDER BY id DESC LIMIT 1",
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

CREATE TABLE IF NOT EXISTS test_runs (
    id          INTEGER PRIMARY KEY,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    exit_code   INTEGER,
    pass_count  INTEGER NOT NULL DEFAULT 0,
    fail_count  INTEGER NOT NULL DEFAULT 0,
    raw_output  TEXT
);

CREATE TABLE IF NOT EXISTS test_results (
    id               INTEGER PRIMARY KEY,
    run_id           INTEGER NOT NULL REFERENCES test_runs(id) ON DELETE CASCADE,
    kind             TEXT    NOT NULL CHECK(kind IN ('pass','fail')),
    name             TEXT    NOT NULL,
    output           TEXT,
    output_truncated INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_test_results_run ON test_results(run_id);

CREATE TABLE IF NOT EXISTS test_stale_files (
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

    async fn make_store() -> TestResultStore {
        let dir = tempfile::tempdir().unwrap();
        TestResultStore::open(&dir.path().join("test_results.db")).unwrap()
    }

    #[tokio::test]
    async fn begin_and_finish_run() {
        let store = make_store().await;
        let run_id = store.begin_run().await.unwrap();
        assert!(run_id > 0);
        store.finish_run(run_id, 0).await.unwrap();
        let summary = store.latest_run().await.unwrap().unwrap();
        assert_eq!(summary.run_id, run_id);
        assert_eq!(summary.exit_code, 0);
    }

    #[tokio::test]
    async fn record_pass_and_fail() {
        let store = make_store().await;
        let run_id = store.begin_run().await.unwrap();
        store
            .record_result(run_id, TestResultKind::Pass, "test_a", None)
            .await
            .unwrap();
        store
            .record_result(
                run_id,
                TestResultKind::Fail,
                "test_b",
                Some("assertion failed"),
            )
            .await
            .unwrap();
        store.finish_run(run_id, 1).await.unwrap();

        let summary = store.latest_run().await.unwrap().unwrap();
        assert_eq!(summary.pass_count, 1);
        assert_eq!(summary.fail_count, 1);
        assert!(!summary.passed());

        let failures = store.latest_failures(10).await.unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].name, "test_b");
    }

    #[tokio::test]
    async fn output_truncated_at_limit() {
        let store = make_store().await;
        let run_id = store.begin_run().await.unwrap();
        let long_output = "x".repeat(OUTPUT_TRUNCATE_CHARS + 100);
        store
            .record_result(run_id, TestResultKind::Fail, "heavy", Some(&long_output))
            .await
            .unwrap();
        store.finish_run(run_id, 1).await.unwrap();

        let failures = store.latest_failures(1).await.unwrap();
        let f = &failures[0];
        assert!(f.output_truncated);
        assert_eq!(f.output.as_ref().unwrap().len(), OUTPUT_TRUNCATE_CHARS);
    }

    #[tokio::test]
    async fn stale_files_roundtrip() {
        let store = make_store().await;
        assert!(!store.has_stale_files().await.unwrap());

        store
            .mark_stale(&[PathBuf::from("src/foo.rs"), PathBuf::from("src/bar.rs")])
            .await
            .unwrap();
        assert!(store.has_stale_files().await.unwrap());

        let stale = store.stale_files_since_last_run().await.unwrap();
        assert_eq!(stale.len(), 2);

        store.clear_stale().await.unwrap();
        assert!(!store.has_stale_files().await.unwrap());
    }

    #[tokio::test]
    async fn clear_orphan_runs_purges_unfinished_rows() {
        let store = make_store().await;
        let r1 = store.begin_run().await.unwrap();
        store.finish_run(r1, 0).await.unwrap();
        let _r2 = store.begin_run().await.unwrap();

        // Latest row is unfinished — run_in_progress would report
        // true forever without cleanup.
        assert!(store.run_in_progress().await.unwrap());

        let purged = store.clear_orphan_runs().await.unwrap();
        assert_eq!(purged, 1);

        assert!(!store.run_in_progress().await.unwrap());
    }

    #[tokio::test]
    async fn clear_orphan_runs_is_idempotent_and_safe_when_empty() {
        let store = make_store().await;
        assert_eq!(store.clear_orphan_runs().await.unwrap(), 0);
        let r1 = store.begin_run().await.unwrap();
        store.finish_run(r1, 0).await.unwrap();
        assert_eq!(store.clear_orphan_runs().await.unwrap(), 0);
    }
}
