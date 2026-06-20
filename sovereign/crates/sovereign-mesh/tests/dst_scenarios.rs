// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic-simulation mesh scenarios. Gated by the `dst` feature:
//!
//!   cargo test -p sovereign-mesh --features dst --test dst_scenarios
//!
//! Without the feature this is an empty test binary, so the default workspace
//! `cargo test` neither compiles nor runs it.
#![cfg(feature = "dst")]

use sovereign_mesh::dst::{check_all, DstMesh, Quiescence};

/// PR1 smoke test: two nodes converge a mesh_store key via the **real** gossip
/// path (`run_one_round`) — not the `sync_mesh_state` broadcast shortcut — and
/// the default invariant pack holds at quiescence.
///
/// This proves the whole PR1 scaffold end-to-end: the per-node `FaultTransport`
/// install seam carries gossip; the injected `TestClock` keeps the harness's
/// epoch-0 member records live (so nothing decays); and the in-process
/// invariant snapshot reads cleanly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dst_two_nodes_converge_mesh_store_key() {
    let dst = DstMesh::start(2).await;

    // Node 0 writes a key; emergent gossip must carry it to node 1.
    dst.store_set(0, "dst-smoke", "k", b"hello-mesh");

    let q = dst.gossip_until_quiescent(8).await;
    assert!(
        matches!(q, Quiescence::Converged { .. }),
        "member views did not converge: {q:?}"
    );

    // The key propagated over the wire to node 1's mesh_store.
    assert_eq!(
        dst.store_get(1, "dst-smoke", "k").as_deref(),
        Some(b"hello-mesh".as_ref()),
        "mesh_store key did not converge to node 1 via real gossip"
    );

    // The default invariant pack holds at quiescence.
    let violations = check_all(&dst.snapshot().await);
    assert!(violations.is_empty(), "invariant violations: {violations:?}");
}
