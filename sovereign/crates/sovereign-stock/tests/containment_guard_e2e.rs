// SPDX-License-Identifier: AGPL-3.0-or-later
//! Acceptance for the distributed-primary containment boot guard, against the
//! stock binary (the process that hosts serve's assembly in-process).
//!
//! The hazard: a node declaring `[shared_model] role = "host"` while
//! `[compute] distributed_primary` is off holds the mesh-sharded model in its
//! OWN process. When a worker leaves, the reload's teardown frees the departed
//! worker's buffers — `ggml-rpc.cpp:386` → `GGML_ABORT` → the process dies
//! (SIGABRT, exit 134), live on 2026-07-27. There is no catchable error path,
//! so the guard (`sovereign_compute::containment::check_containment`, called
//! from compute's `ReloadFactory::build`) refuses the configuration at boot and
//! names the two-line fix.
//!
//! Restored from cli-daemon's `containment_guard_e2e.rs`, which c23f3b4c0
//! deleted when the daemon became its own binary (pb-distribution-o3-tests).
//! Only the REFUSE path runs as a process: the override path would load models
//! and bind. The verdict table is `sovereign_contracts::containment`'s units.

use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-stock");

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("ephemeral port")
        .port()
}

/// A declared host, a primary that would be distributed across mesh workers,
/// and no compute-child boundary, on sandbox ports. The GGUF paths do not
/// exist: the guard must fire BEFORE anything loads them.
fn hazardous_config(root: &Path, client: u16, internal: u16, dead_rails: u16) -> String {
    format!(
        "[models]\nprimary = \"/nonexistent/Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003.gguf\"\n\
         fast = \"/nonexistent/Qwen3.5-0.8B-UD-Q6_K_XL.gguf\"\n\
         embed = \"/nonexistent/Qwen3-Embedding-0.6B-Q8_0.gguf\"\n\n\
         [shared_model]\nrole = \"host\"\n\n\
         [compute]\nenabled = false\ndistributed_primary = false\n\n\
         [daemon]\nclient_port = {client}\ninternal_port = {internal}\n\
         rails_base = \"http://127.0.0.1:{dead_rails}\"\n\n\
         [data]\ndir = \"{r}/data\"\n",
        r = root.display()
    )
}

/// Run `cmd` to exit or a 60 s bound, and return (exit code, combined output,
/// the sandbox ports seen answering while it ran). A guard that fails to fire
/// is killed, never left listening.
fn run_bounded(mut cmd: Command, ports: &[u16]) -> (Option<i32>, String, Vec<u16>) {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    // Drained on their own threads: a child that fills a pipe buffer would
    // otherwise deadlock instead of failing.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    let mut answered = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        for &p in ports {
            let addr = SocketAddr::from(([127, 0, 0, 1], p));
            if TcpStream::connect_timeout(&addr, Duration::from_millis(20)).is_ok()
                && !answered.contains(&p)
            {
                answered.push(p);
            }
        }
        match child.try_wait().expect("try_wait") {
            Some(s) => break Some(s),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let out = format!(
        "{}{}",
        t_out.join().unwrap_or_default(),
        t_err.join().unwrap_or_default()
    );
    (status.and_then(|s| s.code()), out, answered)
}

/// The refusal carries the fix, not just the diagnosis, and fires before
/// model loading.
fn assert_refusal(code: Option<i32>, out: &str, answered: &[u16]) {
    assert!(
        matches!(code, Some(c) if c != 0),
        "the guard must refuse this configuration with a non-zero exit, got {code:?}. Output:\n{out}"
    );
    for expected in [
        "HOST",
        "IN-PROCESS",
        "[compute]",
        "distributed_primary = true",
        "role = \"consumer\"",
    ] {
        assert!(
            out.contains(expected),
            "refusal message must contain {expected:?}. Output:\n{out}"
        );
    }
    assert!(
        !out.contains("does not exist"),
        "the guard must fire BEFORE model loading. Output:\n{out}"
    );
    assert!(
        answered.is_empty(),
        "a refused boot bound a listener on {answered:?}. Output:\n{out}"
    );
}

#[test]
fn a_host_with_an_in_process_distributed_primary_refuses_to_boot() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().join("home");
    for d in [&home, &root.path().join("data"), &root.path().join("rails")] {
        std::fs::create_dir_all(d).expect("dir");
    }
    let (client, internal, serve, dead_rails) =
        (free_port(), free_port(), free_port(), free_port());
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        hazardous_config(root.path(), client, internal, dead_rails),
    )
    .expect("config");

    let mut cmd = Command::new(BIN);
    cmd.args(["run", "--config"])
        .arg(&config)
        .env("HOME", &home)
        .env("SVRNMESH_DATA_DIR", root.path().join("svrnmesh"))
        .env("CW_RAILS_DIR", root.path().join("rails"))
        .env("CW_RAILS_BIN", root.path().join("no-cw-rails"))
        .env("SOVEREIGN_SERVE_PORT", serve.to_string())
        // The VRAM preflight runs first and would refuse the nonexistent GGUFs
        // for an unrelated reason; skipping it leaves the guard under test.
        .env("SOVEREIGN_SKIP_VRAM_CHECK", "1")
        .env_remove("SOVEREIGN_ALLOW_INPROCESS_DISTRIBUTED_PRIMARY")
        .env_remove("SOVEREIGN_RPC_DISCOVER")
        .env_remove("RUST_LOG");
    let (code, out, answered) = run_bounded(cmd, &[client, internal, serve]);
    assert_refusal(code, &out, &answered);
}
