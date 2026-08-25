// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pins that a daemon's `self_node_id` survives a restart against
//! the same `data_dir`.
//!
//! The contract documented at `persist::load_or_generate_self_node_id`
//! and the comment block at `daemon.rs::create_mesh` (line 644: "Use
//! this install's stable NodeId (persisted at `<data_dir>/node_id`).
//! Without this, every `create_mesh` would stamp a fresh random ID,
//! so rejoining users would appear as new peers every time their
//! mesh.json got wiped.") is load-bearing for mesh continuity:
//!
//! - **Peer identity stability.** A user's node_id is what every
//!   peer indexes them under. If create_mesh re-rolls the id on
//!   restart, the user shows up as a new member, the founder's
//!   member list grows zombies, and gossip races on the duplicate.
//! - **Persistence reload after `mesh leave`.** Leaving the mesh
//!   clears `mesh.json` and `join_key.secret` but deliberately
//!   keeps `node_id` so the user comes back under the same
//!   identity. A regression that wipes `node_id` on leave would
//!   silently mint a stranger on the next join.
//!
//! Two assertions across two test pairs:
//!
//! 1. **Restart preserves id.** Build a daemon → `create_mesh` →
//!    record self_node_id → drop → build a *new* daemon against
//!    the same data_dir → `try_resume` → assert id matches.
//! 2. **`leave` preserves id.** Build a daemon → create → leave
//!    → confirm the on-disk `node_id` file still exists → build
//!    a new daemon against the same data_dir → `create_mesh` →
//!    assert id matches the pre-leave id.
use std::time::Duration;

use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_core::setup_config::SetupConfig;
use sovereign_mesh::DaemonServices;

#[tokio::test]
async fn node_id_survives_daemon_restart_against_same_data_dir() {
    let tmp = tempfile::tempdir().unwrap();

    // Round 1: create the mesh, record the node_id, drop everything.
    let id_before = {
        let daemon = EmbeddedDaemon::new(tmp.path().to_path_buf(), SetupConfig::unconfigured(), DaemonServices::MeshAdmin);
        daemon
            .create_mesh("persistence test", "node-A")
            .await
            .expect("create_mesh succeeds against a fresh data_dir");
        let id = daemon
            .self_node_id()
            .await
            .expect("self_node_id present after create_mesh");
        daemon.shutdown().await.expect("graceful shutdown");
        // Give the bound tokio::spawn'd listener a tick to release
        // its tasks before we drop the daemon. Not load-bearing for
        // the assertion, just keeps the test output clean.
        tokio::time::sleep(Duration::from_millis(50)).await;
        id
    };

    // Round 2: build a fresh daemon against the same data_dir and
    // resume. The resume path calls `persist::load_or_generate_self_node_id`,
    // which must find the file written in round 1.
    let daemon = EmbeddedDaemon::new(tmp.path().to_path_buf(), SetupConfig::unconfigured(), DaemonServices::MeshAdmin);
    let resumed = daemon
        .try_resume()
        .await
        .expect("try_resume must not error on a valid data_dir");
    assert!(
        resumed,
        "try_resume must return true when mesh.json + node_id exist on disk"
    );

    let id_after = daemon
        .self_node_id()
        .await
        .expect("self_node_id must be readable after try_resume");
    assert_eq!(
        id_before, id_after,
        "node_id must round-trip across restart; pre={id_before:?} post={id_after:?}. \
         A mismatch here means the user would re-enter the mesh as a new identity, \
         leaving a zombie in every peer's member list."
    );

    daemon.shutdown().await.expect("graceful shutdown");
}

#[tokio::test]
async fn node_id_survives_mesh_leave_and_is_reused_on_next_create() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();

    // Pre-leave: create + record id.
    let id_before = {
        let daemon = EmbeddedDaemon::new(data_dir.clone(), SetupConfig::unconfigured(), DaemonServices::MeshAdmin);
        daemon
            .create_mesh("pre-leave mesh", "node-A")
            .await
            .expect("create_mesh succeeds");
        let id = daemon.self_node_id().await.expect("self id present");
        // Explicit leave (NOT shutdown): wipes mesh.json + join_key.secret
        // but is contracted to leave node_id in place.
        daemon.leave().await.expect("leave succeeds");
        id
    };

    // Sanity: the contract says `leave` keeps `node_id` on disk so
    // a future `mesh join` or `mesh create` reuses the identity.
    let node_id_path = data_dir.join("node_id");
    assert!(
        node_id_path.exists(),
        "leave() must NOT remove {} — that file is the user's identity \
         continuity across mesh changes (see daemon.rs::create_mesh)",
        node_id_path.display()
    );

    // Post-leave: build a new daemon, create a new mesh, assert the
    // founder id matches the pre-leave value.
    let daemon = EmbeddedDaemon::new(data_dir, SetupConfig::unconfigured(), DaemonServices::MeshAdmin);
    daemon
        .create_mesh("post-leave mesh", "node-A-still")
        .await
        .expect("create_mesh succeeds on a leftover-but-no-mesh data_dir");
    let id_after = daemon
        .self_node_id()
        .await
        .expect("self id present after second create_mesh");
    assert_eq!(
        id_before, id_after,
        "post-leave create_mesh must reuse the persisted node_id"
    );

    daemon.shutdown().await.expect("graceful shutdown");
}
