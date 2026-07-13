// SPDX-License-Identifier: AGPL-3.0-or-later
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
//! ## Queue-one-rerun, don't abort
//!
//! On every `on_files_changed` call, if a run is already in flight the watcher
//! sets a `rerun_pending` flag and does NOT abort — the in-flight task will
//! start one more iteration when it finishes. This prevents the abort-storm
//! seen during active coding (every keystroke killing a long-running test
//! suite and starting over from scratch — net forward progress: zero).
//! `force_run` (operator-triggered) still preempts so RunTestsTool callers
//! see a fresh result quickly. The stale-files table is updated before the
//! new run starts so WatcherStatus reflects current reality even during a run.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinHandle;

use crate::test_results::{TestResultKind, TestResultStore};
use crate::watcher_coordinator::{BackgroundWatcher, WatcherStatus};
use corpus_engine_yield::YieldHook;

/// Default cooldown after a subprocess completes before consuming a
/// queued rerun. See `lint_watcher::DEFAULT_RERUN_COOLDOWN_MS` for
/// rationale.
const DEFAULT_RERUN_COOLDOWN_MS: u64 = 3000;

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
    rerun_pending: Arc<AtomicBool>,
    /// Optional shared run slot — see `LintWatcher::run_slot`. Sharing
    /// one `Arc<Semaphore>` with 1 permit between the lint and test
    /// watchers serializes their cargo subprocesses so they don't
    /// compound memory pressure.
    run_slot: Option<Arc<Semaphore>>,
    cooldown_ms: u64,
    /// Optional foreground back-pressure hook. When set, the
    /// subprocess runner waits until `should_yield()` returns false
    /// before spawning cargo. Same rationale as LintWatcher — a
    /// workspace cargo-test mid-inference can push RSS past jetsam.
    yield_hook: Arc<std::sync::RwLock<Option<Arc<dyn YieldHook>>>>,
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
            rerun_pending: Arc::new(AtomicBool::new(false)),
            run_slot: None,
            cooldown_ms: DEFAULT_RERUN_COOLDOWN_MS,
            yield_hook: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Wire in a foreground back-pressure hook. Late-binding via
    /// interior mutability — daemon_cmd builds the watcher before
    /// EmbeddedDaemon, then installs the hook after startup.
    pub fn set_yield_hook(&self, hook: Arc<dyn YieldHook>) {
        if let Ok(mut guard) = self.yield_hook.write() {
            *guard = Some(hook);
        }
    }

    /// Share a run slot with the `LintWatcher` (or any other heavy
    /// subprocess watcher) to serialize cargo invocations.
    pub fn with_run_slot(mut self, slot: Arc<Semaphore>) -> Self {
        self.run_slot = Some(slot);
        self
    }

    /// Override the post-run cooldown (default 3s). Set to 0 to fire
    /// queued reruns immediately.
    pub fn with_cooldown_ms(mut self, ms: u64) -> Self {
        self.cooldown_ms = ms;
        self
    }

    /// Force a run immediately regardless of file changes. Used by
    /// `RunTestsTool` to let agents trigger a synchronous test run.
    pub async fn force_run(&self) {
        self.spawn_run(true).await;
    }

    /// Spawn a fresh run. If `preempt` is true, aborts any in-flight run
    /// first (force_run path). If false, the caller must verify no run is in
    /// flight before calling — see [`on_files_changed`].
    async fn spawn_run(&self, preempt: bool) {
        if preempt {
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
        let rerun_pending = Arc::clone(&self.rerun_pending);
        let run_slot = self.run_slot.clone();
        let cooldown_ms = self.cooldown_ms;
        let yield_hook_shared = Arc::clone(&self.yield_hook);

        let handle = tokio::spawn(async move {
            // Loop: run the test subprocess, then check whether any file
            // changes arrived during the run. If so, take the flag and run
            // again — exactly one follow-up iteration, no matter how many
            // changes piled up.
            loop {
                // Serialize against sibling watchers (typically the lint
                // runner) holding the shared slot.
                let _permit = match &run_slot {
                    Some(slot) => match slot.clone().acquire_owned().await {
                        Ok(p) => {
                            tracing::debug!("TestWatcher: acquired shared run slot");
                            Some(p)
                        }
                        Err(_) => {
                            tracing::warn!("TestWatcher: shared run slot closed");
                            break;
                        }
                    },
                    None => None,
                };

                // Foreground back-pressure (mirrors LintWatcher).
                // Re-read the hook each iteration so a late-installed
                // hook activates on the next cargo invocation.
                let mut logged_wait = false;
                loop {
                    let hook_now = yield_hook_shared.read().ok().and_then(|g| g.clone());
                    match hook_now {
                        Some(h) if h.should_yield() => {
                            if !logged_wait {
                                tracing::info!(
                                    "TestWatcher: yielding to foreground inference; deferring cargo run"
                                );
                                logged_wait = true;
                            }
                            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                        }
                        _ => break,
                    }
                }
                if logged_wait {
                    tracing::info!("TestWatcher: foreground idle — proceeding with cargo run");
                }

                if let Err(e) = run_subprocess(
                    command.clone(),
                    working_dir.clone(),
                    timeout_secs,
                    Arc::clone(&store),
                )
                .await
                {
                    tracing::warn!("test runner failed: {e}");
                }
                // Release the permit before the cooldown so a sibling
                // watcher can take the slot while we wait.
                drop(_permit);

                if rerun_pending.swap(false, Ordering::SeqCst) {
                    if cooldown_ms > 0 {
                        tracing::info!(
                            cooldown_ms,
                            "TestWatcher: rerun queued — sleeping for cooldown to coalesce further edits"
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(cooldown_ms)).await;
                        rerun_pending.store(false, Ordering::SeqCst);
                    }
                    tracing::info!("TestWatcher: rerunning — file changes arrived during last run");
                    continue;
                }
                break;
            }
            // Clear the handle when done so we don't hold a dangling Arc.
            let mut guard = handle_slot.lock().await;
            *guard = None;
        });

        let mut guard = self.run_handle.lock().await;
        *guard = Some(handle);
    }

    /// True iff a subprocess run is currently executing.
    async fn run_in_flight(&self) -> bool {
        self.run_handle
            .lock()
            .await
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
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
        // Mark changed files as stale before restarting so the status reflects
        // "stale" immediately — even before the new run finishes.
        if let Err(e) = self.store.mark_stale(&paths).await {
            tracing::warn!("TestWatcher: failed to mark stale files: {e}");
        }
        if self.run_in_flight().await {
            // Don't abort the in-flight run — test suites can take minutes
            // on a large workspace, and aborting them on every keystroke
            // means the operator never sees a fresh result during active
            // coding. Set the rerun flag; the in-flight task will pick it
            // up when done.
            self.rerun_pending.store(true, Ordering::SeqCst);
            tracing::info!(
                count = paths.len(),
                "TestWatcher: files changed during in-flight run; queued rerun"
            );
        } else {
            tracing::info!(
                count = paths.len(),
                "TestWatcher: files changed, starting run"
            );
            self.spawn_run(false).await;
        }
    }

    async fn current_status(&self) -> WatcherStatus {
        if self.run_in_flight().await {
            return WatcherStatus::Running;
        }

        match self.store.latest_run().await {
            Err(_) | Ok(None) => WatcherStatus::NeverRun,
            Ok(Some(summary)) => match self.store.stale_files_since_last_run().await {
                Ok(stale) if stale.is_empty() => WatcherStatus::Fresh {
                    pass: summary.passed(),
                    last_run_at: summary.finished_at,
                },
                Ok(stale) => WatcherStatus::Stale { stale_since: stale },
                Err(_) => WatcherStatus::Fresh {
                    pass: summary.passed(),
                    last_run_at: summary.finished_at,
                },
            },
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
    child_cmd.kill_on_drop(true); // kill child when task is aborted or runtime drops
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
                tracing::warn!(timeout_secs, "test runner: timed out — killing process");
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
    tracing::info!(run_id, exit_code, elapsed_ms, "test run finished");

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
            tracing::info!(run_id, "test runner summary: {}", line);
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

    /// Second on_files_changed during in-flight run queues a rerun rather
    /// than aborting. Both file paths are still marked stale immediately.
    #[tokio::test]
    async fn queues_rerun_during_in_flight_run() {
        let store = make_store().await;
        // First command: sleep for 30 seconds (would be cancelled under old
        // semantics; under new semantics it stays running).
        let watcher = make_watcher("sleep 30", Arc::clone(&store));

        watcher
            .on_files_changed(vec![PathBuf::from("src/foo.rs")])
            .await;
        // Fire again — must not abort the in-flight sleep.
        watcher
            .on_files_changed(vec![PathBuf::from("src/bar.rs")])
            .await;

        // Both paths get marked stale immediately.
        let stale = store.stale_files_since_last_run().await.unwrap();
        assert_eq!(stale.len(), 2);

        // And the in-flight run is still running — confirms we didn't
        // abort it the way the old `cancels_in_flight_run` test expected.
        assert!(watcher.run_in_flight().await);
        // The second flush should have set the rerun flag.
        assert!(watcher.rerun_pending.load(Ordering::SeqCst));
    }
}
