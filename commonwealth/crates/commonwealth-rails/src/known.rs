// SPDX-License-Identifier: AGPL-3.0-or-later
//! The meshes this node belongs to but is not active on: cw-rails' own
//! known-mesh store, under its own data root.
//!
//! The ACTIVE mesh stays where it always was, `<data-dir>/mesh.json` and
//! `join_key.secret`, so a data root written before this store starts
//! exactly as it did. A PARKED mesh is the same two files under
//! `<data-dir>/meshes/<mesh-id-hex>/`, written by the same
//! [`identity`] functions. Switching moves one pair out and another in, and
//! forgetting deletes a parked pair. The inference daemon keeps its own list
//! in its own root (`sovereign-mesh` persist.rs), which cw-rails never opens
//! (FIVE_PROGRAMS §4 rule 1); the flip migrates that list here.

use std::path::{Path, PathBuf};

use commonwealth_core::ids::MeshId;
use commonwealth_core::mesh::Mesh;

use crate::identity::{self, StoreRefusal};

pub const MESHES_DIR: &str = "meshes";

/// One parked membership, as read off disk.
#[derive(Debug)]
pub struct Parked {
    pub mesh: Mesh,
    pub join_key: Option<String>,
}

pub fn meshes_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(MESHES_DIR)
}

fn parked_dir(data_dir: &Path, id: &MeshId) -> PathBuf {
    meshes_dir(data_dir).join(id.to_hex())
}

/// Set a membership down without giving it up: its roster, secret and
/// invite key survive, so coming back is a resume rather than a join.
pub fn park(data_dir: &Path, mesh: &Mesh, join_key: Option<&str>) -> Result<(), StoreRefusal> {
    let dir = parked_dir(data_dir, &mesh.id);
    identity::save_mesh(&dir, mesh)?;
    match join_key {
        Some(key) => identity::save_join_key(&dir, key)?,
        None => identity::clear_join_key(&dir)?,
    }
    tracing::info!(target: "rails", mesh = %mesh.name, mesh_id = %mesh.id,
                   dir = %dir.display(), "known: mesh parked");
    Ok(())
}

/// Every parked membership, by name. A directory whose `mesh.json` does not
/// read is named in the log and left out, never read as an empty mesh.
pub fn parked(data_dir: &Path) -> Result<Vec<Parked>, StoreRefusal> {
    let root = meshes_dir(data_dir);
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(StoreRefusal::Io(root, e)),
    };
    let mut out = Vec::new();
    for dir in entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        match identity::load_mesh(&dir) {
            Ok(Some(mesh)) => out.push(Parked {
                join_key: identity::load_join_key(&dir)?,
                mesh,
            }),
            Ok(None) => {
                tracing::warn!(target: "rails", dir = %dir.display(), "known: a parked directory holds no mesh.json — not listed")
            }
            Err(e) => {
                tracing::warn!(target: "rails", dir = %dir.display(), error = %e, "known: a parked mesh does not read — not listed")
            }
        }
    }
    out.sort_by(|a, b| a.mesh.name.cmp(&b.mesh.name));
    Ok(out)
}

/// The parked membership an operator-typed reference names, by the one rule
/// every known-mesh list reads (`names_mesh`).
pub fn resolve<'a>(parked: &'a [Parked], reference: &str) -> Option<&'a Parked> {
    parked.iter().find(|p| {
        commonwealth_discovery::membership::names_mesh(&p.mesh.name, &p.mesh.id.to_hex(), reference)
    })
}

/// Delete a parked membership. Absent is already done.
pub fn remove(data_dir: &Path, id: &MeshId) -> Result<(), StoreRefusal> {
    let dir = parked_dir(data_dir, id);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {
            tracing::info!(target: "rails", mesh_id = %id, "known: parked mesh removed");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(StoreRefusal::Io(dir, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh(name: &str) -> (Mesh, String) {
        commonwealth_discovery::membership::init_mesh(name, "me", Vec::new())
    }

    #[test]
    fn a_parked_mesh_lists_with_its_key_and_resolves_by_name_or_prefix() {
        let d = tempfile::tempdir().unwrap();
        assert!(
            parked(d.path()).unwrap().is_empty(),
            "no store is no meshes"
        );
        let (lab, key) = mesh("Lab");
        let (club, _) = mesh("Club");
        park(d.path(), &lab, Some(&key)).unwrap();
        park(d.path(), &club, None).unwrap();

        let list = parked(d.path()).unwrap();
        assert_eq!(
            list.iter()
                .map(|p| p.mesh.name.as_str())
                .collect::<Vec<_>>(),
            ["Club", "Lab"]
        );
        let got = resolve(&list, "lab").expect("by name, any case");
        assert_eq!(got.mesh.id, lab.id);
        assert_eq!(got.join_key.as_deref(), Some(key.as_str()));
        assert_eq!(
            got.mesh.mesh_secret, lab.mesh_secret,
            "the credential survives parking"
        );
        let hex = lab.id.to_hex();
        assert!(resolve(&list, &hex[..8]).is_some());
        assert!(resolve(&list, &hex[..7]).is_none(), "below 8 is a guess");
        assert!(resolve(&list, "Club").unwrap().join_key.is_none());
    }

    /// The active files at the root are not the store's, and removing a
    /// parked mesh touches nothing else.
    #[test]
    fn removing_a_parked_mesh_leaves_the_rest() {
        let d = tempfile::tempdir().unwrap();
        let (active, _) = mesh("Active");
        identity::save_mesh(d.path(), &active).unwrap();
        let (lab, _) = mesh("Lab");
        let (club, _) = mesh("Club");
        park(d.path(), &lab, None).unwrap();
        park(d.path(), &club, None).unwrap();
        remove(d.path(), &lab.id).unwrap();
        remove(d.path(), &lab.id).expect("absent is already done");
        let names: Vec<_> = parked(d.path())
            .unwrap()
            .into_iter()
            .map(|p| p.mesh.name)
            .collect();
        assert_eq!(names, ["Club"]);
        assert_eq!(
            identity::load_mesh(d.path()).unwrap().unwrap().id,
            active.id
        );
    }
}
