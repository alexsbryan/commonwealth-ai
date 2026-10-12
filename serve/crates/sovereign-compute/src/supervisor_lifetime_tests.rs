// SPDX-License-Identifier: AGPL-3.0-or-later
//! Nothing a supervised child forks outlives it, and nothing it runs
//! outlives the supervising process, however that process dies.
#![allow(clippy::await_holding_lock)]

use super::*;
use std::os::unix::fs::PermissionsExt;

/// Alive and not a zombie: an orphan waits to be reaped by init.
fn process_alive(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|stat| {
            let state = stat.rsplit(')').next().unwrap_or("").trim_start();
            !state.starts_with('Z')
        })
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn terminate_takes_down_what_the_child_forked() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    let (port, _server) = spawn_health_server().await;
    // The grandchild ignores SIGTERM (an ignored disposition survives
    // exec), as a model process mid-teardown might; only the group
    // SIGKILL that ends every generation can reach it.
    let gc = dir.path().join("grandchild.pid");
    let script = write_script(
        &dir,
        "child.sh",
        &format!(
            "#!/bin/sh\n(trap '' TERM; exec sleep 300) &\necho $! > {}\nwhile :; do sleep 1; done\n",
            gc.display()
        ),
    );
    let mut config = base_config(
        script,
        dir.path().join("crashes"),
        format!("http://127.0.0.1:{port}/v1/models"),
    );
    config.terminate_grace = Duration::from_secs(1);
    let supervisor = Arc::new(Supervisor::new(config));
    let mut states = supervisor.subscribe();
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };
    let mut healthy = false;
    for _ in 0..40 {
        if let Ok(Ok(SupervisorState::Healthy { .. })) =
            tokio::time::timeout(Duration::from_millis(150), states.recv()).await
        {
            healthy = true;
            break;
        }
    }
    assert!(healthy, "child never became healthy");
    let grandchild: i32 = std::fs::read_to_string(&gc)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        process_alive(grandchild),
        "grandchild {grandchild} not running"
    );

    assert!(supervisor.terminate());
    tokio::time::timeout(Duration::from_secs(5), run_handle)
        .await
        .expect("run loop did not exit after terminate()")
        .unwrap();
    let mut gone = false;
    for _ in 0..20 {
        if !process_alive(grandchild) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        gone,
        "grandchild {grandchild} outlived the terminated child"
    );
}

/// The child is told to die with its parent: `PR_GET_PDEATHSIG`, read by
/// the exec'd process itself (the setting survives exec), is SIGKILL.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_child_carries_a_parent_death_signal() {
    let _timing = supervisor_timing_lock();
    let dir = TempDir::new().unwrap();
    let (port, _server) = spawn_health_server().await;
    let report = dir.path().join("pdeathsig");
    let script = write_script(
        &dir,
        "child.sh",
        &format!(
            // exec: a forked subshell would not inherit the signal.
            "#!/bin/sh\nexec python3 -c 'import ctypes, time; l=ctypes.CDLL(None); \
             s=ctypes.c_int(); l.prctl(2, ctypes.byref(s)); \
             open(\"{}\", \"w\").write(str(s.value)); time.sleep(30)'\n",
            report.display()
        ),
    );
    let config = base_config(
        script,
        dir.path().join("crashes"),
        format!("http://127.0.0.1:{port}/v1/models"),
    );
    let supervisor = Arc::new(Supervisor::new(config));
    let run_handle = {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await })
    };
    let mut seen = String::new();
    for _ in 0..50 {
        seen = std::fs::read_to_string(&report).unwrap_or_default();
        if !seen.trim().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    supervisor.terminate();
    let _ = tokio::time::timeout(Duration::from_secs(5), run_handle).await;
    assert_eq!(
        seen.trim(),
        libc::SIGKILL.to_string(),
        "the child's PR_GET_PDEATHSIG"
    );
}

/// Env that turns this test binary into the supervising parent of
/// `a_sigkilled_parent_leaves_no_child`.
const PARENT_ROLE: &str = "SUPERVISOR_TEST_PARENT_DIR";

/// The parent half: a multi-thread runtime whose worker spawns the child,
/// as the daemon's does. Runs only when the test below re-executes this
/// binary with [`PARENT_ROLE`] set; otherwise it returns at once.
#[cfg(target_os = "linux")]
#[test]
fn sigkilled_parent_role() {
    let Ok(dir) = std::env::var(PARENT_ROLE) else {
        return;
    };
    let dir = PathBuf::from(dir);
    // A current-thread runtime driven first by a thread that then exits:
    // whatever that thread polled, it forked, so a child whose
    // parent-death signal followed the forking thread would be told its
    // parent died while this process lives on (the daemon's case, which
    // reached it through a retired tokio worker).
    let rt = Arc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
    );
    let first = Arc::clone(&rt);
    let setup_dir = dir.clone();
    std::thread::spawn(move || {
        first.block_on(async move {
            let (port, server) = spawn_health_server().await;
            std::mem::forget(server);
            let script = setup_dir.join("child.sh");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\necho $$ >> {}\nexec sleep 300\n",
                    setup_dir.join("child.pid").display()
                ),
            )
            .unwrap();
            let mut perm = std::fs::metadata(&script).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&script, perm).unwrap();
            let config = base_config(
                script,
                setup_dir.join("crashes"),
                format!("http://127.0.0.1:{port}/v1/models"),
            );
            let supervisor = Arc::new(Supervisor::new(config));
            let mut states = supervisor.subscribe();
            tokio::spawn({
                let sup = Arc::clone(&supervisor);
                async move { sup.run().await }
            });
            while !matches!(states.recv().await, Ok(SupervisorState::Healthy { .. })) {}
            let log = setup_dir.join("states.log");
            tokio::spawn(async move {
                while let Ok(state) = states.recv().await {
                    let line = format!("{state:?}\n");
                    let _ = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&log)
                        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
                }
            });
            std::mem::forget(supervisor);
        })
    })
    .join()
    .unwrap();
    rt.block_on(async { tokio::time::sleep(Duration::from_secs(300)).await });
}

/// The daemon is SIGKILLed: nothing runs its shutdown, and only the
/// child's parent-death signal can stop it.
#[cfg(target_os = "linux")]
#[test]
fn a_sigkilled_parent_leaves_no_child() {
    let dir = TempDir::new().unwrap();
    let mut parent = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "supervisor::tests::lifetime::sigkilled_parent_role",
            "--nocapture",
        ])
        .env(PARENT_ROLE, dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(dir.path().join("parent.err")).unwrap())
        .spawn()
        .unwrap();
    let pidfile = dir.path().join("child.pid");
    let mut child = None;
    for _ in 0..100 {
        if let Some(pid) = std::fs::read_to_string(&pidfile)
            .ok()
            .and_then(|s| s.lines().next().and_then(|l| l.trim().parse::<i32>().ok()))
        {
            child = Some(pid);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let child = child.expect("the supervised child never started");
    // The thread that drove the first spawn has exited by now; the child
    // must still be the one and only generation.
    std::thread::sleep(Duration::from_secs(2));
    let spawns = std::fs::read_to_string(&pidfile).unwrap().lines().count();
    assert!(
        process_alive(child) && spawns == 1,
        "the child died or was respawned ({spawns} spawns); parent exited: {:?}; states: {}; parent stderr: {}",
        parent.try_wait(),
        std::fs::read_to_string(dir.path().join("states.log")).unwrap_or_default(),
        std::fs::read_to_string(dir.path().join("parent.err")).unwrap_or_default()
    );
    parent.kill().unwrap();
    let _ = parent.wait();
    let mut gone = false;
    for _ in 0..50 {
        if !process_alive(child) {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !gone {
        // SAFETY: test cleanup of a pid we spawned.
        unsafe { libc::kill(child, libc::SIGKILL) };
    }
    assert!(gone, "child {child} outlived its SIGKILLed parent");
}
