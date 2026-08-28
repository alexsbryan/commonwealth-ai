// SPDX-License-Identifier: AGPL-3.0-or-later
//! `rotate_invite` must refuse while any online peer could still be
//! partitioned by the rotation.
//!
//! This is the gate the credential split turns on. A peer running a pre-split
//! build authorizes gossip on `invite_key_hash` (the compat arm in
//! `Mesh::gossip_authorized`), so rotating that hash drops exactly those peers
//! — silently, symmetrically, with nothing red on either side. That is the
//! original bug, re-entering through the upgrade window rather than the
//! steady state.
//!
//! The guard shipped INERT: it filtered on `mesh.mesh_secret`, our OWN
//! credential, which is non-zero on every migrated node, so the predicate was
//! always false and the refusal never fired. Nothing caught it because nothing
//! tested it — the refusal variant appeared in no test in the workspace
//! (ARCH §18.1: a check with no failing input you can name is not a check).
//!
//! Each test below fails against that inert filter.

use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::{MemberRecord, NodeStatus};
use sovereign_core::setup_config::SetupConfig;
use sovereign_mesh::daemon::{EmbeddedDaemon, MeshError};

mod common;
use common::{empty_capabilities, mesh_admin_services};

/// Stand up a daemon holding one mesh plus a synthetic ONLINE peer.
///
/// Injecting the member directly is sound here for the same reason
/// `join_parks_not_leaves` does it: the guard's inputs are member status and our own
/// recorded observation of that member's build, never the provenance of the
/// record.
async fn daemon_with_online_peer(
    peer_name: &str,
) -> (std::sync::Arc<EmbeddedDaemon>, NodeId, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    daemon.create_mesh("rotate-guard-mesh", "founder").await.unwrap();

    let state = daemon.app_state().await.expect("app_state after create_mesh");
    let peer_id = NodeId::from_u128(0x5150_6060_7070_8080);
    {
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
                name: peer_name.into(),
                invited_by: peer_id,
                joined_at: 0,
                last_seen: 100,
                status: NodeStatus::Online,
                capabilities: empty_capabilities(),
                addresses: vec!["127.0.0.1:50002".parse().unwrap()],
            },
        );
    }
    (daemon, peer_id, tmp)
}

/// The headline case. A peer we have never confirmed post-split is online, so
/// rotating would partition it. Refuse, and NAME it — an operator cannot act
/// on "some peer".
///
/// It lands in `unconfirmed`, not `pre_split`: this peer is unreachable, so the
/// confirmation round `rotate_invite` runs first learns nothing about it, and
/// "we could not ask" is not "we asked and it is old".
#[tokio::test]
async fn rotate_refuses_while_an_unconfirmed_peer_is_online() {
    let (daemon, _peer_id, _tmp) = daemon_with_online_peer("stale-peer").await;

    match daemon.rotate_invite(false).await {
        Err(MeshError::RotateWouldPartition {
            pre_split,
            unconfirmed,
        }) => {
            assert!(
                unconfirmed.iter().any(|p| p == "stale-peer"),
                "the refusal must name the peer that blocked it, got {unconfirmed:?}"
            );
            assert!(
                pre_split.is_empty(),
                "a peer we never reached must not be reported as an old BUILD — \
                 that sends the operator hunting for a node to upgrade that may \
                 not exist, got {pre_split:?}"
            );
        }
        other => panic!("expected RotateWouldPartition, got {other:?}"),
    }
}

/// The wording defect this split exists to fix, stated as an assertion: the two
/// populations get different prose, because they need different actions.
#[tokio::test]
async fn the_refusal_tells_an_unconfirmed_peer_apart_from_an_old_build() {
    let (daemon, peer_id, _tmp) = daemon_with_online_peer("old-build").await;
    let state = daemon.app_state().await.unwrap();
    state.observe_peer_split_generation(peer_id, false);

    let message = daemon
        .rotate_invite(false)
        .await
        .expect_err("an observed pre-split peer blocks rotation")
        .to_string();
    assert!(
        message.contains("pre-split build") && message.contains("upgrade them first"),
        "an observed OLD BUILD must be told to upgrade, got: {message}"
    );

    let (daemon, _peer_id, _tmp) = daemon_with_online_peer("never-reached").await;
    let message = daemon
        .rotate_invite(false)
        .await
        .expect_err("an unconfirmed peer blocks rotation")
        .to_string();
    assert!(
        message.contains("not been confirmed since this daemon started"),
        "an UNCONFIRMED peer must not be reported as an old build — that is the \
         overclaim, and after a restart it describes the whole fleet. Got: {message}"
    );
    assert!(
        !message.contains("pre-split build"),
        "…and it must not say pre-split at all. Got: {message}"
    );
}

/// Unknown is not "safe". A peer we have positively observed as PRE-split is
/// the same refusal — this pins that the guard reads the recorded observation
/// rather than merely the absence of one.
#[tokio::test]
async fn rotate_refuses_a_peer_confirmed_pre_split() {
    let (daemon, peer_id, _tmp) = daemon_with_online_peer("old-build").await;
    let state = daemon.app_state().await.unwrap();
    state.observe_peer_split_generation(peer_id, false);

    assert!(
        matches!(
            daemon.rotate_invite(false).await,
            Err(MeshError::RotateWouldPartition { .. })
        ),
        "a peer observed sending no mesh_secret must block rotation"
    );
}

/// The other direction, and the one that makes the guard usable: once every
/// online peer is confirmed post-split, rotation proceeds. A guard that can
/// never be satisfied would just push everyone to `--force`.
#[tokio::test]
async fn rotate_proceeds_once_every_online_peer_is_confirmed_post_split() {
    let (daemon, peer_id, _tmp) = daemon_with_online_peer("upgraded-peer").await;
    let state = daemon.app_state().await.unwrap();
    state.observe_peer_split_generation(peer_id, true);

    let rotated = daemon
        .rotate_invite(false)
        .await
        .expect("a fully upgraded fleet rotates without --force");
    assert!(
        !rotated.join_key.is_empty(),
        "a successful rotation must hand back the new invite"
    );
}

/// `--force` is the documented override. It must still work while a peer is
/// unconfirmed — that is the whole point of typing it.
#[tokio::test]
async fn force_rotates_even_with_an_unconfirmed_peer_online() {
    let (daemon, _peer_id, _tmp) = daemon_with_online_peer("stale-peer").await;

    daemon
        .rotate_invite(true)
        .await
        .expect("--force overrides the pre-split refusal");
}

/// An upgrade must be able to CLEAR the flag. If a peer's first observed round
/// were sticky, one pre-split round would block rotation for the life of the
/// daemon even after that peer upgraded.
#[tokio::test]
async fn a_peer_that_upgrades_stops_blocking_rotation() {
    let (daemon, peer_id, _tmp) = daemon_with_online_peer("upgrading-peer").await;
    let state = daemon.app_state().await.unwrap();

    state.observe_peer_split_generation(peer_id, false);
    assert!(daemon.rotate_invite(false).await.is_err(), "blocked while old");

    state.observe_peer_split_generation(peer_id, true);
    daemon
        .rotate_invite(false)
        .await
        .expect("the peer upgraded; the refusal must lift");
}
