// SPDX-License-Identifier: AGPL-3.0-or-later
//! Acceptance for the distributed-primary containment boot guard.
//!
//! The hazard: a node declaring `[shared_model] role = "host"` while
//! `[compute] distributed_primary` is off holds the mesh-sharded model in the
//! DAEMON'S OWN process. When a worker leaves, the discovery loop reloads the
//! primary, and that reload's teardown must free the departed worker's buffers —
//! `ggml-rpc.cpp:386` → `GGML_ABORT` → the whole daemon dies (SIGABRT, exit
//! 134). It happened live on 2026-07-27, twice, and there is no catchable error
//! path in ggml's RPC client to fix it at runtime.
//!
//! So the guard refuses the configuration at boot and names the two-line fix,
//! the same posture the P0.2 fast-alias guard takes with a silent OOM. This test
//! is the acceptance criterion for that: the hazardous config must produce a
//! sentence a semitechnical operator can act on, not a crash three days later.
//!
//! Scope note: only the REFUSE path is exercised as an e2e. The
//! override-proceeds path would carry on into model loading and listener
//! binding, which would contend with the operator's real daemon on :9741 — the
//! verdict table itself is covered by the unit tests in `build::containment`.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-cli-daemon");

fn hazardous_home(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("containment-{tag}-{}", std::process::id()));
    let cfg_dir = home.join(".sovereign");
    std::fs::create_dir_all(&cfg_dir).expect("config dir");
    // A declared host, a primary that would be distributed across mesh
    // workers, and no compute-child boundary. Model paths are deliberately
    // nonexistent: the guard must fire BEFORE anything tries to load them, so
    // reaching a "failed to load models" error would itself be a failure.
    std::fs::write(
        cfg_dir.join("config.toml"),
        r#"
[models]
primary = "/nonexistent/Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003.gguf"
fast = "/nonexistent/Qwen3.5-0.8B-UD-Q6_K_XL.gguf"
embed = "/nonexistent/Qwen3-Embedding-0.6B-Q8_0.gguf"

[shared_model]
role = "host"

[compute]
enabled = false
distributed_primary = false
"#,
    )
    .expect("write config");
    home
}

/// Run the daemon against `home` and return (exit code, combined output).
/// Bounded: a guard that fails to fire must not leave a daemon running against
/// the developer's real ports.
fn run_daemon_bounded(home: &PathBuf) -> (Option<i32>, String) {
    let mut child = Command::new(BIN)
        .arg("daemon")
        .arg("run")
        .env("HOME", home)
        // The VRAM preflight runs first and would refuse this config for an
        // unrelated reason: the GGUF paths are deliberately nonexistent, so its
        // size estimate overflows to u64::MAX. Skipping it is what lets the
        // containment guard be the thing under test. (Worth noting for a future
        // reader: containment is a pure config fact and is cheaper to evaluate
        // than VRAM, so there is an argument for checking it first.)
        .env("SOVEREIGN_SKIP_VRAM_CHECK", "1")
        .env_remove("SOVEREIGN_ALLOW_INPROCESS_DISTRIBUTED_PRIMARY")
        .env_remove("SOVEREIGN_RPC_DISCOVER")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn daemon");

    // Drain both pipes on their own threads. A guard that refuses prints
    // several lines; if we waited on exit first and read afterwards, a child
    // that filled a pipe buffer would deadlock instead of failing the test.
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

    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(s) => break Some(s),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };

    let out = format!(
        "{}{}",
        t_out.join().unwrap_or_default(),
        t_err.join().unwrap_or_default()
    );
    (status.and_then(|s| s.code()), out)
}

#[test]
fn a_host_with_an_in_process_distributed_primary_refuses_to_boot() {
    let home = hazardous_home("refuse");
    let (code, out) = run_daemon_bounded(&home);

    assert_eq!(
        code,
        Some(1),
        "the daemon must refuse this configuration, exit 1. Output:\n{out}"
    );

    // The message is the whole point of choosing refusal over a warning: it has
    // to carry the fix, not just the diagnosis.
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

    // If the guard had not fired, boot would have continued into model loading
    // and failed on the nonexistent GGUFs instead — a different, useless error.
    assert!(
        !out.contains("failed to load models"),
        "the guard must fire BEFORE model loading. Output:\n{out}"
    );

    let _ = std::fs::remove_dir_all(&home);
}
