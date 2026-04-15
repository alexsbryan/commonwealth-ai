//! Lint-runner as a [`BackgroundWatcher`] plugin.
//!
//! Nearly identical to [`super::test_watcher::TestWatcher`] — the differences
//! are the Tier 2 event schema (adds `warn` kind, `line`/`col` fields) and the
//! tighter output truncation (500 chars).
//!
//! ## Tier 2 lint protocol
//!
//! ```jsonc
//! {"t":"pass","n":"src/auth/login.rs"}
//! {"t":"fail","n":"src/auth/login.rs","out":"error[E0499]: cannot borrow...","line":34,"col":5}
//! {"t":"warn","n":"src/auth/login.rs","out":"warning: unused variable","line":12,"col":9}
//! {"t":"summary","pass":47,"fail":1,"warn":3,"ms":2841}
//! ```
//!
//! Warnings (exit code 0) do NOT make the run status "failing". Only non-zero
//! exit code or at least one `fail` event marks a run as failed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::lint_results::{LintResultKind, LintResultStore};
use crate::update::watcher_coordinator::{BackgroundWatcher, WatcherStatus};

// ─── LintWatcher ─────────────────────────────────────────────────────────────

/// Runs a configured lint command on file changes and stores results for MCP
/// queries.
pub struct LintWatcher {
    command: String,
    working_dir: Option<PathBuf>,
    timeout_secs: u64,
    store: Arc<LintResultStore>,
    run_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl LintWatcher {
    pub fn new(
        command: impl Into<String>,
        working_dir: Option<PathBuf>,
        timeout_secs: u64,
        store: Arc<LintResultStore>,
    ) -> Self {
        Self {
            command: command.into(),
            working_dir,
            timeout_secs,
            store,
            run_handle: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn force_run(&self) {
        self.start_run().await;
    }

    async fn start_run(&self) {
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
            if let Err(e) = run_lint_subprocess(command, working_dir, timeout_secs, store).await {
                tracing::warn!("lint runner failed: {e}");
            }
            let mut guard = handle_slot.lock().await;
            *guard = None;
        });

        let mut guard = self.run_handle.lock().await;
        *guard = Some(handle);
    }
}

#[async_trait]
impl BackgroundWatcher for LintWatcher {
    fn id(&self) -> &'static str {
        "lint"
    }

    fn description(&self) -> &'static str {
        "Lint runner (cargo check / clippy / eslint / mypy)"
    }

    async fn on_files_changed(&self, paths: Vec<PathBuf>) {
        tracing::info!(
            count = paths.len(),
            "LintWatcher: files changed, (re)starting lint"
        );
        if let Err(e) = self.store.mark_stale(&paths).await {
            tracing::warn!("LintWatcher: failed to mark stale: {e}");
        }
        self.start_run().await;
    }

    async fn current_status(&self) -> WatcherStatus {
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

async fn run_lint_subprocess(
    command: String,
    working_dir: Option<PathBuf>,
    timeout_secs: u64,
    store: Arc<LintResultStore>,
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
            tracing::warn!(command = %command, "lint runner: spawn failed: {e}");
            store.finish_run(run_id, -1, 0).await?;
            return Ok(());
        }
    };

    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");

    let store_inner = Arc::clone(&store);
    let mut raw_lines: Vec<String> = Vec::new();
    let mut reader = BufReader::new(stdout).lines();

    let _stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(line = %line, "lint runner stderr");
        }
    });

    let timeout = tokio::time::Duration::from_secs(timeout_secs);
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        match tokio::time::timeout_at(deadline, reader.next_line()).await {
            Err(_) => {
                tracing::warn!(timeout_secs, "lint runner timed out");
                let _ = child.kill().await;
                store_inner.finish_run(run_id, -2, start.elapsed().as_millis() as u64).await?;
                return Ok(());
            }
            Ok(Err(e)) => {
                tracing::warn!("lint runner stdout read error: {e}");
                break;
            }
            Ok(Ok(None)) => break,
            Ok(Ok(Some(line))) => {
                raw_lines.push(line.clone());
                parse_and_record_lint_event(&line, run_id, &store_inner).await;
            }
        }
    }

    let exit_code = match child.wait().await {
        Ok(status) => status.code().unwrap_or(-1),
        Err(_) => -1,
    };

    let elapsed = start.elapsed().as_millis() as u64;
    tracing::info!(run_id, exit_code, elapsed_ms = elapsed, "lint run finished");

    let raw = raw_lines.join("\n");
    store.store_raw_output(run_id, &raw).await?;
    store.finish_run(run_id, exit_code, elapsed).await?;

    Ok(())
}

async fn parse_and_record_lint_event(line: &str, run_id: i64, store: &LintResultStore) {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
        tracing::trace!("lint tier-1 line: {line}");
        return;
    };

    let Some(t) = val.get("t").and_then(|v| v.as_str()) else {
        return;
    };

    match t {
        "pass" => {
            let file = val.get("n").and_then(|v| v.as_str()).unwrap_or("unknown");
            if let Err(e) = store
                .record_result(run_id, LintResultKind::Pass, file, None, None, None)
                .await
            {
                tracing::warn!("LintWatcher: record pass: {e}");
            }
        }
        "fail" | "warn" => {
            let kind = if t == "fail" { LintResultKind::Fail } else { LintResultKind::Warn };
            let file = val.get("n").and_then(|v| v.as_str()).unwrap_or("unknown");
            let output = val.get("out").and_then(|v| v.as_str());
            let line = val.get("line").and_then(|v| v.as_u64()).map(|v| v as u32);
            let col = val.get("col").and_then(|v| v.as_u64()).map(|v| v as u32);
            if let Err(e) = store
                .record_result(run_id, kind, file, output, line, col)
                .await
            {
                tracing::warn!("LintWatcher: record {t}: {e}");
            }
        }
        "summary" => {
            tracing::info!(run_id, "lint summary: {line}");
        }
        _ => {}
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_store() -> Arc<LintResultStore> {
        let dir = tempfile::tempdir().unwrap();
        Arc::new(LintResultStore::open(&dir.path().join("lint.db")).unwrap())
    }

    fn make_watcher(command: &str, store: Arc<LintResultStore>) -> LintWatcher {
        LintWatcher::new(command, None, 10, store)
    }

    #[test]
    fn lint_watcher_as_background_watcher() {
        let store = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(make_store());
        let watcher = Arc::new(make_watcher("echo done", store));
        let _: Arc<dyn BackgroundWatcher> = watcher;
    }

    #[tokio::test]
    async fn lint_tier2_warn_not_failure() {
        let store = make_store().await;
        let cmd = r#"echo '{"t":"warn","n":"src/foo.rs","out":"unused var","line":1}' && echo '{"t":"summary","pass":1,"fail":0,"warn":1,"ms":50}'"#;
        let watcher = make_watcher(cmd, Arc::clone(&store));
        watcher.force_run().await;
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        let summary = store.latest_run().await.unwrap().unwrap();
        assert_eq!(summary.warn_count, 1);
        assert_eq!(summary.fail_count, 0);
        // Exit code 0 with only warnings: should still be passing.
        assert!(summary.passed());
    }

    #[tokio::test]
    async fn lint_fail_event_recorded() {
        let store = make_store().await;
        let cmd = r#"echo '{"t":"fail","n":"src/bar.rs","out":"type mismatch","line":10,"col":3}' && exit 1"#;
        let watcher = make_watcher(cmd, Arc::clone(&store));
        watcher.force_run().await;
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        let failures = store.latest_failures(10).await.unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].file, "src/bar.rs");
        assert_eq!(failures[0].line, Some(10));
        assert_eq!(failures[0].col, Some(3));
    }
}
