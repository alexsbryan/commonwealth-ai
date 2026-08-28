// SPDX-License-Identifier: AGPL-3.0-or-later
//! `join_key.secret` survives daemon restart and round-trips through
//! `current_invite`.
//!
//! Why this matters: the founder's invite link is reconstructed at
//! display time from the cached plaintext key. `Mesh.invite_key_hash`
//! is one-way, so once the in-memory copy is dropped (restart, crash,
//! or process kill), nobody can reconstruct the link — and the
//! founder's mesh share UI silently goes blank.
//!
//! The contract is documented at `daemon.rs::current_invite`:
//!
//!   "Reconstructed on demand from the cached key + the current mesh
//!    name via build_join_link, so a mesh rename ... is automatically
//!    picked up without invalidating the secret file."
//!
//! Three assertions across two tests:
//!
//! 1. **Restart preserves the invite.** `create_mesh` → record key →
//!    drop → fresh daemon against same data_dir → `try_resume` →
//!    `current_invite` returns the same key + a still-valid link.
//! 2. **`leave` clears the secret.** After `leave`, the `join_key.secret`
//!    file must be gone — leaving it around would let a stale invite
//!    link from a prior mesh re-appear in the next mesh's share UI.
//! 3. **Resume with no `join_key.secret` is non-fatal.** Pre-feature
//!    persisted meshes have no cached key; `try_resume` must succeed
//!    anyway and `current_invite` must return `None`. The share UI
//!    handles `None` by hiding the invite card; a regression that
//!    panicked or returned an Err here would brick the resume path.
mod common;
use common::mesh_admin_services;

use std::time::Duration;

use sovereign_core::setup_config::SetupConfig;
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::persist;

#[tokio::test]
async fn join_key_persists_across_restart_and_current_invite_returns_same_key() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();

    // Round 1: create the mesh, snapshot the invite (key + link).
    let (key_before, link_before) = {
        let daemon = EmbeddedDaemon::new(
            data_dir.clone(),
            SetupConfig::unconfigured(),
            mesh_admin_services(),
        );
        let create = daemon
            .create_mesh("join-key restart test", "founder")
            .await
            .expect("create_mesh succeeds against a fresh data_dir");
        // The CreateMeshResult also carries the key; we'll compare
        // it against current_invite to confirm both paths agree.
        let invite = daemon
            .current_invite()
            .await
            .expect("current_invite must be present right after create_mesh");
        // Sanity: the key from current_invite matches the key from
        // CreateMeshResult — otherwise the cache write didn't fire.
        assert_eq!(
            invite.0, create.join_key,
            "current_invite key MUST equal the CreateMeshResult key — \
             a mismatch means the join_key_plaintext cache was \
             populated from a different source than persist::save_join_key"
        );
        // Also confirm the on-disk file holds the same content.
        let on_disk = persist::load_join_key(&data_dir)
            .expect("load_join_key must not error after create_mesh")
            .expect(
                "join_key.secret MUST be on disk after create_mesh — \
                     otherwise the next process restart loses the invite forever",
            );
        assert_eq!(
            on_disk, create.join_key,
            "on-disk join_key.secret MUST match the in-memory key"
        );
        daemon.shutdown().await.expect("graceful shutdown");
        tokio::time::sleep(Duration::from_millis(50)).await;
        (invite.0, invite.1)
    };

    // Round 2: fresh daemon, same data_dir → resume → check invite
    // is back. The resume path explicitly calls `persist::load_join_key`
    // to restore the in-memory cache.
    let daemon = EmbeddedDaemon::new(data_dir, SetupConfig::unconfigured(), mesh_admin_services());
    let resumed = daemon
        .try_resume()
        .await
        .expect("try_resume must not error on a valid data_dir");
    assert!(
        resumed,
        "try_resume must return true when mesh.json + join_key.secret \
         + node_id all exist on disk"
    );

    let invite_after = daemon.current_invite().await.expect(
        "current_invite must be Some after try_resume restored a \
             cached join_key — a None here means the share UI would \
             silently go blank after a routine restart",
    );
    assert_eq!(
        invite_after.0, key_before,
        "plaintext join_key MUST round-trip across restart — \
         pre={key_before:?} post={:?}",
        invite_after.0
    );
    // The link is reconstructed from key + mesh name; it should be
    // byte-identical since neither input changed across the restart.
    assert_eq!(
        invite_after.1, link_before,
        "reconstructed join_link MUST match the original — \
         a mismatch here means deep_link::build_join_link is \
         non-deterministic or the mesh name didn't round-trip"
    );

    daemon.shutdown().await.expect("graceful shutdown");
}

#[tokio::test]
async fn leave_clears_join_key_secret_so_next_mesh_does_not_inherit_stale_invite() {
    // The reverse contract: `leave` MUST wipe join_key.secret along
    // with mesh.json. Leaving the secret around would let a brand-new
    // mesh accidentally surface the *previous* mesh's invite link in
    // its share UI — at best confusing, at worst a security regression
    // (the old key would still work against any peer that hadn't yet
    // observed the leave).
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();

    let daemon = EmbeddedDaemon::new(
        data_dir.clone(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    daemon
        .create_mesh("pre-leave mesh", "node-A")
        .await
        .expect("create_mesh succeeds");
    let key_file = persist::join_key_file(&data_dir);
    assert!(
        key_file.exists(),
        "create_mesh should write {} — that's the precondition for \
         the leave-clears-it half of the contract",
        key_file.display()
    );

    daemon.leave().await.expect("leave succeeds");

    assert!(
        !key_file.exists(),
        "leave() MUST remove {} — leaving it around would surface a \
         stale invite link in the next mesh's share UI",
        key_file.display()
    );
    // current_invite must also report None after leave — the in-memory
    // cache is the other half of the contract, and shouldn't outlive
    // the on-disk file.
    let post_leave_invite = daemon.current_invite().await;
    assert!(
        post_leave_invite.is_none(),
        "current_invite MUST be None after leave; got: {:?}",
        post_leave_invite
    );
}

#[tokio::test]
async fn resume_with_missing_join_key_secret_is_non_fatal() {
    // Backwards-compat contract: pre-feature persisted meshes
    // (mesh.json present, no join_key.secret) must still resume
    // cleanly. The current_invite returns None in this case so the
    // share UI hides the invite card; rotate is the recovery path.
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();

    // Set up: create a mesh, then surgically remove the
    // join_key.secret file. Simulates a daemon restored from a
    // pre-feature backup.
    {
        let daemon = EmbeddedDaemon::new(
            data_dir.clone(),
            SetupConfig::unconfigured(),
            mesh_admin_services(),
        );
        daemon
            .create_mesh("pre-feature mesh", "founder")
            .await
            .expect("create_mesh succeeds");
        daemon.shutdown().await.expect("graceful shutdown");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let key_file = persist::join_key_file(&data_dir);
    std::fs::remove_file(&key_file)
        .expect("test setup: remove join_key.secret to simulate pre-feature state");

    // Resume should succeed, current_invite should return None.
    let daemon = EmbeddedDaemon::new(data_dir, SetupConfig::unconfigured(), mesh_admin_services());
    let resumed = daemon.try_resume().await.expect(
        "try_resume MUST succeed even when join_key.secret is missing — \
                 panicking here would brick every pre-feature backup",
    );
    assert!(resumed, "try_resume must still return true");
    assert!(
        daemon.current_invite().await.is_none(),
        "current_invite MUST return None when no cached key exists — \
         the share UI handles None by hiding the invite card; a stale \
         value here would render a bogus link"
    );

    daemon.shutdown().await.expect("graceful shutdown");
}
