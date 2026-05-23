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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinHandle;

use crate::lint_results::{LintResultKind, LintResultStore};
use crate::update::watcher_coordinator::{BackgroundWatcher, WatcherStatus};
use crate::yield_hook::YieldHook;

/// Default cooldown after a subprocess completes before consuming a
/// queued rerun. Coalesces bursts of file edits arriving during the
/// previous run into one rather than chain-spawning another full
/// cargo invocation immediately. 3s is short enough that an operator
/// editing one file at a time barely notices, long enough to let
/// macOS reclaim memory from the just-completed cargo process.
const DEFAULT_RERUN_COOLDOWN_MS: u64 = 3000;

// ─── LintWatcher ─────────────────────────────────────────────────────────────

/// Runs a configured lint command on file changes and stores results for MCP
/// queries.
///
/// ## Run semantics
///
/// `on_files_changed` and `force_run` differ deliberately:
///
/// - `on_files_changed` (debounced filesystem events): if a run is already in
///   flight, **does not abort it** — instead sets a `rerun_pending` flag, and
///   the in-flight run's tokio task starts another iteration when it finishes.
///   This prevents the abort-storm seen during active coding (every keystroke
///   killing a 60+ second monorepo `cargo check` and starting over from
///   scratch — net forward progress: zero).
/// - `force_run` (operator-triggered, e.g. RunLintTool / RunTestsTool):
///   preempts the in-flight run because the operator is actively waiting on a
///   fresh result.
///
/// The `rerun_pending` flag collapses arbitrarily many file-change flushes
/// during one run into exactly one follow-up run — bursts of edits can never
/// queue more than one extra iteration.
pub struct LintWatcher {
    command: String,
    working_dir: Option<PathBuf>,
    timeout_secs: u64,
    store: Arc<LintResultStore>,
    run_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
    rerun_pending: Arc<AtomicBool>,
    /// Optional global slot. When set, the subprocess runner acquires
    /// a permit before spawning cargo and releases on completion.
    /// Sharing one `Arc<Semaphore>` (with 1 permit) between the
    /// `LintWatcher` and `TestWatcher` serializes their cargo
    /// invocations — the thundering-herd fix.
    run_slot: Option<Arc<Semaphore>>,
    cooldown_ms: u64,
    /// Optional foreground back-pressure hook. When set, the
    /// subprocess runner waits until `should_yield()` returns false
    /// before spawning cargo. Background freshness work yields to
    /// user-facing inference — a workspace cargo-check during a
    /// 35B chat turn can easily push RSS into jetsam territory.
    ///
    /// Shared via `Arc<RwLock>` between the watcher and its spawned
    /// runner task. Late-binding: daemon_cmd builds the watcher
    /// before EmbeddedDaemon (and thus AppStateYieldHook) exists,
    /// then installs the hook via `set_yield_hook` after
    /// `start_daemon` returns. The runner task re-reads the slot
    /// each iteration so a hook installed mid-run takes effect on
    /// the next cargo invocation.
    yield_hook: Arc<std::sync::RwLock<Option<Arc<dyn YieldHook>>>>,
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
            rerun_pending: Arc::new(AtomicBool::new(false)),
            run_slot: None,
            cooldown_ms: DEFAULT_RERUN_COOLDOWN_MS,
            yield_hook: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Wire in a foreground back-pressure hook. The lint subprocess
    /// waits until `should_yield()` returns false before each run,
    /// preventing the workspace cargo check from contending with a
    /// hot inference slot for memory.
    ///
    /// Interior mutability via `RwLock` lets the daemon install the
    /// hook after the watcher is already running — `daemon_cmd`
    /// builds the watcher early in startup, then EmbeddedDaemon
    /// builds AppStateYieldHook and back-installs it.
    pub fn set_yield_hook(&self, hook: Arc<dyn YieldHook>) {
        if let Ok(mut guard) = self.yield_hook.write() {
            *guard = Some(hook);
        }
    }

    /// Share a run slot with other watchers (typically the
    /// `TestWatcher`). With a 1-permit semaphore the two
    /// subprocess-running watchers serialize instead of
    /// running cargo concurrently and doubling memory pressure.
    pub fn with_run_slot(mut self, slot: Arc<Semaphore>) -> Self {
        self.run_slot = Some(slot);
        self
    }

    /// Override the post-run cooldown (default 3s). Set to 0 to
    /// disable — the next rerun will fire immediately after the
    /// previous one finishes (the pre-cooldown behavior).
    pub fn with_cooldown_ms(mut self, ms: u64) -> Self {
        self.cooldown_ms = ms;
        self
    }

    /// Operator-triggered run. Preempts any in-flight run so the operator
    /// sees a fresh result immediately.
    pub async fn force_run(&self) {
        self.spawn_run(true).await;
    }

    /// Spawn a fresh run. If `preempt` is true, aborts any in-flight run
    /// first. If false, the caller must verify no run is in flight before
    /// calling — see [`on_files_changed`].
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
        // Clone the shared `Arc<RwLock>` so the spawned task can
        // re-read the slot each iteration. A hook installed
        // late (after EmbeddedDaemon starts) becomes active on the
        // next loop iteration without restarting the watcher.
        let yield_hook_shared = Arc::clone(&self.yield_hook);

        let handle = tokio::spawn(async move {
            // Loop: run the lint subprocess, then check whether any file
            // changes arrived during the run. If so, take the flag and run
            // again — exactly one follow-up iteration, no matter how many
            // changes piled up. The check happens AFTER the subprocess
            // completes so an abort-storm can never replace this task.
            loop {
                // Serialize against any sibling watcher (typically the
                // test runner) holding the shared run slot. The permit
                // guard releases on scope exit, even on early-return /
                // panic paths inside the subprocess runner.
                let _permit = match &run_slot {
                    Some(slot) => match slot.clone().acquire_owned().await {
                        Ok(p) => {
                            tracing::debug!("LintWatcher: acquired shared run slot");
                            Some(p)
                        }
                        Err(_) => {
                            // Semaphore closed — bail without running.
                            tracing::warn!("LintWatcher: shared run slot closed");
                            break;
                        }
                    },
                    None => None,
                };

                // Foreground back-pressure: wait until inference has
                // gone idle for the configured yield window. Polls at
                // 5s so the lint run starts within a few seconds of
                // the last chat request, but never overlaps. Without
                // this, a workspace cargo-check (~2-4 GB peak) racing
                // a 35B chat slot has pushed RSS to jetsam threshold.
                let mut logged_wait = false;
                loop {
                    let hook_now = yield_hook_shared
                        .read()
                        .ok()
                        .and_then(|g| g.clone());
                    match hook_now {
                        Some(h) if h.should_yield() => {
                            if !logged_wait {
                                tracing::info!(
                                    "LintWatcher: yielding to foreground inference; deferring cargo run"
                                );
                                logged_wait = true;
                            }
                            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                        }
                        _ => break,
                    }
                }
                if logged_wait {
                    tracing::info!(
                        "LintWatcher: foreground idle — proceeding with cargo run"
                    );
                }

                if let Err(e) =
                    run_lint_subprocess(command.clone(), working_dir.clone(), timeout_secs, Arc::clone(&store)).await
                {
                    tracing::warn!("lint runner failed: {e}");
                }
                // Release the permit BEFORE the cooldown so a sibling
                // watcher can use the slot while we're waiting.
                drop(_permit);

                if rerun_pending.swap(false, Ordering::SeqCst) {
                    if cooldown_ms > 0 {
                        tracing::info!(
                            cooldown_ms,
                            "LintWatcher: rerun queued — sleeping for cooldown to coalesce further edits"
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(cooldown_ms)).await;
                        // Drain any extra flags set during the cooldown
                        // window into this single rerun.
                        rerun_pending.store(false, Ordering::SeqCst);
                    }
                    tracing::info!("LintWatcher: rerunning — file changes arrived during last run");
                    continue;
                }
                break;
            }
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
impl BackgroundWatcher for LintWatcher {
    fn id(&self) -> &'static str {
        "lint"
    }

    fn description(&self) -> &'static str {
        "Lint runner (cargo check / clippy / eslint / mypy)"
    }

    async fn on_files_changed(&self, paths: Vec<PathBuf>) {
        // Sample up to 5 paths for diagnostic logging — helps catch the
        // "watcher loop on its own build artifacts" pattern by showing
        // which paths leaked through `interesting_coordinator_paths`.
        let sample: Vec<String> = paths
            .iter()
            .take(5)
            .map(|p| p.display().to_string())
            .collect();
        if let Err(e) = self.store.mark_stale(&paths).await {
            tracing::warn!("LintWatcher: failed to mark stale: {e}");
        }
        if self.run_in_flight().await {
            // Don't abort the in-flight run — that throws away seconds-to-
            // minutes of compilation work on every keystroke. Set the
            // rerun flag; the in-flight task will pick it up when done.
            self.rerun_pending.store(true, Ordering::SeqCst);
            tracing::info!(
                count = paths.len(),
                ?sample,
                "LintWatcher: files changed during in-flight run; queued rerun"
            );
        } else {
            tracing::info!(
                count = paths.len(),
                ?sample,
                "LintWatcher: files changed, starting lint"
            );
            self.spawn_run(false).await;
        }
    }

    async fn current_status(&self) -> WatcherStatus {
        let running = self.run_in_flight().await;

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

    // Snapshot the paths that triggered this run BEFORE clearing the
    // stale set — the runner script (e.g. `sovereign-lint.sh`) uses
    // them via `SOVEREIGN_CHANGED_PATHS` to scope `cargo check` to
    // the actually-touched crates. Without this the script falls
    // back to `git status`, which sees the entire working tree (not
    // just what fs_change fired for) and runs a near-workspace
    // check on every save. Joined with `:` to mirror Unix PATH and
    // avoid quoting woes for paths with spaces.
    let changed_paths_env: String = store
        .stale_files_since_last_run()
        .await
        .ok()
        .map(|paths| {
            paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(":")
        })
        .unwrap_or_default();

    store.clear_stale().await?;

    let start = Instant::now();
    let mut child_cmd = Command::new("sh");
    child_cmd.arg("-c").arg(&command);
    // Pass the snapshot through so the script can scope. Empty string
    // means "no fs_change context" — the script falls through to its
    // own discovery (workspace check, or a `git status` interactive
    // path), which is the right behavior for the initial run before
    // any events have fired.
    if !changed_paths_env.is_empty() {
        child_cmd.env("SOVEREIGN_CHANGED_PATHS", &changed_paths_env);
    }
    child_cmd.kill_on_drop(true); // kill child when task is aborted or runtime drops
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

    /// Two `on_files_changed` calls during a long-running lint must not
    /// abort the in-flight run. The second flush sets `rerun_pending`,
    /// the in-flight task picks it up when done. Same contract as the
    /// `test_watcher::queues_rerun_during_in_flight_run` test.
    #[tokio::test]
    async fn queues_rerun_during_in_flight_run() {
        let store = make_store().await;
        let watcher = make_watcher("sleep 30", Arc::clone(&store));

        watcher.on_files_changed(vec![PathBuf::from("src/foo.rs")]).await;
        watcher
            .on_files_changed(vec![PathBuf::from("src/bar.rs")])
            .await;

        let stale = store.stale_files_since_last_run().await.unwrap();
        assert_eq!(stale.len(), 2);

        assert!(watcher.run_in_flight().await);
        assert!(watcher.rerun_pending.load(Ordering::SeqCst));
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
