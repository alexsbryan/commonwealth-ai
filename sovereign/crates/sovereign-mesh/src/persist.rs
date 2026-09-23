// SPDX-License-Identifier: AGPL-3.0-or-later
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
//!   - the full `Mesh` (members, invite_key_hash, peers, mesh_id, name)
//!   - the founder/self `NodeId` so we can resume under the same
//!     identity rather than appearing as a new member
//!
//! `HashMap<NodeId, MemberRecord>` can't serde_json-encode directly
//! (NodeId is a byte array, not a string), so we flatten `members`
//! to a `Vec<MemberRecord>` on write and reassemble on read — same
//! trick as `commonwealth_core::mesh::MeshWire`.
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, MeshPeering};
use serde::{Deserialize, Serialize};

/// Filename at `<data_dir>/mesh.json`. ONE definition lives beside its
/// identity reader (`sovereign_contracts::node_identity::MESH_JSON_FILE`,
/// five-programs fp-33) so the writer and the reader cannot drift on which
/// file identity comes from; re-exported here at the historical name
/// (ARCH §10.6).
pub use sovereign_contracts::node_identity::MESH_JSON_FILE as MESH_FILE;

/// Filename at `<data_dir>/join_key.secret` — plaintext join key for
/// the currently-active mesh. Persisted so the active-mesh UI can
/// re-display the invite link after a daemon restart without forcing
/// a rotation. Written 0600 on Unix; mesh.json sits next to it but
/// only carries the salted hash so this is the only file the user
/// must keep secret. See `save_join_key` / `load_join_key`.
pub const JOIN_KEY_FILE: &str = "join_key.secret";

/// Marker file at `<data_dir>/client-exposed`. Its presence means the
/// operator opted this daemon into serving REMOTE callers (an explicit
/// `mesh create` / `mesh join` — never the silent solo-mesh auto-create
/// at first boot). `start_daemon` reads it to bump the client-API bind
/// from loopback to `0.0.0.0` (and thus require a bearer token). A
/// separate persisted signal — NOT mesh.json presence, since every
/// daemon has a solo mesh — so "is a mesh" and "is shared" stay
/// distinct. Removed on `leave_mesh` to re-secure. See
/// `set_client_exposed` / `client_exposed` / `clear_client_exposed`.
pub const CLIENT_EXPOSED_FILE: &str = "client-exposed";

// ── Node identity (five-programs fp-33) ──────────────────────────────────
// The `node_id` file, the mesh.json identity read, and the FULL precedence
// decider moved to `sovereign_contracts::node_identity`: the files are
// cross-program contracts — the daemon writes them, every CLI surface that
// stamps records reads them — and the workbench must resolve its identity
// without linking this crate. Every historically-public item stays
// reachable at its historical path (ARCH §10.6); `save_node_id` was
// module-private and stays that way (imported for the precedence tests
// below, which still pin the write/read round-trip against real
// `PersistedMesh`-written files — the pin that keeps the contracts-side
// mesh.json projection honest).
use sovereign_contracts::node_identity::save_node_id;
pub use sovereign_contracts::node_identity::{
    load_node_id, load_or_generate_self_node_id, node_id_file, resolve_self_node_id, NODE_ID_FILE,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedMesh {
    pub self_node_id: NodeId,
    pub mesh_id: MeshId,
    pub name: String,
    /// Gossip-auth credential. Absent from any `mesh.json` written before the
    /// credential split, hence `#[serde(default)]` — [`migrate_legacy_layout`]
    /// fills it in deterministically on first read.
    #[serde(default)]
    pub mesh_secret: [u8; 32],
    /// Serialized under its historical key so a `mesh.json` written by a
    /// pre-split build still parses, and so a downgrade still reads ours.
    #[serde(rename = "join_key_hash")]
    pub invite_key_hash: [u8; 32],
    /// Invite TTL. Persisted (it used to be node-local RAM and died on
    /// restart, which silently disarmed every encrypted mesh's expiry).
    #[serde(default)]
    pub invite_expires_at: Option<u64>,
    /// Monotonic invite counter. Persisted so a restart does not reset the
    /// mesh to version 0 and re-adopt a rotation it already has.
    #[serde(default)]
    pub invite_version: u64,
    /// Mesh-wide encryption policy ([`Mesh::require_encryption`]),
    /// persisted so the daemon re-derives its enforcement posture on
    /// boot. `#[serde(default)]` keeps existing mesh.json files readable.
    #[serde(default)]
    pub require_encryption: bool,
    pub members: Vec<MemberRecord>,
    pub peers: Vec<MeshPeering>,
}

impl PersistedMesh {
    pub fn from_live(mesh: &Mesh, self_node_id: NodeId) -> Self {
        Self {
            self_node_id,
            mesh_id: mesh.id,
            name: mesh.name.clone(),
            mesh_secret: mesh.mesh_secret,
            invite_key_hash: mesh.invite_key_hash,
            invite_version: mesh.invite_version,
            invite_expires_at: mesh.invite_expires_at,
            require_encryption: mesh.require_encryption,
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
            mesh_secret: self.mesh_secret,
            invite_key_hash: self.invite_key_hash,
            invite_version: self.invite_version,
            invite_expires_at: self.invite_expires_at,
            require_encryption: self.require_encryption,
            members,
            peers: self.peers,
        };
        (mesh, self.self_node_id)
    }

    /// Fill in a `mesh_secret` for a record that predates the credential
    /// split. Returns whether anything changed, so the caller can log a
    /// one-time migration rather than every read.
    ///
    /// See [`commonwealth_discovery::membership::derive_legacy_mesh_secret`]
    /// for why this is derived rather than random: every node must land on the
    /// same value with no message exchanged, or the upgrade partitions the
    /// mesh it is meant to protect.
    pub fn ensure_mesh_secret(&mut self) -> bool {
        if self.mesh_secret != commonwealth_core::mesh::MESH_SECRET_UNSET {
            return false;
        }
        self.mesh_secret = commonwealth_discovery::membership::derive_legacy_mesh_secret(
            &self.mesh_id,
            &self.invite_key_hash,
        );
        true
    }
}

// The multi-mesh layout vocabulary — the `meshes/` directory, the `active`
// pointer and its raw read — moved to `sovereign_contracts::node_identity`
// beside the identity reader that must follow the same layout (five-programs
// fp-33); re-exported here at their historical paths (ARCH §10.6). The typed
// `active_mesh_id` stays: `MeshId` is commonwealth-core's, which contracts
// may not name.
pub use sovereign_contracts::node_identity::{
    active_mesh_hex, active_pointer, ACTIVE_FILE, MESHES_DIR,
};

/// `<root>/meshes`.
pub fn meshes_dir(root: &Path) -> PathBuf {
    root.join(MESHES_DIR)
}

/// `<root>/meshes/<mesh-id-hex>` — everything scoped to ONE membership.
pub fn mesh_dir(root: &Path, mesh_id: &MeshId) -> PathBuf {
    meshes_dir(root).join(mesh_id.to_hex())
}

/// Which mesh is active, if any. `None` on a clean install, or on a legacy
/// layout that [`migrate_legacy_layout`] has not run against yet.
pub fn active_mesh_id(root: &Path) -> Option<MeshId> {
    active_mesh_hex(root).and_then(|hex| MeshId::from_hex(&hex))
}

/// Point `active` at `mesh_id`. Atomic; creates `<root>` if needed.
pub fn set_active(root: &Path, mesh_id: &MeshId) -> std::io::Result<()> {
    fs::create_dir_all(root)?;
    let target = active_pointer(root);
    let tmp = target.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(mesh_id.to_hex().as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &target)
}

/// Forget which mesh was active without deleting any mesh. Used by `leave`,
/// which drops one membership but must not disturb the parked ones.
pub fn clear_active(root: &Path) -> std::io::Result<()> {
    match fs::remove_file(active_pointer(root)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// The directory the ACTIVE mesh's files live in — `<root>/meshes/<id>` when a
/// pointer exists, else `<root>` itself.
///
/// The fallback is what lets every existing caller of [`mesh_file`],
/// [`join_key_file`] and the client-exposed marker keep working untouched: on a
/// legacy layout they resolve exactly where they always did, and after
/// migration they follow the pointer. One accessor, two layouts (ARCH §7.5 —
/// a path derived by hand in two places is a split-brain waiting to happen).
fn active_dir(root: &Path) -> PathBuf {
    match active_mesh_id(root) {
        Some(id) => mesh_dir(root, &id),
        None => root.to_path_buf(),
    }
}

pub fn mesh_file(data_dir: &Path) -> PathBuf {
    active_dir(data_dir).join(MESH_FILE)
}

pub fn join_key_file(data_dir: &Path) -> PathBuf {
    active_dir(data_dir).join(JOIN_KEY_FILE)
}

/// Every mesh this node is a member of, active and parked, newest-joined
/// first is NOT guaranteed — order follows directory iteration.
pub fn list_known(root: &Path) -> Vec<PersistedMesh> {
    let Ok(entries) = fs::read_dir(meshes_dir(root)) else {
        // No `meshes/` dir: either a clean install, or a legacy layout whose
        // single mesh still sits at the root. Report the legacy one rather
        // than claiming this node has joined nothing.
        return load_from_dir(root).into_iter().collect();
    };
    entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| load_from_dir(&e.path()))
        .collect()
}

/// Read a `PersistedMesh` out of a specific directory, migrating its secret in
/// memory if the file predates the split. `None` if absent or unparseable.
fn load_from_dir(dir: &Path) -> Option<PersistedMesh> {
    let bytes = fs::read(dir.join(MESH_FILE)).ok()?;
    let mut parsed: PersistedMesh = serde_json::from_slice(&bytes).ok()?;
    parsed.ensure_mesh_secret();
    Some(parsed)
}

/// Resolve an operator-typed mesh reference against a known-mesh listing.
///
/// ONE rule, because there were four and they disagreed (ARCH §10.6). `switch`
/// accepted an 8-character id prefix and `forget` did not, and the HTTP switch
/// and the CLI forget each re-derived their own copy — so a reference that
/// switched a mesh could not forget it, and the fix had to be made in four
/// places or in none.
///
/// - name, case-insensitive
/// - full id hex
/// - an id prefix of at least 8 hex characters: short enough to type from
///   `svrn mesh list`, long enough that a collision is not a realistic
///   accident. Below 8 we refuse rather than guess, because the wrong match
///   here switches or DELETES the wrong mesh.
pub fn resolve_known<'a>(known: &'a [PersistedMesh], target: &str) -> Option<&'a PersistedMesh> {
    let needle = target.trim().to_lowercase();
    if needle.is_empty() {
        return None;
    }
    known.iter().find(|m| {
        let id_hex = m.mesh_id.to_hex();
        m.name.to_lowercase() == needle
            || id_hex == needle
            || (needle.len() >= 8 && id_hex.starts_with(&needle))
    })
}

/// Drop a mesh from disk entirely. Refuses to forget the ACTIVE mesh — leaving
/// the node pointing at a directory that no longer exists would present as a
/// corrupt install rather than as the mistake it is.
pub fn forget(root: &Path, mesh_id: &MeshId) -> std::io::Result<()> {
    if active_mesh_id(root).as_ref() == Some(mesh_id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to forget the active mesh — switch or leave first",
        ));
    }
    let dir = mesh_dir(root, mesh_id);
    match fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Move a pre-multi-mesh `<root>/mesh.json` (plus its join key and
/// client-exposed marker) into `<root>/meshes/<id>/`, derive its `mesh_secret`,
/// and point `active` at it. Idempotent and safe to call on every boot.
///
/// Returns `Ok(true)` when it actually moved something, so the caller can log
/// the migration once instead of on every start.
pub fn migrate_legacy_layout(root: &Path) -> std::io::Result<bool> {
    let legacy = root.join(MESH_FILE);
    if !legacy.exists() || active_mesh_id(root).is_some() {
        return Ok(false);
    }
    let bytes = fs::read(&legacy)?;
    let mut parsed: PersistedMesh = serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let derived = parsed.ensure_mesh_secret();

    let dir = mesh_dir(root, &parsed.mesh_id);
    fs::create_dir_all(&dir)?;
    let payload = serde_json::to_vec_pretty(&parsed)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(dir.join(MESH_FILE), &payload)?;

    // Carry the siblings across before the pointer flips, so a crash midway
    // leaves the legacy layout intact rather than a half-populated new one.
    if let Ok(key) = fs::read(root.join(JOIN_KEY_FILE)) {
        fs::write(dir.join(JOIN_KEY_FILE), &key)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(dir.join(JOIN_KEY_FILE), fs::Permissions::from_mode(0o600));
        }
    }

    set_active(root, &parsed.mesh_id)?;

    let _ = fs::remove_file(&legacy);
    let _ = fs::remove_file(root.join(JOIN_KEY_FILE));

    tracing::info!(
        mesh = %parsed.name,
        derived_secret = derived,
        dir = %dir.display(),
        "mesh: migrated to the multi-mesh layout"
    );
    Ok(true)
}

/// Persist the plaintext `join_key` for the active mesh. Atomic
/// (write-tmp-then-rename) and 0600 on Unix so other local users
/// can't read it.
///
/// Why store the plaintext at all: `Mesh.invite_key_hash` is one-way,
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

/// Persist the plaintext invite key for a SPECIFIC mesh rather than the active
/// one. Needed because a join writes the key for the mesh it just joined, which
/// is not yet the active one at that instant, and because tests set up parked
/// meshes with their own keys.
pub fn save_join_key_for(data_dir: &Path, mesh_id: &MeshId, join_key: &str) -> std::io::Result<()> {
    let dir = mesh_dir(data_dir, mesh_id);
    fs::create_dir_all(&dir)?;
    let target = dir.join(JOIN_KEY_FILE);
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

fn client_exposed_file(data_dir: &Path) -> std::path::PathBuf {
    // NODE-level, deliberately not per-mesh. "This node serves remote callers"
    // is a property of the machine, not of a membership, and `expose_client_api`
    // is called BEFORE `create_mesh`/`join_mesh` — there is no active mesh dir
    // to write into at that moment. The per-mesh half of the bind decision is
    // `mesh.require_encryption`, which `start_daemon` already re-reads on every
    // resume and switch.
    data_dir.join(CLIENT_EXPOSED_FILE)
}

/// Mark this daemon as opted into serving remote callers. Idempotent
/// (creates an empty marker file). See [`CLIENT_EXPOSED_FILE`].
pub fn set_client_exposed(data_dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(data_dir)?;
    fs::File::create(client_exposed_file(data_dir))?.sync_all()
}

/// True iff the client-exposed marker is present.
pub fn client_exposed(data_dir: &Path) -> bool {
    client_exposed_file(data_dir).exists()
}

/// Remove the client-exposed marker (re-secure to loopback on next
/// start). Called on `leave_mesh`. Idempotent.
pub fn clear_client_exposed(data_dir: &Path) -> std::io::Result<()> {
    match fs::remove_file(client_exposed_file(data_dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Atomically persist `mesh` + `self_node_id` into that mesh's OWN directory,
/// `<data_dir>/meshes/<mesh_id>/mesh.json`. Write-to-tempfile-then-rename so a
/// crash mid-write can't corrupt the previous state.
///
/// **Writing a mesh does not make it the active one.** Saving used to re-point
/// `active` at its subject, which made every caller an implicit switcher — and
/// two of them are periodic: the gossip loop re-persists every round, and the
/// mesh-mutation hook fires on every accepted join. So a round still in flight
/// for the mesh we just PARKED would re-point `active` back at it moments after
/// `switch_mesh` moved the pointer, silently undoing the switch. `switch_mesh`
/// already calls [`set_active`] itself; the two callers that genuinely
/// establish activeness (`create_mesh`, `join_mesh`) use [`save_and_activate`].
/// One decider per question (ARCH §10.6): "store this mesh's state" and "make
/// this mesh live" are different decisions and now have different functions.
///
/// The target is derived from `mesh.id` rather than [`mesh_file`], which
/// resolves through the active pointer. That is what removes the crash window
/// too: the pointer no longer has to be moved BEFORE the file exists for the
/// write to land in the right place.
pub fn save(data_dir: &Path, mesh: &Mesh, self_node_id: NodeId) -> std::io::Result<()> {
    fs::create_dir_all(data_dir)?;
    let dir = mesh_dir(data_dir, &mesh.id);
    fs::create_dir_all(&dir)?;
    let target = dir.join(MESH_FILE);
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // `mesh.json` carries `mesh_secret` since the 2026-08-26 credential
        // split, so it is now at least as sensitive as `join_key.secret` beside
        // it — which has been 0600 all along. The secret authorizes gossip and
        // NEVER rotates, so a local read is permanent: strictly worse than
        // leaking the invite key, which at least can be rotated out.
        //
        // Best-effort like `save_join_key`: a failure here does not invalidate
        // the write, the file is present, just possibly group-readable.
        let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// [`save`], then point `active` at the mesh — the two-step every caller that
/// genuinely ESTABLISHES a membership wants: `create_mesh` and `join_mesh`.
///
/// Ordered file-then-pointer on purpose. The other order left a window where
/// `active` named a directory with no `mesh.json` in it yet, and a crash there
/// booted into a mesh that does not exist on disk. This order's worst case is
/// a mesh written but not yet activated, which the next boot simply does not
/// resume — recoverable, and never a dangling pointer.
pub fn save_and_activate(
    data_dir: &Path,
    mesh: &Mesh,
    self_node_id: NodeId,
) -> std::io::Result<()> {
    save(data_dir, mesh, self_node_id)?;
    if active_mesh_id(data_dir).as_ref() != Some(&mesh.id) {
        set_active(data_dir, &mesh.id)?;
    }
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
            let mut parsed: PersistedMesh = serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            // A record written before the credential split carries no secret.
            // Derive it here rather than at every read site, so no caller can
            // forget and end up gossiping a zeroed secret.
            parsed.ensure_mesh_secret();
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
    use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
    use commonwealth_core::mesh::NodeStatus;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn sample_mesh() -> (Mesh, NodeId) {
        let node_id = NodeId::generate();
        let member = MemberRecord {
            removed_at: None,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
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
                origins: Vec::new(),
                media_allow: Vec::new(),
                media_available: None,

                embed_model: None,
                benchmark: None,
                current_in_flight: None,
                anchor: None,
            },
            addresses: vec!["192.168.1.10:9742".parse().unwrap()],
        };
        let mut members = HashMap::new();
        members.insert(node_id, member);
        let mesh = Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id: MeshId::generate(),
            name: "Persisted Mesh".into(),
            invite_key_hash: [42u8; 32],
            invite_version: 0,
            require_encryption: false,
            members,
            peers: vec![],
        };
        (mesh, node_id)
    }

    #[test]
    fn save_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let (mesh, node_id) = sample_mesh();
        save_and_activate(tmp.path(), &mesh, node_id).unwrap();

        let loaded = load(tmp.path()).unwrap().expect("file should exist");
        assert_eq!(loaded.self_node_id, node_id);
        assert_eq!(loaded.name, "Persisted Mesh");
        assert_eq!(loaded.members.len(), 1);
        assert_eq!(loaded.invite_key_hash, [42u8; 32]);

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
        save_and_activate(tmp.path(), &mesh, node_id).unwrap();
        assert!(mesh_file(tmp.path()).exists());

        clear(tmp.path()).unwrap();
        assert!(!mesh_file(tmp.path()).exists());

        // Second call on a missing file is a no-op, not an error.
        clear(tmp.path()).unwrap();
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
    fn load_on_corrupt_file_returns_err() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path()).unwrap();
        fs::write(mesh_file(tmp.path()), b"not json").unwrap();
        assert!(load(tmp.path()).is_err());
    }

    /// `sample_mesh` plus a second member, so a test can put a REAL peer's id
    /// into the node_id file — the shape of the 2026-08-20 failure.
    fn mesh_with_peer() -> (Mesh, NodeId, NodeId) {
        let (mut mesh, self_id) = sample_mesh();
        let peer_id = NodeId::generate();
        let mut peer = mesh.members.values().next().unwrap().clone();
        peer.node_id = peer_id;
        peer.name = "BeefyMac".into();
        mesh.members.insert(peer_id, peer);
        (mesh, self_id, peer_id)
    }

    /// The bug: `<data_dir>/node_id` holds a PEER's identity. Adopting it makes
    /// two machines claim one id — self-filtering inverts and this node's work
    /// is attributed to that peer across the mesh.
    #[test]
    fn node_id_file_holding_a_peers_id_loses_to_mesh_json() {
        let tmp = TempDir::new().unwrap();
        let (mesh, self_id, peer_id) = mesh_with_peer();
        save_and_activate(tmp.path(), &mesh, self_id).unwrap();
        save_node_id(tmp.path(), &peer_id).unwrap();

        assert_eq!(
            resolve_self_node_id(tmp.path()),
            self_id,
            "must not adopt a known peer's id as its own"
        );
        assert_eq!(
            load_node_id(tmp.path()).unwrap(),
            Some(self_id),
            "and must repair the file so the divergence cannot recur on restart"
        );
    }

    /// The guard is narrow on purpose. An id that is simply not in the mesh is
    /// this node's own stable identity, and file precedence still holds — a
    /// fresh join must never rotate it.
    #[test]
    fn node_id_file_still_wins_when_it_is_not_a_peer() {
        let tmp = TempDir::new().unwrap();
        let (mesh, self_id, _peer_id) = mesh_with_peer();
        save_and_activate(tmp.path(), &mesh, self_id).unwrap();
        let stable = NodeId::generate();
        save_node_id(tmp.path(), &stable).unwrap();

        assert_eq!(resolve_self_node_id(tmp.path()), stable);
        assert_eq!(
            load_node_id(tmp.path()).unwrap(),
            Some(stable),
            "an uncontested file is left alone"
        );
    }

    #[test]
    fn mesh_json_id_is_used_when_no_node_id_file_exists() {
        let tmp = TempDir::new().unwrap();
        let (mesh, self_id, _peer_id) = mesh_with_peer();
        save_and_activate(tmp.path(), &mesh, self_id).unwrap();
        assert_eq!(resolve_self_node_id(tmp.path()), self_id);
    }

    // ── Multi-mesh layout + the credential split ────────────────────────────

    /// The property the whole rolling upgrade rests on: two nodes holding the
    /// same pre-split `mesh.json` derive the SAME secret with no coordination.
    /// If this is ever made random, the first node to upgrade partitions
    /// itself from every node that has not.
    #[test]
    fn legacy_records_derive_one_identical_secret_with_no_coordination() {
        let (mesh, node_a) = sample_mesh();
        let node_b = NodeId::generate();

        let mut a = PersistedMesh::from_live(&mesh, node_a);
        let mut b = PersistedMesh::from_live(&mesh, node_b);
        a.mesh_secret = [0u8; 32];
        b.mesh_secret = [0u8; 32];

        assert!(a.ensure_mesh_secret(), "a zeroed secret is filled in");
        assert!(b.ensure_mesh_secret());
        assert_eq!(
            a.mesh_secret, b.mesh_secret,
            "two nodes upgrading independently must land on the same secret"
        );
        assert_ne!(a.mesh_secret, [0u8; 32]);
        assert!(
            !a.ensure_mesh_secret(),
            "a record that already has a secret is left alone"
        );
    }

    /// Migration is a pure function of what is already on disk, so it can run
    /// on every boot. Also pins that it does not lose the invite key.
    #[test]
    fn legacy_layout_migrates_once_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let (mesh, self_id) = sample_mesh();
        let mesh_id = mesh.id;

        // Hand-write the OLD layout: mesh.json + join_key.secret at the root,
        // no `active` pointer, no `meshes/` dir.
        let mut legacy = PersistedMesh::from_live(&mesh, self_id);
        legacy.mesh_secret = [0u8; 32];
        fs::create_dir_all(tmp.path()).unwrap();
        fs::write(
            tmp.path().join(MESH_FILE),
            serde_json::to_vec_pretty(&legacy).unwrap(),
        )
        .unwrap();
        fs::write(tmp.path().join(JOIN_KEY_FILE), b"cwth-aaaa-bbbb-cccc").unwrap();

        assert!(
            migrate_legacy_layout(tmp.path()).unwrap(),
            "first run moves"
        );
        assert!(
            !migrate_legacy_layout(tmp.path()).unwrap(),
            "second run is a no-op"
        );

        assert_eq!(active_mesh_id(tmp.path()), Some(mesh_id));
        assert!(!tmp.path().join(MESH_FILE).exists(), "legacy file removed");
        assert!(mesh_dir(tmp.path(), &mesh_id).join(MESH_FILE).exists());

        let loaded = load(tmp.path()).unwrap().unwrap();
        assert_ne!(loaded.mesh_secret, [0u8; 32], "secret derived on migrate");
        assert_eq!(loaded.invite_key_hash, mesh.invite_key_hash);
        assert_eq!(
            load_join_key(tmp.path()).unwrap().as_deref(),
            Some("cwth-aaaa-bbbb-cccc"),
            "the invite key followed its mesh into the new layout"
        );
    }

    /// The point of the feature: a second mesh does not disturb the first.
    /// Byte-identical check on the parked mesh, modelled on `join_parks_not_leaves`.
    #[test]
    fn two_meshes_coexist_and_switching_does_not_touch_the_parked_one() {
        let tmp = TempDir::new().unwrap();
        let (mesh_a, self_id) = sample_mesh();
        let (mut mesh_b, _) = sample_mesh();
        mesh_b.name = "Second Mesh".into();

        save_and_activate(tmp.path(), &mesh_a, self_id).unwrap();
        save_join_key(tmp.path(), "cwth-1111-1111-1111").unwrap();
        let a_bytes = fs::read(mesh_dir(tmp.path(), &mesh_a.id).join(MESH_FILE)).unwrap();

        // Joining/creating the second mesh makes it active...
        save_and_activate(tmp.path(), &mesh_b, self_id).unwrap();
        save_join_key(tmp.path(), "cwth-2222-2222-2222").unwrap();
        assert_eq!(active_mesh_id(tmp.path()), Some(mesh_b.id));

        // ...and the first is untouched, roster and key both.
        assert_eq!(
            fs::read(mesh_dir(tmp.path(), &mesh_a.id).join(MESH_FILE)).unwrap(),
            a_bytes,
            "parking a mesh must not rewrite it"
        );
        assert_eq!(list_known(tmp.path()).len(), 2);

        // Switching back finds mesh A's own invite key, not mesh B's.
        set_active(tmp.path(), &mesh_a.id).unwrap();
        assert_eq!(
            load_join_key(tmp.path()).unwrap().as_deref(),
            Some("cwth-1111-1111-1111"),
            "the invite key is per-mesh, not per-node"
        );
    }

    #[test]
    fn forget_refuses_the_active_mesh_and_drops_a_parked_one() {
        let tmp = TempDir::new().unwrap();
        let (mesh_a, self_id) = sample_mesh();
        let (mut mesh_b, _) = sample_mesh();
        mesh_b.name = "Parked".into();
        save_and_activate(tmp.path(), &mesh_a, self_id).unwrap();
        save_and_activate(tmp.path(), &mesh_b, self_id).unwrap(); // b is now active

        assert!(
            forget(tmp.path(), &mesh_b.id).is_err(),
            "forgetting the active mesh would strand the pointer"
        );
        forget(tmp.path(), &mesh_a.id).unwrap();
        assert_eq!(list_known(tmp.path()).len(), 1);
    }

    /// P6, the switch race. `save` used to re-point `active` at its subject,
    /// which made every caller an implicit switcher — including the two that
    /// fire on a timer: the gossip loop's per-round re-persist and the
    /// mesh-mutation hook. A round still in flight for the mesh we just PARKED
    /// therefore re-pointed `active` back at it moments after `switch_mesh`
    /// moved the pointer, and the switch silently came undone.
    #[test]
    fn an_in_flight_round_for_a_parked_mesh_cannot_steal_the_active_pointer() {
        let tmp = TempDir::new().unwrap();
        let (mesh_a, self_id) = sample_mesh();
        let (mut mesh_b, _) = sample_mesh();
        mesh_b.name = "Second Mesh".into();

        save_and_activate(tmp.path(), &mesh_a, self_id).unwrap();
        save_and_activate(tmp.path(), &mesh_b, self_id).unwrap();
        assert_eq!(active_mesh_id(tmp.path()), Some(mesh_b.id));

        // The gossip loop for the mesh we parked, one round behind.
        save(tmp.path(), &mesh_a, self_id).unwrap();

        assert_eq!(
            active_mesh_id(tmp.path()),
            Some(mesh_b.id),
            "a periodic re-persist of a PARKED mesh must not undo a switch"
        );
    }

    /// covers: FE-8
    ///
    /// The same separation from the other side: a plain `save` still writes the
    /// mesh's own file, in the mesh's own directory, whichever mesh is active.
    /// That is what removes the crash window — the pointer no longer has to
    /// move BEFORE the file exists for the write to land correctly.
    #[test]
    fn save_writes_into_the_meshs_own_directory_without_moving_the_pointer() {
        let tmp = TempDir::new().unwrap();
        let (mesh_a, self_id) = sample_mesh();
        let (mut mesh_b, _) = sample_mesh();
        mesh_b.name = "Parked".into();

        save_and_activate(tmp.path(), &mesh_a, self_id).unwrap();
        save(tmp.path(), &mesh_b, self_id).unwrap();

        assert!(
            mesh_dir(tmp.path(), &mesh_b.id).join(MESH_FILE).exists(),
            "the parked mesh's state must still be written"
        );
        assert_eq!(active_mesh_id(tmp.path()), Some(mesh_a.id));
        assert_eq!(list_known(tmp.path()).len(), 2);
    }

    /// `save_and_activate` orders file-then-pointer, so `active` never names a
    /// directory that has no `mesh.json` in it. The old order set the pointer
    /// first, and a crash in that window booted into a mesh that did not exist.
    #[test]
    fn the_active_pointer_never_names_a_mesh_without_state() {
        let tmp = TempDir::new().unwrap();
        let (mesh_a, self_id) = sample_mesh();
        save_and_activate(tmp.path(), &mesh_a, self_id).unwrap();

        let active = active_mesh_id(tmp.path()).expect("a mesh is active");
        assert!(
            mesh_dir(tmp.path(), &active).join(MESH_FILE).exists(),
            "the pointer must only ever name a mesh whose state is on disk"
        );
        assert!(load(tmp.path()).unwrap().is_some());
    }

    /// The rule the four copies disagreed about: an id prefix resolves, and a
    /// prefix shorter than 8 does NOT — a wrong match here deletes a mesh.
    #[test]
    fn resolve_known_accepts_name_full_id_and_an_eight_char_prefix() {
        let tmp = TempDir::new().unwrap();
        let (mut mesh, self_id) = sample_mesh();
        mesh.name = "Study Group".into();
        save_and_activate(tmp.path(), &mesh, self_id).unwrap();
        let known = list_known(tmp.path());
        let hex = mesh.id.to_hex();

        assert!(resolve_known(&known, "Study Group").is_some());
        assert!(
            resolve_known(&known, "  study group  ").is_some(),
            "trimmed + case-folded"
        );
        assert!(resolve_known(&known, &hex).is_some());
        assert!(resolve_known(&known, &hex[..8]).is_some());
        assert!(
            resolve_known(&known, &hex[..7]).is_none(),
            "7 characters is a guess, and the caller may be `forget`"
        );
        assert!(resolve_known(&known, "").is_none());
        assert!(resolve_known(&known, "no-such-mesh").is_none());
    }
}
