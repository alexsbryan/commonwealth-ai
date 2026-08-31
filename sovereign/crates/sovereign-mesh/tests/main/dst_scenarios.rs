// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic-simulation mesh scenarios. Gated by the `dst` feature:
//!
//!   cargo test -p sovereign-mesh --features dst --test main dst_scenarios
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
    assert!(
        violations.is_empty(),
        "invariant violations: {violations:?}"
    );
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
    // Re-converge and assert the full invariant pack holds. AGREED quiesce: a
    // stable-but-not-yet-agreed plateau must not pass as converged — that was
    // this test's intermittent failure. Budget 32 (not 16): agreement is strictly
    // harder to reach than stability, so the post-heal live-set needs more rounds
    // to propagate to ALL nodes under unseeded gossip order. 16 occasionally hit
    // MaxRoundsExceeded (~1-in-15 overnight); the loop returns early once
    // agreement lands, so the larger budget only costs anything on the slow tail.
    let q = dst.gossip_until_quiescent_agreed(32).await;
    assert!(
        matches!(q, Quiescence::Converged { .. }),
        "did not reconverge: {q:?}"
    );
    let violations = check_all(&dst.snapshot().await);
    assert!(
        violations.is_empty(),
        "post-heal violations: {violations:?}"
    );
}

/// The agreed-quiescence variant must REJECT a stably-disagreeing mesh — the
/// exact post-heal plateau that made `partition_then_heal_reconverges` flaky.
/// With an ACTIVE partition (decayed, not healed) the two groups settle into
/// stable but contradictory live-sets: plain `gossip_until_quiescent` reports
/// `Converged` (stability reached), while `gossip_until_quiescent_agreed`
/// correctly holds out to the round budget (no agreement). This pins the
/// distinction the fix relies on, and is why the agreed variant is
/// heal-then-assert ONLY — running it under an active partition would (rightly)
/// never converge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agreed_quiesce_rejects_stable_disagreement() {
    let dst = DstMesh::start(4).await;
    assert!(matches!(
        dst.gossip_until_quiescent(8).await,
        Quiescence::Converged { .. }
    ));
    let ids = dst.node_ids();
    // Split {0,1} | {2,3} and decay the cross-group peers — do NOT heal.
    {
        let mut p = dst.policy().write().unwrap();
        for &a in &[0usize, 1] {
            for &b in &[2usize, 3] {
                p.partition(ids[a], ids[b]);
            }
        }
    }
    dst.clock().advance(3601);
    for _ in 0..6 {
        dst.sweep().await;
    }
    // Stability still holds (views have stopped changing)…
    assert!(
        matches!(
            dst.gossip_until_quiescent(8).await,
            Quiescence::Converged { .. }
        ),
        "an active partition should still reach a stable fixpoint"
    );
    // …but the two groups disagree on the live-set, so AGREEMENT must not be
    // claimed: the agreed variant spins out to the round budget.
    assert!(
        matches!(
            dst.gossip_until_quiescent_agreed(8).await,
            Quiescence::MaxRoundsExceeded { .. }
        ),
        "agreed-quiesce must reject a stably-disagreeing (partitioned) mesh"
    );
}

/// Wire faults (a throttled peer + a truncated stream) plus a backward clock
/// jump during gossip must not break convergence once healed: gossip tolerates a
/// dribbling/truncating peer, and offline-decay ignores a non-monotonic clock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wire_faults_and_clock_jump_back_reconverge() {
    let dst = DstMesh::start(4).await;
    assert!(matches!(
        dst.gossip_until_quiescent(8).await,
        Quiescence::Converged { .. }
    ));
    dst.slow_peer(0, 1, 8); // node0 sees node1 as a crawl (8 B/s)
    dst.truncate_stream(2, 3, 16); // node2 sees node3's response cut at 16 bytes
    dst.clock_jump_back(1, 120); // node1's wall clock steps back 2 minutes
    for _ in 0..4 {
        dst.sweep().await;
    }
    dst.clear_faults();
    // AGREED quiesce (heal-then-assert): require all up nodes to converge to the
    // identical view, not merely to stop changing. Budget 32 — agreement needs
    // more rounds than stability to fully propagate (see partition_then_heal).
    let q = dst.gossip_until_quiescent_agreed(32).await;
    assert!(
        matches!(q, Quiescence::Converged { .. }),
        "did not reconverge: {q:?}"
    );
    let violations = check_all(&dst.snapshot().await);
    assert!(
        violations.is_empty(),
        "post-heal violations: {violations:?}"
    );
}

// Host-failover (SingleHostUnderFailover) is covered without a dedicated DST
// scenario: `NoSplitBrain` already asserts every up node agrees on
// `partition::elect_leader` over its live set, and the shared-model host is
// `should_host` = pin-else-leader, so leader-agreement IS host-agreement; the
// runtime ≤1-host property is the soak's HTTP `shared_model_single_host` check,
// and the pin logic has `partition::should_host` unit tests. A dedicated DST
// leader-crash scenario was tried but only duplicated NoSplitBrain while adding
// gossip-order flakiness, so it was dropped.

/// Seeded compound chaos: random partitions / wire-faults / downs over many
/// rounds, then heal everything and require reconvergence + the invariant pack.
/// A failing seed reproduces the exact schedule. The standing fuzz target.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn seeded_chaos_soak() {
    // Env-tunable for an on-demand heavy fuzz; defaults keep CI fast.
    //   SOVEREIGN_DST_CHAOS_SEEDS=50 SOVEREIGN_DST_CHAOS_NODES=7 SOVEREIGN_DST_CHAOS_ROUNDS=30
    let envn = |k: &str, d: u64| -> u64 {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
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
        // Heal everything, then require clean reconvergence — AGREED, so a
        // stable-but-disagreeing plateau can't masquerade as converged. Budget 40
        // (more nodes → more rounds for full agreement to propagate).
        dst.clear_faults();
        let q = dst.gossip_until_quiescent_agreed(40).await;
        assert!(
            matches!(q, Quiescence::Converged { .. }),
            "seed={seed} did not reconverge: {q:?}"
        );
        let violations = check_all(&dst.snapshot().await);
        assert!(
            violations.is_empty(),
            "seed={seed} violations: {violations:?}"
        );
    }
}
