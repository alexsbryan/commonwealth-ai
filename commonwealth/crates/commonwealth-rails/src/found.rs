// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cw-rails found <mesh-name>` — start a mesh with this node as its first
//! member, and the invite that grows it.
//!
//! Founding is `commonwealth_discovery::membership::init_mesh_with_identity`,
//! the same constructor the inference daemon founds with; this module only
//! decides where the result is written. The mesh is founded encrypted
//! (`require_encryption`), because this process reaches peers by key or not
//! at all, so its invites carry `iroh=`.
//!
//! **A cw-rails invite does not expire.** The daemon arms a 24-hour TTL and
//! renews it with `svrn mesh rotate`; cw-rails has no rotate, and a TTL with
//! no rotate would end admission for good a day after founding. A member that
//! rotates the invite elsewhere makes this node's key stale, and
//! [`invite_link`] then answers with that absence by name.

use std::path::{Path, PathBuf};

use commonwealth_core::mesh::Mesh;
use commonwealth_discovery::deep_link::build_join_link;
use commonwealth_discovery::membership::{init_mesh_with_identity, verify_join_key};

use crate::{identity, Refusal};

#[derive(Debug, thiserror::Error)]
pub enum FoundRefusal {
    #[error(
        "{0} already holds a mesh — this node is on one. Founding would orphan it; \
         give the new mesh its own --data-dir"
    )]
    AlreadyMeshed(PathBuf),
}

/// What a founding wrote.
#[derive(Debug)]
pub struct Founded {
    pub mesh: Mesh,
    pub join_key: String,
}

/// Found `mesh_name` under `data_dir` as `node_name`, and write `mesh.json`
/// and the invite key. The node key and id are the ones `run` will bind, so
/// the founder's roster row is the member `run` serves as.
pub fn found(data_dir: &Path, mesh_name: &str, node_name: &str) -> Result<Founded, Refusal> {
    if identity::load_mesh(data_dir)?.is_some() {
        tracing::warn!(target: "rails", data_dir = %data_dir.display(), "found: refused, a mesh is already here");
        return Err(FoundRefusal::AlreadyMeshed(identity::mesh_file(data_dir)).into());
    }
    std::fs::create_dir_all(data_dir)
        .map_err(|e| identity::StoreRefusal::DataDir(data_dir.to_path_buf(), e))?;
    let key = commonwealth_transport::identity::load_or_generate_node_key(data_dir);
    let self_id = identity::load_or_generate_node_id(data_dir)?;
    let (mesh, join_key) = init_mesh_with_identity(
        mesh_name,
        node_name,
        Vec::new(),
        self_id,
        Some(commonwealth_transport::identity::node_pubkey(&key)),
        true,
    );
    // The key first: a mesh.json with no key beside it is a founder that
    // can never print an invite.
    identity::save_join_key(data_dir, &join_key)?;
    identity::save_mesh(data_dir, &mesh)?;
    tracing::info!(target: "rails", mesh = %mesh.name, mesh_id = %mesh.id, node_id = %self_id,
                   "found: mesh founded");
    Ok(Founded { mesh, join_key })
}

/// Why this node cannot hand out an invite right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteAbsent {
    /// Solo, or joined: only a founder holds the raw key.
    NoKey,
    /// The key on disk no longer opens this mesh — another member rotated it.
    Rotated,
    /// The endpoint has no relay and no direct address to put in a dial.
    NoDial,
}

impl InviteAbsent {
    pub fn reason(&self) -> &'static str {
        match self {
            InviteAbsent::NoKey => "this node holds no invite key: only the member that founded the mesh with `cw-rails found` mints invites here",
            InviteAbsent::Rotated => "the invite key on disk no longer matches the mesh: another member rotated it, and that member holds the current one",
            InviteAbsent::NoDial => "the endpoint has no relay and no direct address yet, so an invite would name nobody to dial",
        }
    }
}

/// The invite a joiner needs: the key, the mesh name, and this endpoint's
/// dial string, in the link form every joiner parses.
pub fn invite_link(
    join_key: Option<&str>,
    mesh: &Mesh,
    dial: Option<&str>,
) -> Result<String, InviteAbsent> {
    let key = join_key.ok_or(InviteAbsent::NoKey)?;
    if !verify_join_key(key, &mesh.invite_key_hash) {
        return Err(InviteAbsent::Rotated);
    }
    let dial = dial.ok_or(InviteAbsent::NoDial)?;
    Ok(build_join_link(
        key,
        None,
        Some(&mesh.name),
        Some(dial),
        mesh.require_encryption,
        mesh.invite_expires_at,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A founding writes the mesh and the key, and the key opens the mesh.
    #[test]
    fn a_founding_writes_a_mesh_its_key_opens() {
        let dir = tempfile::tempdir().unwrap();
        let f = found(dir.path(), "Lab", "founder").expect("an empty root founds");
        let mesh = identity::load_mesh(dir.path()).unwrap().expect("mesh.json");
        assert_eq!(mesh.id, f.mesh.id);
        assert_eq!(mesh.members.len(), 1);
        assert!(mesh.require_encryption);
        let key = identity::load_join_key(dir.path())
            .unwrap()
            .expect("the key");
        assert!(verify_join_key(&key, &mesh.invite_key_hash));
    }

    /// The failing input: a second founding over a mesh would orphan it.
    #[test]
    fn founding_over_a_mesh_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        found(dir.path(), "Lab", "founder").unwrap();
        let err = found(dir.path(), "Other", "founder").unwrap_err();
        assert!(
            matches!(err, Refusal::Found(FoundRefusal::AlreadyMeshed(_))),
            "{err}"
        );
    }

    #[test]
    fn an_invite_carries_the_key_and_the_dial_and_joiners_read_it() {
        let dir = tempfile::tempdir().unwrap();
        let f = found(dir.path(), "Lab", "founder").unwrap();
        let dial = format!("{}@127.0.0.1:4433", hex::encode([7u8; 32]));
        let link = invite_link(Some(&f.join_key), &f.mesh, Some(&dial)).expect("an invite");
        let (key, got) = crate::join::dial_of(&link).expect("a joiner reads it");
        assert_eq!(
            (key.as_str(), got.as_str()),
            (f.join_key.as_str(), dial.as_str())
        );
    }

    #[test]
    fn each_absence_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let f = found(dir.path(), "Lab", "founder").unwrap();
        let dial = Some("x@127.0.0.1:1");
        assert_eq!(invite_link(None, &f.mesh, dial), Err(InviteAbsent::NoKey));
        let stale = commonwealth_discovery::membership::generate_join_key();
        assert_eq!(
            invite_link(Some(&stale), &f.mesh, dial),
            Err(InviteAbsent::Rotated)
        );
        assert_eq!(
            invite_link(Some(&f.join_key), &f.mesh, None),
            Err(InviteAbsent::NoDial)
        );
    }
}
