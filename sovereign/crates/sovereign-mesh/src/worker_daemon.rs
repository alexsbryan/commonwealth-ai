// SPDX-License-Identifier: AGPL-3.0-or-later
//! Worker-mode daemon entry point.
//!
//! Spec: `sovereign/docs/EPHEMERAL_WORKER_PODS.md`. Called from
//! `sovereign-cli daemon` when `--worker-mode` is passed. Replaces the
//! whole persistent-peer wiring (gossip, mesh join, /v1/chat exposure,
//! load-balancer participation) with a single HTTPS listener on
//! `:9742` that serves the four owner-only worker routes.
//!
//! ## What this module does
//!
//! - Decodes the bootstrap blob (env var `SOVEREIGN_BOOTSTRAP` or file).
//! - Derives the Ed25519 TLS keypair + self-signed cert from the
//!   blob's seed (so the cert matches the thumbprint the owner pinned
//!   *before* the pod booted — no TOFU window).
//! - Builds [`WorkerState`] from the blob and the supplied runner.
//! - Binds `0.0.0.0:9742` with rustls termination and serves the
//!   [`worker_router`] returned by `worker_http.rs`.
//!
//! ## What this module does NOT do
//!
//! - Load any inference runtime, mesh state, gossip layer, or admin
//!   surface. Worker mode is intentionally a different shape — pods
//!   that boot here can't be load-balancer candidates or join the
//!   mesh, because the wiring for those simply isn't here.
//! - Generate the runner. The caller (the CLI's daemon command) picks
//!   the runner — typically a stub during the MVP, swapped for a real
//!   sovereign-pipeline-backed runner once that integration lands.

use std::net::SocketAddr;
use std::sync::Arc;

use crate::worker_http::{
    worker_router, CompletedUnit, EmitCompletedFn, JobManifest, WorkerRunner, WorkerState,
};
use crate::worker_pod::{self_signed_cert, BootstrapBlob, WORKER_PORT};

/// Disk-dump coordination signals — `(complete_flag, notify_handle)`.
/// Shared between [`WorkerState`] (writer; flips on dump completion)
/// and runners that need to gate on the dump (e.g.
/// `SubprocessRunner`, which can't `--config` the child daemon at a
/// file that doesn't exist yet).
pub type DiskDumpSignals = (
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::sync::Arc<tokio::sync::Notify>,
);

/// Build a fresh pair of disk-dump signals. Both are owned by `Arc`
/// so the caller can clone the handles into a runner before passing
/// the originals through to [`run_worker_mode`].
pub fn new_disk_dump_signals() -> DiskDumpSignals {
    (
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        std::sync::Arc::new(tokio::sync::Notify::new()),
    )
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerDaemonError {
    #[error("worker-pod: {0}")]
    WorkerPod(#[from] crate::worker_pod::WorkerPodError),
    #[error("state: {0}")]
    State(String),
    #[error("tls: {0}")]
    Tls(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("blob: {0}")]
    Blob(String),
}

pub type Result<T> = std::result::Result<T, WorkerDaemonError>;

/// Stub runner that immediately ACKs every input unit. Useful for
/// validating the wire protocol end-to-end against a real Vast pod
/// before the real `sovereign-pipeline` runner is wired in.
///
/// Production wiring will replace this with a runner that shells to
/// the existing pipeline enrichment loop; the swap is a single line
/// in the CLI's worker-mode dispatcher.
pub struct EchoRunner;

impl WorkerRunner for EchoRunner {
    fn dispatch(&self, manifest: JobManifest, emit: EmitCompletedFn) {
        for u in manifest.units {
            emit(CompletedUnit {
                unit_id: u.unit_id,
                payload: serde_json::json!({"echo": u.kind, "input": u.payload}),
                completed_at_unix: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            });
        }
    }
}

/// Resolve the bootstrap blob from env or file. Returns the decoded
/// blob plus a short description of where it came from (for logs).
pub fn load_bootstrap_blob(
    env_var: &str,
    file_path: Option<&std::path::Path>,
) -> Result<(BootstrapBlob, String)> {
    if let Some(path) = file_path {
        let raw = std::fs::read_to_string(path)?;
        let blob = crate::worker_pod::decode_bootstrap(&raw)?;
        return Ok((blob, format!("file://{}", path.display())));
    }
    let raw = std::env::var(env_var).map_err(|_| {
        WorkerDaemonError::Blob(format!(
            "neither --bootstrap-blob nor ${env_var} is set; \
             worker mode requires one of these (see EPHEMERAL_WORKER_PODS.md)"
        ))
    })?;
    let blob = crate::worker_pod::decode_bootstrap(&raw)?;
    Ok((blob, format!("env:{env_var}")))
}

/// Run the worker daemon forever (or until the caller cancels the
/// task). Blocks the calling task on the axum-server future.
///
/// `models_dir`, when set, triggers the disk-dump watcher: as soon as
/// every upload in the manifest is complete, bytes are atomically
/// written to `<models_dir>/<name>` and a child-daemon config is
/// generated at `<models_dir>/../config.toml`. SubprocessRunner
/// (Phase 2) will spawn `sovereign-cli daemon run` against that
/// config to do actual inference work.
pub async fn run_worker_mode(
    blob: BootstrapBlob,
    runner: Arc<dyn WorkerRunner>,
    bind_addr: Option<SocketAddr>,
    models_dir: Option<std::path::PathBuf>,
) -> Result<()> {
    run_worker_mode_with_signals(blob, runner, bind_addr, models_dir, None, None).await
}

/// Same as [`run_worker_mode`] but allows the caller to supply
/// pre-built disk-dump signals so a [`SubprocessRunner`] (or any
/// runner that wants to gate on the dump) can observe the same flag
/// the [`WorkerState`] writes. Also accepts an optional inference
/// proxy config — when `Some`, the pod's `:9742` router mounts the
/// `/v1/chat/completions` etc. forwarding routes so the owner-side
/// mesh scheduler can route inference here.
///
/// When `signals` is `None`, fresh `Arc`s are minted — equivalent
/// to the [`run_worker_mode`] behaviour. Most production callers
/// will pass `Some(signals)` so the runner is woken correctly.
/// When `proxy` is `None`, the inference routes are not mounted
/// (the `/internal/worker/*` surface still serves; pure-dispatch
/// pods keep working).
///
/// [`SubprocessRunner`]: crate::worker_subprocess_runner::SubprocessRunner
pub async fn run_worker_mode_with_signals(
    blob: BootstrapBlob,
    runner: Arc<dyn WorkerRunner>,
    bind_addr: Option<SocketAddr>,
    models_dir: Option<std::path::PathBuf>,
    signals: Option<DiskDumpSignals>,
    proxy: Option<Arc<crate::worker_inference_proxy::InferenceProxyConfig>>,
) -> Result<()> {
    // axum-server's TLS path (and any rustls construction) needs a
    // crypto provider installed at process scope. We choose
    // aws_lc_rs to match reqwest's default — the same provider
    // services both halves of the protocol. Idempotent: a second
    // install is silently ignored, so embedded callers that already
    // wired one up aren't disturbed.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let (dump_complete, dump_ready) = signals.unwrap_or_else(new_disk_dump_signals);
    let mut state_inner =
        WorkerState::from_blob_with_signals(blob.clone(), runner, dump_complete, dump_ready)
            .map_err(WorkerDaemonError::State)?;
    state_inner.inference_proxy = proxy;
    let state = Arc::new(state_inner);
    // Kick off background fetches for any URL-backed entries in the
    // manifest. The pod's `/health` shows progress; the dispatch
    // handler's existing precondition wait covers the case where the
    // owner gets ahead of the fetcher.
    state.spawn_url_fetches();
    // If the daemon was given a models dir, spawn the disk-dump
    // watcher — bytes will land on disk + a child-daemon config is
    // written when every upload completes. Phase 2 will spawn the
    // child off this.
    if let Some(dir) = models_dir {
        state.spawn_disk_dump_watcher(dir);
    }
    // Capture `proxy_enabled` before `state` moves into worker_router
    // — the boot log line below needs the flag for operator-side
    // glassbox visibility.
    let proxy_enabled = state.inference_proxy.is_some();
    let router = worker_router(state);

    let (cert_der, key_der) = self_signed_cert(&blob.seed)?;
    // `axum_server::tls_rustls::RustlsConfig::from_der` expects the
    // cert chain as Vec<Vec<u8>> and the private key as Vec<u8>. Our
    // helper returns DER for both — feed straight through.
    let tls_config = axum_server::tls_rustls::RustlsConfig::from_der(vec![cert_der], key_der)
        .await
        .map_err(|e| WorkerDaemonError::Tls(format!("rustls config: {e}")))?;

    let addr = bind_addr.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], WORKER_PORT)));
    // Stamp the build's git SHA into the boot log so a stale
    // container running pre-fix code is one grep away from
    // diagnosis. See `sovereign/crates/sovereign-mesh/build.rs` for
    // the stamp source. Falls back to "unknown" when `.git` isn't
    // reachable AND `SOVEREIGN_GIT_SHA` wasn't set at build time.
    let git_sha: &str = env!("SOVEREIGN_GIT_SHA");
    tracing::info!(
        addr = %addr,
        job_id = %blob.job_id,
        expected_uploads = blob.expected_uploads.len(),
        git_sha = %git_sha,
        proxy_enabled,
        "worker daemon listening"
    );

    axum_server::bind_rustls(addr, tls_config)
        .serve(router.into_make_service_with_connect_info::<SocketAddr>())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker_pod::{mint_bootstrap, BootstrapInputs};
    use ed25519_dalek::SigningKey;
    use std::collections::BTreeMap;

    #[test]
    fn load_bootstrap_from_env_round_trip() {
        let owner = SigningKey::from_bytes(&[31u8; 32]);
        let (blob, _) = mint_bootstrap(BootstrapInputs {
            job_id: "load-test".into(),
            owner_signing: &owner,
            expected_uploads: BTreeMap::new(),
            ttl_seconds: 60,
            seed_override: Some([55u8; 32]),
        })
        .unwrap();
        let encoded = crate::worker_pod::encode_bootstrap(&blob).unwrap();
        // SAFETY: tests are single-threaded by default within a module
        // and we don't spawn elsewhere here.
        unsafe {
            std::env::set_var("TEST_WORKER_BOOTSTRAP", &encoded);
        }
        let (loaded, source) = load_bootstrap_blob("TEST_WORKER_BOOTSTRAP", None).unwrap();
        assert_eq!(loaded, blob);
        assert!(source.starts_with("env:"));
        unsafe {
            std::env::remove_var("TEST_WORKER_BOOTSTRAP");
        }
    }

    #[test]
    fn load_bootstrap_missing_env_is_clear_error() {
        // Use a var name that's almost certainly unset.
        let err = load_bootstrap_blob("SOVEREIGN_NOTSET_TEST_XYZ", None).unwrap_err();
        assert!(matches!(err, WorkerDaemonError::Blob(_)));
        let msg = err.to_string();
        assert!(msg.contains("worker mode"), "msg was: {msg}");
    }

    #[test]
    fn echo_runner_emits_one_completed_per_unit() {
        use crate::worker_http::WorkUnit;
        let manifest = JobManifest {
            job_id: "j".into(),
            units: vec![
                WorkUnit {
                    unit_id: 1,
                    kind: "k1".into(),
                    payload: serde_json::json!(null),
                },
                WorkUnit {
                    unit_id: 2,
                    kind: "k2".into(),
                    payload: serde_json::json!(null),
                },
            ],
            config: serde_json::json!({}),
        };
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count_c = count.clone();
        let emit: EmitCompletedFn = std::sync::Arc::new(move |_unit| {
            count_c.fetch_add(1, std::sync::atomic::Ordering::Release);
        });
        EchoRunner.dispatch(manifest, emit);
        assert_eq!(count.load(std::sync::atomic::Ordering::Acquire), 2);
    }
}
