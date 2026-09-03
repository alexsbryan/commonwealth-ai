// SPDX-License-Identifier: AGPL-3.0-or-later
//! Multi-mesh membership: a node belongs to many meshes and is live in one.
//!
//! `persist.rs`'s unit tests cover the on-disk half (layout, migration,
//! per-mesh invite keys). This covers the daemon's half: what it will and
//! won't switch to, and that parking preserves the mesh being set down.
//!
//! The full switch — teardown, rebind, resume, gossip — is exercised by hand
//! against two real machines (see the plan's verification section); the piece
//! that can go wrong silently in-process is the guard logic and the
//! preservation, which is what lives here.
use crate::common;
use crate::common::mesh_admin_services;

use commonwealth_core::mesh::Mesh;
use sovereign_core::setup_config::SetupConfig;
use sovereign_mesh::daemon::{EmbeddedDaemon, MeshError};
use sovereign_mesh::persist;

/// Write a second, PARKED mesh straight to disk under `root`, leaving the
/// active pointer where it is. Mirrors what a join to a second mesh leaves
/// behind, without needing a second daemon to hand it to us.
fn park_a_mesh(
    root: &std::path::Path,
    name: &str,
    self_id: commonwealth_core::ids::NodeId,
) -> Mesh {
    let (mut mesh, _key) = commonwealth_discovery::membership::init_mesh_with_node_id(
        name,
        "self",
        vec!["127.0.0.1:9742".parse().unwrap()],
        self_id,
    );
    mesh.name = name.to_string();
    // No pointer dance: `persist::save` writes a mesh's state and nothing else.
    // It used to re-point `active` at its subject, so this helper had to put
    // the pointer back by hand — which is the same undo an in-flight gossip
    // round performed against a real switch (P6).
    persist::save(root, &mesh, self_id).unwrap();
    mesh
}

#[tokio::test]
async fn switching_to_a_mesh_we_do_not_belong_to_is_refused_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    daemon.create_mesh("only-mesh", "founder").await.unwrap();

    let err = daemon.switch_mesh("a mesh that does not exist").await;
    assert!(
        matches!(err, Err(MeshError::UnknownMesh(_))),
        "expected UnknownMesh, got {err:?}"
    );
    // And the refusal is inert: we are still in the mesh we started in.
    let live = daemon.mesh_state().await.expect("still running");
    assert_eq!(live.status.name, "only-mesh");
}

#[tokio::test]
async fn switching_to_the_active_mesh_is_refused_rather_than_bouncing_the_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    daemon.create_mesh("home", "founder").await.unwrap();

    // A no-op switch that tore down and rebound the listeners would look to
    // the user like a spurious ~10s outage, so it has to be refused, not
    // silently performed.
    let err = daemon.switch_mesh("home").await;
    assert!(
        matches!(err, Err(MeshError::MeshAlreadyActive(_))),
        "expected MeshAlreadyActive, got {err:?}"
    );
    assert!(
        daemon.is_running().await,
        "a refused switch must not stop us"
    );
}

#[tokio::test]
async fn a_parked_mesh_is_listed_and_keeps_its_own_roster_and_key() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    daemon.create_mesh("active-mesh", "founder").await.unwrap();
    let self_id = daemon.self_node_id().await.expect("running");

    let parked = park_a_mesh(tmp.path(), "parked-mesh", self_id);
    persist::save_join_key_for(tmp.path(), &parked.id, "cwth-dead-beef-cafe").unwrap();

    let known = daemon.known_meshes();
    assert_eq!(known.len(), 2, "both memberships are visible");
    let names: Vec<&str> = known.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"active-mesh"));
    assert!(names.contains(&"parked-mesh"));

    // The parked mesh carries its own secret and its own invite key — that is
    // what makes coming back to it a resume rather than a join.
    let parked_rec = known.iter().find(|m| m.name == "parked-mesh").unwrap();
    assert_ne!(
        parked_rec.mesh_secret, [0u8; 32],
        "a mesh created after the credential split has a real secret"
    );
    assert_eq!(persist::active_mesh_id(tmp.path()), {
        let active = known.iter().find(|m| m.name == "active-mesh").unwrap();
        Some(active.mesh_id)
    });
}

#[tokio::test]
async fn forgetting_the_active_mesh_is_refused_but_a_parked_one_goes() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    daemon.create_mesh("active-mesh", "founder").await.unwrap();
    let self_id = daemon.self_node_id().await.expect("running");
    park_a_mesh(tmp.path(), "parked-mesh", self_id);

    assert!(
        daemon.forget_mesh("active-mesh").is_err(),
        "forgetting the active mesh would strand the active pointer"
    );
    daemon.forget_mesh("parked-mesh").unwrap();
    assert_eq!(daemon.known_meshes().len(), 1);
}

/// `switch` and `forget` must accept the SAME reference. They did not: switch
/// resolved an 8-character id prefix and forget only a full id or an exact
/// name, so a prefix an operator had just used to switch was rejected by the
/// very next command. Four copies of the rule, one of them different.
#[tokio::test]
async fn switch_and_forget_resolve_a_mesh_the_same_way() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    daemon.create_mesh("active-mesh", "founder").await.unwrap();
    let self_id = daemon.self_node_id().await.expect("running");
    let parked = park_a_mesh(tmp.path(), "parked-mesh", self_id);
    let prefix = parked.id.to_hex()[..8].to_string();

    // The reference switch accepts...
    daemon
        .switch_mesh(&prefix)
        .await
        .expect("an 8-char id prefix switches");
    // ...and, once it is parked again, the reference forget accepts.
    daemon
        .switch_mesh("active-mesh")
        .await
        .expect("switch back");
    daemon
        .forget_mesh(&prefix)
        .expect("the same 8-char prefix must forget");
    assert_eq!(daemon.known_meshes().len(), 1);
}

/// covers: FE-10
///
/// Leaving deleted the mesh's `mesh.json` and left `active` still naming it.
/// Boot looked healthy — `load` returns `None`, `resume_active` returns false —
/// while `forget` refused that mesh forever, because forget refuses the ACTIVE
/// one and nothing could move the pointer off it. `persist::clear_active` was
/// written for this and had no caller anywhere in the workspace.
#[tokio::test]
async fn leaving_clears_the_active_pointer_rather_than_stranding_it() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    daemon.create_mesh("leaving-mesh", "founder").await.unwrap();
    let left = persist::active_mesh_id(tmp.path()).expect("a mesh is active after create");

    daemon.leave().await.unwrap();

    assert_eq!(
        persist::active_mesh_id(tmp.path()),
        None,
        "a pointer at a mesh we just deleted is unforgettable litter"
    );
    assert!(
        !persist::mesh_dir(tmp.path(), &left).exists(),
        "leaving already deleted everything inside; the husk goes with it"
    );
    assert!(persist::list_known(tmp.path()).is_empty());
}

/// Parking is the opposite, and the distinction is the whole feature: a PARKED
/// mesh keeps its pointer-eligible state on disk so re-entry is a resume.
#[tokio::test]
async fn a_parked_mesh_survives_where_a_left_one_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    daemon.create_mesh("active-mesh", "founder").await.unwrap();
    let self_id = daemon
        .self_node_id()
        .await
        .expect("node id after create_mesh");
    let parked = park_a_mesh(tmp.path(), "parked-mesh", self_id);

    daemon.leave().await.unwrap();

    assert!(
        persist::mesh_dir(tmp.path(), &parked.id).exists(),
        "leaving the ACTIVE mesh must not touch a parked one"
    );
    assert_eq!(persist::list_known(tmp.path()).len(), 1);
}
