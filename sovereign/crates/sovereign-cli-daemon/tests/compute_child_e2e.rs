// SPDX-License-Identifier: AGPL-3.0-or-later
//! Crash-isolation acceptance (DISTRIBUTED_PILOT_READINESS.md P1).
//!
//! Spawns a REAL mock compute child via the daemon binary
//! (`--compute-child --role mock`), then `kill`s it mid-stream and asserts:
//!   1. the stream terminates promptly with a terminal `StreamFrame::Error`
//!      (the daemon-side is never left hanging on a dead socket);
//!   2. the test process (the daemon analog) stays alive throughout — a
//!      child SIGKILL/SIGABRT is a value here, not a process death;
//!   3. the supervisor respawns the child back to `serving` (restart
//!      recorded);
//!   4. a completion issued after recovery succeeds.
//!
//! Covers both SIGKILL (`kill -9`) and the uncatchable ggml SIGABRT
//! (`kill -6`) — the exact fault the process boundary exists to contain.

use std::time::{Duration, Instant};

use futures::StreamExt;
use sovereign_compute::child::ChildLifecycle;
use sovereign_compute::manager::ComputeChildManager;
use sovereign_contracts::{CompletionRequest, InferenceProvider, StreamFrame};

/// The daemon binary — its `--compute-child` arm runs the mock child.
const BIN: &str = env!("CARGO_BIN_EXE_sovereign-cli-daemon");

async fn wait_all_serving(mgr: &ComputeChildManager, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let statuses = mgr.statuses();
        if !statuses.is_empty()
            && statuses
                .iter()
                .all(|s| matches!(s.lifecycle, ChildLifecycle::Serving))
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

fn kill(pid: u32, signal: &str) {
    let _ = std::process::Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .stderr(std::process::Stdio::null())
        .status();
}

fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Poll until `pid` is gone (or the timeout lapses) — deterministic
/// synchronization so the stream assertions don't race the signal delivery.
async fn wait_until_dead(pid: u32, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if !pid_alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Kill the mock child mid-stream with `signal` and assert the full
/// crash-isolation + recovery cycle.
async fn crash_and_recover(signal: &str) {
    let crash_dir =
        std::env::temp_dir().join(format!("compute-e2e-{}-{signal}", std::process::id()));
    // 300 tokens × 25ms ≈ 7.5s of streaming — ample room to kill mid-stream.
    let mgr = ComputeChildManager::start_mock_slot("crashslot", BIN.into(), crash_dir, 300, 25);

    assert!(
        wait_all_serving(&mgr, Duration::from_secs(20)).await,
        "child never reached serving"
    );
    let pid = mgr.statuses()[0].pid.expect("a serving child has a pid");
    let child = mgr
        .routes()
        .get("crashslot")
        .expect("route exists")
        .clone();

    // Start a streaming completion and read one token to be genuinely mid-stream.
    let mut req = CompletionRequest::default();
    req.prompt = "hello".into();
    let mut stream = child
        .complete_stream_with_finish(&req)
        .await
        .expect("stream should start");
    let first = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("first frame timed out");
    assert!(
        matches!(first, Some(StreamFrame::Token(_))),
        "expected a first Token frame, got {first:?}"
    );

    // Kill the child mid-stream and wait until it's confirmed gone.
    assert!(pid_alive(pid), "child should be alive before the kill");
    kill(pid, signal);
    assert!(
        wait_until_dead(pid, Duration::from_secs(5)).await,
        "child pid {pid} did not die after signal {signal}"
    );

    // The stream MUST terminate promptly with a terminal Error — not hang.
    let mut saw_error = false;
    let drained = tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(frame) = stream.next().await {
            if matches!(frame, StreamFrame::Error(_)) {
                saw_error = true;
            }
        }
    })
    .await;
    assert!(
        drained.is_ok(),
        "stream did not terminate after the child was killed (hung)"
    );
    assert!(
        saw_error,
        "stream did not end with a terminal Error frame after child death"
    );

    // The supervisor respawns the child back to serving.
    assert!(
        wait_all_serving(&mgr, Duration::from_secs(30)).await,
        "child did not recover to serving after {signal}"
    );
    assert!(
        mgr.statuses()[0].restarts >= 1,
        "a restart should have been recorded"
    );

    // A completion after recovery succeeds.
    let mut req2 = CompletionRequest::default();
    req2.prompt = "again".into();
    let resp = tokio::time::timeout(Duration::from_secs(10), child.complete(&req2))
        .await
        .expect("post-recovery completion timed out")
        .expect("post-recovery completion failed");
    assert_eq!(resp.text, "mock response");

    mgr.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigkill_child_midstream_recovers() {
    crash_and_recover("9").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigabrt_child_midstream_recovers() {
    // SIGABRT is the uncatchable ggml teardown fault the boundary contains.
    crash_and_recover("6").await;
}
