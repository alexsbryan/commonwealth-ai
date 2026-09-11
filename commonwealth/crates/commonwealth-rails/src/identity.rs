// SPDX-License-Identifier: AGPL-3.0-or-later
//! What this daemon persists: a node key, a node id, and a mesh.
//!
//! All three files sit in the data dir under the SAME names and the same
//! formats the inference daemon uses — `node_key` (32 raw seed bytes,
//! `commonwealth_transport::identity`), `node_id` (16 RAW bytes, the format
//! `sovereign-mesh/src/persist.rs` reads), and `mesh.json`. That is not
//! politeness: a person who runs both on one machine, or who points a tool at
//! either data dir, should not discover that two processes in the same project
//! disagree about what a node id file is.
//!
//! # `mesh.json` is a [`MeshWire`], not a [`Mesh`], and that is forced
//!
//! WATCHED FAILING before it was written this way: `serde_json::to_vec(&mesh)`
//! is `Err("key must be a string")`, because `Mesh::members` is a
//! `HashMap<NodeId, MemberRecord>` and `NodeId` serializes as a byte array,
//! which JSON cannot use as an object key. That is the single reason
//! `MeshWire` exists, and this daemon already links it for the wire — so the
//! projection is reused rather than a second one minted for disk (ARCH §10.6,
//! §19). [`SecretDisclosure::Disclose`], because disk is where the real
//! `mesh_secret` has to live: a redacted one on restart is a node that can no
//! longer prove membership to anybody.
//!
//! `sovereign-mesh`'s `PersistedMesh` is the OTHER reader of a file by this
//! name and is deliberately not shared — it carries `self_node_id`, spells its
//! id key `mesh_id`, and a disk format and a wire format answer to different
//! compatibility clocks. The two files are not interchangeable, and nothing
//! here claims they are; what is shared is the projection, not the record.
//!
//! The signing itself is not here. `load_or_generate_node_key`,
//! `node_pubkey`, `sign_join_proof` and `sign_dial_info` are
//! `commonwealth_transport::identity`'s, and this module only decides WHERE
//! they are applied.

use std::path::{Path, PathBuf};

use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::{Mesh, MeshWire, SecretDisclosure};

pub const NODE_ID_FILE: &str = "node_id";
pub const MESH_FILE: &str = "mesh.json";

#[derive(Debug, thiserror::Error)]
pub enum StoreRefusal {
    #[error("the data dir {0} could not be created: {1}")]
    DataDir(PathBuf, std::io::Error),
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0} is {1} bytes, expected 16 — this is not a node_id file")]
    BadNodeId(PathBuf, usize),
    #[error("{0} is not a mesh: {1}")]
    BadMesh(PathBuf, serde_json::Error),
}

pub fn node_id_file(data_dir: &Path) -> PathBuf {
    data_dir.join(NODE_ID_FILE)
}

pub fn mesh_file(data_dir: &Path) -> PathBuf {
    data_dir.join(MESH_FILE)
}

/// This node's stable id, minted and persisted on first call and identical on
/// every call after. A rejoin under the same data dir is the same member, not
/// a second row on somebody's roster.
pub fn load_or_generate_node_id(data_dir: &Path) -> Result<NodeId, StoreRefusal> {
    if let Some(id) = load_node_id(data_dir)? {
        return Ok(id);
    }
    let id = NodeId::generate();
    save_node_id(data_dir, &id)?;
    tracing::info!(
        target: "rails",
        node_id = %id,
        path = %node_id_file(data_dir).display(),
        "identity: minted a node id"
    );
    Ok(id)
}

pub fn load_node_id(data_dir: &Path) -> Result<Option<NodeId>, StoreRefusal> {
    let path = node_id_file(data_dir);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(StoreRefusal::Io(path, e)),
    };
    let arr: [u8; 16] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| StoreRefusal::BadNodeId(path.clone(), bytes.len()))?;
    // `NodeId` is macro-defined in commonwealth-core with `[u8; 16]` as its
    // single field and no constructor from raw bytes, so the serde path is
    // the only door from outside that crate. `sovereign-mesh/src/persist.rs`
    // reaches the same file the same way — one FORMAT with two readers, not
    // two formats.
    let id: NodeId = serde_json::from_value(serde_json::json!(arr))
        .map_err(|e| StoreRefusal::BadMesh(path, e))?;
    Ok(Some(id))
}

pub fn save_node_id(data_dir: &Path, id: &NodeId) -> Result<(), StoreRefusal> {
    ensure_dir(data_dir)?;
    let target = node_id_file(data_dir);
    write_atomic(&target, id.as_bytes())
}

/// The persisted mesh, or `None` when this node has not joined one.
pub fn load_mesh(data_dir: &Path) -> Result<Option<Mesh>, StoreRefusal> {
    let path = mesh_file(data_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(StoreRefusal::Io(path, e)),
    };
    let wire: MeshWire =
        serde_json::from_str(&text).map_err(|e| StoreRefusal::BadMesh(path.clone(), e))?;
    let mesh = wire.into_mesh();
    tracing::debug!(
        target: "rails",
        mesh = %mesh.name,
        members = mesh.members.len(),
        path = %path.display(),
        "identity: mesh loaded"
    );
    Ok(Some(mesh))
}

pub fn save_mesh(data_dir: &Path, mesh: &Mesh) -> Result<(), StoreRefusal> {
    ensure_dir(data_dir)?;
    let target = mesh_file(data_dir);
    let text = serde_json::to_vec_pretty(&MeshWire::for_peer(mesh, SecretDisclosure::Disclose))
        .map_err(|e| StoreRefusal::BadMesh(target.clone(), e))?;
    write_atomic(&target, &text)
}

fn ensure_dir(data_dir: &Path) -> Result<(), StoreRefusal> {
    std::fs::create_dir_all(data_dir).map_err(|e| StoreRefusal::DataDir(data_dir.to_path_buf(), e))
}

/// tmp-then-rename, 0600. A half-written `mesh.json` is a daemon that boots
/// into a roster nobody has.
fn write_atomic(target: &Path, bytes: &[u8]) -> Result<(), StoreRefusal> {
    use std::io::Write;
    let tmp = target.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| StoreRefusal::Io(tmp.clone(), e))?;
        f.write_all(bytes)
            .map_err(|e| StoreRefusal::Io(tmp.clone(), e))?;
        f.sync_all().map_err(|e| StoreRefusal::Io(tmp.clone(), e))?;
    }
    std::fs::rename(&tmp, target).map_err(|e| StoreRefusal::Io(target.to_path_buf(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The identity survives a restart, which is what makes a rejoin the same
    /// member rather than a second row on somebody's roster.
    #[test]
    fn a_node_id_is_minted_once_and_read_back_forever() {
        let d = tempfile::tempdir().unwrap();
        let first = load_or_generate_node_id(d.path()).unwrap();
        let again = load_or_generate_node_id(d.path()).unwrap();
        assert_eq!(first, again);
        assert_eq!(
            std::fs::read(node_id_file(d.path())).unwrap().len(),
            16,
            "the file is 16 RAW bytes — the format sovereign-mesh reads"
        );
    }

    /// **The failing input.** A truncated or foreign file is refused by name.
    /// Before this check the 16-byte assumption was a `try_into().unwrap()`
    /// away from a panic on somebody's stray file.
    #[test]
    fn a_node_id_file_of_the_wrong_length_is_refused_by_name() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(node_id_file(d.path()), b"short").unwrap();
        let err = load_node_id(d.path()).unwrap_err();
        assert!(matches!(err, StoreRefusal::BadNodeId(_, 5)), "{err}");
    }

    #[test]
    fn an_absent_mesh_is_none_and_a_saved_one_round_trips() {
        let d = tempfile::tempdir().unwrap();
        assert!(load_mesh(d.path()).unwrap().is_none());
        let (mesh, _key) =
            commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
        save_mesh(d.path(), &mesh).unwrap();
        let back = load_mesh(d.path()).unwrap().expect("a mesh");
        assert_eq!(back.id, mesh.id);
        assert_eq!(back.name, "Lab");
        assert_eq!(back.members.len(), 1);
        // The credential survives the round trip. A redacted one on disk is a
        // node that boots unable to prove membership to anybody, and it would
        // read here as a perfectly good mesh.
        assert_eq!(back.mesh_secret, mesh.mesh_secret);
        assert_ne!(back.mesh_secret, [0u8; 32]);
        assert_eq!(back.invite_key_hash, mesh.invite_key_hash);
    }

    /// A `mesh.json` that is not a mesh is a refusal, never an empty roster:
    /// booting with no members and gossiping that to peers would erase the
    /// operator's own record of who is in the mesh.
    #[test]
    fn a_corrupt_mesh_file_is_refused_rather_than_read_as_empty() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(mesh_file(d.path()), "{\"id\":").unwrap();
        assert!(matches!(
            load_mesh(d.path()).unwrap_err(),
            StoreRefusal::BadMesh(_, _)
        ));
    }
}
