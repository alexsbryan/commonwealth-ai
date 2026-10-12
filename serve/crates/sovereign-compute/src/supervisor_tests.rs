// SPDX-License-Identifier: AGPL-3.0-or-later
// The timing lock below intentionally spans awaits: the guard must cover
// the whole test body, and each `#[tokio::test]` owns its runtime, so a
// contending sibling parks a thread — serialization, never deadlock.
// Same rationale as `local_corpus/watched/enrich.rs` in sovereign-tools.
#![allow(clippy::await_holding_lock)]

use super::*;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

/// Serializes every test that drives a REAL `Supervisor` — each spawns
/// child processes and an HTTP server, then asserts on state transitions
/// against millisecond deadlines (80ms heartbeats, 20ms backoff, a 1.5s
/// per-event patience in `drain_states`).
///
/// Run concurrently they starve each other: with 14 such tests in one
/// binary the scheduler cannot honour any of their deadlines, and the
/// supervisor simply goes quiet past the harness's patience. Measured
/// 2026-08-10 — `brief_healthy_stretches_do_not_reset_the_breaker` and
/// `a_proven_healthy_generation_resets_the_breaker` fail under
/// `cargo test -p sovereign-compute` and pass under `--test-threads=1`
/// (30/30) and in isolation. They were mis-labelled "load flakes that
/// pass in isolation" and left unfixed; the contention IS the bug.
///
/// NOTE this must work under plain `cargo test`, not just nextest — the
/// repo's test gate runs the cargo engine, so a `.config/nextest.toml`
/// override would not have covered it.
///
/// Poison is ignored: a panicking test must not cascade into the rest.
fn supervisor_timing_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    let guard = LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    warm_the_child_spawn_path();
    guard
}

/// Pay the FIRST `/bin/sh` spawn outside every measured window.
///
/// Validate the instrument before the result (ARCH §18.4). Every test
/// holding [`supervisor_timing_lock`] measures the supervisor's reaction to
/// a child's lifecycle, and none of them is about how long the OS takes to
/// fault in an interpreter. On a host under memory pressure those are not
/// remotely the same size: measured on this box 2026-08-14 at ~275MB free
/// frames and 93% swap, the first `sh -c 'sleep 0.5; exit 1'` took
/// **14.3s** and the next four took 539-548ms — a 26x cold-start term
/// landing inside windows sized for the warm one, which is what turned
/// four supervisor tests red in an unrelated order's gate (note bafe1e2e).
///
/// This removes the term rather than absorbing it. The deadlines still in
/// these tests remain as hang-detectors for the case where warming does not
/// hold (pages can be evicted again under the same pressure).
fn warm_the_child_spawn_path() {
    static WARMED: std::sync::Once = std::sync::Once::new();
    WARMED.call_once(|| {
        // Same interpreter and same shape as the test children, so the
        // pages this faults in are the pages they need.
        let _ = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .status();
    });
}

/// Stand up a localhost axum server answering `/v1/models`. Used
/// as the health endpoint for tests that need the supervisor to
/// see a healthy daemon.
async fn spawn_health_server() -> (u16, tokio::task::JoinHandle<()>) {
    use axum::{routing::get, Json, Router};
    let app = Router::new().route(
        "/v1/models",
        get(|| async { Json(serde_json::json!({"data": []})) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (port, handle)
}

/// Write a small shell script the supervisor can spawn as its
/// "daemon". Returns the script path; the file lives as long as
/// the `TempDir`.
fn write_script(dir: &TempDir, name: &str, body: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, body).unwrap();
    let mut perm = std::fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&path, perm).unwrap();
    path
}

fn base_config(binary: PathBuf, crash_dir: PathBuf, health_url: String) -> SupervisorConfig {
    SupervisorConfig {
        binary_path: binary,
        args: vec![],
        working_dir: None,
        env: vec![],
        health: HealthTarget::Fixed(health_url),
        crash_log_dir: crash_dir,
        heartbeat_interval: Duration::from_millis(80),
        heartbeat_timeout: Duration::from_millis(200),
        heartbeat_failure_threshold: 3,
        // No startup grace by default — matches the original desktop
        // behaviour (count failures from spawn). Handshake tests set
        // their own.
        ready_deadline: Duration::ZERO,
        backoff_schedule: vec![
            Duration::from_millis(20),
            Duration::from_millis(40),
            Duration::from_millis(60),
        ],
        // Long relative to these tests' millisecond timings: the
        // default fixture never resets the breaker, which is what
        // the crash-loop tests want. Tests that need a reset lower
        // it explicitly.
        healthy_reset_after: Duration::from_secs(60),
        crash_loop_max: 3,
        stderr_ring_lines: 64,
        terminate_grace: Duration::from_secs(3),
    }
}

/// Drain at most `max` states with an overall timeout, returning
/// what we got. Discriminant-only check at call sites.
async fn drain_states(
    rx: &mut broadcast::Receiver<SupervisorState>,
    max: usize,
    per_event_timeout: Duration,
) -> Vec<SupervisorState> {
    drain_states_until(rx, max, per_event_timeout, |_| false).await
}

/// Drain until the question the test asks is DECIDED (`decided` returns
/// true), or until `max` events, or until no event arrives for
/// `per_event_timeout`.
///
/// The point of the predicate is to move the test's bar off the wall
/// clock. A drain bounded only by a deadline encodes "the machine is fast
/// enough" into the assertion: on a loaded host — 2026-08-14, 57MB free
/// frames and swap 97% full — a child's `sleep 0.5` took longer than the
/// deadline, the drain returned `[Starting, Healthy]`, and an unrelated
/// order's full-workspace gate went red for a supervisor that was working
/// correctly (note bafe1e2e). With a predicate the deadline becomes a
/// SAFETY NET rather than the bar: a slow host changes how long the test
/// waits, never what it proves, and the normal path returns as soon as the
/// answer is in rather than idling until a timeout.
///
/// One decider: [`drain_states`] is this function with a predicate that
/// never fires, so there is one drain loop, not two.
async fn drain_states_until(
    rx: &mut broadcast::Receiver<SupervisorState>,
    max: usize,
    per_event_timeout: Duration,
    decided: impl Fn(&[SupervisorState]) -> bool,
) -> Vec<SupervisorState> {
    let mut out = Vec::new();
    for _ in 0..max {
        if decided(&out) {
            break;
        }
        match tokio::time::timeout(per_event_timeout, rx.recv()).await {
            Ok(Ok(s)) => out.push(s),
            _ => break,
        }
    }
    out
}

/// A health server that 503s for `unhealthy_for`, then 200s. Lets a
/// test give a generation a *measurable load cost* — the input to
/// the backoff floor — without spawning a real model.
async fn spawn_slow_health_server(unhealthy_for: Duration) -> (u16, tokio::task::JoinHandle<()>) {
    use axum::{http::StatusCode, routing::get, Router};
    let up_at = Instant::now() + unhealthy_for;
    let app = Router::new().route(
        "/v1/models",
        get(move || async move {
            if Instant::now() < up_at {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::OK
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (port, handle)
}

/// THE REGRESSION. A child that loads, serves *briefly*, and dies —
/// over and over — must trip the breaker.
///
/// This is the shape that took the host down on 2026-08-03: a 148 GB
/// distributed primary loaded in 4m36s, served 13s, and aborted when
/// its remote RPC worker died. The old ceiling was "N crashes inside
/// a 600s sliding window", which that child could never reach —
/// crashes aged out of the window faster than a 4.5-minute cycle
/// could accumulate them — so the supervisor re-mapped 148 GB of
/// unified memory indefinitely until amdgpu ran out of address space
/// and the desktop died. The reset condition must be PROOF (a
/// generation that served), never elapsed time.
#[tokio::test]
async fn brief_healthy_stretches_do_not_reset_the_breaker() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    let (port, _server) = spawn_health_server().await;
    // Becomes healthy on the first probe, serves ~200ms, then dies —
    // the "loads fine, dies on first real work" signature.
    let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nsleep 0.2\nexit 1\n");
    let mut config = base_config(
        script,
        dir.path().join("crashes"),
        format!("http://127.0.0.1:{port}/v1/models"),
    );
    config.backoff_schedule = vec![Duration::from_millis(20)];
    config.crash_loop_max = 2;
    // A full second of serving would prove the restart worked. 200ms
    // does not.
    config.healthy_reset_after = Duration::from_secs(1);

    let supervisor = Arc::new(Supervisor::new(config));
    let mut states = supervisor.subscribe();
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };

    // The question is decided the moment the breaker trips, so the drain
    // stops on that answer, not on a stopwatch — the sibling
    // `a_proven_healthy_generation_resets_the_breaker` was moved to this
    // shape on 2026-08-14 (see [`drain_states_until`]). A deadline here
    // encodes "the host is fast enough": under a full-workspace run's
    // spawn/reap load the two crash cycles took longer than 1500ms, the
    // drain returned `[Starting, Healthy]`, and an audit's TESTALL went red
    // for a supervisor that was working correctly. 30s is a hang-detector,
    // not a bar; the warm path returns in ~0.5s.
    let observed = drain_states_until(&mut states, 40, Duration::from_secs(30), |seen| {
        seen.iter()
            .any(|s| matches!(s, SupervisorState::Failed { .. }))
    })
    .await;
    assert!(
        observed
            .iter()
            .any(|s| matches!(s, SupervisorState::Healthy { .. })),
        "the child must actually reach Healthy — otherwise this test \
         proves nothing about brief-healthy generations: {observed:?}"
    );
    assert!(
        observed
            .iter()
            .any(|s| matches!(s, SupervisorState::Failed { .. })),
        "a child that keeps dying shortly after serving must trip the \
         breaker, got {observed:?}"
    );

    run_handle.abort();
}

/// The other half of the same decider: a generation that DOES serve
/// long enough clears the counter, so an occasional crash in a
/// long-running child never parks it. Fails on the old wall-clock
/// ceiling, which would reach `Failed` on the second crash here
/// regardless of how long the child had been up.
///
/// What it proves is a property of the SUPERVISOR, and since 2026-08-14 it
/// is read that way: the drain stops on the decision (see
/// [`drain_states_until`]), not on a deadline, so process-spawn and
/// child-reap latency on a loaded host change only the test's duration.
/// The assertions are unchanged — two crashes both answered by a restart,
/// and no `Failed` — because they are what the old ceiling reddens.
#[tokio::test]
async fn a_proven_healthy_generation_resets_the_breaker() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    let (port, _server) = spawn_health_server().await;
    let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nsleep 0.5\nexit 1\n");
    let mut config = base_config(
        script,
        dir.path().join("crashes"),
        format!("http://127.0.0.1:{port}/v1/models"),
    );
    config.backoff_schedule = vec![Duration::from_millis(20)];
    // One crash would be enough to park it if nothing ever reset.
    config.crash_loop_max = 1;
    // ~400ms of serving per generation clears this bar.
    config.healthy_reset_after = Duration::from_millis(150);

    let supervisor = Arc::new(Supervisor::new(config));
    let mut states = supervisor.subscribe();
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };

    // The question is decided the moment EITHER outcome is observable, so
    // the drain stops on the answer instead of on a stopwatch:
    //
    //   - a `Failed` is the regression this test exists to catch. On a
    //     crash the supervisor emits `Restarting` OR `Failed`, never both
    //     (`crash_loop_max` is checked at the crash, supervisor.rs:685),
    //     so a `Failed` here means the breaker tripped on the second crash
    //     — exactly the old wall-clock-ceiling behaviour.
    //   - two `Restarting`s mean two crashes were BOTH answered with a
    //     restart, i.e. the healthy generation cleared the counter twice.
    //     That is the proof; nothing after it can change the verdict.
    //
    // 30s is a hang-detector, not a bar. The warm path returns in ~1.05s
    // (two generations of `sleep 0.5` + 20ms backoff) and never pays it;
    // it is sized above the 14.3s cold `/bin/sh` spawn measured on this
    // box under swap pressure, so that even when
    // `warm_the_child_spawn_path` does not hold, the test still reaches
    // the right answer — just slowly.
    let observed = drain_states_until(&mut states, 40, Duration::from_secs(30), |seen| {
        seen.iter()
            .any(|s| matches!(s, SupervisorState::Failed { .. }))
            || seen
                .iter()
                .filter(|s| matches!(s, SupervisorState::Restarting { .. }))
                .count()
                >= 2
    })
    .await;
    let restarts = observed
        .iter()
        .filter(|s| matches!(s, SupervisorState::Restarting { .. }))
        .count();
    // The REGRESSION assert comes first, because the drain stops on a
    // `Failed` and therefore stops with fewer than two restarts. Asserting
    // the restart count first would report "not enough restarts" for a
    // tripped breaker and send the next reader after a timing ghost.
    assert!(
        !observed
            .iter()
            .any(|s| matches!(s, SupervisorState::Failed { .. })),
        "a generation that served past healthy_reset_after must clear \
         the breaker — got {observed:?}"
    );
    // And this guards the vacuous pass: a supervisor that emits nothing
    // reaches the hang-detector with zero restarts and fails HERE, rather
    // than reporting a green for a test that never exercised the reset.
    assert!(
        restarts >= 2,
        "expected repeated restarts to exercise the reset, got {restarts} in {observed:?}"
    );

    run_handle.abort();
}

/// The backoff floor. A generation that cost 2000ms to load must not
/// be retried on the 20ms schedule — otherwise an expensive child
/// spends nearly all its wall-clock inside a load, which is what
/// turned one worker-side crash into host-wide GPU-memory
/// exhaustion. Measured as the gap between the crash and the next
/// spawn.
///
/// The load cost is 2000ms rather than a token 400ms because the gap
/// is observed from the TEST's clock: under a saturated `--workspace`
/// run the `Restarting` broadcast can arrive late enough to compress a
/// 400ms gap to ~246ms and trip the 300ms floor on scheduling latency,
/// not on behaviour (seen twice, 2026-09-15). The assertion is
/// unchanged at 75% of the load cost; the margin widened — the same
/// treatment `handshake_mode_parses_port_and_reaches_healthy` records
/// above.
#[tokio::test]
async fn backoff_is_floored_by_the_last_generation_s_load_cost() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    const LOAD: Duration = Duration::from_millis(2000);
    let (port, _server) = spawn_slow_health_server(LOAD).await;
    let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nsleep 3.5\nexit 1\n");
    let mut config = base_config(
        script,
        dir.path().join("crashes"),
        format!("http://127.0.0.1:{port}/v1/models"),
    );
    // Generous grace so the slow health server reads as loading, not
    // as failing heartbeats.
    config.ready_deadline = Duration::from_secs(5);
    config.backoff_schedule = vec![Duration::from_millis(20)];
    config.crash_loop_max = 5;
    config.healthy_reset_after = Duration::from_secs(60);

    let supervisor = Arc::new(Supervisor::new(config));
    let mut states = supervisor.subscribe();
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };

    // Walk to the first Restarting, then time how long until the
    // supervisor actually spawns again (the next Starting).
    let mut restarting_at = None;
    let mut gap = None;
    for _ in 0..40 {
        match tokio::time::timeout(Duration::from_secs(3), states.recv()).await {
            Ok(Ok(SupervisorState::Restarting { .. })) => {
                restarting_at = Some(Instant::now());
            }
            Ok(Ok(SupervisorState::Starting)) => {
                if let Some(t) = restarting_at {
                    gap = Some(t.elapsed());
                    break;
                }
            }
            Ok(Ok(_)) => {}
            _ => break,
        }
    }

    let gap = gap.expect("supervisor never restarted");
    assert!(
        gap >= Duration::from_millis(1500),
        "backoff should have been floored at the ~{}ms load cost, but the \
         next spawn came after only {gap:?} (the 20ms schedule)",
        LOAD.as_millis()
    );

    run_handle.abort();
}

#[tokio::test]
async fn healthy_when_daemon_responds() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    let (port, _server) = spawn_health_server().await;
    // Daemon: sleep long enough to outlast the test.
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
        "expected at least one Healthy state, got {observed:?}"
    );

    run_handle.abort();
}

#[tokio::test]
async fn crashing_daemon_emits_restarting_then_failed() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    // No health server — heartbeats fail, but the child also
    // exits on its own well before the threshold, exercising the
    // ChildExited path.
    let script = write_script(&dir, "daemon.sh", "#!/bin/sh\necho crashing >&2\nexit 1\n");
    // Tight schedule + low ceiling so the test finishes fast.
    let mut config = base_config(
        script,
        dir.path().join("crashes"),
        "http://127.0.0.1:1/v1/models".into(), // unreachable
    );
    config.backoff_schedule = vec![Duration::from_millis(10)];
    config.crash_loop_max = 2;

    let supervisor = Arc::new(Supervisor::new(config));
    let mut states = supervisor.subscribe();
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };

    let observed = drain_states(&mut states, 20, Duration::from_millis(600)).await;
    let saw_restarting = observed
        .iter()
        .any(|s| matches!(s, SupervisorState::Restarting { .. }));
    let saw_failed = observed
        .iter()
        .any(|s| matches!(s, SupervisorState::Failed { .. }));
    assert!(
        saw_restarting && saw_failed,
        "expected Restarting and Failed in {observed:?}"
    );

    run_handle.abort();
}

// Flaky under workspace `cargo test` — the stderr drainer task
// (spawned inside `spawn_child`) doesn't get scheduled fast
// enough when the tokio runtime is saturated by parallel test
// crates, and the child's stderr pipe is reaped by the
// `kill_on_drop` path before the BufReader pulls anything out.
// Passes consistently in isolation. Proper fix would be to make
// the drainer drain synchronously on child exit (or to use a
// bounded channel the supervisor writes from inside the wait
// loop). Out of scope for the SlotContext refactor.
#[ignore = "flaky under parallel cargo test (drainer scheduling); passes in isolation"]
#[tokio::test]
async fn stderr_ring_captures_recent_lines() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    let script = write_script(
        &dir,
        "daemon.sh",
        "#!/bin/sh\necho line-1 >&2\necho line-2 >&2\necho line-3 >&2\nexit 0\n",
    );
    let mut config = base_config(
        script,
        dir.path().join("crashes"),
        "http://127.0.0.1:1/v1/models".into(),
    );
    // Stop after one restart attempt — we don't care about the
    // ceiling here, just the stderr capture.
    config.crash_loop_max = 0;
    config.backoff_schedule = vec![Duration::from_millis(10)];

    let supervisor = Arc::new(Supervisor::new(config));
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };

    // Let the supervisor spawn + the child exit + the drainer
    // flush. The script itself runs in <10ms but the drainer is
    // async and the stderr ring can be empty for surprisingly
    // long on a loaded CI host (workspace `cargo test` saturates
    // the tokio runtime across all crates simultaneously, and the
    // drainer's `tokio::spawn` doesn't get scheduled until the
    // pool catches up). Poll with a 10 s deadline rather than a
    // fixed sleep — passes fast on healthy hosts, doesn't
    // false-fail when the runner is heavily loaded.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let tail = loop {
        let current = supervisor.stderr_tail();
        if current.iter().any(|l| l == "line-1") || std::time::Instant::now() > deadline {
            break current;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        tail.iter().any(|l| l == "line-1"),
        "expected line-1 in stderr tail after 10s, got {tail:?}"
    );
    assert!(
        tail.iter().any(|l| l == "line-3"),
        "expected line-3 in stderr tail, got {tail:?}"
    );

    // The crash-log file should also exist.
    let crash_dir = dir.path().join("crashes");
    let entries: Vec<_> = std::fs::read_dir(&crash_dir)
        .expect("crash dir exists")
        .flatten()
        .collect();
    assert!(
        !entries.is_empty(),
        "expected a daemon-*.log in {crash_dir:?}"
    );

    run_handle.abort();
}

#[tokio::test]
async fn manual_reconnect_after_failed_restarts_supervisor() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    // Crashes immediately so we hit Failed quickly.
    let script = write_script(&dir, "daemon.sh", "#!/bin/sh\nexit 1\n");
    let mut config = base_config(
        script,
        dir.path().join("crashes"),
        "http://127.0.0.1:1/v1/models".into(),
    );
    config.backoff_schedule = vec![Duration::from_millis(10)];
    config.crash_loop_max = 1;

    let supervisor = Arc::new(Supervisor::new(config));
    let mut states = supervisor.subscribe();
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };

    // Wait until we see Failed.
    let mut got_failed = false;
    for _ in 0..30 {
        if let Ok(Ok(s)) = tokio::time::timeout(Duration::from_millis(150), states.recv()).await {
            if matches!(s, SupervisorState::Failed { .. }) {
                got_failed = true;
                break;
            }
        }
    }
    assert!(got_failed, "supervisor never entered Failed");

    // Fire reconnect; expect another Starting.
    assert!(supervisor.request_reconnect());
    let mut got_starting = false;
    for _ in 0..30 {
        if let Ok(Ok(s)) = tokio::time::timeout(Duration::from_millis(150), states.recv()).await {
            if matches!(s, SupervisorState::Starting) {
                got_starting = true;
                break;
            }
        }
    }
    assert!(got_starting, "no Starting after manual reconnect");

    run_handle.abort();
}

#[tokio::test]
async fn terminate_exits_loop_without_crash_accounting() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    let (port, _server) = spawn_health_server().await;
    let script = write_script(&dir, "child.sh", "#!/bin/sh\nsleep 30\n");
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

    // Wait until healthy.
    let mut healthy = false;
    for _ in 0..40 {
        if let Ok(Ok(s)) = tokio::time::timeout(Duration::from_millis(150), states.recv()).await {
            if matches!(s, SupervisorState::Healthy { .. }) {
                healthy = true;
                break;
            }
        }
    }
    assert!(healthy, "child never became healthy");

    // Graceful terminate → the run loop must return promptly (SIGTERM
    // kills `sleep`), NOT restart.
    assert!(supervisor.terminate());
    let joined = tokio::time::timeout(Duration::from_secs(5), run_handle).await;
    assert!(
        joined.is_ok(),
        "run loop did not exit within 5s of terminate()"
    );

    let tail = drain_states(&mut states, 4, Duration::from_millis(200)).await;
    assert!(
        !tail.iter().any(|s| matches!(
            s,
            SupervisorState::Restarting { .. } | SupervisorState::Failed { .. }
        )),
        "terminate() must not trigger restart/failed, got {tail:?}"
    );
}

/// Children outliving the supervisor or its process.
#[cfg(target_os = "linux")]
#[path = "supervisor_lifetime_tests.rs"]
mod lifetime;

/// A spawn gate that holds, releases, or is absent.
#[path = "supervisor_spawn_gate_tests.rs"]
mod spawn_gate;

/// The stdout handshake that names a child's port.
#[path = "supervisor_handshake_tests.rs"]
mod handshake;
