//! On-disk persistence for a running mesh.
//!
//! Without this, `EmbeddedDaemon` is purely in-memory: you create a
//! mesh, close the app, and the daemon's state vanishes. That breaks
//! two user stories at once — the founder doesn't see their own mesh
//! after a restart, and any would-be joiner can't find them on the
//! LAN because nobody is advertising.
//!
//! The solution is a small JSON blob at
//! `<data_dir>/mesh.json` containing:
//!   - the full `Mesh` (members, join_key_hash, peers, mesh_id, name)
//!   - the founder/self `NodeId` so we can resume under the same
//!     identity rather than appearing as a new member
//!
//! `HashMap<NodeId, MemberRecord>` can't serde_json-encode directly
//! (NodeId is a byte array, not a string), so we flatten `members`
//! to a `Vec<MemberRecord>` on write and reassemble on read — same
//! trick as `commonwealth_api::routes_internal::MeshWire`.
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, MeshPeering};
use serde::{Deserialize, Serialize};

/// Filename at `<data_dir>/mesh.json`.
pub const MESH_FILE: &str = "mesh.json";

/// Filename at `<data_dir>/join_key.secret` — plaintext join key for
/// the currently-active mesh. Persisted so the active-mesh UI can
/// re-display the invite link after a daemon restart without forcing
/// a rotation. Written 0600 on Unix; mesh.json sits next to it but
/// only carries the salted hash so this is the only file the user
/// must keep secret. See `save_join_key` / `load_join_key`.
pub const JOIN_KEY_FILE: &str = "join_key.secret";

/// Filename at `<data_dir>/node_id` — 16 raw bytes, mode 0600.
///
/// This is the daemon's stable identity across mesh create/join
/// cycles. Generated exactly once on first boot, and never
/// regenerated. Without this, every `create_mesh` and every
/// `join_mesh` would call `NodeId::generate()` and stamp out a
/// fresh 16-byte random ID, causing:
///   - Zombie accumulation: each rejoin adds a new member to the
///     founder's mesh, old "us" entries never get GC'd.
///   - Failed self-identification: status / collaborate handlers
///     can't find a stable "me" record across restarts.
///   - Churning UI: the member's displayed identity changes every
///     time the daemon restarts.
pub const NODE_ID_FILE: &str = "node_id";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedMesh {
    pub self_node_id: NodeId,
    pub mesh_id: MeshId,
    pub name: String,
    pub join_key_hash: [u8; 32],
    pub members: Vec<MemberRecord>,
    pub peers: Vec<MeshPeering>,
}

impl PersistedMesh {
    pub fn from_live(mesh: &Mesh, self_node_id: NodeId) -> Self {
        Self {
            self_node_id,
            mesh_id: mesh.id,
            name: mesh.name.clone(),
            join_key_hash: mesh.join_key_hash,
            members: mesh.members.values().cloned().collect(),
            peers: mesh.peers.clone(),
        }
    }

    pub fn into_live(self) -> (Mesh, NodeId) {
        use std::collections::HashMap;
        let members = self
            .members
            .into_iter()
            .map(|m| (m.node_id, m))
            .collect::<HashMap<_, _>>();
        let mesh = Mesh {
            id: self.mesh_id,
            name: self.name,
            join_key_hash: self.join_key_hash,
            members,
            peers: self.peers,
        };
        (mesh, self.self_node_id)
    }
}

pub fn mesh_file(data_dir: &Path) -> PathBuf {
    data_dir.join(MESH_FILE)
}

pub fn join_key_file(data_dir: &Path) -> PathBuf {
    data_dir.join(JOIN_KEY_FILE)
}

pub fn node_id_file(data_dir: &Path) -> PathBuf {
    data_dir.join(NODE_ID_FILE)
}

/// Load this daemon's stable `NodeId` from `<data_dir>/node_id`.
/// On first boot (file missing), generate a fresh ID and persist it
/// atomically before returning.
///
/// Once this has returned a given NodeId for a given data_dir, every
/// future call returns the same value — the identity survives
/// `sovereign mesh leave` (we leave `node_id` in place on leave so
/// the user re-joins with their familiar identity), crashes,
/// reinstalls that preserve `~/.sovereign`, etc. The only way to
/// churn identity is for the user to manually `rm ~/.sovereign/node_id`.
///
/// Errors: any filesystem/serialization failure bubbles up as an
/// `io::Error`. Callers currently log-and-continue by falling back
/// to `NodeId::generate()` for the in-memory value, trading identity
/// stability for availability — see [`load_or_generate_self_node_id`]
/// for the convenience wrapper that does this.
pub fn load_node_id(data_dir: &Path) -> std::io::Result<Option<NodeId>> {
    let path = node_id_file(data_dir);
    match fs::read(&path) {
        Ok(bytes) => {
            if bytes.len() != 16 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "node_id file at {} is {} bytes, expected 16",
                        path.display(),
                        bytes.len()
                    ),
                ));
            }
            let arr: [u8; 16] = bytes.try_into().unwrap();
            // NodeId is defined via macro in commonwealth-core with
            // `[u8; 16]` as its single field. We can't construct it
            // directly from outside that crate — go through the
            // serde path using a tiny JSON shim.
            let id: NodeId = serde_json::from_value(serde_json::json!(arr))
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            Ok(Some(id))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Persist this daemon's stable `NodeId`. Idempotent — calling
/// twice with the same ID is a no-op from the caller's perspective,
/// but still rewrites the file (tmp-then-rename, so atomic).
fn save_node_id(data_dir: &Path, id: &NodeId) -> std::io::Result<()> {
    fs::create_dir_all(data_dir)?;
    let target = node_id_file(data_dir);
    let tmp = target.with_extension("id.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(id.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Load-or-generate wrapper with graceful fallback. First boot
/// writes the file; subsequent boots return the persisted ID.
/// On I/O error writing the generated ID, returns the fresh ID
/// anyway and logs — the daemon is still usable, just loses
/// identity stability until the file can be written.
pub fn load_or_generate_self_node_id(data_dir: &Path) -> NodeId {
    match load_node_id(data_dir) {
        Ok(Some(id)) => id,
        Ok(None) => {
            let fresh = NodeId::generate();
            if let Err(e) = save_node_id(data_dir, &fresh) {
                tracing::warn!(
                    error = %e,
                    data_dir = %data_dir.display(),
                    "node_id persistence failed — daemon will run with a fresh \
                     ID this session; rejoins will appear as a new peer to \
                     the founder"
                );
            } else {
                tracing::info!(
                    node_id = %fresh,
                    "node_id: generated + persisted stable identity (first boot)"
                );
            }
            fresh
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "node_id: failed to load persisted ID — using fresh this session"
            );
            NodeId::generate()
        }
    }
}

/// Persist the plaintext `join_key` for the active mesh. Atomic
/// (write-tmp-then-rename) and 0600 on Unix so other local users
/// can't read it.
///
/// Why store the plaintext at all: `Mesh.join_key_hash` is one-way,
/// so once the in-memory copy is dropped (daemon restart, app crash),
/// nobody can reconstruct the link. Either we ask the user to rotate
/// — which churns the link they already shared — or we cache the
/// plaintext alongside `mesh.json`. We chose the cache. Sensitivity
/// is the same as what `sovereign mesh create` already prints to the
/// terminal; loopback-only HTTP keeps remote machines from reading
/// it back.
pub fn save_join_key(data_dir: &Path, join_key: &str) -> std::io::Result<()> {
    fs::create_dir_all(data_dir)?;
    let target = join_key_file(data_dir);
    let tmp = target.with_extension("secret.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(join_key.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Ignore — best-effort. A failure here doesn't invalidate the
        // write; the file's still present, just possibly group-readable.
        let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Read the cached plaintext join key. `Ok(None)` when the file
/// doesn't exist (clean install, or pre-`save_join_key` daemon).
pub fn load_join_key(data_dir: &Path) -> std::io::Result<Option<String>> {
    match fs::read_to_string(join_key_file(data_dir)) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Delete the cached plaintext join key. Called on `leave_mesh`
/// alongside `clear`. Idempotent.
pub fn clear_join_key(data_dir: &Path) -> std::io::Result<()> {
    match fs::remove_file(join_key_file(data_dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Atomically persist `mesh` + `self_node_id` to `<data_dir>/mesh.json`.
/// Write-to-tempfile-then-rename so a crash mid-write can't corrupt
/// the previous state.
pub fn save(data_dir: &Path, mesh: &Mesh, self_node_id: NodeId) -> std::io::Result<()> {
    fs::create_dir_all(data_dir)?;
    let target = mesh_file(data_dir);
    let tmp = target.with_extension("json.tmp");
    let payload = PersistedMesh::from_live(mesh, self_node_id);
    let bytes = serde_json::to_vec_pretty(&payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &target)?;
    Ok(())
}

/// Read `<data_dir>/mesh.json`. Returns `Ok(None)` if the file
/// doesn't exist (clean first run); `Ok(Some(..))` on a good read;
/// `Err` if the file exists but can't be parsed — callers typically
/// log and proceed as if no mesh.
pub fn load(data_dir: &Path) -> std::io::Result<Option<PersistedMesh>> {
    let target = mesh_file(data_dir);
    match fs::read(&target) {
        Ok(bytes) => {
            let parsed: PersistedMesh = serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            Ok(Some(parsed))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Result of [`rotate_join_key`] — carries the plaintext of the
/// freshly-generated key (shown to the user once; not re-recoverable)
/// plus the mesh name for display in the banner.
#[derive(Debug, Clone)]
pub struct RotatedKey {
    pub mesh_name: String,
    pub join_key: String,
}

/// Generate a fresh plaintext join key, overwrite the persisted
/// `mesh.join_key_hash`, and return the plaintext to the caller.
///
/// Existing members (stored as `MemberRecord`s with their own node
/// ids) remain connected — rotation only affects *future* joins.
/// A running daemon holds its own in-memory copy of the old hash and
/// will not pick up the new one until restart; the CLI tells the user
/// to restart in that case.
///
/// Returns `Ok(None)` if no mesh is persisted. Returns `Err` on I/O
/// or serialization failure.
pub fn rotate_join_key(data_dir: &Path) -> std::io::Result<Option<RotatedKey>> {
    let Some(persisted) = load(data_dir)? else {
        return Ok(None);
    };
    let new_key = commonwealth_discovery::membership::generate_join_key();
    let new_hash = commonwealth_discovery::membership::hash_join_key(&new_key);
    let (mut mesh, self_node_id) = persisted.into_live();
    mesh.join_key_hash = new_hash;
    let mesh_name = mesh.name.clone();
    save(data_dir, &mesh, self_node_id)?;
    // Refresh the cached plaintext too so the share UI shows the new
    // link immediately on next status poll, not after a restart.
    save_join_key(data_dir, &new_key)?;
    Ok(Some(RotatedKey {
        mesh_name,
        join_key: new_key,
    }))
}

/// Remove the persisted mesh file. Called on `leave_mesh`.
/// Returns Ok even if the file doesn't exist — the post-condition
/// ("no persisted mesh") holds either way.
pub fn clear(data_dir: &Path) -> std::io::Result<()> {
    let target = mesh_file(data_dir);
    match fs::remove_file(&target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
    use commonwealth_core::mesh::NodeStatus;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn sample_mesh() -> (Mesh, NodeId) {
        let node_id = NodeId::generate();
        let member = MemberRecord {
            node_id,
            name: "Alice".into(),
            invited_by: node_id,
            joined_at: 100,
            last_seen: 100,
            status: NodeStatus::Online,
            capabilities: NodeCapabilities {
                hardware: HardwareProfile {
                    gpus: vec![],
                    system_ram_gb: 0,
                    cpu_cores: 0,
                    total_storage_gb: 0,
                    free_storage_gb: 0,
                    network_bandwidth_mbps: None,
                },
                available: AvailableResources::default(),
                active_processes: vec![],
                hosted_corpora: vec![],
                reported_at: 100,
                inference_availability: 1.0,
                inference_capable: false,
                loaded_models: vec![],

                embed_model: None,
                benchmark: None,
                current_in_flight: None,
            },
            addresses: vec!["192.168.1.10:9742".parse().unwrap()],
        };
        let mut members = HashMap::new();
        members.insert(node_id, member);
        let mesh = Mesh {
            id: MeshId::generate(),
            name: "Persisted Mesh".into(),
            join_key_hash: [42u8; 32],
            members,
            peers: vec![],
        };
        (mesh, node_id)
    }

    #[test]
    fn save_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let (mesh, node_id) = sample_mesh();
        save(tmp.path(), &mesh, node_id).unwrap();

        let loaded = load(tmp.path()).unwrap().expect("file should exist");
        assert_eq!(loaded.self_node_id, node_id);
        assert_eq!(loaded.name, "Persisted Mesh");
        assert_eq!(loaded.members.len(), 1);
        assert_eq!(loaded.join_key_hash, [42u8; 32]);

        let (restored, restored_node) = loaded.into_live();
        assert_eq!(restored.id, mesh.id);
        assert_eq!(restored.members.len(), 1);
        assert_eq!(restored_node, node_id);
    }

    #[test]
    fn load_returns_none_when_missing() {
        let tmp = TempDir::new().unwrap();
        assert!(load(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn clear_removes_file_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let (mesh, node_id) = sample_mesh();
        save(tmp.path(), &mesh, node_id).unwrap();
        assert!(mesh_file(tmp.path()).exists());

        clear(tmp.path()).unwrap();
        assert!(!mesh_file(tmp.path()).exists());

        // Second call on a missing file is a no-op, not an error.
        clear(tmp.path()).unwrap();
    }

    #[test]
    fn rotate_join_key_updates_hash_and_returns_new_plaintext() {
        let tmp = TempDir::new().unwrap();
        let (mesh, node_id) = sample_mesh();
        let original_hash = mesh.join_key_hash;
        save(tmp.path(), &mesh, node_id).unwrap();

        let rotated = rotate_join_key(tmp.path())
            .unwrap()
            .expect("mesh exists, rotation should return Some");
        assert_eq!(rotated.mesh_name, "Persisted Mesh");
        assert!(rotated.join_key.starts_with("cwth-"));

        // Persisted file now reflects the new hash.
        let reloaded = load(tmp.path()).unwrap().unwrap();
        assert_ne!(reloaded.join_key_hash, original_hash, "hash must change");
        // The returned plaintext verifies against the new hash.
        let expected = commonwealth_discovery::membership::hash_join_key(&rotated.join_key);
        assert_eq!(reloaded.join_key_hash, expected);
        // Members + node id survive rotation.
        assert_eq!(reloaded.self_node_id, node_id);
        assert_eq!(reloaded.members.len(), 1);
    }

    #[test]
    fn rotate_join_key_on_missing_mesh_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert!(rotate_join_key(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn join_key_save_load_roundtrips() {
        let tmp = TempDir::new().unwrap();
        assert!(load_join_key(tmp.path()).unwrap().is_none());
        save_join_key(tmp.path(), "cwth-1111-2222-3333").unwrap();
        let read_back = load_join_key(tmp.path()).unwrap();
        assert_eq!(read_back.as_deref(), Some("cwth-1111-2222-3333"));
    }

    #[test]
    fn join_key_clear_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        save_join_key(tmp.path(), "cwth-1111-2222-3333").unwrap();
        clear_join_key(tmp.path()).unwrap();
        clear_join_key(tmp.path()).unwrap(); // no-op second time
        assert!(load_join_key(tmp.path()).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn join_key_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        save_join_key(tmp.path(), "cwth-1111-2222-3333").unwrap();
        let mode = fs::metadata(join_key_file(tmp.path()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "join_key.secret must be 0600 on Unix");
    }

    #[test]
    fn rotate_join_key_persists_plaintext() {
        let tmp = TempDir::new().unwrap();
        let (mesh, node_id) = sample_mesh();
        save(tmp.path(), &mesh, node_id).unwrap();
        let rotated = rotate_join_key(tmp.path()).unwrap().unwrap();
        let cached = load_join_key(tmp.path()).unwrap();
        assert_eq!(cached.as_deref(), Some(rotated.join_key.as_str()));
    }

    #[test]
    fn load_on_corrupt_file_returns_err() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path()).unwrap();
        fs::write(mesh_file(tmp.path()), b"not json").unwrap();
        assert!(load(tmp.path()).is_err());
    }
}
