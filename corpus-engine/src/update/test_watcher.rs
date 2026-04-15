//! Test-suite runner as a [`BackgroundWatcher`] plugin.
//!
//! [`TestWatcher`] runs a configured shell command on file changes, parses its
//! stdout as Tier 2 JSONL events, and stores structured results in a SQLite
//! database for the `test_status` and `get_run_output` MCP tools to read.
//!
//! ## Tier 2 protocol
//!
//! The subprocess writes one JSON object per line to stdout:
//!
//! ```jsonc
//! {"t":"pass","n":"module::test_name"}
//! {"t":"fail","n":"module::test_name","out":"thread 'main' panicked at 'assertion failed'..."}
//! {"t":"summary","pass":47,"fail":1,"ms":2841}
//! ```
//!
//! Lines that don't match this schema are treated as Tier 1 output (raw text
//! logged at debug level). The subprocess may emit both Tier 2 events and
//! prose output — the parser handles intermixed lines gracefully.
//!
//! ## Cancel-and-restart
//!
//! On every `on_files_changed` call, any in-progress run is aborted by killing
//! the child process and abandoning the task. The stale-files table is updated
//! before the new run starts so the WatcherStatus reflects current reality even
//! during a run.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::test_results::{TestResultKind, TestResultStore};
use crate::update::watcher_coordinator::{BackgroundWatcher, WatcherStatus};

// ─── TestWatcher ─────────────────────────────────────────────────────────────

/// Runs a configured test command on file changes and stores results for MCP
/// queries. Implements `BackgroundWatcher` for use with `WatcherCoordinator`.
pub struct TestWatcher {
    command: String,
    working_dir: Option<PathBuf>,
    timeout_secs: u64,
    store: Arc<TestResultStore>,
    /// Current in-progress run handle. `None` when idle.
    run_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl TestWatcher {
    pub fn new(
        command: impl Into<String>,
        working_dir: Option<PathBuf>,
        timeout_secs: u64,
        store: Arc<TestResultStore>,
    ) -> Self {
        Self {
            command: command.into(),
            working_dir,
            timeout_secs,
            store,
            run_handle: Arc::new(Mutex::new(None)),
        }
    }

    /// Force a run immediately regardless of file changes. Used by
    /// `RunTestsTool` to let agents trigger a synchronous test run.
    pub async fn force_run(&self) {
        self.start_run().await;
    }

    /// Cancel the current in-progress run (if any) and start a new one.
    async fn start_run(&self) {
        // Cancel previous run.
        {
            let mut guard = self.run_handle.lock().await;
            if let Some(handle) = guard.take() {
                handle.abort();
            }
        }

        let command = self.command.clone();
        let working_dir = self.working_dir.clone();
        let timeout_secs = self.timeout_secs;
        let store = Arc::clone(&self.store);
        let handle_slot = Arc::clone(&self.run_handle);

        let handle = tokio::spawn(async move {
            if let Err(e) = run_subprocess(command, working_dir, timeout_secs, store).await {
                tracing::warn!("test runner failed: {e}");
            }
            // Clear the handle when done so we don't hold a dangling Arc.
            let mut guard = handle_slot.lock().await;
            *guard = None;
        });

        let mut guard = self.run_handle.lock().await;
        *guard = Some(handle);
    }
}

#[async_trait]
impl BackgroundWatcher for TestWatcher {
    fn id(&self) -> &'static str {
        "test"
    }

    fn description(&self) -> &'static str {
        "Test suite runner"
    }

    async fn on_files_changed(&self, paths: Vec<PathBuf>) {
        tracing::info!(
            count = paths.len(),
            "TestWatcher: files changed, (re)starting run"
        );
        // Mark changed files as stale before restarting so the status reflects
        // "stale" immediately — even before the new run finishes.
        if let Err(e) = self.store.mark_stale(&paths).await {
            tracing::warn!("TestWatcher: failed to mark stale files: {e}");
        }
        self.start_run().await;
    }

    async fn current_status(&self) -> WatcherStatus {
        // Check if a run is actively in progress.
        let running = self
            .run_handle
            .lock()
            .await
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false);

        if running {
            return WatcherStatus::Running;
        }

        match self.store.latest_run().await {
            Err(_) | Ok(None) => WatcherStatus::NeverRun,
            Ok(Some(summary)) => {
                match self.store.stale_files_since_last_run().await {
                    Ok(stale) if stale.is_empty() => WatcherStatus::Fresh {
                        pass: summary.passed(),
                        last_run_at: summary.finished_at,
                    },
                    Ok(stale) => WatcherStatus::Stale { stale_since: stale },
                    Err(_) => WatcherStatus::Fresh {
                        pass: summary.passed(),
                        last_run_at: summary.finished_at,
                    },
                }
            }
        }
    }
}

// ─── Subprocess runner ────────────────────────────────────────────────────────

/// Run the test command, parse Tier 2 events from stdout, and store results.
async fn run_subprocess(
    command: String,
    working_dir: Option<PathBuf>,
    timeout_secs: u64,
    store: Arc<TestResultStore>,
) -> crate::error::Result<()> {
    let run_id = store.begin_run().await?;
    store.clear_stale().await?;

    let start = Instant::now();
    let mut child_cmd = Command::new("sh");
    child_cmd.arg("-c").arg(&command);
    child_cmd.stdout(std::process::Stdio::piped());
    child_cmd.stderr(std::process::Stdio::piped());

    if let Some(ref dir) = working_dir {
        child_cmd.current_dir(dir);
    }

    let mut child = match child_cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(command = %command, "test runner: spawn failed: {e}");
            store.finish_run(run_id, -1).await?;
            return Ok(());
        }
    };

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    let store_stdout = Arc::clone(&store);
    let mut raw_output_lines: Vec<String> = Vec::new();

    // Read stdout line by line, parsing Tier 2 events.
    let mut reader = BufReader::new(stdout).lines();
    let stderr_reader = BufReader::new(stderr);

    // Drain stderr in background so the process doesn't block on it.
    let _stderr_task = tokio::spawn(async move {
        let mut lines = stderr_reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(line = %line, "test runner stderr");
        }
    });

    // Timeout wrapper.
    let timeout = tokio::time::Duration::from_secs(timeout_secs);
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        match tokio::time::timeout_at(deadline, reader.next_line()).await {
            Err(_elapsed) => {
                tracing::warn!(
                    timeout_secs,
                    "test runner: timed out — killing process"
                );
                let _ = child.kill().await;
                store_stdout.finish_run(run_id, -2).await?;
                return Ok(());
            }
            Ok(Err(e)) => {
                tracing::warn!("test runner: stdout read error: {e}");
                break;
            }
            Ok(Ok(None)) => break, // EOF
            Ok(Ok(Some(line))) => {
                raw_output_lines.push(line.clone());
                tracing::trace!(line = %line, "test runner stdout");
                parse_and_record_tier2(&line, run_id, &store_stdout).await;
            }
        }
    }

    // Wait for child to exit.
    let exit_code = match child.wait().await {
        Ok(status) => status.code().unwrap_or(-1),
        Err(_) => -1,
    };

    let elapsed_ms = start.elapsed().as_millis() as u64;
    tracing::info!(
        run_id,
        exit_code,
        elapsed_ms,
        "test run finished"
    );

    // Store the full raw output for get_run_output.
    let raw = raw_output_lines.join("\n");
    store.store_raw_output(run_id, &raw).await?;
    store.finish_run(run_id, exit_code).await?;

    Ok(())
}

/// Parse a single line and record it into the store. Non-Tier-2 lines are
/// logged at trace and ignored by the store.
async fn parse_and_record_tier2(line: &str, run_id: i64, store: &TestResultStore) {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
        tracing::trace!("test runner tier-1 line: {line}");
        return;
    };

    let Some(t) = val.get("t").and_then(|v| v.as_str()) else {
        return;
    };

    match t {
        "pass" => {
            if let Some(name) = val.get("n").and_then(|v| v.as_str()) {
                if let Err(e) = store
                    .record_result(run_id, TestResultKind::Pass, name, None)
                    .await
                {
                    tracing::warn!("TestWatcher: record pass failed: {e}");
                }
            }
        }
        "fail" => {
            let name = val.get("n").and_then(|v| v.as_str()).unwrap_or("unknown");
            let output = val.get("out").and_then(|v| v.as_str());
            if let Err(e) = store
                .record_result(run_id, TestResultKind::Fail, name, output)
                .await
            {
                tracing::warn!("TestWatcher: record fail failed: {e}");
            }
        }
        "summary" => {
            // The summary line is informational. We derive pass/fail counts
            // from the individual events, not the summary, so they stay in
            // sync even if the command exits mid-run.
            tracing::info!(
                run_id,
                "test runner summary: {}",
                line
            );
        }
        other => {
            tracing::trace!("test runner unknown event type '{other}': {line}");
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_store() -> Arc<TestResultStore> {
        let dir = tempfile::tempdir().unwrap();
        Arc::new(TestResultStore::open(&dir.path().join("test.db")).unwrap())
    }

    fn make_watcher(command: &str, store: Arc<TestResultStore>) -> TestWatcher {
        TestWatcher::new(command, None, 10, store)
    }

    /// TestWatcher compiles as Arc<dyn BackgroundWatcher>.
    #[test]
    fn test_watcher_as_background_watcher() {
        let store = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(make_store());
        let watcher = Arc::new(make_watcher("echo done", store));
        let _: Arc<dyn BackgroundWatcher> = watcher;
    }

    /// Empty store returns NeverRun status.
    #[tokio::test]
    async fn status_never_run_on_empty_store() {
        let store = make_store().await;
        let watcher = make_watcher("echo done", Arc::clone(&store));
        let status = watcher.current_status().await;
        assert!(matches!(status, WatcherStatus::NeverRun));
    }

    /// Tier 2 pass events are parsed and recorded.
    #[tokio::test]
    async fn tier2_pass_events_recorded() {
        let store = make_store().await;
        let cmd = r#"echo '{"t":"pass","n":"foo::test_a"}' && echo '{"t":"pass","n":"foo::test_b"}' && echo '{"t":"summary","pass":2,"fail":0,"ms":10}'"#;
        let watcher = make_watcher(cmd, Arc::clone(&store));
        // Trigger directly via force_run to avoid file-system dependency.
        watcher.force_run().await;

        // Give the background task a moment to finish.
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        let summary = store.latest_run().await.unwrap().unwrap();
        assert_eq!(summary.pass_count, 2);
        assert_eq!(summary.fail_count, 0);
        assert!(summary.passed());
    }

    /// Tier 2 fail events are parsed and stored with output.
    #[tokio::test]
    async fn tier2_fail_events_recorded() {
        let store = make_store().await;
        let cmd = r#"echo '{"t":"fail","n":"bar::panics","out":"assertion failed at line 42"}' && exit 1"#;
        let watcher = make_watcher(cmd, Arc::clone(&store));
        watcher.force_run().await;

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        let summary = store.latest_run().await.unwrap().unwrap();
        assert_eq!(summary.fail_count, 1);
        assert!(!summary.passed());

        let failures = store.latest_failures(10).await.unwrap();
        assert_eq!(failures[0].name, "bar::panics");
        assert_eq!(
            failures[0].output.as_deref(),
            Some("assertion failed at line 42")
        );
    }

    /// Second on_files_changed cancels the first and starts fresh.
    #[tokio::test]
    async fn cancels_in_flight_run() {
        let store = make_store().await;
        // First command: sleep for 30 seconds (will be cancelled).
        let watcher = make_watcher("sleep 30", Arc::clone(&store));

        // Start a long-running command.
        watcher.on_files_changed(vec![PathBuf::from("src/foo.rs")]).await;
        // Immediately fire again — should cancel the first.
        watcher
            .on_files_changed(vec![PathBuf::from("src/bar.rs")])
            .await;

        // The stale files should include both paths.
        let stale = store.stale_files_since_last_run().await.unwrap();
        assert_eq!(stale.len(), 2);
    }
}
