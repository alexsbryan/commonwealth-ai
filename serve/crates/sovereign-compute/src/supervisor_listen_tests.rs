// SPDX-License-Identifier: AGPL-3.0-or-later
//! `TcpListening` mode: a raw-TCP child, connected to until it answers once.

use super::*;

/// Accepts on `listener` and counts the connections, closing each.
fn count_accepts(listener: tokio::net::TcpListener) -> Arc<AtomicU32> {
    let accepted = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&accepted);
    tokio::spawn(async move {
        while listener.accept().await.is_ok() {
            counter.fetch_add(1, Ordering::SeqCst);
        }
    });
    accepted
}

/// A listening child is connected to once, then left alone: rpc-server
/// serves one client on a backlog of 1, so a probe while a head is
/// connected would queue behind it and read as a failure.
#[tokio::test]
async fn a_listening_child_is_probed_until_it_answers_then_left_alone() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = count_accepts(listener);
    let script = write_script(&dir, "peer.sh", "#!/bin/sh\nsleep 5\n");
    let mut config = base_config(script, dir.path().join("crashes"), String::new());
    config.health = HealthTarget::TcpListening(addr);

    let supervisor = Arc::new(Supervisor::new(config));
    let mut states = supervisor.subscribe();
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };
    let observed = drain_states_until(&mut states, 6, Duration::from_millis(1500), |s| {
        s.iter()
            .any(|s| matches!(s, SupervisorState::Healthy { .. }))
    })
    .await;
    assert!(
        observed
            .iter()
            .any(|s| matches!(s, SupervisorState::Healthy { .. })),
        "a listening child never read healthy: {observed:?}"
    );
    // Ten heartbeat intervals with no second connect.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "the supervisor probed a child that had already answered"
    );
    run_handle.abort();
}

/// Nothing listening at the address: the connect fails, and the failures
/// count toward the heartbeat threshold like any probe's.
#[tokio::test]
async fn a_child_that_never_listens_is_restarted() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    // Bound and released: nothing listens there now.
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let script = write_script(&dir, "peer.sh", "#!/bin/sh\nsleep 5\n");
    let mut config = base_config(script, dir.path().join("crashes"), String::new());
    config.health = HealthTarget::TcpListening(addr);

    let supervisor = Arc::new(Supervisor::new(config));
    let mut states = supervisor.subscribe();
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };
    let observed = drain_states_until(&mut states, 12, Duration::from_millis(1500), |s| {
        s.iter()
            .any(|s| matches!(s, SupervisorState::Restarting { .. }))
    })
    .await;
    assert!(
        !observed
            .iter()
            .any(|s| matches!(s, SupervisorState::Healthy { .. })),
        "nothing listens, yet the child read healthy: {observed:?}"
    );
    assert!(
        observed.iter().any(|s| matches!(
            s,
            SupervisorState::Restarting { reason, .. } if reason.contains("failed heartbeats")
        )),
        "expected a restart on failed connects, got {observed:?}"
    );
    run_handle.abort();
}
