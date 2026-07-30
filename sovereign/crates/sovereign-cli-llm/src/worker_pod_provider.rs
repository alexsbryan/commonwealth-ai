// SPDX-License-Identifier: AGPL-3.0-or-later
//! Vast `WorkerProvider` impl + owner-key persistence.
//!
//! Spec: `sovereign/docs/EPHEMERAL_WORKER_PODS.md`. The
//! [`VastWorkerProvider`] is the concrete implementation
//! `WorkerController` consumes when the operator picks Vast — it wraps
//! the existing `sovereign_pipeline::pod::*` shell-outs and adds an
//! `address()` discovery poll backed by `vastai show instance --raw`.
//!
//! Lives in `sovereign-cli` (not sovereign-mesh or sovereign-pipeline)
//! because that's the only crate already depending on both: pipeline
//! provides the vastai create/destroy helpers, mesh provides the
//! `WorkerProvider` trait. Avoiding a dep cycle between those two
//! crates is the load-bearing constraint here.

use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use serde::Deserialize;
use sovereign_mesh::worker_controller::{
    JobSpec, ProviderError, ProviderInstance, ProviderResult, PublicAddress, WorkerProvider,
};
use sovereign_mesh::worker_pod::WORKER_PORT;
use sovereign_pipeline::pod;

/// Path on disk for the owner's persistent Ed25519 signing key. Lives
/// alongside the pipeline pod ledger so a single `chmod 700
/// ~/.sovereign` covers both.
pub fn owner_key_path() -> PathBuf {
    sovereign_cli_shared::dirs::sovereign_root().join("worker_owner_key.bin")
}

/// Load the owner's signing key, generating + persisting a fresh one
/// on first use. The file is the raw 32-byte Ed25519 seed; readers
/// re-derive the keypair via `SigningKey::from_bytes`.
///
/// Why raw bytes and not PKCS#8: this file is read once at owner
/// startup and never crosses a process boundary in a structured form
/// — the persisted-bytes round-trip is dead simple. No tooling reads
/// it; the only consumer is `WorkerController::new`.
pub fn load_or_create_owner_key() -> std::io::Result<SigningKey> {
    load_or_create_owner_key_at(&owner_key_path())
}

/// Path-explicit form used by tests. Avoids $HOME-based discovery so
/// parallel tests don't race on `set_var("HOME", …)`.
pub fn load_or_create_owner_key_at(path: &Path) -> std::io::Result<SigningKey> {
    if path.exists() {
        let bytes = std::fs::read(path)?;
        if bytes.len() != 32 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: expected 32 bytes, got {}", path.display(), bytes.len()),
            ));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        return Ok(SigningKey::from_bytes(&seed));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    use rand::TryRngCore;
    let mut seed = [0u8; 32];
    let mut rng = rand::rngs::OsRng;
    rng.try_fill_bytes(&mut seed)
        .map_err(|e| std::io::Error::other(format!("OS rng: {e}")))?;
    // Write atomically: write-to-temp + rename. Plain `write_all`
    // would leave a zero-length file on crash mid-write.
    let tmp = path.with_extension("bin.tmp");
    std::fs::write(&tmp, seed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&tmp, perms);
    }
    std::fs::rename(&tmp, path)?;
    Ok(SigningKey::from_bytes(&seed))
}

// ───── Vast provider ─────────────────────────────────────────────────

/// Vast-backed [`WorkerProvider`]. Constructed with the target
/// container image and an [`Offer`] picked from `vastai search`.
pub struct VastWorkerProvider {
    pub image: String,
    pub disk_gb: u32,
    pub label: String,
    /// The offer the controller will create on. Stored so `create()`
    /// has the price + offer-id at the moment of `vastai create`.
    pub offer: pod::Offer,
    /// Internal pod port the daemon binds. Vast maps it to a host
    /// port; the [`address`] method resolves the mapping.
    pub worker_port: u16,
}

impl VastWorkerProvider {
    pub fn new(image: String, disk_gb: u32, label: String, offer: pod::Offer) -> Self {
        Self {
            image,
            disk_gb,
            label,
            offer,
            worker_port: WORKER_PORT,
        }
    }
}

impl WorkerProvider for VastWorkerProvider {
    fn create(&self, bootstrap_b64: &str, _spec: &JobSpec) -> ProviderResult<ProviderInstance> {
        // The onstart_cmd is intentionally minimal vs. the legacy
        // path: no Tailscale env, no R2 env, no mesh-join env. The
        // pod's entrypoint reads SOVEREIGN_BOOTSTRAP and does the
        // rest.
        let onstart_cmd = format!(
            "set -eu\n\
             export SOVEREIGN_BOOTSTRAP='{bootstrap_b64}'\n\
             exec /entrypoint.sh\n",
        );
        let req = pod::CreateRequest {
            offer_id: self.offer.id,
            image: &self.image,
            disk_gb: self.disk_gb,
            onstart_cmd: &onstart_cmd,
            env: "",
            label: &self.label,
            ssh: true,
        };
        let created = pod::create_instance(&req, &self.offer)
            .map_err(|e| ProviderError::Other(format!("vastai create: {e}")))?;
        Ok(ProviderInstance {
            instance_id: created.vast_id,
            gpu_name: created.gpu_name,
            cost_per_hour: created.cost_per_hour,
        })
    }

    fn address(&self, instance_id: &str) -> ProviderResult<Option<PublicAddress>> {
        let raw = vastai_show_instance_raw(instance_id)
            .map_err(|e| ProviderError::Other(format!("vastai show: {e}")))?;
        let parsed: ShowInstance = serde_json::from_str(&raw)
            .map_err(|e| ProviderError::Other(format!("vastai show json: {e}")))?;
        // Wait until the pod is actually running — `public_ipaddr` is
        // populated earlier than `actual_status=running`, but trying
        // to connect to a pod that hasn't started its entrypoint yet
        // is wasted polls.
        if parsed.actual_status.as_deref() != Some("running") {
            return Ok(None);
        }
        let Some(public) = parsed.public_ipaddr else {
            return Ok(None);
        };
        let port_key = format!("{}/tcp", self.worker_port);
        let Some(mappings) = parsed.ports.and_then(|m| m.get(&port_key).cloned()) else {
            return Ok(None);
        };
        let Some(first) = mappings.into_iter().next() else {
            return Ok(None);
        };
        let port: u16 = first
            .host_port
            .parse()
            .map_err(|e| ProviderError::Other(format!("vastai HostPort parse: {e}")))?;
        Ok(Some(PublicAddress { host: public, port }))
    }

    fn destroy(&self, instance_id: &str) -> ProviderResult<()> {
        pod::destroy_instance(instance_id)
            .map_err(|e| ProviderError::Other(format!("vastai destroy: {e}")))?;
        Ok(())
    }
}

// ───── Multi-offer Vast provider (drives `MultiPodCoordinator`) ──────

/// A [`WorkerProvider`] backed by a queue of pre-picked Vast offers.
/// Each call to [`create`] dispenses the next offer in the queue.
/// Returns `Provider::Other` when the queue is empty — that should
/// only happen if the caller asked for more pods than offers were
/// staged, which is a user-side bug.
///
/// Lives in the CLI crate (not `sovereign-mesh`) for the same reason
/// the single-offer provider does: keeping `vastai` shell-outs out of
/// the mesh crate avoids the dep cycle that would otherwise form.
///
/// [`create`]: WorkerProvider::create
pub struct MultiOfferVastWorkerProvider {
    pub image: String,
    pub disk_gb: u32,
    /// Used as a prefix; per-pod labels become `<label>-p<index>`.
    pub label_prefix: String,
    pub worker_port: u16,
    /// Queue of offers — popped in order. The first `create()` call
    /// gets `offers[0]`, the second `offers[1]`, etc.
    offers: std::sync::Mutex<Vec<pod::Offer>>,
    /// Monotonically increasing per-pod index, used for label suffix.
    next_index: std::sync::atomic::AtomicUsize,
}

impl MultiOfferVastWorkerProvider {
    pub fn new(image: String, disk_gb: u32, label_prefix: String, offers: Vec<pod::Offer>) -> Self {
        Self {
            image,
            disk_gb,
            label_prefix,
            worker_port: WORKER_PORT,
            offers: std::sync::Mutex::new(offers),
            next_index: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn remaining_offers(&self) -> usize {
        self.offers.lock().map(|v| v.len()).unwrap_or(0)
    }
}

impl WorkerProvider for MultiOfferVastWorkerProvider {
    fn create(&self, bootstrap_b64: &str, _spec: &JobSpec) -> ProviderResult<ProviderInstance> {
        let offer = {
            let mut guard = self
                .offers
                .lock()
                .map_err(|e| ProviderError::Other(format!("offer queue poisoned: {e}")))?;
            if guard.is_empty() {
                return Err(ProviderError::Other(
                    "offer queue exhausted — pod_count exceeded staged offers".into(),
                ));
            }
            // Pop the front so first-pod = best-offer (offers are
            // pre-sorted by reliability/price).
            guard.remove(0)
        };
        let pod_index = self
            .next_index
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let label = format!("{}-p{pod_index}", self.label_prefix);

        let onstart_cmd = format!(
            "set -eu\n\
             export SOVEREIGN_BOOTSTRAP='{bootstrap_b64}'\n\
             exec /entrypoint.sh\n",
        );
        let req = pod::CreateRequest {
            offer_id: offer.id,
            image: &self.image,
            disk_gb: self.disk_gb,
            onstart_cmd: &onstart_cmd,
            env: "",
            label: &label,
            ssh: true,
        };
        let created = pod::create_instance(&req, &offer)
            .map_err(|e| ProviderError::Other(format!("vastai create (pod {pod_index}): {e}")))?;
        Ok(ProviderInstance {
            instance_id: created.vast_id,
            gpu_name: created.gpu_name,
            cost_per_hour: created.cost_per_hour,
        })
    }

    fn address(&self, instance_id: &str) -> ProviderResult<Option<PublicAddress>> {
        // Address logic is identical to the single-offer path — every
        // pod is the same `vastai show` shape regardless of which
        // offer it was created against.
        let raw = vastai_show_instance_raw(instance_id)
            .map_err(|e| ProviderError::Other(format!("vastai show: {e}")))?;
        let parsed: ShowInstance = serde_json::from_str(&raw)
            .map_err(|e| ProviderError::Other(format!("vastai show json: {e}")))?;
        if parsed.actual_status.as_deref() != Some("running") {
            return Ok(None);
        }
        let Some(public) = parsed.public_ipaddr else {
            return Ok(None);
        };
        let port_key = format!("{}/tcp", self.worker_port);
        let Some(mappings) = parsed.ports.and_then(|m| m.get(&port_key).cloned()) else {
            return Ok(None);
        };
        let Some(first) = mappings.into_iter().next() else {
            return Ok(None);
        };
        let port: u16 = first
            .host_port
            .parse()
            .map_err(|e| ProviderError::Other(format!("vastai HostPort parse: {e}")))?;
        Ok(Some(PublicAddress { host: public, port }))
    }

    fn destroy(&self, instance_id: &str) -> ProviderResult<()> {
        pod::destroy_instance(instance_id)
            .map_err(|e| ProviderError::Other(format!("vastai destroy: {e}")))?;
        Ok(())
    }
}

/// Vast's `show instance` JSON has many more fields than we need; we
/// deserialize only what `address()` consumes. Unknown fields are
/// silently ignored (serde default).
#[derive(Debug, Deserialize)]
struct ShowInstance {
    #[serde(default)]
    actual_status: Option<String>,
    #[serde(default)]
    public_ipaddr: Option<String>,
    /// Map of "internal-port/proto" → list of mappings. Each mapping
    /// is `{HostIp, HostPort}`. Vast emits these as strings.
    #[serde(default)]
    ports: Option<std::collections::BTreeMap<String, Vec<PortMapping>>>,
}

#[derive(Debug, Deserialize, Clone)]
struct PortMapping {
    #[serde(default, rename = "HostPort")]
    host_port: String,
}

fn vastai_show_instance_raw(instance_id: &str) -> std::io::Result<String> {
    let out = std::process::Command::new("vastai")
        .arg("show")
        .arg("instance")
        .arg(instance_id)
        .arg("--raw")
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "vastai show instance {instance_id} exited {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr),
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_key_is_persisted_and_reloaded() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("worker_owner_key.bin");
        let k1 = load_or_create_owner_key_at(&path).unwrap();
        let k2 = load_or_create_owner_key_at(&path).unwrap();
        assert_eq!(k1.verifying_key().to_bytes(), k2.verifying_key().to_bytes());
    }

    #[test]
    fn owner_key_rejects_bad_file_length() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("worker_owner_key.bin");
        std::fs::write(&path, b"too short").unwrap();
        let err = load_or_create_owner_key_at(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn show_instance_parses_running_pod() {
        let json = r#"{
            "id": 12345,
            "actual_status": "running",
            "public_ipaddr": "1.2.3.4",
            "ports": {
                "9742/tcp": [{ "HostIp": "0.0.0.0", "HostPort": "32774" }]
            }
        }"#;
        let parsed: ShowInstance = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.actual_status.as_deref(), Some("running"));
        assert_eq!(parsed.public_ipaddr.as_deref(), Some("1.2.3.4"));
        let map = parsed.ports.unwrap();
        let bound = map.get("9742/tcp").unwrap().first().unwrap();
        assert_eq!(bound.host_port, "32774");
    }

    #[test]
    fn show_instance_handles_pending_pod() {
        // Before public_ipaddr is populated.
        let json = r#"{
            "id": 12345,
            "actual_status": "loading"
        }"#;
        let parsed: ShowInstance = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.actual_status.as_deref(), Some("loading"));
        assert!(parsed.public_ipaddr.is_none());
        assert!(parsed.ports.is_none());
    }
}
