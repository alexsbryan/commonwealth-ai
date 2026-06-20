// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic-simulation mesh scenarios. Gated by the `dst` feature:
//!
//!   cargo test -p sovereign-mesh --features dst --test dst_scenarios
//!
//! Without the feature this is an empty test binary, so the default workspace
//! `cargo test` neither compiles nor runs it.
#![cfg(feature = "dst")]

use sovereign_mesh::dst::{check_all, DstMesh, FaultSchedule, Quiescence};

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

/// A crashed peer decays to offline on the survivors and is never shown as a
/// live ghost — exercises offline-decay (A) + the no-ghost invariant under a
/// real `shutdown` + advancing clock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn downed_peer_decays_then_no_ghost() {
    let mut dst = DstMesh::start(3).await;
    // Healthy: everyone observes everyone (local-contact stamped at the base clock).
    assert!(matches!(
        dst.gossip_until_quiescent(8).await,
        Quiescence::Converged { .. }
    ));
    // Crash node 2 (real server shutdown + policy down).
    dst.crash(2);
    // Advance every clock past the offline threshold; node 2 is never re-observed.
    dst.clock().advance(3601);
    for _ in 0..3 {
        dst.sweep().await;
    }
    let snap = dst.snapshot().await;
    let violations = check_all(&snap);
    assert!(
        violations.is_empty(),
        "crashed peer should decay cleanly, no ghost: {violations:?}"
    );
}

/// A node whose wall-clock runs hours ahead must NOT flap its peers (or itself)
/// Offline — the DST-level proof that offline-decay measures local-observation
/// staleness, not the peer's gossiped `last_seen` (fix A / the ~9-min flap).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clock_skew_does_not_false_decay() {
    let dst = DstMesh::start(3).await;
    dst.skew_node(1, 7200); // node 1's clock +2h
    assert!(matches!(
        dst.gossip_until_quiescent(10).await,
        Quiescence::Converged { .. }
    ));
    let violations = check_all(&dst.snapshot().await);
    assert!(
        violations.is_empty(),
        "clock skew caused a false decay / split: {violations:?}"
    );
}

/// Partition the mesh, let cross-group peers decay, heal, and require clean
/// reconvergence — the load-bearing distributed-systems test the dead library
/// gossip's convergence tests only *pretended* to cover.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partition_then_heal_reconverges() {
    let dst = DstMesh::start(4).await;
    assert!(matches!(
        dst.gossip_until_quiescent(8).await,
        Quiescence::Converged { .. }
    ));
    let ids = dst.node_ids();
    // Split {0,1} | {2,3}.
    {
        let mut p = dst.policy().write().unwrap();
        for &a in &[0usize, 1] {
            for &b in &[2usize, 3] {
                p.partition(ids[a], ids[b]);
            }
        }
    }
    // Advance past the threshold so the cross-group peers decay.
    dst.clock().advance(3601);
    for _ in 0..4 {
        dst.sweep().await;
    }
    // Heal.
    {
        let mut p = dst.policy().write().unwrap();
        for &a in &[0usize, 1] {
            for &b in &[2usize, 3] {
                p.heal(ids[a], ids[b]);
            }
        }
    }
    // Re-converge and assert the full invariant pack holds.
    let q = dst.gossip_until_quiescent(16).await;
    assert!(matches!(q, Quiescence::Converged { .. }), "did not reconverge: {q:?}");
    let violations = check_all(&dst.snapshot().await);
    assert!(violations.is_empty(), "post-heal violations: {violations:?}");
}

/// Seeded compound chaos: random partitions / wire-faults / downs over many
/// rounds, then heal everything and require reconvergence + the invariant pack.
/// A failing seed reproduces the exact schedule. The standing fuzz target.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn seeded_chaos_soak() {
    // Env-tunable for an on-demand heavy fuzz; defaults keep CI fast.
    //   SOVEREIGN_DST_CHAOS_SEEDS=50 SOVEREIGN_DST_CHAOS_NODES=7 SOVEREIGN_DST_CHAOS_ROUNDS=30
    let envn = |k: &str, d: u64| -> u64 {
        std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
    };
    let n_seeds = envn("SOVEREIGN_DST_CHAOS_SEEDS", 3);
    let n_nodes = envn("SOVEREIGN_DST_CHAOS_NODES", 5) as usize;
    let n_rounds = envn("SOVEREIGN_DST_CHAOS_ROUNDS", 16) as usize;
    for seed in 0..n_seeds {
        let dst = DstMesh::start(n_nodes).await;
        let ids = dst.node_ids();
        assert!(matches!(
            dst.gossip_until_quiescent(8).await,
            Quiescence::Converged { .. }
        ));
        let schedule = FaultSchedule::generate(seed, &ids, n_rounds);
        for round in 0..schedule.rounds {
            dst.apply_schedule_round(&schedule, round);
            dst.sweep().await;
        }
        // Heal everything, then require clean reconvergence.
        dst.clear_faults();
        let q = dst.gossip_until_quiescent(24).await;
        assert!(
            matches!(q, Quiescence::Converged { .. }),
            "seed={seed} did not reconverge: {q:?}"
        );
        let violations = check_all(&dst.snapshot().await);
        assert!(violations.is_empty(), "seed={seed} violations: {violations:?}");
    }
}
