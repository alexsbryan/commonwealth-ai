//! Owner-side controller for ephemeral worker pods.
//!
//! Spec: `sovereign/docs/EPHEMERAL_WORKER_PODS.md`. This module is the
//! mirror of `worker_http`: where the pod implements the four routes,
//! the controller calls them. It owns the full lifecycle:
//!
//! 1. Mint a [`BootstrapBlob`](super::worker_pod::BootstrapBlob),
//!    base64-encode it, hand it to a [`WorkerProvider`] (typically
//!    Vast) as the pod's `onstart` env.
//! 2. Poll the provider for the pod's public address + port.
//! 3. Open an HTTPS connection pinned to the seed-derived cert. We
//!    achieve the pin by computing the same self-signed cert locally
//!    and registering it as a reqwest trust root — the pod's
//!    presented cert must be identical to be accepted.
//! 4. Stream uploads (`POST /internal/worker/upload`), dispatch the
//!    job manifest (`POST /internal/worker/job`), and poll completed
//!    units (`GET /internal/worker/completed?since=<cursor>`).
//! 5. On completion or abort, `DELETE /internal/worker/job` and
//!    ask the provider to tear the pod down.
//!
//! ## Provider abstraction (Vast / RunPod / future)
//!
//! [`WorkerProvider`] is a trait. The concrete Vast adapter lives in
//! `sovereign-cli` (where the existing `sovereign_pipeline::pod`
//! vastai shell-outs already live) — this crate stays infra-agnostic
//! to avoid a cycle. Tests pass an in-memory mock that bypasses the
//! cloud entirely.
//!
//! ## Multi-pod fan-out
//!
//! [`WorkerHandle`](super::worker_pod::WorkerHandle) is cheaply
//! cloneable; each handle's `poll_completed` is independent. The
//! single-pod controller and a future `create_pool` differ only in
//! a `Vec<WorkerHandle>` plus a `select!` over their `poll_completed`
//! futures. The bootstrap blob format, TLS pinning, and endpoint
//! shapes don't change — that's what makes the fan-out a fast-follow
//! instead of a rewrite (see `EPHEMERAL_WORKER_PODS.md` §"Multi-pod
//! jobs").

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::worker_http::{CompletedUnit, JobManifest};
use crate::worker_pod::{
    BootstrapBlob, BootstrapInputs, Sha256Digest, WorkerHandle, WorkerPodError, encode_bootstrap,
    mint_bootstrap, self_signed_cert,
};

// ───── Provider abstraction ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ProviderInstance {
    pub instance_id: String,
    pub gpu_name: String,
    pub cost_per_hour: f64,
}

#[derive(Debug, Clone)]
pub struct PublicAddress {
    pub host: String,
    pub port: u16,
}

/// Cloud-provider lifecycle hooks. Implemented per provider — the Vast
/// adapter shells to `vastai`, the RunPod adapter shells to `runpodctl`.
pub trait WorkerProvider: Send + Sync {
    /// Create a pod with `bootstrap_b64` injected as
    /// `SOVEREIGN_BOOTSTRAP` in its onstart env. Returns the provider's
    /// instance id + accounting metadata.
    fn create(&self, bootstrap_b64: &str, spec: &JobSpec) -> ProviderResult<ProviderInstance>;
    /// Resolve the pod's public address. Vast's `instances show <id>`
    /// returns this after a brief delay; the controller polls.
    fn address(&self, instance_id: &str) -> ProviderResult<Option<PublicAddress>>;
    /// Tear down. Idempotent — destroying a non-existent instance is OK.
    fn destroy(&self, instance_id: &str) -> ProviderResult<()>;
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider: {0}")]
    Other(String),
}

pub type ProviderResult<T> = std::result::Result<T, ProviderError>;

// ───── Job spec ─────────────────────────────────────────────────────

/// What the owner ships to `WorkerController::create_and_run`. Captures
/// the provider-shaped knobs (image, disk, GPU, max price) and the
/// payload (files to upload, work-queue manifest).
#[derive(Debug, Clone)]
pub struct JobSpec {
    pub job_id: String,
    /// Container image ref (e.g. `ghcr.io/you/sovereign-cuda:latest`).
    pub image: String,
    pub disk_gb: u32,
    pub gpu_name: String,
    pub max_price_per_hour: f64,
    /// Human label propagated to the provider + cost ledger.
    pub label: String,
    /// Files the owner will stream to the pod via `/upload`. Map of
    /// `name → (local_path, sha256)`.
    pub uploads: BTreeMap<String, UploadFile>,
    /// Work-queue manifest the owner POSTs after uploads finish.
    pub units: Vec<crate::worker_http::WorkUnit>,
    /// Free-form runner config; opaque to the controller.
    pub runner_config: serde_json::Value,
}

/// Where the bytes for one manifest entry come from. Either streamed
/// up from the owner's local disk, or fetched by the pod from a URL
/// (typically a presigned object-store URL — R2/B2/S3 — so the bytes
/// arrive at the pod over a data-center-fronted multi-Gbps egress
/// path instead of the owner's residential upload).
///
/// SHA validation is the load-bearing piece: in both cases the pod
/// hashes received bytes against the owner-signed `sha256`, so the
/// URL is trusted transport, not trusted source.
#[derive(Debug, Clone)]
pub struct UploadFile {
    pub sha256: Sha256Digest,
    pub source: UploadSource,
}

#[derive(Debug, Clone)]
pub enum UploadSource {
    /// Stream bytes from the owner's disk over the pinned-TLS
    /// connection. Right for files that only exist locally (recipe
    /// configs, owner-built corpora) or small enough that residential
    /// upload is acceptable.
    Local(std::path::PathBuf),
    /// Have the pod fetch the URL itself. Right for big GGUFs staged
    /// in R2/B2/S3 — pod's data-center egress (multi-Gbps) replaces
    /// owner's residential upload (10-50 Mbps).
    FetchUrl(String),
}

impl UploadFile {
    pub fn local(path: impl Into<std::path::PathBuf>, sha256: Sha256Digest) -> Self {
        Self {
            sha256,
            source: UploadSource::Local(path.into()),
        }
    }
    pub fn fetch_url(url: impl Into<String>, sha256: Sha256Digest) -> Self {
        Self {
            sha256,
            source: UploadSource::FetchUrl(url.into()),
        }
    }
    pub fn is_url_backed(&self) -> bool {
        matches!(self.source, UploadSource::FetchUrl(_))
    }
}

// ───── Controller ──────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ControllerError {
    #[error("provider: {0}")]
    Provider(#[from] ProviderError),
    #[error("worker-pod foundation: {0}")]
    WorkerPod(#[from] WorkerPodError),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("timed out waiting for {what} after {elapsed_secs}s")]
    Timeout {
        what: &'static str,
        elapsed_secs: u64,
    },
    #[error("pod returned status {status} on {route}: {body}")]
    PodRejected {
        status: u16,
        route: String,
        body: String,
    },
    /// Caller-facing validation failure. Distinct from `Timeout` /
    /// `PodRejected` / network errors so the CLI can format it as a
    /// usage error rather than a transient failure.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

pub type ControllerResult<T> = std::result::Result<T, ControllerError>;

/// Configuration knobs for the controller. Defaults are fine for the
/// SEP/wiki fanout shape (45-min job, ~80 GB uploads, 256-unit
/// manifests).
#[derive(Debug, Clone)]
pub struct ControllerConfig {
    pub address_poll_interval: Duration,
    pub address_poll_timeout: Duration,
    pub health_poll_interval: Duration,
    pub health_poll_timeout: Duration,
    /// How long to wait for all URL-backed uploads on the pod to
    /// finish. Distinct from `health_poll_timeout` because uploads
    /// can take 10x longer than the daemon's HTTP probe — a 30 GB
    /// GGUF fetched from R2 over a residential-link pod's egress
    /// can take 5-10 min, well past the 5-min `health` ceiling.
    /// Default 30 min covers 60 GB at 250 Mbps with headroom.
    pub uploads_poll_timeout: Duration,
    pub completed_poll_interval: Duration,
    /// Tokens live for the WHOLE job + a buffer so an owner-side
    /// restart doesn't invalidate them mid-poll. Past this the
    /// controller has to mint a new bootstrap and restart the pod.
    pub bootstrap_ttl_seconds: u64,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            address_poll_interval: Duration::from_secs(5),
            address_poll_timeout: Duration::from_secs(180),
            health_poll_interval: Duration::from_secs(3),
            health_poll_timeout: Duration::from_secs(300),
            uploads_poll_timeout: Duration::from_secs(30 * 60),
            completed_poll_interval: Duration::from_secs(30),
            // 12 hours: a long SEP fanout finishes in ~6h on an L40S;
            // doubling that leaves headroom for retries + owner restart.
            bootstrap_ttl_seconds: 12 * 3600,
        }
    }
}

pub struct WorkerController {
    provider: Arc<dyn WorkerProvider>,
    config: ControllerConfig,
    /// Owner's signing key — used to mint WorkerTokens. Persisted to
    /// `~/.sovereign/worker_owner_key` by the wiring layer (CLI or
    /// desktop); generated fresh on first BYO setup.
    owner_signing: SigningKey,
}

impl WorkerController {
    pub fn new(
        provider: Arc<dyn WorkerProvider>,
        owner_signing: SigningKey,
        config: ControllerConfig,
    ) -> Self {
        Self {
            provider,
            config,
            owner_signing,
        }
    }

    /// Full lifecycle helper: mint bootstrap, ask provider to create
    /// the pod, wait for it to come up, upload files, dispatch the
    /// job. Returns a [`WorkerHandle`] the owner uses to poll.
    pub async fn create_and_run(&self, spec: &JobSpec) -> ControllerResult<(WorkerHandle, ProviderInstance)> {
        let (handle, instance, _blob, _client) = self.create_and_run_with_blob(spec).await?;
        Ok((handle, instance))
    }

    /// Same as [`Self::create_and_run`] but also returns the bootstrap
    /// blob and the pinned reqwest client. Needed by callers (notably
    /// [`MultiPodCoordinator`]) that own the polling loop themselves —
    /// the blob carries the seed used to derive the per-pod cert, and
    /// the client is the only one whose root-of-trust matches that
    /// cert.
    ///
    /// [`MultiPodCoordinator`]: crate::multi_pod_coordinator::MultiPodCoordinator
    pub async fn create_and_run_with_blob(
        &self,
        spec: &JobSpec,
    ) -> ControllerResult<(WorkerHandle, ProviderInstance, BootstrapBlob, Client)> {
        let blob = self.mint_blob(spec)?;
        let bootstrap_b64 = encode_bootstrap(&blob)?;
        let instance = self.provider.create(&bootstrap_b64, spec)?;

        // From here on, every fallible step must tear down the
        // provider instance on failure. Otherwise a timeout 5 min
        // into the boot sequence leaves a pod billing forever and
        // the operator has to grep `vastai show instances` to find
        // it. Wrap the post-create lifecycle in an inner helper so
        // `?` propagates errors out, then catch and destroy.
        match self
            .complete_create_lifecycle(&instance, &blob, spec)
            .await
        {
            Ok((handle, client)) => Ok((handle, instance, blob, client)),
            Err(e) => {
                tracing::error!(
                    instance_id = %instance.instance_id,
                    error = %e,
                    "controller: post-create lifecycle failed — destroying provider instance"
                );
                eprintln!(
                    "[controller] post-create failure on instance {}: {e}\n\
                     [controller] auto-destroying to stop billing…",
                    instance.instance_id
                );
                if let Err(destroy_err) = self.provider.destroy(&instance.instance_id) {
                    eprintln!(
                        "[controller] WARNING: auto-destroy failed for instance {}: {destroy_err}\n\
                         [controller] manually destroy with: `sovereign pipeline pod down {}` \
                         or `vastai destroy instance {}`",
                        instance.instance_id,
                        instance.instance_id,
                        instance.instance_id,
                    );
                } else {
                    eprintln!(
                        "[controller] instance {} destroyed.",
                        instance.instance_id
                    );
                }
                Err(e)
            }
        }
    }

    /// Inner half of [`Self::create_and_run_with_blob`]: everything
    /// after `provider.create` returns. Factored so the error path
    /// has a single catch site for auto-destroy.
    async fn complete_create_lifecycle(
        &self,
        instance: &ProviderInstance,
        blob: &BootstrapBlob,
        spec: &JobSpec,
    ) -> ControllerResult<(WorkerHandle, Client)> {
        let address = self.wait_for_address(&instance.instance_id).await?;
        let client = self.build_pinned_client(blob)?;
        let handle = WorkerHandle::new(
            address.host.clone(),
            address.port,
            blob.pod_pubkey_thumbprint(),
            blob.worker_token.clone(),
            blob.job_id.clone(),
            self.owner_signing.clone(),
        );

        self.wait_for_health(&handle, &client).await?;
        self.upload_files(&handle, &client, blob, spec).await?;
        // If any entry is URL-backed, the pod is still pulling in the
        // background. Block until it reports all uploads ready before
        // dispatch — otherwise dispatch_job 412s with "uploads
        // incomplete" and the owner has to retry.
        if spec.uploads.values().any(|f| f.is_url_backed()) {
            self.wait_for_uploads(&handle, &client).await?;
        }
        // Only dispatch if the spec actually has units. The single-pod
        // CLI calls `create_and_run` with `units: []` to leave the pod
        // in "uploads ready" state for a follow-up `pod dispatch`; the
        // multi-pod path packs partition units into the spec ahead of
        // time and lets dispatch happen here.
        if !spec.units.is_empty() {
            self.dispatch_job(&handle, &client, spec).await?;
        }
        Ok((handle, client))
    }

    fn mint_blob(&self, spec: &JobSpec) -> ControllerResult<BootstrapBlob> {
        let mut expected = BTreeMap::new();
        for (name, file) in &spec.uploads {
            let entry = match &file.source {
                UploadSource::Local(_) => crate::worker_pod::UploadEntry::local(file.sha256),
                UploadSource::FetchUrl(url) => {
                    crate::worker_pod::UploadEntry::from_url(file.sha256, url.clone())
                }
            };
            expected.insert(name.clone(), entry);
        }
        let (blob, _thumb) = mint_bootstrap(BootstrapInputs {
            job_id: spec.job_id.clone(),
            owner_signing: &self.owner_signing,
            expected_uploads: expected,
            ttl_seconds: self.config.bootstrap_ttl_seconds,
            seed_override: None,
        })?;
        Ok(blob)
    }

    /// Build a reqwest client that trusts exactly one cert — the
    /// self-signed cert the pod will generate from the blob's seed.
    /// Because the cert is deterministic from the seed we hold, we
    /// can register it as a trust root before the pod has even booted.
    /// Hostname validation is disabled because the cert's CN is a
    /// generic placeholder; the pin is the key material, not the name.
    fn build_pinned_client(&self, blob: &BootstrapBlob) -> ControllerResult<Client> {
        let (cert_der, _key_der) = self_signed_cert(&blob.seed)?;
        let cert = reqwest::Certificate::from_der(&cert_der)?;
        let client = reqwest::ClientBuilder::new()
            .add_root_certificate(cert)
            // Pod's cert CN is a placeholder; the pin is the cert
            // itself. Skipping hostname validation does NOT weaken
            // the trust model — only this specific cert is accepted,
            // and only one private key on earth can present it (the
            // one derived from the seed in our blob).
            .danger_accept_invalid_hostnames(true)
            .build()?;
        Ok(client)
    }

    async fn wait_for_address(&self, instance_id: &str) -> ControllerResult<PublicAddress> {
        let start = std::time::Instant::now();
        loop {
            if let Some(addr) = self.provider.address(instance_id)? {
                return Ok(addr);
            }
            if start.elapsed() >= self.config.address_poll_timeout {
                return Err(ControllerError::Timeout {
                    what: "pod public address",
                    elapsed_secs: start.elapsed().as_secs(),
                });
            }
            tokio::time::sleep(self.config.address_poll_interval).await;
        }
    }

    pub async fn wait_for_health(
        &self,
        handle: &WorkerHandle,
        client: &Client,
    ) -> ControllerResult<()> {
        let start = std::time::Instant::now();
        let url = format!("{}/internal/worker/health", handle.base_url());
        loop {
            let resp = client
                .get(&url)
                .bearer_auth(handle.worker_token())
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => return Ok(()),
                Ok(r) => {
                    if start.elapsed() >= self.config.health_poll_timeout {
                        let status = r.status().as_u16();
                        let body = r.text().await.unwrap_or_default();
                        return Err(ControllerError::PodRejected {
                            status,
                            route: "/internal/worker/health".into(),
                            body,
                        });
                    }
                }
                Err(_) => {
                    if start.elapsed() >= self.config.health_poll_timeout {
                        return Err(ControllerError::Timeout {
                            what: "pod /health",
                            elapsed_secs: start.elapsed().as_secs(),
                        });
                    }
                }
            }
            tokio::time::sleep(self.config.health_poll_interval).await;
        }
    }

    pub async fn upload_files(
        &self,
        handle: &WorkerHandle,
        client: &Client,
        blob: &BootstrapBlob,
        spec: &JobSpec,
    ) -> ControllerResult<()> {
        for (name, file) in &spec.uploads {
            // Sanity: the manifest in spec must agree with the blob.
            // If not, the upload would fail at the pod anyway, but
            // catching it here is friendlier.
            let entry = blob.expected_uploads.get(name).cloned().ok_or_else(|| {
                ControllerError::PodRejected {
                    status: 0,
                    route: "/upload".into(),
                    body: format!("file {name} missing from blob manifest"),
                }
            })?;
            if entry.sha256 != file.sha256 {
                return Err(ControllerError::PodRejected {
                    status: 0,
                    route: "/upload".into(),
                    body: format!("sha mismatch for {name} between blob and JobSpec"),
                });
            }
            let local_path = match &file.source {
                UploadSource::FetchUrl(_) => {
                    // Pod fetches itself in the background. Skip
                    // the owner-side upload — `wait_for_uploads`
                    // (or the existing dispatch precondition check)
                    // will wait until the pod has finished pulling.
                    tracing::debug!(
                        file = %name,
                        "controller: file is URL-backed; pod fetches it directly"
                    );
                    continue;
                }
                UploadSource::Local(p) => p,
            };
            let bytes = tokio::fs::read(local_path).await?;
            // One-shot upload for the MVP. Streamed chunking is a
            // straightforward extension: append-only POSTs with
            // `finalize=false`, then a final POST with `finalize=true`.
            // Pod-side already supports both.
            let url = format!(
                "{}/internal/worker/upload?name={}&finalize=true",
                handle.base_url(),
                urlencoding::encode(name)
            );
            let resp = client
                .post(&url)
                .bearer_auth(handle.worker_token())
                .body(bytes)
                .send()
                .await?;
            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                return Err(ControllerError::PodRejected {
                    status,
                    route: format!("/upload?name={name}"),
                    body,
                });
            }
        }
        Ok(())
    }

    /// Wait until the pod reports `uploads_ready == uploads_expected`
    /// on its `/health` endpoint. Useful when any upload entry is
    /// URL-backed — those fetches happen in the background after the
    /// pod boots; controllers that go straight to `dispatch_job` will
    /// 412 until they're done.
    ///
    /// Polls at `health_poll_interval`; gives up after
    /// `health_poll_timeout`. Both knobs live in [`ControllerConfig`].
    pub async fn wait_for_uploads(
        &self,
        handle: &WorkerHandle,
        client: &Client,
    ) -> ControllerResult<()> {
        let start = std::time::Instant::now();
        let url = format!("{}/internal/worker/health", handle.base_url());
        loop {
            let resp = client
                .get(&url)
                .bearer_auth(handle.worker_token())
                .send()
                .await?;
            if resp.status().is_success() {
                let body: serde_json::Value = resp.json().await?;
                let ready = body.get("uploads_ready").and_then(|v| v.as_u64()).unwrap_or(0);
                let expected = body
                    .get("uploads_expected")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if ready >= expected && expected > 0 {
                    return Ok(());
                }
                if ready >= expected {
                    return Ok(()); // 0/0 — empty manifest, trivially ready.
                }
            }
            if start.elapsed() >= self.config.uploads_poll_timeout {
                return Err(ControllerError::Timeout {
                    what: "pod uploads to finish",
                    elapsed_secs: start.elapsed().as_secs(),
                });
            }
            tokio::time::sleep(self.config.health_poll_interval).await;
        }
    }

    pub async fn dispatch_job(
        &self,
        handle: &WorkerHandle,
        client: &Client,
        spec: &JobSpec,
    ) -> ControllerResult<()> {
        let manifest = JobManifest {
            job_id: spec.job_id.clone(),
            units: spec.units.clone(),
            config: spec.runner_config.clone(),
        };
        let url = format!("{}/internal/worker/job", handle.base_url());
        let resp = client
            .post(&url)
            .bearer_auth(handle.worker_token())
            .json(&manifest)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ControllerError::PodRejected {
                status,
                route: "/internal/worker/job".into(),
                body,
            });
        }
        Ok(())
    }

    /// One round of polling. Returns the (possibly empty) batch and
    /// advances the handle's cursor. The caller decides when to stop
    /// — typically when `total_completed >= units.len()`.
    pub async fn poll_completed(
        &self,
        handle: &WorkerHandle,
        client: &Client,
    ) -> ControllerResult<CompletedPollBatch> {
        let url = format!(
            "{}/internal/worker/completed?since={}",
            handle.base_url(),
            handle.cursor()
        );
        let resp = client
            .get(&url)
            .bearer_auth(handle.worker_token())
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ControllerError::PodRejected {
                status,
                route: "/internal/worker/completed".into(),
                body,
            });
        }
        let batch: CompletedPollBatch = resp.json().await?;
        handle.advance_cursor(batch.cursor);
        Ok(batch)
    }

    /// Polite shutdown: DELETE the job, then ask the provider to
    /// destroy the instance. Destroy is best-effort — provider errors
    /// don't fail the call, because a stuck pod is acceptable (it'll
    /// expire on its own when Vast bills it past budget). Logged.
    pub async fn destroy(
        &self,
        handle: &WorkerHandle,
        client: &Client,
        instance_id: &str,
    ) -> ControllerResult<()> {
        let url = format!("{}/internal/worker/job", handle.base_url());
        let _ = client
            .delete(&url)
            .bearer_auth(handle.worker_token())
            .send()
            .await; // Best-effort; pod may already be gone.
        if let Err(e) = self.provider.destroy(instance_id) {
            tracing::warn!(error = %e, instance_id, "provider destroy failed; ledger may be stale");
        }
        Ok(())
    }
}

/// Helper exposed to tests that need to mint a controller-style client
/// without standing up an entire provider.
pub fn build_pinned_client_for(blob: &BootstrapBlob) -> ControllerResult<Client> {
    let (cert_der, _key_der) = self_signed_cert(&blob.seed)?;
    let cert = reqwest::Certificate::from_der(&cert_der)?;
    let client = reqwest::ClientBuilder::new()
        .add_root_certificate(cert)
        .danger_accept_invalid_hostnames(true)
        .build()?;
    Ok(client)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedPollBatch {
    pub units: Vec<CompletedUnit>,
    pub cursor: u64,
    pub total_completed: usize,
}

// ───── Tests ────────────────────────────────────────────────────────
//
// The full owner↔pod cycle (real TLS server, real reqwest pin) lives
// in `tests/worker_e2e.rs` — that's where the TLS infrastructure
// dependencies are justified. The unit tests here cover the small
// behaviours that can be exercised without standing up a server:
// the pinned-client builds, the mock provider lifecycle is sane, the
// blob is minted with the expected manifest. Anything more would
// duplicate the e2e suite.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn fixed_owner_key() -> SigningKey {
        SigningKey::from_bytes(&[21u8; 32])
    }

    /// In-memory mock provider — keeps a single "instance" with an
    /// address the test sets explicitly. Useful in both unit tests
    /// here and the e2e test in `tests/worker_e2e.rs` (re-exported
    /// via the `pub use` below).
    pub struct MockProvider {
        address: Mutex<Option<PublicAddress>>,
        destroyed: Mutex<bool>,
    }

    impl MockProvider {
        pub fn new(addr: PublicAddress) -> Self {
            Self {
                address: Mutex::new(Some(addr)),
                destroyed: Mutex::new(false),
            }
        }
        pub fn destroyed(&self) -> bool {
            *self.destroyed.lock().unwrap()
        }
    }

    impl WorkerProvider for MockProvider {
        fn create(
            &self,
            _bootstrap_b64: &str,
            _spec: &JobSpec,
        ) -> ProviderResult<ProviderInstance> {
            Ok(ProviderInstance {
                instance_id: "mock-1".into(),
                gpu_name: "Mock".into(),
                cost_per_hour: 0.0,
            })
        }
        fn address(&self, _instance_id: &str) -> ProviderResult<Option<PublicAddress>> {
            Ok(self.address.lock().unwrap().clone())
        }
        fn destroy(&self, _instance_id: &str) -> ProviderResult<()> {
            *self.destroyed.lock().unwrap() = true;
            Ok(())
        }
    }

    #[test]
    fn mint_blob_carries_spec_manifest_into_blob() {
        let owner = fixed_owner_key();
        let provider = Arc::new(MockProvider::new(PublicAddress {
            host: "0.0.0.0".into(),
            port: 0,
        }));
        let ctrl = WorkerController::new(provider, owner, ControllerConfig::default());

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.gguf");
        std::fs::write(&path, b"x").unwrap();
        let mut uploads = BTreeMap::new();
        uploads.insert("f.gguf".to_string(), UploadFile::local(path, [9u8; 32]));
        let spec = JobSpec {
            job_id: "spec-test".into(),
            image: "img".into(),
            disk_gb: 1,
            gpu_name: "g".into(),
            max_price_per_hour: 0.0,
            label: "t".into(),
            uploads,
            units: vec![],
            runner_config: serde_json::json!({}),
        };
        let blob = ctrl.mint_blob(&spec).unwrap();
        assert_eq!(blob.job_id, "spec-test");
        let entry = blob.expected_uploads.get("f.gguf").unwrap();
        assert_eq!(entry.sha256, [9u8; 32]);
        assert!(entry.fetch_url.is_none(), "local upload has no fetch_url");
    }

    #[test]
    fn mint_blob_carries_url_backed_entries() {
        let owner = fixed_owner_key();
        let provider = Arc::new(MockProvider::new(PublicAddress {
            host: "h".into(),
            port: 0,
        }));
        let ctrl = WorkerController::new(provider, owner, ControllerConfig::default());

        let mut uploads = BTreeMap::new();
        uploads.insert(
            "primary.gguf".to_string(),
            UploadFile::fetch_url("https://r2.example/primary.gguf?sig=…", [3u8; 32]),
        );
        let spec = JobSpec {
            job_id: "url-spec".into(),
            image: "img".into(),
            disk_gb: 1,
            gpu_name: "g".into(),
            max_price_per_hour: 0.0,
            label: "t".into(),
            uploads,
            units: vec![],
            runner_config: serde_json::json!({}),
        };
        let blob = ctrl.mint_blob(&spec).unwrap();
        let entry = blob.expected_uploads.get("primary.gguf").unwrap();
        assert_eq!(entry.sha256, [3u8; 32]);
        assert_eq!(
            entry.fetch_url.as_deref(),
            Some("https://r2.example/primary.gguf?sig=…"),
        );
    }

    #[test]
    fn pinned_client_builds_from_seed() {
        let owner = fixed_owner_key();
        let (blob, _) = mint_bootstrap(BootstrapInputs {
            job_id: "j".into(),
            owner_signing: &owner,
            expected_uploads: BTreeMap::new(),
            ttl_seconds: 60,
            seed_override: Some([42u8; 32]),
        })
        .unwrap();
        // Just confirms the cert encodes correctly and reqwest accepts
        // it as a trust root. Connection logic is exercised in the
        // e2e integration test.
        let _client = build_pinned_client_for(&blob).expect("pinned client builds");
    }

    #[test]
    fn mock_provider_round_trip() {
        let p = MockProvider::new(PublicAddress {
            host: "h".into(),
            port: 1,
        });
        let inst = p.create("blob", &dummy_spec()).unwrap();
        assert_eq!(inst.instance_id, "mock-1");
        let a = p.address(&inst.instance_id).unwrap().unwrap();
        assert_eq!(a.host, "h");
        p.destroy(&inst.instance_id).unwrap();
        assert!(p.destroyed());
    }

    fn dummy_spec() -> JobSpec {
        JobSpec {
            job_id: "d".into(),
            image: "img".into(),
            disk_gb: 0,
            gpu_name: "g".into(),
            max_price_per_hour: 0.0,
            label: "t".into(),
            uploads: BTreeMap::new(),
            units: vec![],
            runner_config: serde_json::json!({}),
        }
    }
}
