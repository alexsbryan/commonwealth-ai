//! Supervises an external sovereign-cli daemon child process for the
//! desktop app.
//!
//! # Why this module exists
//!
//! Today the desktop's Local mode constructs an `EmbeddedDaemon`
//! in-process within the Tauri host (`bootstrap.rs`). The daemon's
//! inference path links ggml/llama.cpp via `sovereign-inference`, and
//! crashes there have historically been **process-level SEGVs** — see
//! the recent A3B ROCm crash and the Vulkan-fix project memories.
//! When that path lives in-process, a model SEGV takes down the chat
//! UI with no path back. Moving the daemon behind a child-process
//! boundary lets the supervisor catch the crash and present a
//! reconnect surface instead of a dead window.
//!
//! # Responsibilities
//!
//! - Spawn `<binary> <args>` as a child process. `kill_on_drop` is
//!   set so a Tauri panic doesn't orphan the daemon.
//! - Heartbeat the daemon's health URL (typically `/v1/models`) at a
//!   configurable cadence.
//! - On child exit, **or** after N consecutive heartbeat failures,
//!   restart with the configured exponential backoff.
//! - If the daemon crashes more than `crash_loop_max` times within
//!   `crash_loop_window`, stop auto-restarting and surface a `Failed`
//!   state. Only an explicit `request_reconnect()` from the UI wakes
//!   the supervisor back up — auto-relaunch never silently burns CPU
//!   in a tight crash loop.
//! - Drain child stderr into a bounded ring buffer. On each restart,
//!   persist the buffer to `<crash_log_dir>/daemon-<unix-ts>.log` so
//!   the user (or a "send report" action) can attach the trailing
//!   output that preceded the crash.
//! - Publish every state transition over a `broadcast::Sender` so the
//!   Tauri layer can map them to UI events without this module ever
//!   importing tauri.
//!
//! # Non-responsibilities
//!
//! - No Tauri imports. The integration glue lives in `bootstrap.rs` /
//!   `main.rs` and subscribes to this module's channel.
//! - No HTTP client construction beyond the heartbeat — peer
//!   inference and route handling stay in the child daemon.
//! - No knowledge of `SetupConfig` semantics. Callers compute the
//!   binary path, args, ports, and crash-log dir; this module just
//!   spawns and supervises.
//!
//! # Integration pattern
//!
//! ```ignore
//! let supervisor = Arc::new(Supervisor::new(config));
//! let mut states = supervisor.subscribe();
//! let task = {
//!     let sup = Arc::clone(&supervisor);
//!     tokio::spawn(async move { sup.run().await })
//! };
//! // ...UI subscribes to `states`, calls `supervisor.request_reconnect()`...
//! // On app quit:
//! task.abort();
//! ```
//!
//! `JoinHandle::abort()` drops the run future; the in-flight `Child`
//! handle's `kill_on_drop(true)` SIGKILLs the daemon. For v1 that's
//! the same behaviour as `kill -9 sovereign-cli` — the daemon writes
//! `mesh.json` atomically on each state change, so on-disk persistence
//! survives an abrupt termination. A graceful SIGTERM-with-grace path
//! can layer on later without changing this contract.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, Mutex as AsyncMutex};
use tracing::{debug, error, info, warn};

/// Static configuration. All fields required — defaults belong at the
/// call site (the desktop's bootstrap) so test fixtures don't quietly
/// inherit production values.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub binary_path: PathBuf,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// Probed by GET; any 2xx counts as healthy.
    pub health_url: String,
    pub crash_log_dir: PathBuf,
    pub heartbeat_interval: Duration,
    pub heartbeat_timeout: Duration,
    /// Consecutive failed heartbeats before the supervisor kills the
    /// child and counts it as a crash.
    pub heartbeat_failure_threshold: u32,
    /// Schedule of delays before successive restart attempts. After
    /// the last entry, restarts use the final entry indefinitely
    /// (until the crash-loop ceiling triggers `Failed`).
    pub backoff_schedule: Vec<Duration>,
    pub crash_loop_window: Duration,
    pub crash_loop_max: u32,
    pub stderr_ring_lines: usize,
}

/// State the supervisor publishes. Serialised tag is `kind`, so the
/// frontend can `switch` on a discriminant.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SupervisorState {
    /// First boot, or between a previous exit and the next spawn.
    Starting,
    /// Child is running and the last heartbeat succeeded.
    Healthy { pid: u32, since_unix: u64 },
    /// Child is running but the last N heartbeats failed.
    Unhealthy { pid: u32, consecutive_failures: u32 },
    /// Auto-restart in progress. `after_secs` is how long the
    /// supervisor will wait before the next spawn.
    Restarting {
        attempt: u32,
        after_secs: u64,
        reason: String,
    },
    /// Crash-loop ceiling exceeded — auto-restart is OFF. The UI
    /// should surface a Reconnect banner pointing at the last crash
    /// log.
    Failed {
        reason: String,
        last_crash_log: Option<PathBuf>,
    },
}

/// Internal reason a supervise-loop iteration ended. Used to decide
/// whether to count toward the crash-loop ceiling and what reason to
/// surface in the next `Restarting` event.
#[derive(Debug)]
enum ExitOutcome {
    /// Child returned, regardless of status code. We restart for both
    /// successful exits (e.g. someone sent SIGTERM outside our
    /// control) and crashes — the crash-loop ceiling catches
    /// pathological repeats either way.
    ChildExited { code: Option<i32> },
    /// Calling `Child::wait()` itself errored. Rare; treated as a
    /// crash for restart accounting.
    WaitError(String),
    /// Heartbeat threshold exceeded. The supervisor killed the
    /// child; this counts as a crash.
    HeartbeatFailed { consecutive_failures: u32 },
    /// `request_reconnect()` was called while the child was alive.
    /// Not counted as a crash; clears backoff state.
    ManualReconnect,
    /// All references to the supervisor have been dropped (mpsc
    /// channel closed) — the integration layer wants the run loop to
    /// exit. The supervisor kills the child and returns; the outer
    /// `run()` method observes this and breaks out of the loop.
    Shutdown,
}

impl ExitOutcome {
    fn reason(&self) -> String {
        match self {
            Self::ChildExited { code: Some(c) } => format!("daemon exited with code {c}"),
            Self::ChildExited { code: None } => "daemon exited by signal".into(),
            Self::WaitError(e) => format!("wait error: {e}"),
            Self::HeartbeatFailed { consecutive_failures } => {
                format!("daemon stopped responding ({consecutive_failures} failed heartbeats)")
            }
            Self::ManualReconnect => "manual reconnect".into(),
            Self::Shutdown => "supervisor shutdown".into(),
        }
    }
}

pub struct Supervisor {
    config: SupervisorConfig,
    state_tx: broadcast::Sender<SupervisorState>,
    reconnect_tx: mpsc::UnboundedSender<()>,
    reconnect_rx: AsyncMutex<Option<mpsc::UnboundedReceiver<()>>>,
    /// Bounded ring of recent stderr lines. The `Mutex` here is sync,
    /// not async — the stderr drainer task pushes one line at a time
    /// with negligible hold time, so contention against
    /// `stderr_tail()` readers is fine.
    stderr_ring: Arc<Mutex<VecDeque<String>>>,
}

impl Supervisor {
    pub fn new(config: SupervisorConfig) -> Self {
        let (state_tx, _) = broadcast::channel(64);
        let (reconnect_tx, reconnect_rx) = mpsc::unbounded_channel();
        let stderr_ring = Arc::new(Mutex::new(VecDeque::with_capacity(
            config.stderr_ring_lines.max(1),
        )));
        Self {
            config,
            state_tx,
            reconnect_tx,
            reconnect_rx: AsyncMutex::new(Some(reconnect_rx)),
            stderr_ring,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SupervisorState> {
        self.state_tx.subscribe()
    }

    /// Latest stderr lines, oldest-first. Bounded by
    /// `config.stderr_ring_lines`.
    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr_ring
            .lock()
            .expect("stderr_ring poisoned")
            .iter()
            .cloned()
            .collect()
    }

    /// External signal: user clicked the Reconnect banner button, or
    /// the integration layer wants to force a fresh start. Resets the
    /// backoff and crash-loop counters on the next iteration. Returns
    /// `false` if the supervisor's run loop has already exited.
    pub fn request_reconnect(&self) -> bool {
        self.reconnect_tx.send(()).is_ok()
    }

    /// Main loop. Spawns, supervises, restarts until the crash-loop
    /// ceiling trips. After `Failed`, the loop blocks on a reconnect
    /// signal — auto-relaunch never burns CPU in a runaway crash
    /// loop. Returns only when the reconnect channel closes
    /// (i.e. all `Supervisor` references have been dropped).
    pub async fn run(&self) {
        let mut reconnect_rx = self
            .reconnect_rx
            .lock()
            .await
            .take()
            .expect("Supervisor::run called twice");
        let http = match reqwest::Client::builder()
            .timeout(self.config.heartbeat_timeout)
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                error!(error = %e, "supervisor: cannot build reqwest client");
                self.broadcast(SupervisorState::Failed {
                    reason: format!("cannot build heartbeat client: {e}"),
                    last_crash_log: None,
                });
                return;
            }
        };

        let mut crash_times: VecDeque<Instant> = VecDeque::new();
        let mut attempt: u32 = 0;

        loop {
            self.broadcast(SupervisorState::Starting);
            let mut child = match self.spawn_child().await {
                Ok(c) => c,
                Err(e) => {
                    let reason = format!("spawn failed: {e}");
                    warn!(error = %e, "supervisor: spawn failed");
                    // Spawn failure counts toward the ceiling — usually
                    // it's a missing binary path or a permissions
                    // problem and shouldn't be retried forever.
                    if !self.handle_crash(
                        &mut attempt,
                        &mut crash_times,
                        ExitOutcome::WaitError(reason.clone()),
                        None,
                        &mut reconnect_rx,
                    )
                    .await
                    {
                        return;
                    }
                    continue;
                }
            };

            let pid = child.id().unwrap_or(0);
            info!(pid, binary = %self.config.binary_path.display(), "supervisor: child spawned");

            let outcome = self.supervise_until_exit(&mut child, pid, &http, &mut reconnect_rx).await;
            let crash_log = self.persist_crash_log(pid, &outcome).await;

            match outcome {
                ExitOutcome::Shutdown => {
                    // Channel closed; nothing more for us to do.
                    info!("supervisor: shutdown — exiting run loop");
                    return;
                }
                ExitOutcome::ManualReconnect => {
                    info!("supervisor: manual reconnect; resetting backoff");
                    attempt = 0;
                    crash_times.clear();
                    // Loop back to Starting immediately.
                }
                _ => {
                    if !self.handle_crash(
                        &mut attempt,
                        &mut crash_times,
                        outcome,
                        crash_log,
                        &mut reconnect_rx,
                    )
                    .await
                    {
                        return;
                    }
                }
            }
        }
    }

    /// Common path for "something went wrong, decide whether to back
    /// off + retry or surface Failed and wait for reconnect."
    /// Returns `false` if the reconnect channel closed (shutdown).
    async fn handle_crash(
        &self,
        attempt: &mut u32,
        crash_times: &mut VecDeque<Instant>,
        outcome: ExitOutcome,
        crash_log: Option<PathBuf>,
        reconnect_rx: &mut mpsc::UnboundedReceiver<()>,
    ) -> bool {
        let now = Instant::now();
        crash_times.retain(|t| now.duration_since(*t) < self.config.crash_loop_window);
        crash_times.push_back(now);

        if crash_times.len() as u32 > self.config.crash_loop_max {
            warn!(
                recent_crashes = crash_times.len(),
                ceiling = self.config.crash_loop_max,
                window_secs = self.config.crash_loop_window.as_secs(),
                "supervisor: crash-loop ceiling exceeded; awaiting manual reconnect"
            );
            self.broadcast(SupervisorState::Failed {
                reason: format!(
                    "{} — daemon crashed {} times in {}s",
                    outcome.reason(),
                    crash_times.len(),
                    self.config.crash_loop_window.as_secs()
                ),
                last_crash_log: crash_log,
            });
            if !wait_for_reconnect(reconnect_rx).await {
                return false;
            }
            *attempt = 0;
            crash_times.clear();
            return true;
        }

        let delay = self
            .config
            .backoff_schedule
            .get(*attempt as usize)
            .copied()
            .or_else(|| self.config.backoff_schedule.last().copied())
            .unwrap_or(Duration::from_secs(30));
        self.broadcast(SupervisorState::Restarting {
            attempt: *attempt + 1,
            after_secs: delay.as_secs(),
            reason: outcome.reason(),
        });
        *attempt = attempt.saturating_add(1);
        wait_or_reconnect(delay, reconnect_rx).await
    }

    fn broadcast(&self, state: SupervisorState) {
        debug!(?state, "supervisor: state");
        // Errors only mean "no subscribers attached" — fine to drop.
        let _ = self.state_tx.send(state);
    }

    async fn spawn_child(&self) -> std::io::Result<Child> {
        let mut cmd = Command::new(&self.config.binary_path);
        cmd.args(&self.config.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(wd) = &self.config.working_dir {
            cmd.current_dir(wd);
        }
        for (k, v) in &self.config.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn()?;

        // Drain stderr into the ring buffer. Without this the child
        // can block on a full stderr pipe — and we'd lose the crash
        // context anyway.
        if let Some(stderr) = child.stderr.take() {
            let ring = Arc::clone(&self.stderr_ring);
            let cap = self.config.stderr_ring_lines.max(1);
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let mut guard = ring.lock().expect("stderr_ring poisoned");
                    while guard.len() >= cap {
                        guard.pop_front();
                    }
                    guard.push_back(line);
                }
            });
        }
        // Discard stdout. `sovereign daemon run` writes structured
        // logs to stderr; piped-but-undrained stdout would backpressure
        // the child if anything ever writes there.
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                while matches!(reader.next_line().await, Ok(Some(_))) {}
            });
        }

        Ok(child)
    }

    async fn supervise_until_exit(
        &self,
        child: &mut Child,
        pid: u32,
        http: &reqwest::Client,
        reconnect_rx: &mut mpsc::UnboundedReceiver<()>,
    ) -> ExitOutcome {
        let mut consecutive_failures: u32 = 0;
        let mut ticker = tokio::time::interval(self.config.heartbeat_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // First tick fires immediately; we want to give the child a
        // moment to bind before probing, otherwise the very first
        // probe always reads as a failure.
        ticker.tick().await;
        let mut last_health: Option<bool> = None;

        loop {
            tokio::select! {
                exit = child.wait() => {
                    return match exit {
                        Ok(status) => ExitOutcome::ChildExited { code: status.code() },
                        Err(e) => ExitOutcome::WaitError(e.to_string()),
                    };
                }
                msg = reconnect_rx.recv() => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    if msg.is_none() {
                        // All `Supervisor` references dropped → the
                        // integration layer wants us to stop. Returning
                        // `Shutdown` (vs. `ManualReconnect`) is what
                        // keeps the outer loop from spinning on a
                        // closed channel — see `run()`.
                        info!(pid, "supervisor: reconnect channel closed; shutting down");
                        return ExitOutcome::Shutdown;
                    }
                    info!(pid, "supervisor: reconnect requested; killing child");
                    return ExitOutcome::ManualReconnect;
                }
                _ = ticker.tick() => {
                    let ok = http.get(&self.config.health_url).send().await
                        .map(|r| r.status().is_success())
                        .unwrap_or(false);
                    if ok {
                        if last_health != Some(true) {
                            let since = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            self.broadcast(SupervisorState::Healthy { pid, since_unix: since });
                        }
                        consecutive_failures = 0;
                        last_health = Some(true);
                    } else {
                        consecutive_failures += 1;
                        if consecutive_failures >= self.config.heartbeat_failure_threshold {
                            warn!(
                                pid,
                                consecutive_failures,
                                "supervisor: heartbeat threshold exceeded; killing child"
                            );
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                            return ExitOutcome::HeartbeatFailed { consecutive_failures };
                        }
                        if last_health != Some(false) || consecutive_failures > 1 {
                            self.broadcast(SupervisorState::Unhealthy {
                                pid,
                                consecutive_failures,
                            });
                        }
                        last_health = Some(false);
                    }
                }
            }
        }
    }

    async fn persist_crash_log(&self, pid: u32, outcome: &ExitOutcome) -> Option<PathBuf> {
        if matches!(outcome, ExitOutcome::ManualReconnect | ExitOutcome::Shutdown) {
            return None;
        }
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Err(e) = tokio::fs::create_dir_all(&self.config.crash_log_dir).await {
            warn!(
                error = %e,
                path = %self.config.crash_log_dir.display(),
                "supervisor: cannot create crash log dir"
            );
            return None;
        }
        let path = self.config.crash_log_dir.join(format!("daemon-{ts}.log"));
        let lines = self.stderr_tail();
        let body = format!(
            "# sovereign daemon crash report\n\
             # pid: {pid}\n\
             # exit: {}\n\
             # captured: {} stderr lines\n\
             # captured_at_unix: {ts}\n\n{}\n",
            outcome.reason(),
            lines.len(),
            lines.join("\n")
        );
        match tokio::fs::write(&path, body).await {
            Ok(()) => {
                info!(path = %path.display(), "supervisor: crash log written");
                Some(path)
            }
            Err(e) => {
                warn!(
                    error = %e,
                    path = %path.display(),
                    "supervisor: crash log write failed"
                );
                None
            }
        }
    }
}

async fn wait_or_reconnect(d: Duration, rx: &mut mpsc::UnboundedReceiver<()>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(d) => true,
        msg = rx.recv() => msg.is_some(),
    }
}

async fn wait_for_reconnect(rx: &mut mpsc::UnboundedReceiver<()>) -> bool {
    rx.recv().await.is_some()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    /// Stand up a localhost axum server answering `/v1/models`. Used
    /// as the health endpoint for tests that need the supervisor to
    /// see a healthy daemon.
    async fn spawn_health_server() -> (u16, tokio::task::JoinHandle<()>) {
        use axum::{routing::get, Json, Router};
        let app = Router::new().route(
            "/v1/models",
            get(|| async { Json(serde_json::json!({"data": []})) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (port, handle)
    }

    /// Write a small shell script the supervisor can spawn as its
    /// "daemon". Returns the script path; the file lives as long as
    /// the `TempDir`.
    fn write_script(dir: &TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        let mut perm = std::fs::metadata(&path).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&path, perm).unwrap();
        path
    }

    fn base_config(binary: PathBuf, crash_dir: PathBuf, health_url: String) -> SupervisorConfig {
        SupervisorConfig {
            binary_path: binary,
            args: vec![],
            working_dir: None,
            env: vec![],
            health_url,
            crash_log_dir: crash_dir,
            heartbeat_interval: Duration::from_millis(80),
            heartbeat_timeout: Duration::from_millis(200),
            heartbeat_failure_threshold: 3,
            backoff_schedule: vec![
                Duration::from_millis(20),
                Duration::from_millis(40),
                Duration::from_millis(60),
            ],
            crash_loop_window: Duration::from_secs(60),
            crash_loop_max: 3,
            stderr_ring_lines: 64,
        }
    }

    /// Drain at most `max` states with an overall timeout, returning
    /// what we got. Discriminant-only check at call sites.
    async fn drain_states(
        rx: &mut broadcast::Receiver<SupervisorState>,
        max: usize,
        per_event_timeout: Duration,
    ) -> Vec<SupervisorState> {
        let mut out = Vec::new();
        for _ in 0..max {
            match tokio::time::timeout(per_event_timeout, rx.recv()).await {
                Ok(Ok(s)) => out.push(s),
                _ => break,
            }
        }
        out
    }

    #[tokio::test]
    async fn healthy_when_daemon_responds() {
        let dir = TempDir::new().unwrap();
        let (port, _server) = spawn_health_server().await;
        // Daemon: sleep long enough to outlast the test.
        let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nsleep 5\n");
        let config = base_config(
            script,
            dir.path().join("crashes"),
            format!("http://127.0.0.1:{port}/v1/models"),
        );

        let supervisor = Arc::new(Supervisor::new(config));
        let mut states = supervisor.subscribe();
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        let observed = drain_states(&mut states, 6, Duration::from_millis(800)).await;
        assert!(
            observed
                .iter()
                .any(|s| matches!(s, SupervisorState::Healthy { .. })),
            "expected at least one Healthy state, got {observed:?}"
        );

        run_handle.abort();
    }

    #[tokio::test]
    async fn crashing_daemon_emits_restarting_then_failed() {
        let dir = TempDir::new().unwrap();
        // No health server — heartbeats fail, but the child also
        // exits on its own well before the threshold, exercising the
        // ChildExited path.
        let script = write_script(&dir, "daemon.sh", "#!/bin/sh\necho crashing >&2\nexit 1\n");
        // Tight schedule + low ceiling so the test finishes fast.
        let mut config = base_config(
            script,
            dir.path().join("crashes"),
            "http://127.0.0.1:1/v1/models".into(), // unreachable
        );
        config.backoff_schedule = vec![Duration::from_millis(10)];
        config.crash_loop_max = 2;

        let supervisor = Arc::new(Supervisor::new(config));
        let mut states = supervisor.subscribe();
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        let observed = drain_states(&mut states, 20, Duration::from_millis(600)).await;
        let saw_restarting = observed
            .iter()
            .any(|s| matches!(s, SupervisorState::Restarting { .. }));
        let saw_failed = observed
            .iter()
            .any(|s| matches!(s, SupervisorState::Failed { .. }));
        assert!(
            saw_restarting && saw_failed,
            "expected Restarting and Failed in {observed:?}"
        );

        run_handle.abort();
    }

    #[tokio::test]
    async fn stderr_ring_captures_recent_lines() {
        let dir = TempDir::new().unwrap();
        let script = write_script(
            &dir,
            "daemon.sh",
            "#!/bin/sh\necho line-1 >&2\necho line-2 >&2\necho line-3 >&2\nexit 0\n",
        );
        let mut config = base_config(
            script,
            dir.path().join("crashes"),
            "http://127.0.0.1:1/v1/models".into(),
        );
        // Stop after one restart attempt — we don't care about the
        // ceiling here, just the stderr capture.
        config.crash_loop_max = 0;
        config.backoff_schedule = vec![Duration::from_millis(10)];

        let supervisor = Arc::new(Supervisor::new(config));
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        // Let the supervisor spawn + the child exit + the drainer
        // flush. 250ms is generous; the script itself is <10ms.
        tokio::time::sleep(Duration::from_millis(250)).await;
        let tail = supervisor.stderr_tail();
        assert!(
            tail.iter().any(|l| l == "line-1"),
            "expected line-1 in stderr tail, got {tail:?}"
        );
        assert!(
            tail.iter().any(|l| l == "line-3"),
            "expected line-3 in stderr tail, got {tail:?}"
        );

        // The crash-log file should also exist.
        let crash_dir = dir.path().join("crashes");
        let entries: Vec<_> = std::fs::read_dir(&crash_dir)
            .expect("crash dir exists")
            .flatten()
            .collect();
        assert!(
            !entries.is_empty(),
            "expected a daemon-*.log in {crash_dir:?}"
        );

        run_handle.abort();
    }

    #[tokio::test]
    async fn manual_reconnect_after_failed_restarts_supervisor() {
        let dir = TempDir::new().unwrap();
        // Crashes immediately so we hit Failed quickly.
        let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nexit 1\n");
        let mut config = base_config(
            script,
            dir.path().join("crashes"),
            "http://127.0.0.1:1/v1/models".into(),
        );
        config.backoff_schedule = vec![Duration::from_millis(10)];
        config.crash_loop_max = 1;

        let supervisor = Arc::new(Supervisor::new(config));
        let mut states = supervisor.subscribe();
        let run_handle = {
            let sup = Arc::clone(&supervisor);
            tokio::spawn(async move { sup.run().await })
        };

        // Wait until we see Failed.
        let mut got_failed = false;
        for _ in 0..30 {
            if let Ok(Ok(s)) =
                tokio::time::timeout(Duration::from_millis(150), states.recv()).await
            {
                if matches!(s, SupervisorState::Failed { .. }) {
                    got_failed = true;
                    break;
                }
            }
        }
        assert!(got_failed, "supervisor never entered Failed");

        // Fire reconnect; expect another Starting.
        assert!(supervisor.request_reconnect());
        let mut got_starting = false;
        for _ in 0..30 {
            if let Ok(Ok(s)) =
                tokio::time::timeout(Duration::from_millis(150), states.recv()).await
            {
                if matches!(s, SupervisorState::Starting) {
                    got_starting = true;
                    break;
                }
            }
        }
        assert!(got_starting, "no Starting after manual reconnect");

        run_handle.abort();
    }
}
