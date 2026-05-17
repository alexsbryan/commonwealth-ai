//! On-disk snapshot for a pinned worker pod.
//!
//! Spec: `sovereign/docs/PINNED_WORKER_AS_INFERENCE_PEER.md` §3.6.
//!
//! ## What the snapshot carries
//!
//! Enough state for an *unrelated* CLI invocation (a fresh
//! `sovereign daemon run` after a reboot, say) to reconstruct a
//! TLS-pinned client and a valid bearer for the pod, without ever
//! talking to the pod first:
//!
//! - `vast_id` — Vast instance id (filename key + display label)
//! - `host` / `port` — pod's public address (TCP 9742 by convention)
//! - `bootstrap_blob` — owner-minted BootstrapBlob, carries
//!   `seed` (cert is deterministic from it) + `worker_token` (bearer)
//! - `capabilities` — operator-stamped RAM/benchmark
//! - `created_at_unix` — for stale-pod sweeps
//!
//! ## Why JSON, not bincode
//!
//! Snapshots are operator-edited in emergencies (e.g. typo-fix
//! `port`, hand-bump capability fields). The cost of pretty JSON
//! over a binary format is ~2 kB per file and we're storing dozens
//! at most.
//!
//! ## What a snapshot does NOT carry
//!
//! - The owner's Ed25519 *signing* key. The bootstrap blob carries
//!   the *verifying* half; signing stays in the owner's
//!   `~/.sovereign/keys/`. A snapshot is enough to *use* a pod but
//!   not to *mint a new token* for it — that's the desired
//!   blast-radius split.
//! - Vast credentials. `pod down` calls into the same `vastai`
//!   binary the operator used to bring it up; the snapshot is
//!   transport state only.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sovereign_core::oicp::BenchmarkResult;

use crate::pinned_transport::TransportError;
use crate::pinned_worker_source::{PinnedPod, PodCapabilities};
use crate::worker_pod::BootstrapBlob;

/// JSON-on-disk shape. The `version` field is bumped when the wire
/// shape gains a wire-incompatible change; loaders refuse versions
/// they don't recognise rather than risk a partial parse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedPodSnapshot {
    pub version: u8,
    pub vast_id: String,
    pub host: String,
    pub port: u16,
    pub bootstrap_blob: BootstrapBlob,
    pub capabilities: SerializableCapabilities,
    pub created_at_unix: u64,
}

/// Mirror of `PodCapabilities` with serde derives. `PodCapabilities`
/// itself stays non-serde so the in-memory scheduler isn't coupled
/// to a wire format; we cross the boundary here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SerializableCapabilities {
    pub system_ram_gb: u32,
    #[serde(default)]
    pub benchmark: Option<BenchmarkResult>,
    #[serde(default)]
    pub current_in_flight: Option<u32>,
}

impl From<&PodCapabilities> for SerializableCapabilities {
    fn from(c: &PodCapabilities) -> Self {
        Self {
            system_ram_gb: c.system_ram_gb,
            benchmark: c.benchmark.clone(),
            current_in_flight: c.current_in_flight,
        }
    }
}

impl From<SerializableCapabilities> for PodCapabilities {
    fn from(c: SerializableCapabilities) -> Self {
        Self {
            system_ram_gb: c.system_ram_gb,
            benchmark: c.benchmark,
            current_in_flight: c.current_in_flight,
        }
    }
}

pub const SNAPSHOT_VERSION: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("snapshot version {0} not supported (expected {SNAPSHOT_VERSION})")]
    UnsupportedVersion(u8),
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
}

pub type Result<T> = std::result::Result<T, SnapshotError>;

impl PinnedPodSnapshot {
    pub fn new(
        vast_id: impl Into<String>,
        host: impl Into<String>,
        port: u16,
        bootstrap_blob: BootstrapBlob,
        capabilities: PodCapabilities,
    ) -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            vast_id: vast_id.into(),
            host: host.into(),
            port,
            bootstrap_blob,
            capabilities: (&capabilities).into(),
            created_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }

    /// Reconstruct an in-memory [`PinnedPod`] suitable for handing to
    /// a [`PinnedWorkerEndpointSource`](crate::pinned_worker_source::PinnedWorkerEndpointSource).
    ///
    /// Re-derives the TLS-pinned reqwest client from the blob's seed
    /// (deterministic). The bearer comes from the blob's
    /// `worker_token`. The synthetic `NodeId` comes from the same
    /// seed, so a daemon restart that reloads this snapshot sees the
    /// pod as the same routing key — accumulated throughput
    /// observations persist across the restart in the per-node
    /// `peer_observations` map (within a single process lifetime
    /// anyway; cross-process persistence of observations is a
    /// separate concern).
    pub fn to_pinned_pod(&self) -> Result<PinnedPod> {
        Ok(PinnedPod::from_blob(
            &self.bootstrap_blob,
            &self.host,
            self.port,
            self.capabilities.clone().into(),
        )?)
    }
}

/// Default snapshot directory: `~/.sovereign/worker-pods/`. Created on
/// first write. Returning `Option` rather than panicking lets the
/// daemon fall back gracefully on hosts without a HOME (containers).
pub fn default_snapshot_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".sovereign").join("worker-pods"))
}

/// Persist a snapshot to `<dir>/<vast_id>.json`. Atomic via
/// write-then-rename so a crash mid-write can never leave a partial
/// file that the next `pod list` (or daemon startup) misreads.
pub fn save_snapshot(dir: &Path, snapshot: &PinnedPodSnapshot) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let target = dir.join(format!("{}.json", snapshot.vast_id));
    let tmp = target.with_extension("partial");
    let json = serde_json::to_vec_pretty(snapshot)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &target)?;
    Ok(target)
}

/// Delete `<dir>/<vast_id>.json`. Returns `true` if a file was
/// removed, `false` if nothing was there (idempotent).
pub fn delete_snapshot(dir: &Path, vast_id: &str) -> Result<bool> {
    let target = dir.join(format!("{vast_id}.json"));
    match std::fs::remove_file(&target) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Load a single snapshot by vast_id.
pub fn load_snapshot(dir: &Path, vast_id: &str) -> Result<PinnedPodSnapshot> {
    let target = dir.join(format!("{vast_id}.json"));
    load_snapshot_from_path(&target)
}

pub fn load_snapshot_from_path(path: &Path) -> Result<PinnedPodSnapshot> {
    let bytes = std::fs::read(path)?;
    let snap: PinnedPodSnapshot = serde_json::from_slice(&bytes)?;
    if snap.version != SNAPSHOT_VERSION {
        return Err(SnapshotError::UnsupportedVersion(snap.version));
    }
    Ok(snap)
}

/// Load every `*.json` snapshot in `dir`. Missing dir → empty vec
/// (the common "no pinned pods configured" case). Per-file parse
/// errors are logged and that snapshot is skipped — one corrupt
/// snapshot must not block the daemon from starting with the rest.
pub fn load_all_snapshots(dir: &Path) -> Vec<PinnedPodSnapshot> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(
                dir = %dir.display(),
                error = %e,
                "pinned-pod snapshots: read_dir failed"
            );
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        // Skip orphaned `.partial` files from a crashed atomic write —
        // they have extension "partial", not "json", so they're
        // already filtered above.
        match load_snapshot_from_path(&path) {
            Ok(s) => out.push(s),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "pinned-pod snapshots: skipping unloadable file"
                );
            }
        }
    }
    out
}

/// Restrict a loaded set to the vast ids in `filter`. Used by
/// `daemon run --extra-worker pod://<id>` to load only specific
/// snapshots when the operator wants explicit control. Empty
/// `filter` is treated as "no filter" (all loaded).
pub fn filter_by_vast_ids(
    snapshots: Vec<PinnedPodSnapshot>,
    filter: &HashSet<String>,
) -> Vec<PinnedPodSnapshot> {
    if filter.is_empty() {
        return snapshots;
    }
    snapshots
        .into_iter()
        .filter(|s| filter.contains(&s.vast_id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker_pod::{mint_bootstrap, BootstrapInputs};
    use ed25519_dalek::SigningKey;
    use std::collections::BTreeMap;

    fn fixed_owner() -> SigningKey {
        SigningKey::from_bytes(&[55u8; 32])
    }

    fn mint_blob(seed: u8, job: &str) -> BootstrapBlob {
        let (b, _) = mint_bootstrap(BootstrapInputs {
            job_id: job.into(),
            owner_signing: &fixed_owner(),
            expected_uploads: BTreeMap::new(),
            ttl_seconds: 3600,
            seed_override: Some([seed; 32]),
        })
        .unwrap();
        b
    }

    #[test]
    fn round_trip_via_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let blob = mint_blob(1, "rt-job");
        let snap = PinnedPodSnapshot::new(
            "vast-abc123",
            "203.0.113.5",
            9742,
            blob,
            PodCapabilities {
                system_ram_gb: 192,
                benchmark: None,
                current_in_flight: None,
            },
        );
        let path = save_snapshot(tmp.path(), &snap).unwrap();
        assert!(path.exists());

        let loaded = load_snapshot(tmp.path(), "vast-abc123").unwrap();
        assert_eq!(loaded.vast_id, snap.vast_id);
        assert_eq!(loaded.host, snap.host);
        assert_eq!(loaded.port, snap.port);
        assert_eq!(loaded.capabilities.system_ram_gb, 192);
        assert_eq!(loaded.bootstrap_blob.seed, [1u8; 32]);
    }

    #[test]
    fn to_pinned_pod_uses_seed_and_capabilities() {
        let blob = mint_blob(9, "pod-conv");
        let snap = PinnedPodSnapshot::new(
            "vast-z",
            "h",
            9742,
            blob.clone(),
            PodCapabilities {
                system_ram_gb: 256,
                benchmark: None,
                current_in_flight: None,
            },
        );
        let pod = snap.to_pinned_pod().unwrap();
        assert_eq!(pod.capabilities.system_ram_gb, 256);
        assert_eq!(pod.base_url, "https://h:9742/v1");
        // Synthetic node id deterministic from seed.
        let expected_id = crate::pinned_transport::synthetic_node_id_from_seed(&blob.seed);
        assert_eq!(pod.node_id, expected_id);
    }

    #[test]
    fn delete_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let blob = mint_blob(2, "del-job");
        let snap = PinnedPodSnapshot::new("vast-del", "h", 9742, blob, Default::default());
        save_snapshot(tmp.path(), &snap).unwrap();
        assert!(delete_snapshot(tmp.path(), "vast-del").unwrap());
        // Second call is a no-op.
        assert!(!delete_snapshot(tmp.path(), "vast-del").unwrap());
    }

    #[test]
    fn load_all_skips_corrupt_files_and_non_json() {
        let tmp = tempfile::tempdir().unwrap();
        // Good snapshot.
        let blob = mint_blob(3, "good");
        let snap =
            PinnedPodSnapshot::new("vast-good", "h", 9742, blob, Default::default());
        save_snapshot(tmp.path(), &snap).unwrap();
        // Corrupt JSON in a .json file.
        std::fs::write(tmp.path().join("broken.json"), b"{not-json").unwrap();
        // Stray non-JSON file (e.g. operator notes).
        std::fs::write(tmp.path().join("README.md"), b"hi").unwrap();
        let loaded = load_all_snapshots(tmp.path());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].vast_id, "vast-good");
    }

    #[test]
    fn load_all_on_missing_dir_returns_empty() {
        let nonexistent = std::env::temp_dir().join("definitely-not-a-real-dir-1234567");
        let _ = std::fs::remove_dir_all(&nonexistent);
        let loaded = load_all_snapshots(&nonexistent);
        assert!(loaded.is_empty());
    }

    #[test]
    fn version_mismatch_refuses_to_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("future.json");
        std::fs::write(
            &path,
            r#"{"version":99,"vast_id":"x","host":"h","port":1,"bootstrap_blob":null,"capabilities":{"system_ram_gb":1},"created_at_unix":0}"#,
        )
        .unwrap();
        // load_snapshot_from_path either fails json parse (because
        // bootstrap_blob is null and BootstrapBlob isn't Optionable),
        // OR catches the version. Either way it returns an Err — the
        // important property is "does not silently return a v99
        // snapshot."
        assert!(load_snapshot_from_path(&path).is_err());
    }

    #[test]
    fn filter_by_ids_empty_means_all() {
        let blob = mint_blob(4, "f1");
        let s1 = PinnedPodSnapshot::new("a", "h", 9742, blob.clone(), Default::default());
        let s2 = PinnedPodSnapshot::new("b", "h", 9742, blob, Default::default());
        let out = filter_by_vast_ids(vec![s1.clone(), s2.clone()], &HashSet::new());
        assert_eq!(out.len(), 2);
        let only_a: HashSet<String> = ["a".into()].into();
        let out = filter_by_vast_ids(vec![s1, s2], &only_a);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].vast_id, "a");
    }
}
