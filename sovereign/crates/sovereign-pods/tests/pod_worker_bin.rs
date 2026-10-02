// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-pod-worker` with no bootstrap blob refuses by name (exit 2)
//! instead of serving an unowned pod (pb-pods-worker).

use std::process::Command;

#[test]
fn pod_worker_without_bootstrap_refuses_by_name() {
    let out = Command::new(env!("CARGO_BIN_EXE_sovereign-pod-worker"))
        .args(["daemon", "run", "--worker-mode"])
        .env_remove("SOVEREIGN_BOOTSTRAP")
        .output()
        .expect("run sovereign-pod-worker");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr:\n{stderr}");
    assert!(
        stderr.contains("SOVEREIGN_BOOTSTRAP") && stderr.contains("worker mode"),
        "stderr:\n{stderr}"
    );
}
