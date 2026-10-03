// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn pipeline pod …` moved to `svrn mesh pod …` (phase-b-1 (10)). The
//! old spelling answers with a pointer to the new one and exits non-zero —
//! never the generic unknown-subcommand path (principle 6).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_svrn-ingest");

#[test]
fn pipeline_pod_points_at_mesh_pod_and_exits_nonzero() {
    let out = Command::new(BIN)
        .args(["pipeline", "pod", "list"])
        .output()
        .expect("spawn svrn-ingest");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("moved to `svrn mesh pod`") && stderr.contains("svrn mesh pod list"),
        "the old spelling must name the new one: {stderr}"
    );
}
