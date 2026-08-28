// SPDX-License-Identifier: AGPL-3.0-or-later
//! A join never clobbers the mesh you are already in — it PARKS it.
//!
//! Replaces `auto_leave_gate.rs`, which pinned the older answer to the same
//! question. The safety property is unchanged and is the 2026-05-10 incident
//! from HANDOFF_WS2_MESH_FANOUT.md: a user pasted an invite into a daemon that
//! was already in a mesh, the handshake failed, and the original mesh was gone
//! with no local recovery path.
//!
//! What changed is the answer. The old gate REFUSED a populated mesh
//! (`MeshError::AlreadyInPopulatedMesh`) and auto-LEFT a solo one — `leave()`
//! calls `persist::clear()`, which deletes the outgoing `mesh.json` before the
//! handshake runs. That protected populated meshes and destroyed solo ones, and
//! it is why a second membership could not exist outside tests: the switcher
//! (`known_meshes`, `mesh list|switch|forget`, the desktop `MeshList`) was
//! complete and unreachable, because nothing in production ever produced the
//! second membership it switches between.
//!
//! Parking is strictly stronger. Nothing is deleted in either case, so the
//! incident cannot recur for populated OR solo meshes, and the mesh set down is
//! exactly the one `svrn mesh switch` resumes.
//!
//! The discriminating observable is MESH ID CONTINUITY. Under auto-leave, a
//! failed join from a solo mesh destroyed it and rolled back by minting a
//! *fresh* solo mesh with a new id. Under parking the id must survive
//! unchanged, because the rollback resumes the parked mesh rather than
//! re-soloing. `same_mesh_id_survives_*` below fails against the old behaviour
//! for that reason.
//!
//! Not covered here: a SUCCESSFUL join producing two memberships.
//! `EmbeddedDaemon::join_mesh` never passes `direct_peer_hint`, so discovery is
//! iroh/mDNS only and cannot be aimed at an in-process router. That assertion
//! lives in the plan's two-machine verification, step 2.
use std::time::Duration;

use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::{MemberRecord, NodeStatus};
use commonwealth_discovery::membership;
use sovereign_core::setup_config::SetupConfig;
use sovereign_mesh::daemon::{EmbeddedDaemon, MeshError};
use sovereign_mesh::deep_link;
use sovereign_mesh::persist;

mod common;
use common::{empty_capabilities, mesh_admin_services};

/// A join aimed at a mesh no peer here serves: the discovery scan finds
/// nothing and the handshake errors, which is what drives the rollback.
fn unreachable_invite() -> deep_link::DeepLink {
    deep_link::DeepLink::Join {
        join_key: membership::generate_join_key(),
        mesh_name: Some("nowhere".into()),
        relay_hint: None,
        iroh_dial: None,
        encrypted: false,
        expires_at: None,
    }
}

async fn add_peer(daemon: &EmbeddedDaemon, name: &str) {
    let state = daemon.app_state().await.expect("app_state");
    let peer_id = NodeId::from_u128(0x9911_2233_4455_6677);
    let mut mesh = state.inner.mesh.write().await;
    mesh.members.insert(
        peer_id,
        MemberRecord {
            removed_at: None,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
            node_id: peer_id,
            name: name.into(),
            invited_by: peer_id,
            joined_at: 0,
            last_seen: 100,
            status: NodeStatus::Online,
            capabilities: empty_capabilities(),
            addresses: vec!["127.0.0.1:50003".parse().unwrap()],
        },
    );
}

/// The headline: a solo mesh is PARKED, not destroyed. Fails against
/// auto-leave, which cleared `mesh.json` and rolled back into a brand-new solo
/// mesh carrying a different id.
#[tokio::test]
async fn same_mesh_id_survives_a_failed_join_from_a_solo_mesh() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    daemon.create_mesh("solo-mesh", "founder").await.unwrap();
    let before = persist::active_mesh_id(tmp.path()).expect("a mesh is active after create");

    let result = tokio::time::timeout(
        Duration::from_secs(15),
        daemon.join_mesh(&unreachable_invite(), "joiner"),
    )
    .await
    .expect("join_mesh returns well within the timeout");
    assert!(result.is_err(), "a join against an unreachable peer must fail");

    let after = persist::active_mesh_id(tmp.path()).expect("still a mesh after the failed join");
    assert_eq!(
        before, after,
        "the mesh we set down must be the mesh we come back to. A different id \
         means the original was destroyed and a fresh solo minted in its place \
         — the clobber this file exists to prevent"
    );
    assert!(
        daemon.is_running().await,
        "a failed join must leave a running mesh, not strand the client API on :9741"
    );
}

/// Same property with peers present. The old code took a different branch here
/// (refuse, rather than auto-leave), so both need pinning.
#[tokio::test]
async fn same_mesh_id_survives_a_failed_join_from_a_populated_mesh() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    daemon.create_mesh("populated-mesh", "founder").await.unwrap();
    add_peer(&daemon, "synthetic-peer").await;
    let before = persist::active_mesh_id(tmp.path()).expect("a mesh is active");

    let result = tokio::time::timeout(
        Duration::from_secs(15),
        daemon.join_mesh(&unreachable_invite(), "joiner"),
    )
    .await
    .expect("join_mesh returns well within the timeout");
    assert!(result.is_err(), "a join against an unreachable peer must fail");

    assert_eq!(
        Some(before),
        persist::active_mesh_id(tmp.path()),
        "a populated mesh must survive a failed join unchanged"
    );
}

/// The bytes, not just the pointer. `persist::clear` deleting these mid-join is
/// the literal mechanism of the 2026-05-10 incident.
#[tokio::test]
async fn on_disk_state_survives_a_failed_join() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    daemon.create_mesh("populated-mesh", "founder").await.unwrap();
    add_peer(&daemon, "synthetic-peer").await;

    let mesh_json = persist::mesh_file(tmp.path());
    let key_file = persist::join_key_file(tmp.path());
    let mesh_before = std::fs::read(&mesh_json).expect("mesh.json written by create_mesh");
    let key_before = std::fs::read(&key_file).expect("join_key.secret written by create_mesh");

    let _ = tokio::time::timeout(
        Duration::from_secs(15),
        daemon.join_mesh(&unreachable_invite(), "joiner"),
    )
    .await
    .expect("join_mesh returns well within the timeout");

    assert_eq!(
        mesh_before,
        std::fs::read(&mesh_json).expect("mesh.json must still exist"),
        "mesh.json must survive a failed join byte-for-byte"
    );
    assert_eq!(
        key_before,
        std::fs::read(&key_file).expect("join_key.secret must still exist"),
        "the parked mesh keeps its own invite — it is what `mesh switch` resumes with"
    );
}

/// The refusal is gone on purpose. A populated mesh is no longer a reason to
/// decline a join, because parking removes the risk the refusal was managing.
#[tokio::test]
async fn a_populated_mesh_is_no_longer_a_reason_to_refuse() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    daemon.create_mesh("populated-mesh", "founder").await.unwrap();
    add_peer(&daemon, "synthetic-peer").await;

    let result = tokio::time::timeout(
        Duration::from_secs(15),
        daemon.join_mesh(&unreachable_invite(), "joiner"),
    )
    .await
    .expect("join_mesh returns well within the timeout");

    // It still fails — the peer is unreachable — but on the HANDSHAKE, never
    // on a membership precondition.
    match result {
        Err(MeshError::Network(_)) => {}
        Err(other) => panic!(
            "expected the join to fail at the handshake, not to be refused for \
             already being in a mesh: {other:?}"
        ),
        Ok(_) => panic!("the peer is unreachable; this join cannot succeed"),
    }
}
