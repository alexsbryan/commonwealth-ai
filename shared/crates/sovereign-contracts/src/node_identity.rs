// SPDX-License-Identifier: AGPL-3.0-or-later
//! The node-identity FILES — one decider for "which node is this workstation".
//!
//! [`resolve_self_node_id`] and its two entry points read two files under the
//! data dir: the 16-byte `node_id` file and `mesh.json`'s identity fields.
//! Both are CROSS-PROGRAM contracts, not mesh internals: the daemon writes
//! them, and every surface that stamps records other processes read — daemon
//! bootstrap, `svrn` CLI work-atlas stores, portfolio commands — resolves
//! them. One file format with many readers is what `sovereign-contracts` is
//! for, and one precedence decider is what stopped the 2026-07-31 incident
//! (three call sites with three answers, two of them minting a second
//! identity for one workstation) from recurring.
//!
//! Moved here from `sovereign-mesh::persist` by five-programs fp-33 so the
//! workbench binary stops linking the mesh substrate to learn which node it
//! is. `sovereign_mesh::persist` re-exports every public item at its
//! historical path (ARCH §10.6 — a re-export, never a twin), so the daemon
//! and cli-llm callers are untouched.
//!
//! # The mesh.json read is a projection, and the pin lives with the writer
//!
//! This module parses ONLY `self_node_id` and `members[].node_id` out of
//! `mesh.json` — serde ignores every other field. It deliberately does not
//! duplicate the full [`sovereign_mesh::persist::PersistedMesh`] schema (that
//! parser owns the credential migration and stays the only one). The drift
//! guard is structural: the resolver's tests in `sovereign-mesh` write real
//! `mesh.json` files through `PersistedMesh::from_live` and assert the
//! resolution against them, so a rename in the writer's schema breaks those
//! tests instead of silently rotating identities.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use kernel_types::NodeId;
use serde::Deserialize;

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

/// Filename at `<data_dir>/mesh.json` — the mesh's persisted document.
/// ONE definition here beside its identity reader;
/// `sovereign_mesh::persist` re-exports it under its historical name
/// `MESH_FILE` (ARCH §10.6), so the writer and the reader cannot
/// drift apart on which file identity comes from.
pub const MESH_JSON_FILE: &str = "mesh.json";

/// Directory holding one subdirectory per mesh this node has joined.
/// ONE definition here beside the identity reader, which must follow the
/// same layout the writer uses; `sovereign_mesh::persist` re-exports it
/// (ARCH §10.6).
pub const MESHES_DIR: &str = "meshes";

/// Pointer file naming the ACTIVE mesh (hex `MeshId`). Absent = no mesh, or
/// a legacy layout not yet migrated. ONE definition here, same reason as
/// [`MESHES_DIR`].
pub const ACTIVE_FILE: &str = "active";

/// `<data_dir>/active` — the pointer file's path.
pub fn active_pointer(root: &Path) -> PathBuf {
    root.join(ACTIVE_FILE)
}

/// The active mesh's directory name, exactly as the pointer file holds it
/// (whitespace-trimmed, lowercased to match `set_active`'s writer).
/// `None` on a clean install, or on a legacy layout not yet migrated. The
/// typed form (`MeshId`) is `sovereign_mesh::persist::active_mesh_id`,
/// built on this read — the identity reader may not name `MeshId`, which
/// is commonwealth-core's.
pub fn active_mesh_hex(root: &Path) -> Option<String> {
    let raw = fs::read_to_string(active_pointer(root)).ok()?;
    Some(raw.trim().to_lowercase()).filter(|h| !h.is_empty())
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
/// reinstalls that preserve `~/.svrnmesh`, etc. The only way to
/// churn identity is for the user to manually `rm ~/.svrnmesh/node_id`.
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
            // NodeId is defined via macro in kernel-types with
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
///
/// Public because the identity FILE is this module's contract and
/// this is its write half: the repair in [`resolve_self_node_id`] and
/// the first-boot write in [`load_or_generate_self_node_id`] are the
/// in-crate callers, and `sovereign-mesh`'s precedence tests pin the
/// write/read round-trip.
pub fn save_node_id(data_dir: &Path, id: &NodeId) -> std::io::Result<()> {
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

/// Resolve this node's persistent id with the FULL precedence the
/// daemon applies on resume: the `node_id` file, then the id baked
/// into `mesh.json`, then generate-and-persist. Every surface that
/// stamps records other processes will read (work-atlas claims, mesh
/// measurements) MUST use this, against the ROOT data dir — calling
/// [`load_or_generate_self_node_id`] against some other directory mints
/// a second identity for the same workstation. That exact bug shipped:
/// the CLI derived its atlas identity from `<root>/indexes`, so one
/// machine ran as two nodes and self-filtering misfired (2026-07-31).
pub fn resolve_self_node_id(data_dir: &Path) -> NodeId {
    let from_file = load_node_id(data_dir).ok().flatten();
    let persisted = read_mesh_identity(data_dir);

    // ── The file may hold ANOTHER MACHINE's id ──────────────────────────────
    // File-first precedence assumes the `node_id` file is either correct or
    // absent. There is a third state: it holds an id belonging to a DIFFERENT
    // member of this mesh — a data dir copied between workstations, a restored
    // backup, a bind-mount pointed at the wrong host. That is not a stale
    // self-id, it is a collision, and adopting it makes two nodes claim one
    // identity: self-filtering inverts (your own edits read as a peer's),
    // attribution lands on the wrong machine, and peers coordinating around
    // the atlas steer around a node that is not the one editing.
    //
    // Observed 2026-08-20 on this workstation: `<data_dir>/node_id` had held a
    // peer's id since April while `mesh.json` and `/status` both reported the
    // real one, so every locally-observed work-atlas row was stamped with the
    // peer's id and the pre-commit collision guard warned on its own author's
    // edits. Same family as the 2026-07-31 incident above; that one minted a
    // second identity from the wrong directory, this one adopts a real peer's.
    //
    // `mesh.json` wins here and only here. It is the identity the mesh agreed
    // on and the one the daemon presents, and the tie-break is not a guess: an
    // id that names a known peer cannot also be us. Outside this case the file
    // keeps precedence, so a fresh join cannot rotate a stable identity.
    if let (Some(file_id), Some(mesh)) = (from_file, persisted.as_ref()) {
        if file_id != mesh.self_node_id && mesh.members.iter().any(|m| *m == file_id) {
            tracing::error!(
                node_id_file = %file_id,
                mesh_self = %mesh.self_node_id,
                data_dir = %data_dir.display(),
                "node_id: the node_id file holds a PEER's identity — adopting mesh.json's \
                 self_node_id and repairing the file. Two nodes sharing one id breaks \
                 self-filtering and misattributes this machine's work to that peer."
            );
            if let Err(e) = save_node_id(data_dir, &mesh.self_node_id) {
                // Non-fatal: the returned id is already correct for this
                // process. Unrepaired, the warning simply fires again next boot.
                tracing::warn!(
                    error = %e,
                    "node_id: could not repair the node_id file; identity is correct \
                     for this process but the divergence will recur on restart"
                );
            }
            return mesh.self_node_id;
        }
    }

    match from_file {
        Some(id) => id,
        None => match persisted {
            Some(mesh) => mesh.self_node_id,
            None => load_or_generate_self_node_id(data_dir),
        },
    }
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

/// The identity fields this module reads out of `mesh.json`. Everything
/// else in the document — secrets, invite state, peerings — belongs to
/// `sovereign_mesh::persist` and is ignored here (see the module doc for
/// what pins this projection to the writer's schema).
#[derive(Deserialize)]
struct MeshFileIdentity {
    self_node_id: NodeId,
    #[serde(default)]
    members: Vec<MeshFileMember>,
}

#[derive(Deserialize)]
struct MeshFileMember {
    node_id: NodeId,
}

/// `<data_dir>/node_key` — the daemon's own node key, which a data dir from
/// before cw-rails held the node's identity carries. `svrn mesh up`'s
/// identity handover (sovereign-cli-mesh `identity_handover`) copies it to
/// cw-rails and renames it away, so its presence is the handover's own
/// marker (pb-distribution-f8).
pub const PRE_HANDOVER_KEY_FILE: &str = "node_key";

/// Has `svrn mesh up` still to hand this data dir's identity to cw-rails?
/// The one decider: the handover's first check and every surface that tells
/// the operator so call it.
pub fn mesh_handover_pending(data_dir: &Path) -> bool {
    data_dir.join(PRE_HANDOVER_KEY_FILE).exists()
}

/// What the operator is told while the handover waits; `None` once it ran,
/// and on a data dir that never held a key. `work_offer` is whether svrn's
/// config still declares `[compute.work_offer]`, which no donor reads now.
pub fn mesh_handover_notice(data_dir: &Path, work_offer: bool) -> Option<String> {
    let pending = mesh_handover_pending(data_dir);
    tracing::debug!(data_dir = %data_dir.display(), pending, work_offer, "mesh handover: pending?");
    if !pending {
        return None;
    }
    let mut notice = format!(
        "{} is this node's key from before cw-rails: until `svrn mesh up` hands it over, \
         this node is on none of its meshes",
        data_dir.join(PRE_HANDOVER_KEY_FILE).display()
    );
    if work_offer {
        notice.push_str(
            ", and `[compute.work_offer]` offers nothing: the donor runs in cw-rails and \
             reads `[work_offer]` from rails.toml, which `svrn mesh up` writes",
        );
    }
    Some(notice)
}

/// The projection the resolver consumes: the mesh's own id plus every
/// member id (the collision tie-break's membership test).
struct MeshIdentity {
    self_node_id: NodeId,
    members: Vec<NodeId>,
}

/// The path the identity reader reads `mesh.json` from — the ACTIVE mesh's
/// document under the multi-mesh layout, the data-dir root on a legacy
/// layout. Mirrors `sovereign_mesh::persist::mesh_file` exactly; the layout
/// constants above are the shared definitions that keep the two from
/// drifting.
fn identity_mesh_json(root: &Path) -> PathBuf {
    match active_mesh_hex(root) {
        Some(hex) => root.join(MESHES_DIR).join(hex).join(MESH_JSON_FILE),
        None => root.join(MESH_JSON_FILE),
    }
}

/// Read the mesh document down to its identity fields. `None` when
/// the file is absent (clean first run) or unreadable — the same
/// log-and-proceed semantics `sovereign_mesh::persist::load`'s callers
/// apply, because an unparseable document must degrade to "no mesh"
/// rather than to a fresh identity.
fn read_mesh_identity(data_dir: &Path) -> Option<MeshIdentity> {
    let bytes = fs::read(identity_mesh_json(data_dir)).ok()?;
    let parsed: MeshFileIdentity = serde_json::from_slice(&bytes).ok()?;
    Some(MeshIdentity {
        self_node_id: parsed.self_node_id,
        members: parsed.members.into_iter().map(|m| m.node_id).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire form the projection must read: exactly the fields
    /// `PersistedMesh` writes for the identity pair, surrounded by the
    /// fields it writes for everything else this module must ignore.
    const SAMPLE_MESH_JSON: &str = r#"{
        "self_node_id": [1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16],
        "mesh_id": [17,17,17,17,17,17,17,17,17,17,17,17,17,17,17,17],
        "name": "sample",
        "mesh_secret": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
        "join_key_hash": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
        "members": [
            {"node_id": [1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16], "name": "self", "joined_at": 1},
            {"node_id": [16,15,14,13,12,11,10,9,8,7,6,5,4,3,2,1], "name": "peer", "joined_at": 2}
        ],
        "peers": []
    }"#;

    #[test]
    fn the_projection_reads_the_identity_fields_and_ignores_the_rest() {
        let parsed: MeshFileIdentity = serde_json::from_str(SAMPLE_MESH_JSON).unwrap();
        let self_id = NodeId::from_hex("0102030405060708090a0b0c0d0e0f10").unwrap();
        let peer_id = NodeId::from_hex("100f0e0d0c0b0a090807060504030201").unwrap();
        assert_eq!(parsed.self_node_id, self_id);
        let members: Vec<NodeId> = parsed.members.into_iter().map(|m| m.node_id).collect();
        assert_eq!(members, vec![self_id, peer_id]);
    }

    /// A document that is not a mesh.json (or a future schema that renamed
    /// the identity fields) degrades to "no mesh" — never to a generated
    /// identity, which is the resolver's job below the `None`.
    #[test]
    fn an_unreadable_document_is_no_mesh() {
        let parsed: Result<MeshFileIdentity, _> = serde_json::from_str("not json");
        assert!(parsed.is_err());
    }
}
