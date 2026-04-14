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

/// Atomically persist `mesh` + `self_node_id` to `<data_dir>/mesh.json`.
/// Write-to-tempfile-then-rename so a crash mid-write can't corrupt
/// the previous state.
pub fn save(
    data_dir: &Path,
    mesh: &Mesh,
    self_node_id: NodeId,
) -> std::io::Result<()> {
    fs::create_dir_all(data_dir)?;
    let target = mesh_file(data_dir);
    let tmp = target.with_extension("json.tmp");
    let payload = PersistedMesh::from_live(mesh, self_node_id);
    let bytes = serde_json::to_vec_pretty(&payload).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    })?;
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
            let parsed: PersistedMesh = serde_json::from_slice(&bytes).map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, e)
            })?;
            Ok(Some(parsed))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
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
    use commonwealth_core::capabilities::{
        AvailableResources, HardwareProfile, NodeCapabilities,
    };
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
    fn load_on_corrupt_file_returns_err() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path()).unwrap();
        fs::write(mesh_file(tmp.path()), b"not json").unwrap();
        assert!(load(tmp.path()).is_err());
    }
}
