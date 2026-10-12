// SPDX-License-Identifier: AGPL-3.0-or-later
//! `StdoutHandshake` mode: the child announces its port on stdout.

use super::*;

/// Health server answering an arbitrary path (handshake mode uses
/// `/health`, not `/v1/models`).
async fn spawn_health_server_at(path: &'static str) -> (u16, tokio::task::JoinHandle<()>) {
    use axum::{routing::get, Json, Router};
    let app = Router::new().route(
        path,
        get(|| async { Json(serde_json::json!({"data": []})) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (port, handle)
}

#[test]
fn parse_handshake_extracts_port() {
    assert_eq!(
        parse_handshake("SOVEREIGN_COMPUTE_LISTENING {\"port\":54321,\"pid\":99}"),
        Some(54321)
    );
    // No `port` key → None (don't fire on a partial line).
    assert_eq!(
        parse_handshake("SOVEREIGN_COMPUTE_LISTENING {\"pid\":99}"),
        None
    );
    // Not a handshake line at all.
    assert_eq!(parse_handshake("2026-07-20 INFO loading model"), None);
    // Prefix present but the payload isn't JSON.
    assert_eq!(
        parse_handshake("SOVEREIGN_COMPUTE_LISTENING not-json"),
        None
    );
}

#[tokio::test]
async fn handshake_mode_parses_port_and_reaches_healthy() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    // The child will announce THIS server's port; the supervisor builds
    // http://127.0.0.1:{port}/health from the handshake line.
    let (port, _server) = spawn_health_server_at("/health").await;
    let script = write_script(
        &dir,
        "child.sh",
        &format!(
            "#!/bin/sh\necho 'SOVEREIGN_COMPUTE_LISTENING {{\"port\":{port},\"pid\":1}}'\nsleep 60\n"
        ),
    );
    let mut config = base_config(script, dir.path().join("crashes"), String::new());
    // Deadlines are generous on purpose: this asserts the handshake is
    // PARSED and reaches Healthy, not how fast. Under a saturated
    // --workspace run the Starting → Warming gap has exceeded 1.5s,
    // which failed the gate on scheduling latency, not on behaviour.
    config.health = HealthTarget::StdoutHandshake {
        health_path: "/health".into(),
        handshake_deadline: Duration::from_secs(20),
    };
    config.ready_deadline = Duration::from_secs(20);

    let supervisor = Arc::new(Supervisor::new(config));
    let mut states = supervisor.subscribe();
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };

    let observed = drain_states(&mut states, 8, Duration::from_secs(10)).await;
    assert!(
        observed
            .iter()
            .any(|s| matches!(s, SupervisorState::Warming { port: p, .. } if *p == port)),
        "expected Warming with port {port}, got {observed:?}"
    );
    assert!(
        observed
            .iter()
            .any(|s| matches!(s, SupervisorState::Healthy { .. })),
        "expected Healthy after handshake, got {observed:?}"
    );

    run_handle.abort();
}

#[tokio::test]
async fn handshake_timeout_when_child_never_announces() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    // Child prints nothing on stdout and just sleeps → the handshake
    // deadline elapses → spawn is treated as a crash (Restarting), and
    // with crash_loop_max low we reach Failed.
    let script = write_script(&dir, "child.sh", "#!/bin/sh\nsleep 30\n");
    let mut config = base_config(script, dir.path().join("crashes"), String::new());
    config.health = HealthTarget::StdoutHandshake {
        health_path: "/health".into(),
        handshake_deadline: Duration::from_millis(150),
    };
    config.backoff_schedule = vec![Duration::from_millis(10)];
    config.crash_loop_max = 1;

    let supervisor = Arc::new(Supervisor::new(config));
    let mut states = supervisor.subscribe();
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };

    let observed = drain_states(&mut states, 12, Duration::from_millis(500)).await;
    assert!(
        observed
            .iter()
            .any(|s| matches!(s, SupervisorState::Failed { .. })),
        "expected Failed after repeated handshake timeouts, got {observed:?}"
    );

    run_handle.abort();
}
