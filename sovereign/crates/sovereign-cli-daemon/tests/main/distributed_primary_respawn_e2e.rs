// SPDX-License-Identifier: AGPL-3.0-or-later
//! Respawn acceptance for the distributed-primary compute child
//! (DISTRIBUTED_PILOT_READINESS.md P1, the deferred payoff).
//!
//! When the mesh's worker set changes, the distributed primary is NOT reloaded
//! in place — the child is killed and a fresh one is spawned against the new
//! set. That is forced by ggml: freeing a sharded model's buffers on a worker
//! that has already gone aborts the process (`ggml-rpc.cpp:386`), which is
//! precisely how the daemon died on 2026-07-27 (note c4ef6fa0) from the
//! shrink-fast-prune reload meant to protect it.
//!
//! This exercises the respawn machinery with the model-free mock child — the
//! properties under test are process-lifecycle properties, not inference ones:
//!   1. the routing handle taken BEFORE the first spawn still works after a
//!      respawn (the facade holds one `Arc<ChildProvider>` for the slot's life);
//!   2. a respawn really replaces the process (new pid);
//!   3. an in-flight stream terminates promptly with a terminal
//!      `StreamFrame::Error` instead of hanging on a dead socket;
//!   4. the superseded generation's exit does not clobber the fresh child's
//!      state — the slot ends up serving, not failed;
//!   5. `retire()` parks the slot unavailable (the "cluster can't hold it"
//!      posture) and the provider fail-fasts rather than falling through to an
//!      in-process load.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use futures::StreamExt;
use sovereign_compute::child::ChildLifecycle;
use sovereign_compute::manager::{DistributedPrimarySpec, DynamicChildSlot};
use sovereign_contracts::{CompletionRequest, InferenceProvider, StreamFrame};

/// The daemon binary — its `--compute-child` arm runs the mock child.
const BIN: &str = env!("CARGO_BIN_EXE_sovereign-cli-daemon");

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dist-respawn-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn spec(dir: &PathBuf) -> DistributedPrimarySpec {
    DistributedPrimarySpec {
        name: "shared-primary".to_string(),
        model: dir.join("unused-for-mock.gguf"),
        context_size: None,
        n_gpu_layers: None,
        model_ids: vec![],
        handoff_path: dir.join("distribution.json"),
    }
}

async fn wait_serving(slot: &DynamicChildSlot, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if matches!(slot.status().lifecycle, ChildLifecycle::Serving) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_set_change_respawns_the_child_without_breaking_routing() {
    let dir = scratch("swap");
    let slot = DynamicChildSlot::new(spec(&dir), BIN.into(), dir.clone());

    // The routing handle the facade would hold — taken BEFORE anything is
    // spawned, and never re-taken. If a respawn invalidated it, every request
    // after the first worker-set change would fail forever.
    let provider = slot.provider();
    assert!(
        !provider.is_serving(),
        "an unspawned slot must not claim to be serving"
    );

    // 300 tokens × 25 ms ≈ 7.5 s of streaming — room to respawn mid-stream.
    slot.respawn_mock(300, 25);
    assert!(
        wait_serving(&slot, Duration::from_secs(20)).await,
        "child never reached serving"
    );
    let first_pid = slot.status().pid.expect("a serving child has a pid");

    // Go genuinely mid-stream before the worker set "changes".
    let mut req = CompletionRequest::default();
    req.prompt = "hello".into();
    let mut stream = provider
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

    // The worker set changed → kill and respawn.
    slot.respawn_mock(300, 25);

    // (3) The in-flight stream must END, with an error — not hang.
    let mut saw_error = false;
    let drain = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(frame) = stream.next().await {
            if matches!(frame, StreamFrame::Error { .. }) {
                saw_error = true;
            }
        }
    })
    .await;
    assert!(drain.is_ok(), "stream never terminated after the respawn");
    assert!(
        saw_error,
        "a respawn mid-stream must surface a terminal StreamFrame::Error"
    );

    // (2) + (4): a NEW process is serving, and the old generation's exit did
    // not leave the slot latched Failed.
    assert!(
        wait_serving(&slot, Duration::from_secs(30)).await,
        "slot did not return to serving after respawn (status: {:?})",
        slot.status().lifecycle
    );
    let second_pid = slot.status().pid.expect("a serving child has a pid");
    assert_ne!(
        first_pid, second_pid,
        "respawn must replace the process, not reuse it"
    );

    // (1) The pre-spawn handle still routes to the current child.
    let mut after = CompletionRequest::default();
    after.prompt = "after respawn".into();
    let resp = tokio::time::timeout(Duration::from_secs(20), provider.complete(&after))
        .await
        .expect("post-respawn completion timed out")
        .expect("post-respawn completion should succeed");
    assert_eq!(resp.text, "mock response");

    // (5) Retiring parks the slot unavailable and the handle fail-fasts.
    slot.retire("no eligible RPC workers");
    assert!(!slot.is_spawned());
    assert!(!provider.is_serving());
    let err = provider
        .complete(&after)
        .await
        .expect_err("a retired slot must fail fast, never fall through to a local load");
    let msg = err.to_string();
    assert!(
        msg.contains("no eligible RPC workers") || msg.contains("not serving"),
        "the refusal should state why the slot is unavailable, got: {msg}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
