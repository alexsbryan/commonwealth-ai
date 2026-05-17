//! Pod-side HTTP endpoints for ephemeral workers.
//!
//! Spec: `sovereign/docs/EPHEMERAL_WORKER_PODS.md`. This module is the
//! pod's complete external surface in worker mode — no `/v1/chat/*`, no
//! mesh gossip, no admin routes. Every route in this router is
//! protected by the [`require_worker_token`] middleware, which checks
//! the bearer against the bootstrap blob the pod booted with. The
//! owner that minted the blob is the *only* caller these endpoints
//! answer.
//!
//! ## The four routes
//!
//! - `POST /internal/worker/upload?name=<file>` — streamed bytes for
//!   one file. Validated against the manifest in the bootstrap blob.
//! - `POST /internal/worker/job` — work-queue manifest. Transitions the
//!   pod from "uploading" to "running" and dispatches to the runner.
//! - `GET  /internal/worker/completed?since=<cursor>` — cursor-based,
//!   idempotent batch return. Owner re-polls until cursor == total.
//! - `DELETE /internal/worker/job` — graceful shutdown signal.
//!
//! ## Why state lives in this module (not in `daemon.rs`)
//!
//! In worker mode the pod's daemon is just a thin shell that owns one
//! [`WorkerState`] and serves these four routes. The mesh state
//! machinery (`MemberRecord`, gossip, `MeshStore`) isn't loaded at all
//! — the binary stays the same, but the wiring is different. Keeping
//! the worker state local to this module keeps the persistent-peer
//! daemon's data shapes uncluttered.
//!
//! ## What this module does NOT do
//!
//! - Actual enrichment work. The [`WorkerRunner`] trait is a plug-in
//!   point; production wires it to `sovereign-pipeline`. Tests pass a
//!   mock runner that emits completed units immediately so the
//!   cursor/poll loop can be exercised without a model load.
//! - On-disk persistence of the completed queue. The MVP holds it in
//!   memory because pods are by definition ephemeral — if a pod
//!   crashes mid-job, the owner re-creates a fresh pod and re-sends
//!   the unprocessed shard of the manifest. SQLite-backed staging is
//!   listed as a follow-up in `EPHEMERAL_WORKER_PODS.md`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};

use crate::worker_pod::{
    BootstrapBlob, Sha256Digest, WorkerPodError, derive_signing_key, pubkey_thumbprint,
    verify_worker_token,
};

// ───── Job + unit data shapes ───────────────────────────────────────

/// One unit of work in the job manifest the owner dispatches. Opaque
/// to the worker_http layer — the runner is what gives `kind` and
/// `payload` semantics. Wrapping in a typed envelope keeps the cursor
/// + auth logic generic across job types (atom enrichment, Lance
/// fragment generation, ledger emission, …).
///
/// ## `unit_id` convention
///
/// The `/internal/worker/completed` cursor uses `> since` watermark
/// semantics with 0 as the "before anything" baseline. **Callers
/// MUST assign `unit_id >= 1`** — a unit with `unit_id == 0` would
/// never satisfy the `> since` filter and would be silently dropped
/// from every poll response. The single-pod CLI and the multi-pod
/// coordinator both honour this; if you build a manifest directly
/// in tests or custom integrations, start at 1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkUnit {
    pub unit_id: u64,
    pub kind: String,
    pub payload: serde_json::Value,
}

/// The manifest the owner POSTs to `/internal/worker/job`. The runner
/// receives this and is expected to emit a [`CompletedUnit`] for each
/// input unit (or fail loudly — the owner's polling loop is what
/// notices stuck pods, not the worker itself).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobManifest {
    pub job_id: String,
    pub units: Vec<WorkUnit>,
    /// Free-form metadata the runner can consume — recipe id, batch
    /// size, model config overrides, etc. Kept untyped so adding a
    /// new field doesn't bump the protocol version.
    #[serde(default)]
    pub config: serde_json::Value,
}

/// One emission from the runner. `unit_id` echoes the input; `payload`
/// is whatever the runner produces (atom JSON, fragment metadata, …).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedUnit {
    pub unit_id: u64,
    pub payload: serde_json::Value,
    pub completed_at_unix: u64,
}

/// The runner abstraction — production wires this to sovereign-pipeline's
/// enrichment loop; tests use a synchronous in-process stub.
///
/// `dispatch` is the only required method. It must be non-blocking —
/// the implementation spawns its own background tasks and uses the
/// supplied `emit` callback to feed completed units back into the
/// pod's queue. The owner's `/completed` polls are the visibility
/// mechanism; the runner doesn't need to know about HTTP.
pub trait WorkerRunner: Send + Sync + 'static {
    fn dispatch(&self, manifest: JobManifest, emit: EmitCompletedFn);
}

/// Type alias for the runner's emission callback. Boxed so the
/// runner can stash it across spawn boundaries.
pub type EmitCompletedFn = Arc<dyn Fn(CompletedUnit) + Send + Sync>;

// ───── Worker state ─────────────────────────────────────────────────

/// The pod's complete in-memory state. Single-job-per-pod by design
/// (matches the MVP — one rented pod processes one job; multi-pod
/// fanout is at the owner-controller layer).
pub struct WorkerState {
    /// The bootstrap blob the pod booted with — the source of truth
    /// for auth (owner verifying key, pod thumbprint) and upload
    /// validation (expected SHAs).
    pub blob: BootstrapBlob,
    /// Pre-derived for hot-path auth check; recomputing on every
    /// request is fine for Ed25519 (~µs) but explicit is cheaper.
    pub pod_pubkey_thumbprint: Sha256Digest,
    /// Pre-parsed owner verifying key. Stored as `VerifyingKey` to
    /// skip re-parsing the bytes on every auth call.
    pub owner_verifying_key: VerifyingKey,
    /// Filename → SHA-256 of bytes received so far. When equal to the
    /// expected SHA in `blob.expected_uploads`, the file is "ready".
    pub uploads: RwLock<BTreeMap<String, UploadProgress>>,
    /// Append-only completed queue. The runner pushes through the
    /// `emit` callback; the `/completed` handler indexes by unit_id
    /// to honour the `since` cursor.
    pub completed: Mutex<Vec<CompletedUnit>>,
    /// Latest accepted job manifest. None until `/job` is called.
    pub job: RwLock<Option<JobManifest>>,
    /// Set when DELETE /job is received. The runner observes this to
    /// drain gracefully; the HTTP layer just records the flag.
    pub shutdown_requested: std::sync::atomic::AtomicBool,
    /// Set true by [`spawn_disk_dump_watcher`] once every manifested
    /// upload is on disk and the child-daemon config has been written.
    /// Phase 2's `SubprocessRunner` reads this (plus the companion
    /// notify below) to gate child-daemon spawn — you can't `--config
    /// <path>` a daemon at a file that doesn't exist yet.
    ///
    /// Wrapped in `Arc` so the runner can hold its own clone and
    /// observe the same signal — the alternative (giving the runner a
    /// reference to `WorkerState`) would create a cycle because the
    /// state already owns the runner.
    pub disk_dump_complete: Arc<std::sync::atomic::AtomicBool>,
    /// Companion to `disk_dump_complete` — wakes waiters when the dump
    /// transitions to "done". Use the
    /// `notified()`-before-`load()` pattern to avoid TOCTOU races
    /// between subscribers and the watcher task.
    pub disk_dump_ready: Arc<tokio::sync::Notify>,
    /// The pluggable enrichment runner. Wrapped in Arc so handlers can
    /// hand a cloned callback to it.
    pub runner: Arc<dyn WorkerRunner>,
    /// Optional pod-side inference proxy. When `Some`, the worker
    /// router mounts `/v1/chat/completions`, `/v1/models`,
    /// `/v1/embeddings`, and `/oicp/v1/capabilities`, forwarding each
    /// to the child daemon at `child_base_url`. When `None`, those
    /// routes are not mounted at all — a fresh `WorkerState` from
    /// `from_blob` is proxy-disabled, the same shape every existing
    /// test exercised before pinned-pod inference shipped.
    /// Spec: docs/PINNED_WORKER_AS_INFERENCE_PEER.md.
    pub inference_proxy: Option<Arc<crate::worker_inference_proxy::InferenceProxyConfig>>,
}

/// Per-file upload bookkeeping. We accumulate bytes in memory for the
/// MVP — pod scratch is ample for the worst case (a few-dozen GGUFs
/// totalling ~80 GB on a Vast L40S offer) but the production path
/// should stream to disk. Listed as a follow-up.
#[derive(Debug, Default)]
pub struct UploadProgress {
    pub bytes: Vec<u8>,
    pub hasher: Option<Sha256>,
    pub digest: Option<Sha256Digest>,
}

impl WorkerState {
    /// Build state from a bootstrap blob the pod just decoded. Fails
    /// if the embedded owner verifying key isn't a valid Ed25519
    /// point — wire-protocol error, not a runtime failure.
    ///
    /// The disk-dump signals are owned by the new state; callers who
    /// need to share them with a runner (e.g. `SubprocessRunner`) should
    /// use [`Self::from_blob_with_signals`] and pass pre-built `Arc`s.
    pub fn from_blob(blob: BootstrapBlob, runner: Arc<dyn WorkerRunner>) -> Result<Self, String> {
        Self::from_blob_with_signals(
            blob,
            runner,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(tokio::sync::Notify::new()),
        )
    }

    /// Same as [`Self::from_blob`] but accepts pre-built disk-dump
    /// signals so the caller can hand the same `Arc`s to a runner.
    /// `SubprocessRunner` needs this — it has to observe the dump
    /// finishing before it can `--config` the child daemon at the
    /// generated TOML.
    pub fn from_blob_with_signals(
        blob: BootstrapBlob,
        runner: Arc<dyn WorkerRunner>,
        disk_dump_complete: Arc<std::sync::atomic::AtomicBool>,
        disk_dump_ready: Arc<tokio::sync::Notify>,
    ) -> Result<Self, String> {
        let pod_thumbprint = {
            let sk = derive_signing_key(&blob.seed);
            pubkey_thumbprint(&sk.verifying_key())
        };
        let owner_vk = VerifyingKey::from_bytes(&blob.owner_verifying_key)
            .map_err(|e| format!("owner verifying key invalid: {e}"))?;
        Ok(Self {
            blob,
            pod_pubkey_thumbprint: pod_thumbprint,
            owner_verifying_key: owner_vk,
            uploads: RwLock::new(BTreeMap::new()),
            completed: Mutex::new(Vec::new()),
            job: RwLock::new(None),
            shutdown_requested: std::sync::atomic::AtomicBool::new(false),
            disk_dump_complete,
            disk_dump_ready,
            runner,
            inference_proxy: None,
        })
    }

    fn now_unix() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Spawn a background task that polls upload state and dumps
    /// every completed upload to `models_dir` once all of them are
    /// ready. Also writes a child-daemon config at `<models_dir>/../config.toml`
    /// pointing at the dumped files. Runs forever (cheaply — 500ms
    /// poll interval until ready, then logs once and exits).
    ///
    /// Phase 2's SubprocessRunner will await this dump completing
    /// before spawning the child daemon.
    pub fn spawn_disk_dump_watcher(self: &Arc<Self>, models_dir: std::path::PathBuf) {
        let state = Arc::clone(self);
        tokio::spawn(async move {
            let expected = state.blob.expected_uploads.len();
            if expected == 0 {
                return;
            }
            loop {
                let ready = {
                    let uploads = state.uploads.read().await;
                    uploads.values().filter(|p| p.digest.is_some()).count()
                };
                if ready >= expected {
                    if let Err(e) = dump_uploads_to_disk(&state, &models_dir).await {
                        tracing::error!(error = %e, "worker: disk dump failed");
                        // Leave signals unfired so the runner stays
                        // blocked rather than spawning a child against
                        // half-written models. The owner sees stuck
                        // dispatches via `/completed` and re-creates
                        // the pod.
                        return;
                    }
                    let config_path = models_dir
                        .parent()
                        .map(|p| p.join("config.toml"))
                        .unwrap_or_else(|| models_dir.join("config.toml"));
                    if let Err(e) = write_child_daemon_config(&models_dir, &config_path) {
                        tracing::warn!(error = %e, "worker: child-daemon config write failed");
                        // Same rationale — without a config, the child
                        // can't start. Don't fire signals.
                        return;
                    }
                    tracing::info!(
                        path = %config_path.display(),
                        "worker: wrote child-daemon config"
                    );
                    // Atomic-then-notify ordering: subscribers using
                    // the `notified()`-before-`load()` pattern will
                    // either see the new flag value or be woken by
                    // the notify, but never miss both.
                    state
                        .disk_dump_complete
                        .store(true, std::sync::atomic::Ordering::Release);
                    state.disk_dump_ready.notify_waiters();
                    tracing::info!("worker: disk dump complete; child-daemon spawn now unblocked");
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });
    }

    /// Spawn background tasks for every URL-backed entry in the
    /// manifest. Each task downloads the file with reqwest, validates
    /// SHA-256 against the owner-signed entry, and stages the bytes
    /// into the same `uploads` map manual uploads use. The dispatch
    /// handler's existing `uploads_ready < expected` check then waits
    /// naturally — owner code doesn't need to know whether a file came
    /// from R2 or from the laptop.
    ///
    /// Fetch failures are logged but don't unwind anything; the pod
    /// just stays in "uploads not ready" until the owner notices via
    /// `/health` and re-creates the pod (typical Vast-rental
    /// workflow). A more aggressive policy could retry with backoff
    /// — punted to a follow-up.
    pub fn spawn_url_fetches(self: &Arc<Self>) {
        let client = reqwest::Client::builder()
            // Same connect timeout we use everywhere — fail fast on
            // bad DNS / firewalled URLs so the owner sees the error
            // in `/health` quickly.
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        for (name, entry) in &self.blob.expected_uploads {
            let Some(url) = entry.fetch_url.clone() else {
                continue;
            };
            let state = Arc::clone(self);
            let name = name.clone();
            let expected_sha = entry.sha256;
            let client = client.clone();
            tokio::spawn(async move {
                tracing::info!(file = %name, url = %url, "worker: fetching upload from URL");
                match fetch_and_validate(&client, &url, &expected_sha).await {
                    Ok(bytes) => {
                        let len = bytes.len();
                        let mut uploads = state.uploads.write().await;
                        uploads.insert(
                            name.clone(),
                            UploadProgress {
                                bytes,
                                hasher: None,
                                digest: Some(expected_sha),
                            },
                        );
                        tracing::info!(
                            file = %name,
                            bytes = len,
                            "worker: URL fetch complete + SHA validated"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            file = %name,
                            url = %url,
                            error = %e,
                            "worker: URL fetch failed; pod will stay in 'uploads not ready'"
                        );
                    }
                }
            });
        }
    }
}

/// Atomic disk dump of every completed upload. Writes each entry's
/// bytes to `<dir>/<name>` via write-then-rename so a partial write
/// can never be mistaken for a complete file. The dump is idempotent
/// — calling twice is a no-op on the second call (file already exists
/// and matches the manifest SHA).
///
/// Phase 1 of the SubprocessRunner story: the bytes a child daemon
/// would `mmap` need to actually exist on disk. This is the seam
/// where they land.
pub async fn dump_uploads_to_disk(
    state: &WorkerState,
    dir: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let uploads = state.uploads.read().await;
    for (name, progress) in uploads.iter() {
        // Only dump completed uploads (those with a validated digest).
        if progress.digest.is_none() {
            continue;
        }
        let target = dir.join(name);
        if target.exists() {
            // Idempotent — skip if already on disk. SHA was validated
            // when the upload completed, so we trust the existing file.
            continue;
        }
        let tmp = target.with_extension("partial");
        std::fs::write(&tmp, &progress.bytes)?;
        std::fs::rename(&tmp, &target)?;
        tracing::info!(
            file = %name,
            path = %target.display(),
            bytes = progress.bytes.len(),
            "worker: dumped upload to disk"
        );
    }
    Ok(())
}

/// Write a minimal child-daemon config pointing at the dumped model
/// files. Phase 2's SubprocessRunner will spawn `sovereign-cli daemon
/// run` with `SOVEREIGN_CONFIG=<this-path>` so the child loads the
/// uploaded GGUFs.
///
/// Convention: file named `primary.gguf` becomes the primary slot,
/// `embed.gguf` (or anything starting with `embed`) becomes the embed
/// slot. Anything else is ignored — the child daemon doesn't load
/// arbitrary files. Callers can override the convention by writing
/// their own config externally.
pub fn write_child_daemon_config(
    models_dir: &std::path::Path,
    config_path: &std::path::Path,
) -> std::io::Result<()> {
    let mut primary: Option<std::path::PathBuf> = None;
    let mut embed: Option<std::path::PathBuf> = None;
    if let Ok(entries) = std::fs::read_dir(models_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let lower = name.to_lowercase();
            if lower.contains("embed") {
                embed = Some(path);
            } else if lower.ends_with(".gguf") && primary.is_none() {
                primary = Some(path);
            }
        }
    }
    let Some(primary) = primary else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "no primary GGUF found in {} — child daemon config needs at least one *.gguf",
                models_dir.display()
            ),
        ));
    };
    let mut toml = String::new();
    toml.push_str("# Auto-generated by sovereign-mesh::worker_http for child-daemon launch.\n");
    toml.push_str("# Do not edit by hand — regenerated each pod boot.\n\n");
    toml.push_str("[models]\n");
    toml.push_str(&format!("primary = \"{}\"\n", primary.display()));
    if let Some(e) = embed {
        toml.push_str(&format!("embed = \"{}\"\n", e.display()));
    }
    toml.push_str("\n[daemon]\n");
    // Client port stays on the canonical 9741 so any inference call
    // (whether routed through SubprocessRunner or a future
    // out-of-band debug session on the pod) hits the URL Sovereign
    // users already expect.
    toml.push_str("client_port = 9741\n");
    // Internal port is shifted off the daemon's default :9742 because
    // the worker-mode daemon already owns that port. The child needs
    // its own internal port so its admin/mesh surface (if it ever
    // tries to come up — currently it shouldn't because there's no
    // mesh-join config) doesn't fail to bind and abort the daemon.
    toml.push_str("internal_port = 9743\n");
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(config_path, toml)?;
    Ok(())
}

async fn fetch_and_validate(
    client: &reqwest::Client,
    url: &str,
    expected_sha: &Sha256Digest,
) -> std::result::Result<Vec<u8>, String> {
    // Brief retry loop — covers four real-world failure modes that
    // shouldn't put the pod permanently in "uploads not ready":
    // (1) origin-server cold start (R2 / B2 occasionally 503s on the
    //     first request to a recently-created object).
    // (2) DNS propagation lag if the URL was just minted.
    // (3) Body-stream interruption mid-transfer on long downloads
    //     (a 28 GB GGUF over a 250 Mbps link is ~15 min — long
    //     enough for an R2 edge to recycle a connection).
    // (4) Decoded-length mismatch (rare; reqwest surfaces as
    //     `error decoding response body`).
    //
    // **2026-05-16 incident**: a live SEP-on-Vast smoke wedged on a
    // 28 GB Darwin-36B fetch — `.bytes().await` errored at the
    // halfway mark and the prior `?`-propagated body-decode error
    // skipped the retry budget entirely. Two fixes here: (a) stream
    // via `bytes_stream()` so we don't buffer twice (reqwest's
    // internal buffer + our Vec), and (b) catch chunk errors inside
    // the retry loop instead of short-circuiting with `?`.
    //
    // A SHA mismatch is a hard error — no retry. The owner signed the
    // wrong SHA into the blob, or the URL is serving different bytes;
    // both are user-actionable, not transient.
    use futures::StreamExt;
    const PROGRESS_INTERVAL: u64 = 256 * 1024 * 1024;
    let attempts = 6;
    let mut delay = std::time::Duration::from_millis(250);
    let mut last_err = String::new();
    for attempt in 1..=attempts {
        let resp = match client.get(url).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                last_err = format!("status: {}", r.status());
                if attempt < attempts {
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(std::time::Duration::from_secs(5));
                }
                continue;
            }
            Err(e) => {
                last_err = format!("send: {e}");
                if attempt < attempts {
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(std::time::Duration::from_secs(5));
                }
                continue;
            }
        };

        // Pre-size if Content-Length is honest about the body length.
        // Saves Vec doublings on a 28 GB transfer — going from a 16
        // GB capacity to 32 GB on the last push would dominate
        // memory peak.
        let content_length = resp.content_length();
        let mut bytes: Vec<u8> = match content_length {
            Some(n) => Vec::with_capacity(n as usize),
            None => Vec::new(),
        };
        let mut hasher = Sha256::new();
        let mut stream = resp.bytes_stream();
        let mut received: u64 = 0;
        let mut next_progress_at: u64 = PROGRESS_INTERVAL;
        let mut chunk_err: Option<String> = None;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(c) => {
                    hasher.update(&c);
                    bytes.extend_from_slice(&c);
                    received = received.saturating_add(c.len() as u64);
                    if received >= next_progress_at {
                        let mb = received / (1024 * 1024);
                        let pct = content_length
                            .map(|n| format!("{:.1}%", (received as f64 / n as f64) * 100.0))
                            .unwrap_or_else(|| "?%".into());
                        tracing::info!(
                            mb_received = mb,
                            percent = %pct,
                            attempt,
                            "worker: URL fetch progress"
                        );
                        next_progress_at = received.saturating_add(PROGRESS_INTERVAL);
                    }
                }
                Err(e) => {
                    chunk_err = Some(format!(
                        "body: {e} (after {} bytes on attempt {attempt})",
                        received
                    ));
                    break;
                }
            }
        }
        if let Some(e) = chunk_err {
            last_err = e;
            if attempt < attempts {
                tracing::warn!(
                    error = %last_err,
                    attempt,
                    remaining = attempts - attempt,
                    "worker: URL fetch body errored mid-stream — retrying from byte 0"
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(std::time::Duration::from_secs(5));
            }
            continue;
        }

        // Stream consumed cleanly. Validate SHA against the
        // owner-signed expected digest; mismatch is non-retryable.
        let mut got = [0u8; 32];
        got.copy_from_slice(&hasher.finalize());
        if &got != expected_sha {
            return Err(format!(
                "sha mismatch: expected {} got {} (not retried — owner-signed SHA is wrong)",
                hex::encode(expected_sha),
                hex::encode(got),
            ));
        }
        return Ok(bytes);
    }
    Err(last_err)
}

// ───── Router ───────────────────────────────────────────────────────

/// Build the worker-mode router. The caller serves this on `:9742`
/// over the pod's seed-derived TLS cert. Every route is gated by the
/// worker-token middleware — no unauthenticated handler exists.
pub fn worker_router(state: Arc<WorkerState>) -> Router {
    let mut router = Router::new()
        .route("/internal/worker/upload", post(upload_handler))
        .route("/internal/worker/job", post(dispatch_handler))
        .route("/internal/worker/completed", get(completed_handler))
        .route("/internal/worker/job", delete(shutdown_handler))
        .route("/internal/worker/health", get(health_handler));
    // Pinned-pod inference proxy. Mounted only when the pod was
    // configured with an `InferenceProxyConfig` — otherwise these
    // routes don't exist and a request hits the 404 path before the
    // auth layer runs. This is the design from
    // docs/PINNED_WORKER_AS_INFERENCE_PEER.md §3: same router, same
    // auth middleware, no second permission system.
    if state.inference_proxy.is_some() {
        router =
            router.merge(crate::worker_inference_proxy::inference_proxy_routes());
    }
    router
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_worker_token,
        ))
        .with_state(state)
}

// ───── Auth middleware ──────────────────────────────────────────────

async fn require_worker_token(
    State(state): State<Arc<WorkerState>>,
    request: Request,
    next: Next,
) -> Response {
    let header = match request.headers().get(axum::http::header::AUTHORIZATION) {
        Some(h) => h,
        None => return token_reject("missing Authorization header"),
    };
    let raw = match header.to_str() {
        Ok(s) => s,
        Err(_) => return token_reject("Authorization header not utf-8"),
    };
    let token = match raw.strip_prefix("Bearer ") {
        Some(t) => t,
        None => return token_reject("Authorization scheme must be Bearer"),
    };

    match verify_worker_token(
        token,
        &state.owner_verifying_key,
        &state.pod_pubkey_thumbprint,
        Some(state.blob.job_id.as_str()),
        WorkerState::now_unix(),
    ) {
        Ok(_) => next.run(request).await,
        Err(WorkerPodError::TokenExpired { .. }) => token_reject("token expired"),
        Err(WorkerPodError::TokenWrongPod { .. }) => token_reject("token bound to a different pod"),
        Err(WorkerPodError::TokenWrongJob { .. }) => token_reject("token bound to a different job"),
        Err(WorkerPodError::TokenSignatureInvalid) => token_reject("signature invalid"),
        Err(e) => token_reject(&format!("token malformed: {e}")),
    }
}

fn token_reject(reason: &str) -> Response {
    // Keep the reason terse and don't leak which check failed in
    // detail — the audit log on the pod side has the full picture;
    // the wire response is just "no".
    tracing::warn!(reason, "worker token rejected");
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error": "unauthorised" })),
    )
        .into_response()
}

// ───── Health (so the owner's address-discovery poll can probe) ─────

#[derive(Serialize)]
struct HealthResponse {
    job_id: String,
    uploads_ready: usize,
    uploads_expected: usize,
    job_dispatched: bool,
    completed_count: usize,
    shutdown_requested: bool,
}

async fn health_handler(State(state): State<Arc<WorkerState>>) -> Json<HealthResponse> {
    let uploads = state.uploads.read().await;
    let uploads_ready = uploads
        .values()
        .filter(|p| p.digest.is_some())
        .count();
    Json(HealthResponse {
        job_id: state.blob.job_id.clone(),
        uploads_ready,
        uploads_expected: state.blob.expected_uploads.len(),
        job_dispatched: state.job.read().await.is_some(),
        completed_count: state.completed.lock().await.len(),
        shutdown_requested: state
            .shutdown_requested
            .load(std::sync::atomic::Ordering::Acquire),
    })
}

// ───── Upload ───────────────────────────────────────────────────────

#[derive(Deserialize)]
struct UploadQuery {
    name: String,
    /// Set on the final chunk so the handler finalizes the hash and
    /// compares it to the manifest. Otherwise each call is an
    /// append-only streaming chunk.
    #[serde(default)]
    finalize: bool,
}

#[derive(Serialize)]
struct UploadResponse {
    name: String,
    bytes_received: usize,
    ready: bool,
}

async fn upload_handler(
    State(state): State<Arc<WorkerState>>,
    Query(q): Query<UploadQuery>,
    _headers: HeaderMap,
    body: Bytes,
) -> Result<Json<UploadResponse>, Response> {
    let Some(entry) = state.blob.expected_uploads.get(&q.name).cloned() else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "filename not in upload manifest",
                "name": q.name,
            })),
        )
            .into_response());
    };

    // Reject manual upload attempts for URL-backed entries — those
    // are fetched by the pod itself. Without this guard, an owner
    // racing the background fetch could overwrite progress.
    if entry.fetch_url.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "file is URL-backed; pod fetches it directly",
                "name": q.name,
            })),
        )
            .into_response());
    }

    let mut uploads = state.uploads.write().await;
    let progress = uploads.entry(q.name.clone()).or_default();
    if progress.hasher.is_none() {
        progress.hasher = Some(Sha256::new());
    }
    if let Some(h) = progress.hasher.as_mut() {
        h.update(&body);
    }
    progress.bytes.extend_from_slice(&body);
    let bytes_received = progress.bytes.len();

    let mut ready = false;
    if q.finalize {
        let hasher = progress.hasher.take().unwrap_or_default();
        let digest_bytes = hasher.finalize();
        let mut got = [0u8; 32];
        got.copy_from_slice(&digest_bytes);
        if got != entry.sha256 {
            // Reset progress so the owner can retry from byte 0.
            uploads.remove(&q.name);
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "sha256 mismatch — upload rejected",
                    "name": q.name,
                    "expected": hex::encode(entry.sha256),
                    "got": hex::encode(got),
                })),
            )
                .into_response());
        }
        progress.digest = Some(got);
        ready = true;
    }

    Ok(Json(UploadResponse {
        name: q.name,
        bytes_received,
        ready,
    }))
}

// ───── Job dispatch ─────────────────────────────────────────────────

#[derive(Serialize)]
struct DispatchResponse {
    accepted_units: usize,
    job_id: String,
}

async fn dispatch_handler(
    State(state): State<Arc<WorkerState>>,
    Json(manifest): Json<JobManifest>,
) -> Result<Json<DispatchResponse>, Response> {
    if manifest.job_id != state.blob.job_id {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "manifest job_id does not match bootstrap blob",
                "manifest": manifest.job_id,
                "expected": state.blob.job_id,
            })),
        )
            .into_response());
    }
    // Verify all required uploads have arrived (otherwise the runner
    // will crash on first file access).
    let uploads = state.uploads.read().await;
    let missing: Vec<&String> = state
        .blob
        .expected_uploads
        .keys()
        .filter(|name| !uploads.get(*name).is_some_and(|p| p.digest.is_some()))
        .collect();
    if !missing.is_empty() {
        return Err((
            StatusCode::PRECONDITION_FAILED,
            Json(serde_json::json!({
                "error": "uploads incomplete",
                "missing": missing,
            })),
        )
            .into_response());
    }
    drop(uploads);

    // Reject double-dispatch — single-job-per-pod, by design.
    let mut job_slot = state.job.write().await;
    if job_slot.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "job already dispatched" })),
        )
            .into_response());
    }
    let accepted_units = manifest.units.len();
    let job_id = manifest.job_id.clone();
    *job_slot = Some(manifest.clone());
    drop(job_slot);

    // Hand the manifest off to the runner. The runner is responsible
    // for spawning whatever async machinery it needs — we don't
    // .await here so the dispatch HTTP call returns immediately.
    let state_clone = state.clone();
    let emit: EmitCompletedFn = Arc::new(move |unit: CompletedUnit| {
        // Push into the completed queue. We block briefly on the
        // mutex; the runner's caller chose to call us synchronously,
        // so this is consistent with their contract.
        let s = state_clone.clone();
        tokio::spawn(async move {
            let mut q = s.completed.lock().await;
            q.push(unit);
        });
    });
    state.runner.dispatch(manifest, emit);

    Ok(Json(DispatchResponse {
        accepted_units,
        job_id,
    }))
}

// ───── Completed-units poll ─────────────────────────────────────────

#[derive(Deserialize)]
struct CompletedQuery {
    #[serde(default)]
    since: u64,
    /// Cap the batch size so a slow owner doesn't pull the entire
    /// queue in one hit. Defaults to 256.
    #[serde(default = "default_completed_limit")]
    limit: usize,
}
fn default_completed_limit() -> usize {
    256
}

#[derive(Serialize)]
struct CompletedResponse {
    units: Vec<CompletedUnit>,
    cursor: u64,
    total_completed: usize,
}

async fn completed_handler(
    State(state): State<Arc<WorkerState>>,
    Query(q): Query<CompletedQuery>,
) -> Json<CompletedResponse> {
    let completed = state.completed.lock().await;
    let total_completed = completed.len();
    // Cursor is the unit_id watermark — return units whose unit_id > since.
    let mut units: Vec<CompletedUnit> = completed
        .iter()
        .filter(|u| u.unit_id > q.since)
        .take(q.limit)
        .cloned()
        .collect();
    units.sort_by_key(|u| u.unit_id);
    let cursor = units
        .last()
        .map(|u| u.unit_id)
        .unwrap_or(q.since);
    Json(CompletedResponse {
        units,
        cursor,
        total_completed,
    })
}

// ───── Shutdown ─────────────────────────────────────────────────────

#[derive(Serialize)]
struct ShutdownResponse {
    shutdown_requested: bool,
}

async fn shutdown_handler(State(state): State<Arc<WorkerState>>) -> Json<ShutdownResponse> {
    state
        .shutdown_requested
        .store(true, std::sync::atomic::Ordering::Release);
    Json(ShutdownResponse {
        shutdown_requested: true,
    })
}

// ───── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker_pod::{BootstrapInputs, mint_bootstrap, sign_worker_token, TokenClaims};
    use axum::body::Body;
    use axum::http::{Method, Request as HttpRequest, header::CONTENT_TYPE};
    use ed25519_dalek::SigningKey;
    use std::collections::BTreeMap;
    use tower::ServiceExt;

    fn fixed_owner_key() -> SigningKey {
        SigningKey::from_bytes(&[11u8; 32])
    }

    struct InstantRunner;
    impl WorkerRunner for InstantRunner {
        fn dispatch(&self, manifest: JobManifest, emit: EmitCompletedFn) {
            // Echo each unit back as completed, immediately, in order.
            for u in manifest.units {
                emit(CompletedUnit {
                    unit_id: u.unit_id,
                    payload: serde_json::json!({"echo": u.kind}),
                    completed_at_unix: WorkerState::now_unix(),
                });
            }
        }
    }

    fn test_setup(file_bytes: &[u8]) -> (Arc<WorkerState>, BootstrapBlob, String) {
        let mut digest = Sha256::new();
        digest.update(file_bytes);
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&digest.finalize());

        let mut manifest = BTreeMap::new();
        manifest.insert(
            "primary.gguf".to_string(),
            crate::worker_pod::UploadEntry::local(sha),
        );

        let owner = fixed_owner_key();
        let inputs = BootstrapInputs {
            job_id: "job-test".into(),
            owner_signing: &owner,
            expected_uploads: manifest,
            ttl_seconds: 3600,
            seed_override: Some([13u8; 32]),
        };
        let (blob, _thumb) = mint_bootstrap(inputs).unwrap();
        let token = blob.worker_token.clone();
        let state = Arc::new(WorkerState::from_blob(blob.clone(), Arc::new(InstantRunner)).unwrap());
        (state, blob, token)
    }

    fn auth_req(method: Method, uri: &str, token: &str, body: Body) -> HttpRequest<Body> {
        HttpRequest::builder()
            .method(method)
            .uri(uri)
            .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .unwrap()
    }

    async fn read_json(resp: Response) -> serde_json::Value {
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&body).unwrap_or(serde_json::json!(null))
    }

    #[tokio::test]
    async fn unauthenticated_request_is_rejected() {
        let (state, _blob, _token) = test_setup(b"hello");
        let app = worker_router(state);
        let req = HttpRequest::builder()
            .method(Method::GET)
            .uri("/internal/worker/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn invalid_signature_is_rejected() {
        let (state, blob, _good_token) = test_setup(b"hello");
        // Sign a token with a DIFFERENT owner key — owner_verifying_key
        // in the blob is the original; signature won't verify.
        let imposter = SigningKey::from_bytes(&[99u8; 32]);
        let claims = TokenClaims {
            job_id: blob.job_id.clone(),
            owner_pubkey_thumbprint: [0u8; 32],
            pod_pubkey_thumbprint: blob.pod_pubkey_thumbprint(),
            expires_unix: WorkerState::now_unix() + 600,
        };
        let bad_token = sign_worker_token(&imposter, &claims).unwrap();

        let app = worker_router(state);
        let req = auth_req(Method::GET, "/internal/worker/health", &bad_token, Body::empty());
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn valid_token_passes_health() {
        let (state, _blob, token) = test_setup(b"hello");
        let app = worker_router(state);
        let req = auth_req(Method::GET, "/internal/worker/health", &token, Body::empty());
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = read_json(resp).await;
        assert_eq!(v["job_id"], "job-test");
        assert_eq!(v["uploads_ready"], 0);
        assert_eq!(v["uploads_expected"], 1);
    }

    #[tokio::test]
    async fn upload_validates_sha_and_marks_ready() {
        let bytes = b"file-content-bytes";
        let (state, _blob, token) = test_setup(bytes);
        let app = worker_router(state.clone());

        let req = HttpRequest::builder()
            .method(Method::POST)
            .uri("/internal/worker/upload?name=primary.gguf&finalize=true")
            .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(bytes.to_vec()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = read_json(resp).await;
        assert_eq!(v["ready"], true);
        assert_eq!(v["bytes_received"], bytes.len());
    }

    #[tokio::test]
    async fn upload_rejects_sha_mismatch() {
        let real_bytes = b"correct";
        let wrong_bytes = b"wrong!!";
        let (state, _blob, token) = test_setup(real_bytes);
        let app = worker_router(state);

        let req = HttpRequest::builder()
            .method(Method::POST)
            .uri("/internal/worker/upload?name=primary.gguf&finalize=true")
            .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(wrong_bytes.to_vec()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn upload_unknown_file_rejected() {
        let (state, _blob, token) = test_setup(b"x");
        let app = worker_router(state);
        let req = HttpRequest::builder()
            .method(Method::POST)
            .uri("/internal/worker/upload?name=not-in-manifest.gguf&finalize=true")
            .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from("y"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn dispatch_requires_uploads_first() {
        let (state, _blob, token) = test_setup(b"x");
        let app = worker_router(state);
        let manifest = serde_json::json!({
            "job_id": "job-test",
            "units": [{ "unit_id": 1, "kind": "test", "payload": {} }],
        });
        let req = auth_req(
            Method::POST,
            "/internal/worker/job",
            &token,
            Body::from(serde_json::to_vec(&manifest).unwrap()),
        );
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PRECONDITION_FAILED);
    }

    #[tokio::test]
    async fn full_dispatch_and_poll_cycle() {
        let bytes = b"primary";
        let (state, _blob, token) = test_setup(bytes);
        let app = worker_router(state.clone());

        // 1. Upload the file.
        let upload_req = HttpRequest::builder()
            .method(Method::POST)
            .uri("/internal/worker/upload?name=primary.gguf&finalize=true")
            .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(bytes.to_vec()))
            .unwrap();
        let resp = app.clone().oneshot(upload_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 2. Dispatch a 3-unit job.
        let manifest = serde_json::json!({
            "job_id": "job-test",
            "units": [
                { "unit_id": 1, "kind": "a", "payload": {} },
                { "unit_id": 2, "kind": "b", "payload": {} },
                { "unit_id": 3, "kind": "c", "payload": {} },
            ],
        });
        let req = auth_req(
            Method::POST,
            "/internal/worker/job",
            &token,
            Body::from(serde_json::to_vec(&manifest).unwrap()),
        );
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 3. The instant-runner has fired emit() callbacks; each one
        //    spawned a tokio task to push into the completed queue.
        //    Give them a chance to land.
        for _ in 0..50 {
            if state.completed.lock().await.len() == 3 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(state.completed.lock().await.len(), 3);

        // 4. Poll from cursor=0 — expect all three.
        let req = auth_req(
            Method::GET,
            "/internal/worker/completed?since=0",
            &token,
            Body::empty(),
        );
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = read_json(resp).await;
        assert_eq!(v["units"].as_array().unwrap().len(), 3);
        assert_eq!(v["cursor"], 3);

        // 5. Re-poll from cursor=2 — only unit 3.
        let req = auth_req(
            Method::GET,
            "/internal/worker/completed?since=2",
            &token,
            Body::empty(),
        );
        let resp = app.clone().oneshot(req).await.unwrap();
        let v = read_json(resp).await;
        assert_eq!(v["units"].as_array().unwrap().len(), 1);
        assert_eq!(v["units"][0]["unit_id"], 3);

        // 6. DELETE triggers shutdown flag.
        let req = auth_req(Method::DELETE, "/internal/worker/job", &token, Body::empty());
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(state
            .shutdown_requested
            .load(std::sync::atomic::Ordering::Acquire));
    }

    #[tokio::test]
    async fn dump_uploads_to_disk_writes_validated_bytes() {
        let bytes = b"abcdefghij";
        let (state, _blob, _token) = test_setup(bytes);
        // Manually inject a completed upload (skipping the HTTP layer).
        {
            let mut h = Sha256::new();
            h.update(bytes);
            let mut sha = [0u8; 32];
            sha.copy_from_slice(&h.finalize());
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
        let tmp = tempfile::tempdir().unwrap();
        dump_uploads_to_disk(&state, tmp.path()).await.unwrap();
        let written = std::fs::read(tmp.path().join("primary.gguf")).unwrap();
        assert_eq!(written, bytes);
        // Idempotent — second call is a no-op (file already exists).
        dump_uploads_to_disk(&state, tmp.path()).await.unwrap();
    }

    #[tokio::test]
    async fn dump_skips_incomplete_uploads() {
        let (state, _blob, _token) = test_setup(b"x");
        // Insert an upload progress without a digest (i.e., still mid-stream).
        {
            let mut uploads = state.uploads.write().await;
            uploads.insert(
                "in-flight.gguf".to_string(),
                UploadProgress {
                    bytes: b"partial".to_vec(),
                    hasher: Some(Sha256::new()),
                    digest: None,
                },
            );
        }
        let tmp = tempfile::tempdir().unwrap();
        dump_uploads_to_disk(&state, tmp.path()).await.unwrap();
        assert!(
            !tmp.path().join("in-flight.gguf").exists(),
            "incomplete uploads must not be dumped"
        );
    }

    #[test]
    fn child_daemon_config_picks_primary_and_embed() {
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join("models");
        std::fs::create_dir(&models).unwrap();
        std::fs::write(models.join("FINAL-Bench_Darwin-36B-Opus-Q6_K.gguf"), b"x").unwrap();
        std::fs::write(models.join("Qwen3-Embedding-0.6B-Q8_0.gguf"), b"y").unwrap();

        let config = tmp.path().join("config.toml");
        write_child_daemon_config(&models, &config).unwrap();
        let body = std::fs::read_to_string(&config).unwrap();
        assert!(body.contains("primary = "));
        assert!(body.contains("FINAL-Bench_Darwin-36B-Opus-Q6_K.gguf"));
        assert!(body.contains("embed = "));
        assert!(body.contains("Qwen3-Embedding-0.6B-Q8_0.gguf"));
    }

    #[test]
    fn child_daemon_config_errors_with_no_gguf() {
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join("models");
        std::fs::create_dir(&models).unwrap();
        let err = write_child_daemon_config(&models, &tmp.path().join("config.toml")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn manifest_with_wrong_job_id_rejected() {
        let (state, _blob, token) = test_setup(b"x");
        let app = worker_router(state);
        let manifest = serde_json::json!({
            "job_id": "different-job",
            "units": [],
        });
        let req = auth_req(
            Method::POST,
            "/internal/worker/job",
            &token,
            Body::from(serde_json::to_vec(&manifest).unwrap()),
        );
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Regression for the 2026-05-16 SEP-on-Vast incident: a 28 GB
    /// R2 fetch errored mid-body and `fetch_and_validate`'s `?` on
    /// the body-decode error short-circuited the 6-attempt retry
    /// loop, leaving the pod wedged in "uploads not ready". After
    /// the streaming rewrite, mid-stream body errors should:
    ///   1. NOT propagate via `?`
    ///   2. Trigger a retry from byte 0
    ///   3. Succeed on a subsequent attempt when the upstream
    ///      recovers
    ///
    /// We exercise this with an axum mock server that closes the
    /// connection after N bytes on the first 2 attempts, then
    /// serves the full body on attempt 3. The fetcher must reach a
    /// good SHA inside the 6-attempt budget.
    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_and_validate_retries_after_body_stream_error() {
        use axum::body::Body as AxumBody;
        use axum::extract::State;
        use axum::http::StatusCode;
        use axum::response::Response as AxumResponse;
        use axum::routing::get;
        use axum::Router;
        use std::sync::atomic::{AtomicU32, Ordering};
        use tokio::net::TcpListener;
        use tokio::sync::oneshot;

        // Payload the "good" attempt serves; SHA matches what we
        // hand `fetch_and_validate`.
        let payload: Vec<u8> = (0..256 * 1024u32).map(|i| (i & 0xff) as u8).collect();
        let mut hasher = Sha256::new();
        hasher.update(&payload);
        let mut expected = [0u8; 32];
        expected.copy_from_slice(&hasher.finalize());

        let attempts_counter = Arc::new(AtomicU32::new(0));
        let state_payload = Arc::new(payload.clone());

        #[derive(Clone)]
        struct AppState {
            attempts: Arc<AtomicU32>,
            payload: Arc<Vec<u8>>,
        }

        async fn handler(State(state): State<AppState>) -> AxumResponse {
            let n = state.attempts.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                // Truncated body: declare a Content-Length but only
                // send half the bytes, then close. Reqwest surfaces
                // this as a body-stream error — exactly the failure
                // mode the incident showed.
                let half = state.payload.len() / 2;
                let truncated = state.payload[..half].to_vec();
                AxumResponse::builder()
                    .status(StatusCode::OK)
                    .header("Content-Length", state.payload.len().to_string())
                    .body(AxumBody::from(truncated))
                    .unwrap()
            } else {
                AxumResponse::builder()
                    .status(StatusCode::OK)
                    .header("Content-Length", state.payload.len().to_string())
                    .body(AxumBody::from(state.payload.as_ref().clone()))
                    .unwrap()
            }
        }

        let app = Router::new()
            .route("/blob", get(handler))
            .with_state(AppState {
                attempts: attempts_counter.clone(),
                payload: state_payload,
            });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .ok();
        });

        let client = reqwest::Client::builder().build().unwrap();
        let url = format!("http://{addr}/blob");
        let result = fetch_and_validate(&client, &url, &expected).await;
        let _ = tx.send(());

        let bytes = result.expect("retry loop must recover within budget");
        assert_eq!(bytes.len(), payload.len());
        assert_eq!(bytes, payload);
        let attempts = attempts_counter.load(Ordering::SeqCst);
        assert!(
            attempts >= 3,
            "expected at least 3 attempts (2 truncated + 1 good), got {attempts}"
        );
    }

    /// A SHA mismatch on a fully-received body must be a hard error
    /// — no retry. The owner-signed digest is the trust root; if it
    /// disagrees with what the URL serves, retrying buys nothing and
    /// burns time.
    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_and_validate_does_not_retry_on_sha_mismatch() {
        use axum::body::Body as AxumBody;
        use axum::http::StatusCode;
        use axum::response::Response as AxumResponse;
        use axum::routing::get;
        use axum::Router;
        use std::sync::atomic::{AtomicU32, Ordering};
        use tokio::net::TcpListener;
        use tokio::sync::oneshot;

        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_h = attempts.clone();
        let handler = move || {
            let a = attempts_h.clone();
            async move {
                a.fetch_add(1, Ordering::SeqCst);
                AxumResponse::builder()
                    .status(StatusCode::OK)
                    .body(AxumBody::from("wrong-bytes"))
                    .unwrap()
            }
        };
        let app = Router::new().route("/blob", get(handler));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .ok();
        });

        // Expected digest of a different payload — the URL serves
        // "wrong-bytes" but we tell the fetcher we expect SHA(b"hi").
        let mut h = Sha256::new();
        h.update(b"hi");
        let mut expected = [0u8; 32];
        expected.copy_from_slice(&h.finalize());

        let client = reqwest::Client::builder().build().unwrap();
        let url = format!("http://{addr}/blob");
        let result = fetch_and_validate(&client, &url, &expected).await;
        let _ = tx.send(());

        match result {
            Ok(_) => panic!("expected SHA mismatch error"),
            Err(e) => assert!(
                e.contains("sha mismatch"),
                "expected sha-mismatch error, got: {e}"
            ),
        }
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "SHA mismatch must not retry — only one attempt expected"
        );
    }
}
