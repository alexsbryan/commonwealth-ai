// SPDX-License-Identifier: AGPL-3.0-or-later
//! A spawn gate parks the supervisor without spending its crash budget.

use super::*;

/// A gate that is always closed must park the supervisor: no spawns, and —
/// the load-bearing half — no movement on the crash-loop ceiling. The
/// ceiling exists to stop a loop burning CPU on spawns that keep failing;
/// a spawn we deliberately declined costs nothing and must not consume it.
/// Without this the futile-respawn fix would trade three wasted spawns for
/// a slot wedged in `Failed`.
#[tokio::test]
async fn a_held_spawn_gate_does_not_spawn_and_does_not_burn_crash_budget() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    let (port, _server) = spawn_health_server().await;
    // Marker file: written once per spawn. Must never appear.
    let marker = dir.path().join("spawned");
    let script = write_script(
        &dir,
        "daemon.sh",
        &format!("#!/bin/sh\ntouch {}\nexit 1\n", marker.display()),
    );
    let config = base_config(
        script,
        dir.path().join("crashes"),
        format!("http://127.0.0.1:{port}/v1/models"),
    );

    let supervisor =
        Arc::new(
            Supervisor::new(config).with_spawn_gate(Arc::new(|| SpawnVerdict::Hold {
                reason: "no eligible workers".to_string(),
            })),
        );
    let mut states = supervisor.subscribe();
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };

    let observed = drain_states(&mut states, 4, Duration::from_millis(600)).await;
    assert!(
        observed
            .iter()
            .any(|s| matches!(s, SupervisorState::GateHeld { .. })),
        "expected a GateHeld state, got {observed:?}"
    );
    assert!(
        !marker.exists(),
        "the gate was held but a child was spawned anyway"
    );
    assert!(
        !observed
            .iter()
            .any(|s| matches!(s, SupervisorState::Failed { .. })),
        "a held gate must not trip the crash-loop ceiling, got {observed:?}"
    );

    run_handle.abort();
}

/// Anti-wedge. A gate must never be able to hold a slot the manager is
/// trying to replace — without this the gate is a foot-gun that can strand
/// a `terminate()` forever.
#[tokio::test]
async fn terminate_releases_a_held_spawn_gate() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    let (port, _server) = spawn_health_server().await;
    let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nsleep 5\n");
    let config = base_config(
        script,
        dir.path().join("crashes"),
        format!("http://127.0.0.1:{port}/v1/models"),
    );

    let supervisor =
        Arc::new(
            Supervisor::new(config).with_spawn_gate(Arc::new(|| SpawnVerdict::Hold {
                reason: "permanently held".to_string(),
            })),
        );
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };

    // Let it settle into the hold, then ask it to stop.
    tokio::time::sleep(Duration::from_millis(200)).await;
    supervisor.terminate();

    let exited = tokio::time::timeout(Duration::from_secs(5), run_handle).await;
    assert!(
        exited.is_ok(),
        "run() must return promptly when terminated during a gate hold"
    );
}

/// An explicit operator action outranks a heuristic: `request_reconnect`
/// spawns even while the gate says hold.
#[tokio::test]
async fn request_reconnect_breaks_a_held_spawn_gate() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    let (port, _server) = spawn_health_server().await;
    let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nsleep 5\n");
    let config = base_config(
        script,
        dir.path().join("crashes"),
        format!("http://127.0.0.1:{port}/v1/models"),
    );

    let supervisor =
        Arc::new(
            Supervisor::new(config).with_spawn_gate(Arc::new(|| SpawnVerdict::Hold {
                reason: "held".to_string(),
            })),
        );
    let mut states = supervisor.subscribe();
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };

    tokio::time::sleep(Duration::from_millis(150)).await;
    supervisor.request_reconnect();

    let observed = drain_states(&mut states, 6, Duration::from_millis(800)).await;
    assert!(
        observed
            .iter()
            .any(|s| matches!(s, SupervisorState::Healthy { .. })),
        "reconnect must override the gate and spawn, got {observed:?}"
    );

    run_handle.abort();
}

/// Regression anchor: with no gate installed the run loop behaves exactly
/// as it always has.
#[tokio::test]
async fn no_gate_is_exactly_todays_behaviour() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    let (port, _server) = spawn_health_server().await;
    let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nsleep 5\n");
    let config = base_config(
        script,
        dir.path().join("crashes"),
        format!("http://127.0.0.1:{port}/v1/models"),
    );

    let supervisor = Arc::new(Supervisor::new(config));
    let mut states = supervisor.subscribe();
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };

    let observed = drain_states(&mut states, 6, Duration::from_millis(800)).await;
    assert!(
        observed
            .iter()
            .any(|s| matches!(s, SupervisorState::Healthy { .. })),
        "expected Healthy with no gate, got {observed:?}"
    );
    assert!(
        !observed
            .iter()
            .any(|s| matches!(s, SupervisorState::GateHeld { .. })),
        "an ungated supervisor must never publish GateHeld"
    );

    run_handle.abort();
}
