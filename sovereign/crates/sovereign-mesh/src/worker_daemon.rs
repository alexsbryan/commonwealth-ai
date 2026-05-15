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
    CompletedUnit, EmitCompletedFn, JobManifest, WorkerRunner, WorkerState, worker_router,
};
use crate::worker_pod::{BootstrapBlob, WORKER_PORT, self_signed_cert};

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
pub async fn run_worker_mode(
    blob: BootstrapBlob,
    runner: Arc<dyn WorkerRunner>,
    bind_addr: Option<SocketAddr>,
) -> Result<()> {
    // axum-server's TLS path (and any rustls construction) needs a
    // crypto provider installed at process scope. We choose
    // aws_lc_rs to match reqwest's default — the same provider
    // services both halves of the protocol. Idempotent: a second
    // install is silently ignored, so embedded callers that already
    // wired one up aren't disturbed.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let state = Arc::new(
        WorkerState::from_blob(blob.clone(), runner)
            .map_err(WorkerDaemonError::State)?,
    );
    let router = worker_router(state);

    let (cert_der, key_der) = self_signed_cert(&blob.seed)?;
    // `axum_server::tls_rustls::RustlsConfig::from_der` expects the
    // cert chain as Vec<Vec<u8>> and the private key as Vec<u8>. Our
    // helper returns DER for both — feed straight through.
    let tls_config = axum_server::tls_rustls::RustlsConfig::from_der(vec![cert_der], key_der)
        .await
        .map_err(|e| WorkerDaemonError::Tls(format!("rustls config: {e}")))?;

    let addr = bind_addr.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], WORKER_PORT)));
    tracing::info!(
        addr = %addr,
        job_id = %blob.job_id,
        expected_uploads = blob.expected_uploads.len(),
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
    use crate::worker_pod::{BootstrapInputs, mint_bootstrap};
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
        unsafe { std::env::set_var("TEST_WORKER_BOOTSTRAP", &encoded); }
        let (loaded, source) = load_bootstrap_blob("TEST_WORKER_BOOTSTRAP", None).unwrap();
        assert_eq!(loaded, blob);
        assert!(source.starts_with("env:"));
        unsafe { std::env::remove_var("TEST_WORKER_BOOTSTRAP"); }
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
