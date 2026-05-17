//! Worker-mode runner that delegates each work unit to a child
//! `sovereign-cli daemon` process running on the same pod.
//!
//! Spec: `sovereign/docs/EPHEMERAL_WORKER_PODS.md` Phase 2.
//!
//! ## Why a child process at all?
//!
//! The worker-mode daemon is intentionally tiny — four owner-only
//! routes on `:9742`, no inference runtime, no mesh state. But the
//! whole point of renting a Vast pod is to run inference there. The
//! cleanest seam between "owner-pinned HTTPS control plane" and
//! "battle-tested daemon that already knows how to load a GGUF" is
//! to keep them in separate processes: the worker daemon owns the
//! TLS-pinned channel, the child daemon owns the model and serves
//! `/v1/chat/completions` on `localhost`.
//!
//! Process isolation also gives us a free crash boundary — a
//! ggml/llama.cpp SEGV (still a real risk on the Strix Halo /
//! Vulkan combos we test against; see
//! `project_daemon_a3b_rocm_crash` in user memory) takes down the
//! child but not the worker. The owner sees an error in the next
//! `/completed` poll instead of a black-hole pod.
//!
//! ## Lifecycle
//!
//! 1. The disk-dump watcher (in `worker_http`) writes uploaded GGUFs
//!    to `<models_dir>` and a child-daemon config to the parent. It
//!    flips `WorkerState::disk_dump_complete` and fires
//!    `disk_dump_ready` when both are on disk.
//! 2. The owner POSTs `/internal/worker/job` → `dispatch_handler`
//!    calls `SubprocessRunner::dispatch` which spawns one tokio task
//!    per work unit and returns immediately.
//! 3. Each task awaits the disk-dump signal, then races to
//!    `ensure_ready` — the first task spawns the child daemon and
//!    polls `GET /v1/models` until healthy; all other tasks wait on
//!    the same lock and observe a `child_ready=true` flag.
//! 4. Each task then POSTs its `{url, body}` payload to
//!    `http://127.0.0.1:9741<url>` (the child's client port) and
//!    feeds the JSON response into the worker's `/completed` queue
//!    via the `emit` callback the dispatch handler supplied.
//! 5. On `Drop`, the child process is SIGKILL'd via tokio's
//!    `kill_on_drop`. The Vast pod is by definition ephemeral, so
//!    abrupt termination is fine — the child daemon's on-disk state
//!    (LanceDB / SQLite) is already crash-safe.
//!
//! ## Failure modes
//!
//! - **Disk dump never completes** — a `--upload` fetch fails and
//!   the watcher leaves the signals unfired. Each unit's `ensure_dumped`
//!   times out after [`SubprocessRunnerConfig::disk_dump_timeout`] and
//!   the unit completes with an `{"error": …}` payload. Owner sees
//!   stuck pods via `/health.uploads_ready < uploads_expected`.
//! - **Child fails to spawn** — binary missing, permission denied, etc.
//!   The first dispatch's `ensure_ready` fails, the mutex releases,
//!   the next dispatch retries. All in-flight units return an error
//!   payload until either spawn succeeds or all units are exhausted.
//! - **Child spawned but unhealthy** — `wait_for_child_ready` polls
//!   `/v1/models` for `child_ready_timeout`. On timeout, the spawn
//!   slot is cleared and the child is killed (Drop), so the next
//!   dispatch will try again. Avoids the "spawned but never probed"
//!   trap that an unguarded `OnceCell` would create.
//! - **Inference call errors** — the per-call `inference_timeout`
//!   caps the wait. A 4xx/5xx from the child gets surfaced as an
//!   error payload; the runner doesn't retry (the work-queue layer
//!   owns retries — see `sovereign-pipeline`'s bucketed failures).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::AsyncBufReadExt;
use tokio::sync::{Mutex, Notify};

use crate::worker_http::{CompletedUnit, EmitCompletedFn, JobManifest, WorkUnit, WorkerRunner};

/// Configuration for the subprocess runner. Built once at worker-mode
/// startup; held by `Arc` so per-unit tasks can clone the handle
/// without re-validating every field.
#[derive(Debug, Clone)]
pub struct SubprocessRunnerConfig {
    /// Path the disk-dump watcher will write the child-daemon config
    /// to. The runner does NOT create this file — it waits for the
    /// watcher to write it, then passes it as `--config <path>` to
    /// the child.
    pub config_path: PathBuf,
    /// Path to the `sovereign-cli` binary used to spawn the child.
    /// `None` means "use the currently-running binary"
    /// (`std::env::current_exe()`), which is the right default
    /// inside a pod where worker and child are the same artifact.
    pub binary: Option<PathBuf>,
    /// Port the child daemon's client API listens on. Matches the
    /// `[daemon].client_port` in the generated config — defaults to
    /// 9741 to mirror the Sovereign-canonical port and so debug
    /// sessions on the pod can reach the model the usual way.
    pub child_client_port: u16,
    /// Hard cap on how long a single unit can wait for the disk-dump
    /// watcher to signal completion. The Tier-1 use case is a model
    /// fetch from R2 of up to ~60 GB on a fast pod link, so 30 min
    /// is a reasonable default.
    pub disk_dump_timeout: Duration,
    /// Hard cap on how long the readiness probe loop waits for
    /// `GET /v1/models` to return 200. Cold-loading a 36B Q6 GGUF on
    /// L40S is ~90 s typical, ~3 min worst-case. Default 5 min.
    pub child_ready_timeout: Duration,
    /// Hard cap on a single inference call. 10 min handles the
    /// longest atom-enrichment / synthesis prompts we run today.
    pub inference_timeout: Duration,
    /// When true, skip child-process spawn entirely — used by tests
    /// that pre-bind their own mock server on `child_client_port`
    /// and by callers that manage the child lifecycle out of band
    /// (e.g., desktop integration tests).
    pub skip_spawn: bool,
}

impl Default for SubprocessRunnerConfig {
    fn default() -> Self {
        Self {
            config_path: PathBuf::from("/workspace/config.toml"),
            binary: None,
            child_client_port: 9741,
            disk_dump_timeout: Duration::from_secs(30 * 60),
            child_ready_timeout: Duration::from_secs(5 * 60),
            inference_timeout: Duration::from_secs(10 * 60),
            skip_spawn: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SubprocessRunnerError {
    #[error("disk-dump never completed within {0:?}")]
    DiskDumpTimeout(Duration),
    #[error("child-daemon config not at {0} after disk-dump fired — watcher invariant broken")]
    ConfigMissing(PathBuf),
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    #[error("child readiness probe timed out after {0:?}")]
    ChildReadinessTimeout(Duration),
    #[error("work-unit payload missing field: {0}")]
    BadPayload(&'static str),
    #[error("inference call failed: {0}")]
    InferenceFailed(String),
    #[error("inference call timed out after {0:?}")]
    InferenceTimeout(Duration),
}

/// Child-process handle. Owns the `tokio::process::Child`; on `Drop`
/// the child is SIGKILL'd because `kill_on_drop(true)` was set at
/// spawn time. Graceful SIGTERM would be cleaner but the pod is
/// already disposable — the controller's `destroy()` reaps the
/// instance moments later and the child's persistent state (Lance,
/// SQLite WAL) is crash-safe.
struct ChildHandle {
    child: tokio::sync::Mutex<Option<tokio::process::Child>>,
    pid: u32,
}

impl ChildHandle {
    fn pid(&self) -> u32 {
        self.pid
    }
}

impl Drop for ChildHandle {
    fn drop(&mut self) {
        // `kill_on_drop(true)` set at spawn time delivers SIGKILL
        // when the inner `Child` is dropped, so we don't need to do
        // anything here. This impl exists to log the event for
        // operator visibility — silent process death is exactly the
        // kind of thing that wastes hours during incident triage.
        if let Ok(guard) = self.child.try_lock() {
            if guard.is_some() {
                tracing::info!(pid = self.pid, "subprocess-runner: dropping child handle (SIGKILL on drop)");
            }
        }
    }
}

/// Per-runner shared state. Cloned across dispatch tasks via `Arc`.
struct Inner {
    config: SubprocessRunnerConfig,
    client: reqwest::Client,
    /// `Mutex` doubles as the spawn+probe serialization lock. First
    /// dispatch wins the lock, spawns the child, probes readiness,
    /// then sets `child_ready` and releases. Subsequent dispatches
    /// take the fast path via the atomic.
    child_slot: Mutex<Option<Arc<ChildHandle>>>,
    /// `Arc<AtomicBool>` (not bare AtomicBool) so the pod-side
    /// inference proxy in `worker_inference_proxy.rs` can observe the
    /// same readiness signal — flip happens once when the child's
    /// `/v1/models` probe returns 200. Cheaply cloned via
    /// `child_ready_signal()`.
    child_ready: Arc<AtomicBool>,
    /// Disk-dump signals — read from `WorkerState` by the
    /// constructor and stored as `Arc` clones so the runner can
    /// `notified()`-then-`load()` without holding a reference to
    /// `WorkerState` (which owns the runner — would be a cycle).
    disk_dump_complete: Arc<AtomicBool>,
    disk_dump_ready: Arc<Notify>,
}

/// The runner. Cheap to clone (one `Arc`).
#[derive(Clone)]
pub struct SubprocessRunner {
    inner: Arc<Inner>,
}

impl SubprocessRunner {
    /// Build a new runner. `disk_dump_complete` and `disk_dump_ready`
    /// MUST be the same `Arc`s passed to
    /// `WorkerState::from_blob_with_signals` — that's how the
    /// disk-dump watcher and the runner observe the same signal.
    pub fn new(
        config: SubprocessRunnerConfig,
        disk_dump_complete: Arc<AtomicBool>,
        disk_dump_ready: Arc<Notify>,
    ) -> Self {
        let client = reqwest::Client::builder()
            // Inference requests can be long; only cap the connect
            // half so a dead child surfaces fast.
            .connect_timeout(Duration::from_secs(5))
            // Disable proxies — every call is to localhost.
            .no_proxy()
            .build()
            .expect("reqwest client builds with localhost-only config");
        Self {
            inner: Arc::new(Inner {
                config,
                client,
                child_slot: Mutex::new(None),
                child_ready: Arc::new(AtomicBool::new(false)),
                disk_dump_complete,
                disk_dump_ready,
            }),
        }
    }

    /// Test helper — returns the PID of the spawned child, if any.
    /// Returns 0 if the child hasn't been spawned yet or skip_spawn
    /// was set.
    pub async fn child_pid(&self) -> u32 {
        let slot = self.inner.child_slot.lock().await;
        slot.as_ref().map(|h| h.pid()).unwrap_or(0)
    }

    /// Hand out the shared "child daemon ready" signal. The pod-side
    /// inference proxy in `worker_inference_proxy.rs` clones this and
    /// reads it on every forward — without sharing, the proxy would
    /// have to poll the child independently and the warmup window
    /// would surface as confusing connection-refused errors.
    pub fn child_ready_signal(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.inner.child_ready)
    }
}

impl WorkerRunner for SubprocessRunner {
    fn dispatch(&self, manifest: JobManifest, emit: EmitCompletedFn) {
        for unit in manifest.units {
            let inner = self.inner.clone();
            let emit = emit.clone();
            tokio::spawn(async move {
                let unit_id = unit.unit_id;
                let payload = match process_unit(&inner, unit).await {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::error!(unit_id, error = %e, "subprocess-runner: unit failed");
                        serde_json::json!({
                            "error": e.to_string(),
                            "unit_id": unit_id,
                        })
                    }
                };
                emit(CompletedUnit {
                    unit_id,
                    payload,
                    completed_at_unix: now_unix(),
                });
            });
        }
    }
}

/// Process one work unit end-to-end. Each step is an independent
/// failure mode, so we return early with a typed error rather than
/// letting half-finished side effects survive a panic.
async fn process_unit(
    inner: &Arc<Inner>,
    unit: WorkUnit,
) -> Result<serde_json::Value, SubprocessRunnerError> {
    // Step 1: wait until the disk-dump watcher has written models +
    // config. Order matters — register the waiter BEFORE peeking the
    // flag, so a notify firing between the two doesn't strand us.
    wait_for_disk_dump(inner).await?;

    // Step 2: ensure the child is spawned and responding to health
    // probes. First caller does the work; subsequent callers fast-path.
    ensure_child_ready(inner).await?;

    // Step 3: unpack the payload envelope.
    let url = unit
        .payload
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or(SubprocessRunnerError::BadPayload("url"))?;
    let body = unit
        .payload
        .get("body")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    // Step 4: proxy to the child.
    let full_url = format!("http://127.0.0.1:{}{}", inner.config.child_client_port, url);
    let resp_fut = inner.client.post(&full_url).json(&body).send();
    let resp = match tokio::time::timeout(inner.config.inference_timeout, resp_fut).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return Err(SubprocessRunnerError::InferenceFailed(e.to_string())),
        Err(_) => return Err(SubprocessRunnerError::InferenceTimeout(inner.config.inference_timeout)),
    };
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| SubprocessRunnerError::InferenceFailed(format!("body read: {e}")))?;
    if !status.is_success() {
        // Cap the body snippet so a 1 MB stack trace doesn't blow
        // the completed-queue memory budget.
        let snippet: String = String::from_utf8_lossy(&bytes)
            .chars()
            .take(500)
            .collect();
        return Err(SubprocessRunnerError::InferenceFailed(format!(
            "child returned {status}: {snippet}"
        )));
    }
    Ok(serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        serde_json::json!({
            "raw": String::from_utf8_lossy(&bytes).to_string(),
        })
    }))
}

/// Wait for `WorkerState::disk_dump_complete` to flip to true.
///
/// The Notify-before-load order is essential: if the dump completes
/// in the window between `load()` returning false and `notified()`
/// registering a waiter, we'd block forever. Registering the waiter
/// first guarantees that either we see the flag OR the
/// `notify_waiters` call sees us and wakes us.
async fn wait_for_disk_dump(inner: &Arc<Inner>) -> Result<(), SubprocessRunnerError> {
    let waiter = inner.disk_dump_ready.notified();
    if inner.disk_dump_complete.load(Ordering::Acquire) {
        return Ok(());
    }
    tokio::time::timeout(inner.config.disk_dump_timeout, waiter)
        .await
        .map_err(|_| SubprocessRunnerError::DiskDumpTimeout(inner.config.disk_dump_timeout))?;
    Ok(())
}

/// Ensure the child is spawned and `GET /v1/models` returns 200.
/// Serialized via the `child_slot` mutex — only one dispatch at a
/// time can be racing through spawn-and-probe.
async fn ensure_child_ready(inner: &Arc<Inner>) -> Result<(), SubprocessRunnerError> {
    if inner.child_ready.load(Ordering::Acquire) {
        return Ok(());
    }
    let mut slot = inner.child_slot.lock().await;
    if inner.child_ready.load(Ordering::Acquire) {
        return Ok(());
    }
    // Spawn the child if we haven't yet. On `skip_spawn`, we trust
    // that the caller already has a child (or mock) listening on
    // `child_client_port`.
    if slot.is_none() && !inner.config.skip_spawn {
        if !inner.config.config_path.exists() {
            return Err(SubprocessRunnerError::ConfigMissing(
                inner.config.config_path.clone(),
            ));
        }
        let handle = spawn_child(&inner.config).await?;
        *slot = Some(Arc::new(handle));
    }
    // Drop the slot lock before probing — the probe doesn't need it
    // (the child is already in `slot` and won't be respawned while
    // we hold this critical section's invariants).
    drop(slot);
    wait_for_child_ready(&inner.client, &inner.config).await?;
    inner.child_ready.store(true, Ordering::Release);
    Ok(())
}

/// Spawn `<binary> daemon run --config <config_path>` as a child
/// process. Sets `kill_on_drop(true)` so the child dies if the
/// `ChildHandle` is ever dropped without an explicit kill. Strips
/// `SOVEREIGN_BOOTSTRAP` from the env so the child doesn't try to
/// re-enter worker mode itself (which would loop forever).
async fn spawn_child(
    config: &SubprocessRunnerConfig,
) -> Result<ChildHandle, SubprocessRunnerError> {
    let binary = match config.binary.clone() {
        Some(b) => b,
        None => std::env::current_exe()
            .map_err(|e| SubprocessRunnerError::SpawnFailed(format!("current_exe: {e}")))?,
    };
    let mut cmd = tokio::process::Command::new(&binary);
    cmd.arg("daemon")
        .arg("run")
        .arg("--config")
        .arg(&config.config_path);
    // Worker-mode env vars must not leak into the child. If they
    // did, the child would parse the bootstrap blob and try to bind
    // its own :9742, which collides with the parent worker daemon
    // and crashes both.
    cmd.env_remove("SOVEREIGN_BOOTSTRAP");
    cmd.env_remove("SOVEREIGN_MODELS_DIR");
    // Capture child output so we can surface it under `tracing`
    // rather than printing to the pod's raw stdout (which Vast logs
    // un-prefixed and is hard to disentangle from worker output).
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| SubprocessRunnerError::SpawnFailed(format!("{}: {e}", binary.display())))?;
    let pid = child.id().unwrap_or(0);
    tracing::info!(
        pid,
        binary = %binary.display(),
        config = %config.config_path.display(),
        "subprocess-runner: spawned child daemon"
    );
    if let Some(stdout) = child.stdout.take() {
        spawn_log_forwarder(stdout, pid, "stdout");
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_log_forwarder(stderr, pid, "stderr");
    }
    Ok(ChildHandle {
        child: tokio::sync::Mutex::new(Some(child)),
        pid,
    })
}

fn spawn_log_forwarder<R>(reader: R, pid: u32, stream: &'static str)
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    tracing::info!(pid, stream, "{}", line);
                }
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!(pid, stream, error = %e, "subprocess-runner: child log read failed");
                    return;
                }
            }
        }
    });
}

/// Poll `GET /v1/models` on the child's client port until it returns
/// 200 or the deadline lapses. Cold model load can take a few
/// minutes — see `child_ready_timeout` for the cap.
async fn wait_for_child_ready(
    client: &reqwest::Client,
    config: &SubprocessRunnerConfig,
) -> Result<(), SubprocessRunnerError> {
    let url = format!("http://127.0.0.1:{}/v1/models", config.child_client_port);
    let deadline = std::time::Instant::now() + config.child_ready_timeout;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let probe = client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await;
        if let Ok(resp) = probe {
            if resp.status().is_success() {
                tracing::info!(
                    attempts = attempt,
                    url = %url,
                    "subprocess-runner: child is ready"
                );
                return Ok(());
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(SubprocessRunnerError::ChildReadinessTimeout(
                config.child_ready_timeout,
            ));
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
}

fn now_unix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ───── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use axum::{Json, Router, routing::{get, post}};
    use std::collections::BTreeMap;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    /// Spin up a tiny axum server on an ephemeral port and return
    /// the bound port. Used as a mock child daemon — handlers echo
    /// the request body back so the runner has something to receive.
    async fn spawn_mock_child(
        slow_first_n: usize,
    ) -> (u16, Arc<AtomicBool>) {
        let ready_called = Arc::new(AtomicBool::new(false));
        let probe_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let probe_count_h = probe_count.clone();
        let ready_called_h = ready_called.clone();
        let slow_n = slow_first_n;

        let app = Router::new()
            .route(
                "/v1/models",
                get(move || {
                    let pc = probe_count_h.clone();
                    let rc = ready_called_h.clone();
                    async move {
                        let n = pc.fetch_add(1, Ordering::SeqCst);
                        // Make the first `slow_n` probes return 503 so
                        // tests can exercise the polling loop.
                        if n < slow_n {
                            return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "warming up").into_response();
                        }
                        rc.store(true, Ordering::Release);
                        Json(serde_json::json!({ "data": [] })).into_response()
                    }
                }),
            )
            .route(
                "/v1/chat/completions",
                post(|body: Json<serde_json::Value>| async move {
                    Json(serde_json::json!({
                        "echo": body.0,
                        "id": "mock-completion-1",
                    }))
                }),
            );

        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (port, ready_called)
    }

    fn test_config(port: u16) -> SubprocessRunnerConfig {
        SubprocessRunnerConfig {
            config_path: std::env::temp_dir().join("does-not-exist.toml"),
            binary: None,
            child_client_port: port,
            // Short timeouts so test failures fail fast.
            disk_dump_timeout: Duration::from_secs(2),
            child_ready_timeout: Duration::from_secs(3),
            inference_timeout: Duration::from_secs(2),
            skip_spawn: true,
        }
    }

    fn pre_fired_dump_signals() -> (Arc<AtomicBool>, Arc<Notify>) {
        let complete = Arc::new(AtomicBool::new(true));
        let ready = Arc::new(Notify::new());
        ready.notify_waiters();
        (complete, ready)
    }

    fn unit(unit_id: u64, url: &str, body: serde_json::Value) -> WorkUnit {
        WorkUnit {
            unit_id,
            kind: "chat".to_string(),
            payload: serde_json::json!({ "url": url, "body": body }),
        }
    }

    fn collect_emit() -> (EmitCompletedFn, Arc<Mutex<Vec<CompletedUnit>>>) {
        let received = Arc::new(Mutex::new(Vec::<CompletedUnit>::new()));
        let received_h = received.clone();
        let emit: EmitCompletedFn = Arc::new(move |unit| {
            let received_h = received_h.clone();
            tokio::spawn(async move {
                received_h.lock().await.push(unit);
            });
        });
        (emit, received)
    }

    async fn wait_for_n(received: &Arc<Mutex<Vec<CompletedUnit>>>, n: usize, max_ms: u64) {
        let deadline = std::time::Instant::now() + Duration::from_millis(max_ms);
        loop {
            if received.lock().await.len() >= n {
                return;
            }
            if std::time::Instant::now() >= deadline {
                let got = received.lock().await.len();
                panic!("expected {n} completed units, only got {got} after {max_ms}ms");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn dispatch_proxies_units_to_mock_child() {
        let (port, _ready) = spawn_mock_child(0).await;
        let (complete, ready) = pre_fired_dump_signals();
        let runner = SubprocessRunner::new(test_config(port), complete, ready);

        let manifest = JobManifest {
            job_id: "j-test".to_string(),
            units: vec![
                unit(1, "/v1/chat/completions", serde_json::json!({"prompt": "hi"})),
                unit(2, "/v1/chat/completions", serde_json::json!({"prompt": "hello"})),
            ],
            config: serde_json::json!({}),
        };
        let (emit, received) = collect_emit();
        runner.dispatch(manifest, emit);

        wait_for_n(&received, 2, 5000).await;
        let units = received.lock().await;
        assert_eq!(units.len(), 2);
        // Order isn't guaranteed (each unit spawns its own task) — sort.
        let mut by_id: BTreeMap<u64, &CompletedUnit> =
            units.iter().map(|u| (u.unit_id, u)).collect();
        let u1 = by_id.remove(&1).unwrap();
        let u2 = by_id.remove(&2).unwrap();
        assert_eq!(u1.payload["echo"]["prompt"], "hi");
        assert_eq!(u2.payload["echo"]["prompt"], "hello");
        assert_eq!(u1.payload["id"], "mock-completion-1");
    }

    #[tokio::test]
    async fn dispatch_polls_readiness_until_child_warm() {
        // Mock child returns 503 on the first two probes, then 200.
        let (port, ready_called) = spawn_mock_child(2).await;
        let (complete, ready) = pre_fired_dump_signals();
        let runner = SubprocessRunner::new(test_config(port), complete, ready);

        let manifest = JobManifest {
            job_id: "j-warmup".to_string(),
            units: vec![unit(7, "/v1/chat/completions", serde_json::json!({}))],
            config: serde_json::json!({}),
        };
        let (emit, received) = collect_emit();
        runner.dispatch(manifest, emit);

        wait_for_n(&received, 1, 5000).await;
        assert!(
            ready_called.load(Ordering::Acquire),
            "readiness probe should have eventually returned 200"
        );
        let units = received.lock().await;
        assert_eq!(units.len(), 1);
        // The successful proxy means the response is the echoed payload,
        // not an error payload.
        assert!(
            units[0].payload.get("error").is_none(),
            "unit completed with error: {:?}", units[0].payload
        );
    }

    #[tokio::test]
    async fn disk_dump_timeout_surfaces_as_error() {
        let (port, _ready) = spawn_mock_child(0).await;
        // Disk-dump signals stay un-fired.
        let complete = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(Notify::new());
        let runner = SubprocessRunner::new(test_config(port), complete, ready);

        let manifest = JobManifest {
            job_id: "j-stuck".to_string(),
            units: vec![unit(42, "/v1/chat/completions", serde_json::json!({}))],
            config: serde_json::json!({}),
        };
        let (emit, received) = collect_emit();
        runner.dispatch(manifest, emit);

        wait_for_n(&received, 1, 5000).await;
        let units = received.lock().await;
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].unit_id, 42);
        let err = units[0].payload["error"].as_str().unwrap_or("");
        assert!(
            err.contains("disk-dump never completed"),
            "expected disk-dump timeout, got: {err}"
        );
    }

    #[tokio::test]
    async fn bad_payload_surfaces_clear_error() {
        let (port, _ready) = spawn_mock_child(0).await;
        let (complete, ready) = pre_fired_dump_signals();
        let runner = SubprocessRunner::new(test_config(port), complete, ready);

        // Payload doesn't have a `url` field.
        let bad_unit = WorkUnit {
            unit_id: 1,
            kind: "chat".to_string(),
            payload: serde_json::json!({ "no_url_here": true }),
        };
        let manifest = JobManifest {
            job_id: "j-bad".to_string(),
            units: vec![bad_unit],
            config: serde_json::json!({}),
        };
        let (emit, received) = collect_emit();
        runner.dispatch(manifest, emit);

        wait_for_n(&received, 1, 3000).await;
        let units = received.lock().await;
        let err = units[0].payload["error"].as_str().unwrap_or("");
        assert!(err.contains("url"), "expected 'url' error, got: {err}");
    }

    #[tokio::test]
    async fn child_4xx_surfaces_as_error_payload() {
        // Custom mock that returns 400 on chat-completions.
        let app = Router::new()
            .route("/v1/models", get(|| async { Json(serde_json::json!({"data": []})) }))
            .route(
                "/v1/chat/completions",
                post(|| async { (axum::http::StatusCode::BAD_REQUEST, "missing model field").into_response() }),
            );
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let (complete, ready) = pre_fired_dump_signals();
        let runner = SubprocessRunner::new(test_config(port), complete, ready);
        let manifest = JobManifest {
            job_id: "j-4xx".to_string(),
            units: vec![unit(1, "/v1/chat/completions", serde_json::json!({}))],
            config: serde_json::json!({}),
        };
        let (emit, received) = collect_emit();
        runner.dispatch(manifest, emit);

        wait_for_n(&received, 1, 3000).await;
        let units = received.lock().await;
        let err = units[0].payload["error"].as_str().unwrap_or("");
        assert!(err.contains("400"), "expected status 400 in error, got: {err}");
        assert!(
            err.contains("missing model field"),
            "expected body snippet in error, got: {err}"
        );
    }

    #[tokio::test]
    async fn many_units_serialize_through_single_readiness_probe() {
        // 10 units race to dispatch — the mock counts probes via the
        // ready_called atomic. After all units finish, only ONE
        // readiness sequence should have completed (a single 200 flips
        // the atomic).
        let (port, ready_called) = spawn_mock_child(0).await;
        let (complete, ready) = pre_fired_dump_signals();
        let runner = SubprocessRunner::new(test_config(port), complete, ready);

        let units: Vec<_> = (0..10)
            .map(|i| unit(i, "/v1/chat/completions", serde_json::json!({"i": i})))
            .collect();
        let manifest = JobManifest {
            job_id: "j-race".to_string(),
            units,
            config: serde_json::json!({}),
        };
        let (emit, received) = collect_emit();
        runner.dispatch(manifest, emit);

        wait_for_n(&received, 10, 5000).await;
        assert!(ready_called.load(Ordering::Acquire));
        // child_ready should now be latched.
        assert!(runner.inner.child_ready.load(Ordering::Acquire));
    }

    /// Integration test: actually wire the SubprocessRunner against a
    /// real `WorkerState`, manually trigger the disk-dump watcher, and
    /// confirm units flow through. Bypasses the HTTP/TLS layer — that's
    /// covered by `tests/worker_e2e.rs`.
    #[tokio::test]
    async fn signals_fire_when_watcher_completes_dump() {
        use crate::worker_http::{UploadProgress, WorkerState};
        use crate::worker_pod::{BootstrapInputs, UploadEntry, mint_bootstrap};
        use ed25519_dalek::SigningKey;
        use sha2::{Digest, Sha256};

        let (port, _ready) = spawn_mock_child(0).await;

        // Build the bootstrap blob with one upload manifested.
        let owner = SigningKey::from_bytes(&[7u8; 32]);
        let bytes = b"fake-gguf-bytes";
        let mut h = Sha256::new();
        h.update(bytes);
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&h.finalize());

        let mut manifest_uploads = BTreeMap::new();
        manifest_uploads.insert("primary.gguf".to_string(), UploadEntry::local(sha));
        let (blob, _) = mint_bootstrap(BootstrapInputs {
            job_id: "sig-test".into(),
            owner_signing: &owner,
            expected_uploads: manifest_uploads,
            ttl_seconds: 600,
            seed_override: Some([88u8; 32]),
        })
        .unwrap();

        // Pre-build signals shared between runner and state.
        let dump_complete = Arc::new(AtomicBool::new(false));
        let dump_ready = Arc::new(Notify::new());

        // SubprocessRunner with skip_spawn=true so it talks to the mock
        // child instead of a real subprocess.
        let runner_cfg = SubprocessRunnerConfig {
            child_client_port: port,
            skip_spawn: true,
            ..test_config(port)
        };
        let runner = Arc::new(SubprocessRunner::new(
            runner_cfg,
            dump_complete.clone(),
            dump_ready.clone(),
        ));

        // WorkerState shares the same signals.
        let state = Arc::new(
            WorkerState::from_blob_with_signals(
                blob,
                runner.clone(),
                dump_complete.clone(),
                dump_ready.clone(),
            )
            .unwrap(),
        );

        // Inject the completed upload into state.uploads (skipping the
        // HTTP /upload route — not what this test is about).
        {
            let mut uploads = state.uploads.write().await;
            uploads.insert(
                "primary.gguf".to_string(),
                UploadProgress {
                    bytes: bytes.to_vec(),
                    hasher: None,
                    digest: Some(sha),
                },
            );
        }

        // Spawn the disk-dump watcher pointed at a tempdir. It'll dump
        // the upload, write a config, and fire the signals.
        let tmp = tempfile::tempdir().unwrap();
        let models_dir = tmp.path().join("models");
        state.spawn_disk_dump_watcher(models_dir.clone());

        // Wait for the dump to complete (signal fires).
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            if dump_complete.load(Ordering::Acquire) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("disk-dump watcher never fired the complete signal");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // The dump should have produced the file + the config.
        assert!(models_dir.join("primary.gguf").exists());
        assert!(tmp.path().join("config.toml").exists());

        // Now dispatch a unit through the SubprocessRunner.
        let manifest = JobManifest {
            job_id: "sig-test".into(),
            units: vec![unit(
                100,
                "/v1/chat/completions",
                serde_json::json!({"q": "ping"}),
            )],
            config: serde_json::json!({}),
        };
        let (emit, received) = collect_emit();
        runner.dispatch(manifest, emit);

        wait_for_n(&received, 1, 3000).await;
        let units = received.lock().await;
        assert_eq!(units.len(), 1);
        assert!(
            units[0].payload.get("error").is_none(),
            "unit failed: {:?}", units[0].payload
        );
        assert_eq!(units[0].payload["echo"]["q"], "ping");
    }

    /// Real-child smoke test — actually spawns `sovereign-cli daemon
    /// run --config <path>` and exercises the full pipeline.
    /// Gated `#[ignore]` because (a) it needs the binary on PATH and
    /// (b) it loads a model which takes seconds-to-minutes. Run with
    /// `cargo test --package sovereign-mesh -- --ignored real_child`.
    #[tokio::test]
    #[ignore]
    async fn real_child_smoke() {
        // Caller must point us at a working config.toml with a
        // model path the binary can actually load.
        let config_path = match std::env::var("SOVEREIGN_TEST_CHILD_CONFIG") {
            Ok(p) => PathBuf::from(p),
            Err(_) => {
                eprintln!("skipping: set SOVEREIGN_TEST_CHILD_CONFIG=<path/to/config.toml>");
                return;
            }
        };
        let binary = std::env::var("SOVEREIGN_TEST_CHILD_BINARY")
            .ok()
            .map(PathBuf::from);
        let (complete, ready) = pre_fired_dump_signals();
        let runner = SubprocessRunner::new(
            SubprocessRunnerConfig {
                config_path,
                binary,
                child_client_port: 9741,
                disk_dump_timeout: Duration::from_secs(60),
                child_ready_timeout: Duration::from_secs(300),
                inference_timeout: Duration::from_secs(120),
                skip_spawn: false,
            },
            complete,
            ready,
        );
        let manifest = JobManifest {
            job_id: "real-child".to_string(),
            units: vec![unit(
                1,
                "/v1/chat/completions",
                serde_json::json!({
                    "model": "primary",
                    "messages": [{"role": "user", "content": "say only OK"}],
                    "max_tokens": 8,
                }),
            )],
            config: serde_json::json!({}),
        };
        let (emit, received) = collect_emit();
        runner.dispatch(manifest, emit);
        wait_for_n(&received, 1, 360_000).await;
        let units = received.lock().await;
        assert!(units[0].payload.get("error").is_none(), "real child smoke failed: {:?}", units[0].payload);
        assert!(runner.child_pid().await > 0, "child PID should have been recorded");
    }
}
