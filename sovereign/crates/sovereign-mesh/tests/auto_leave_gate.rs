// SPDX-License-Identifier: AGPL-3.0-or-later
//! `MeshError::AlreadyInPopulatedMesh` — the safety gate that
//! refuses to clobber a real mesh's persisted state when `join_mesh`
//! is called against an already-joined daemon.
//!
//! Why this gate exists (from `daemon.rs::join_mesh`):
//!
//!   "`self.leave()` calls `persist::clear()` which deletes mesh.json
//!    AND join_key.secret from disk BEFORE the handshake runs. If
//!    the handshake then fails (bad key, no peer accepting, network
//!    blip, daemon listener fails to re-bind), the user is left
//!    without the original mesh on disk."
//!
//! The 2026-05-10 incident referenced in HANDOFF_WS2_MESH_FANOUT.md
//! is the canonical example: a user pasted an invite into a daemon
//! that was already in a populated mesh, the handshake failed, and
//! the original mesh was gone with no local recovery path.
//!
//! Two assertions:
//!
//! 1. **Populated mesh refuses join_mesh and preserves on-disk state.**
//!    The destructive `persist::clear()` MUST NOT run; mesh.json and
//!    join_key.secret must still be on disk after the call.
//! 2. **Solo mesh accepts the auto-leave path.** The gate's
//!    docstring explicitly carves out single-member meshes ("solo
//!    case is harmless to auto-leave") — a regression that tightened
//!    the gate to also reject solos would break the post-`setup`
//!    bootstrap flow where users paste an invite into a solo auto-
//!    created mesh.
use std::time::Duration;

use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::{MemberRecord, NodeStatus};
use commonwealth_discovery::membership;
use sovereign_mesh::daemon::{EmbeddedDaemon, MeshError};
use sovereign_mesh::deep_link;
use sovereign_mesh::persist;

mod common;
use common::empty_capabilities;

#[tokio::test]
async fn join_mesh_against_populated_mesh_errors_and_preserves_on_disk_state() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    let daemon = EmbeddedDaemon::new(data_dir.clone());

    daemon
        .create_mesh("populated-mesh test", "founder")
        .await
        .expect("create_mesh succeeds on a fresh data_dir");

    // Inject a synthetic second member directly into the AppState's
    // mesh. Skipping the real /internal/join handshake here is fine
    // because the auto-leave gate's only input is `mesh.members.len()`
    // — it never validates the *origin* of those members. Adding one
    // by hand exercises the same code path a real peer admission would.
    let state = daemon
        .app_state()
        .await
        .expect("app_state after create_mesh");
    let peer_id = NodeId::from_u128(0x2222_3333_4444_5555);
    let peer_addr = "127.0.0.1:50001".parse().unwrap();
    {
        let mut mesh = state.inner.mesh.write().await;
        mesh.members.insert(
            peer_id,
            MemberRecord {
                node_id: peer_id,
                name: "synthetic-peer".into(),
                invited_by: peer_id,
                joined_at: 0,
                last_seen: 100,
                status: NodeStatus::Online,
                capabilities: empty_capabilities(),
                addresses: vec![peer_addr],
            },
        );
        assert_eq!(
            mesh.members.len(),
            2,
            "test setup: post-inject mesh must have exactly 2 members \
             so the auto-leave gate's `members > 1` branch fires"
        );
    }

    // Snapshot the on-disk state we expect the failed join to preserve.
    let mesh_file_before = persist::mesh_file(&data_dir);
    let key_file_before = persist::join_key_file(&data_dir);
    assert!(
        mesh_file_before.exists(),
        "test precondition: mesh.json must exist after create_mesh"
    );
    assert!(
        key_file_before.exists(),
        "test precondition: join_key.secret must exist after create_mesh"
    );
    let mesh_bytes_before = std::fs::read(&mesh_file_before).unwrap();
    let key_bytes_before = std::fs::read(&key_file_before).unwrap();

    // Build a totally unrelated join link. The gate fires before the
    // join key is even validated, so it doesn't matter that this key
    // points nowhere — the assertion is that the gate intercepts
    // BEFORE persist::clear runs.
    let foreign_key = membership::generate_join_key();
    let foreign_link = deep_link::DeepLink::Join {
        join_key: foreign_key,
        mesh_name: Some("hypothetical-other-mesh".into()),
        relay_hint: None,
    };

    let result = daemon.join_mesh(&foreign_link, "new-node-name").await;
    match result {
        Err(MeshError::AlreadyInPopulatedMesh { mesh_name, members }) => {
            assert_eq!(
                mesh_name, "populated-mesh test",
                "error must name the CURRENT mesh, not the inbound one"
            );
            assert_eq!(
                members, 2,
                "error must report the current member count so the \
                 caller can confirm what they'd be destroying"
            );
        }
        Err(other) => panic!(
            "expected AlreadyInPopulatedMesh; got {other:?}. \
             A different error means the gate is missing or has the \
             wrong arm — the destructive persist::clear may still have run."
        ),
        Ok(_) => panic!(
            "join_mesh MUST refuse against a populated mesh. \
             A success here means the daemon would have wiped \
             mesh.json + join_key.secret in pursuit of a new mesh, \
             leaving no local-only recovery path on handshake failure."
        ),
    }

    // The whole point of the gate: on-disk state untouched.
    assert!(
        mesh_file_before.exists(),
        "mesh.json MUST remain on disk after a refused join_mesh — \
         the gate's sole purpose is preventing `persist::clear()` from \
         destroying it. A missing file here means the gate fired AFTER \
         clear ran, which is identical to no gate at all."
    );
    assert!(
        key_file_before.exists(),
        "join_key.secret MUST remain on disk after a refused join_mesh \
         — same rationale as mesh.json above."
    );
    assert_eq!(
        std::fs::read(&mesh_file_before).unwrap(),
        mesh_bytes_before,
        "mesh.json contents MUST be byte-identical to before the \
         refused join_mesh — a content change means SOMETHING wrote to \
         it during the failed call"
    );
    assert_eq!(
        std::fs::read(&key_file_before).unwrap(),
        key_bytes_before,
        "join_key.secret contents MUST be byte-identical to before the \
         refused join_mesh"
    );

    // And the live mesh state is also unchanged.
    let mesh = state.inner.mesh.read().await;
    assert_eq!(
        mesh.members.len(),
        2,
        "in-memory mesh members MUST be unchanged after a refused join_mesh; \
         got {}",
        mesh.members.len()
    );
    assert_eq!(
        mesh.name, "populated-mesh test",
        "in-memory mesh name MUST be unchanged after a refused join_mesh"
    );
    drop(mesh);

    daemon.shutdown().await.expect("graceful shutdown");
}

#[tokio::test]
async fn join_mesh_against_solo_mesh_passes_the_gate_and_attempts_handshake() {
    // The docstring carve-out: a freshly-created solo mesh (members == 1)
    // SHOULD auto-leave so the user's first paste-an-invite UX works.
    // Pinning this means future tightening of the gate that breaks
    // the post-`setup` bootstrap flow fails this test.
    //
    // We can't exercise a real handshake against a nonexistent peer,
    // so the assertion shape is: "the gate did NOT fire" — proven by
    // getting an error type OTHER than AlreadyInPopulatedMesh
    // (Network, after the handshake attempt fails to reach any peer).
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(tmp.path().to_path_buf());
    daemon
        .create_mesh("solo-mesh test", "founder")
        .await
        .expect("create_mesh succeeds");

    // Members == 1 (just the founder); gate's auto-leave branch should fire.
    let state = daemon.app_state().await.unwrap();
    assert_eq!(
        state.inner.mesh.read().await.members.len(),
        1,
        "test precondition: solo mesh has exactly one member"
    );

    // Use a valid-format join_key (so the gate gets past format
    // validation) but a key the foreign mesh doesn't know about.
    // After the gate passes, perform_join scans mDNS for matching
    // peers; in the test environment it finds none and errors with
    // Network. The key here is that the error is NOT
    // AlreadyInPopulatedMesh.
    let foreign_key = membership::generate_join_key();
    let foreign_link = deep_link::DeepLink::Join {
        join_key: foreign_key,
        mesh_name: Some("hypothetical-target".into()),
        relay_hint: None,
    };

    // We expect an error (the join handshake has no peer to talk to),
    // but it MUST NOT be AlreadyInPopulatedMesh.
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        daemon.join_mesh(&foreign_link, "joiner-name"),
    )
    .await;
    match result {
        Ok(Err(MeshError::AlreadyInPopulatedMesh { .. })) => panic!(
            "the auto-leave gate MUST NOT fire on solo meshes. \
             The post-`setup` bootstrap creates a solo auto-mesh on \
             daemon start, and the user's first invite-paste flow \
             depends on auto-leave handling it. Tightening this gate \
             to also reject solos breaks the first-run UX."
        ),
        Ok(Err(_)) | Ok(Ok(_)) | Err(_) => {
            // Any other outcome (timeout, network error, handshake
            // failure, even a happy success against a coincidental
            // local peer) means the gate let us past — which is the
            // assertion this test is making.
        }
    }
}
