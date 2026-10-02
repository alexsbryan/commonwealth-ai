// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh pod list` renders the pod cost ledger (phase-b-1 (10): the pod
//! verbs are cmnwlth's). Runs the real sibling binary against a temp data
//! dir, never the operator's `~/.svrnmesh`.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-cli-mesh");

#[test]
fn mesh_pod_list_renders_a_seeded_ledger_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("pipeline-pods.json"),
        r#"{"pods":[{"vast_id":"4242","label":"seeded-pod","recipe_id":"sep-core-v1",
            "gpu_name":"L40S","image":"img","started_at":1700000000,
            "cost_per_hour":0.5,"status":"running"}]}"#,
    )
    .expect("seed ledger");
    let out = Command::new(BIN)
        .args(["mesh", "pod", "list"])
        .env("SVRNMESH_DATA_DIR", dir.path())
        .env("SOVEREIGN_DATA_DIR", dir.path())
        .output()
        .expect("spawn sovereign-cli-mesh");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let row = stdout
        .lines()
        .find(|l| l.starts_with("4242"))
        .unwrap_or_else(|| panic!("no row for the seeded pod: {stdout}"));
    assert!(
        row.contains("live") && row.contains("seeded-pod") && row.contains("L40S"),
        "{row}"
    );
    assert!(stdout.contains("running pods accruing:"), "{stdout}");
}
