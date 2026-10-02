// SPDX-License-Identifier: AGPL-3.0-or-later
//! serve records and drops the pinned-pod snapshot its ranker reads
//! (pb-mesh-dissolve): `svrn pod up` hands `sovereign-serve pod-snapshot
//! record` a `PodSnapshotRequest` on stdin, `svrn pod down` runs `drop`. The
//! file lands where the ranker reads it (`<data root>/worker-pods/<id>.json`)
//! in the schema it reads, so the two halves cannot drift. Failing input: a
//! verb that is not routed exits 2 (`ServeArgs::parse`), and a record that
//! writes elsewhere fails the load below.

use std::io::Write;
use std::process::{Command, Stdio};

use sovereign_contracts::worker_pod::{
    derive_signing_key, mint_bootstrap, BootstrapInputs, PodSnapshotRequest,
};
use sovereign_serving_host::pinned_pod_snapshot::load_snapshot;

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-serve");

fn serve(root: &std::path::Path, args: &[&str], stdin: Option<&str>) -> std::process::Output {
    let mut child = Command::new(BIN)
        .arg("pod-snapshot")
        .args(args)
        .env("SVRNMESH_DATA_DIR", root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("serve runs");
    let mut pipe = child.stdin.take().unwrap();
    pipe.write_all(stdin.unwrap_or_default().as_bytes())
        .unwrap();
    drop(pipe);
    child.wait_with_output().unwrap()
}

#[test]
fn serve_records_the_snapshot_its_ranker_reads_and_drops_it() {
    let root = tempfile::tempdir().unwrap();
    let owner = derive_signing_key(&[55u8; 32]);
    let (blob, _) = mint_bootstrap(BootstrapInputs {
        job_id: "pinned-job".into(),
        owner_signing: &owner,
        expected_uploads: std::collections::BTreeMap::new(),
        ttl_seconds: 3600,
        seed_override: Some([9u8; 32]),
    })
    .expect("a bootstrap blob");
    let request = PodSnapshotRequest {
        vast_id: "vast-77".into(),
        host: "203.0.113.5".into(),
        port: 9742,
        bootstrap_blob: blob.clone(),
        system_ram_gb: 128,
    };

    let out = serve(
        root.path(),
        &["record"],
        Some(&serde_json::to_string(&request).unwrap()),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("wrote snapshot at"));

    let dir = root.path().join("worker-pods");
    let snap = load_snapshot(&dir, "vast-77").expect("the ranker reads what serve wrote");
    assert_eq!(
        (
            snap.host.as_str(),
            snap.port,
            snap.capabilities.system_ram_gb
        ),
        ("203.0.113.5", 9742, 128)
    );
    assert_eq!(snap.bootstrap_blob, blob);

    let out = serve(root.path(), &["drop", "vast-77"], None);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("removed pinned-pod snapshot for vast-77")
    );
    assert!(!dir.join("vast-77.json").exists());

    // Dropping what is not there is not an error, and says nothing.
    let out = serve(root.path(), &["drop", "vast-77"], None);
    assert!(out.status.success());
    assert!(out.stdout.is_empty());

    // A body that is not a request is refused by name, and writes nothing.
    let out = serve(root.path(), &["record"], Some("{}"));
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("PodSnapshotRequest"));
}
