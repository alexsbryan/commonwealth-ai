// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn daemon run --worker-mode` execs the `sovereign-pod-worker` binary
//! (pb-pods-worker), and the worker answers `/internal/worker/health` on
//! :9742 with the bootstrap's job id.
//!
//! The worker binds 0.0.0.0:9742, fixed (worker_daemon.rs), and a host's own
//! daemon may hold 127.0.0.1:9742, so the run happens in a private network
//! namespace (`unshare -rn`): it never touches the host's daemon. Linux only.
//! `SOVEREIGN_POD_WORKER_BIN`, else `sovereign-pod-worker` beside the binary
//! under test; absent is a FAILURE naming the build, never a skip. Two
//! programs' binaries side by side, so the test is the distribution's (moved
//! from sovereign-cli-daemon by pb-distribution-svrn-lift-2: a lifted svrn
//! has no pod worker to exec).
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use ed25519_dalek::SigningKey;
use sovereign_contracts::worker_pod::{encode_bootstrap, mint_bootstrap, BootstrapInputs};

/// The binary under test, `svrn daemon`'s: sovereign-cli-daemon, which a stock
/// install puts beside this package's binary.
fn cli_daemon() -> PathBuf {
    let bin =
        Path::new(env!("CARGO_BIN_EXE_sovereign-stock")).with_file_name("sovereign-cli-daemon");
    assert!(
        bin.is_file(),
        "{} is missing: build it with `cargo build -p sovereign-cli-daemon`",
        bin.display()
    );
    bin
}

fn pod_worker_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("SOVEREIGN_POD_WORKER_BIN") {
        return PathBuf::from(p);
    }
    let beside = cli_daemon().with_file_name("sovereign-pod-worker");
    assert!(
        beside.is_file(),
        "{} is missing: build it with `cargo build -p sovereign-pods`, or set SOVEREIGN_POD_WORKER_BIN",
        beside.display()
    );
    beside
}

/// Inside the namespace: bring loopback up, start the verb, poll health,
/// then report which binary the verb's pid became and what health said.
const SCRIPT: &str = r#"
ip link set lo up || exit 90
"$BIN" daemon run --worker-mode --bootstrap-blob "$BLOB" 2>"$LOG" &
pid=$!
out=""
for _ in $(seq 1 150); do
  out=$(curl -sk --max-time 2 -H "Authorization: Bearer $TOKEN" https://127.0.0.1:9742/internal/worker/health) && [ -n "$out" ] && break
  sleep 0.2
done
echo "EXE=$(readlink /proc/$pid/exe)"
echo "HEALTH=$out"
kill "$pid" 2>/dev/null
wait "$pid" 2>/dev/null
exit 0
"#;

#[test]
fn worker_mode_verb_execs_pod_worker_and_health_answers() {
    let worker = pod_worker_bin();
    let dir = tempfile::tempdir().unwrap();
    let owner = SigningKey::from_bytes(&[31u8; 32]);
    let (blob, _) = mint_bootstrap(BootstrapInputs {
        job_id: "pb-pods-worker-e2e".into(),
        owner_signing: &owner,
        expected_uploads: BTreeMap::new(),
        ttl_seconds: 600,
        seed_override: Some([55u8; 32]),
    })
    .unwrap();
    let blob_path = dir.path().join("bootstrap.b64");
    std::fs::write(&blob_path, encode_bootstrap(&blob).unwrap()).unwrap();
    let log = dir.path().join("worker.err");

    let out = Command::new("unshare")
        .args(["-rn", "sh", "-c", SCRIPT])
        .env("BIN", cli_daemon())
        .env("BLOB", &blob_path)
        .env("TOKEN", &blob.worker_token)
        .env("LOG", &log)
        .env("SOVEREIGN_POD_WORKER_BIN", &worker)
        .env("SOVEREIGN_MODELS_DIR", dir.path().join("models"))
        .env_remove("SOVEREIGN_BOOTSTRAP")
        .output()
        .expect("run unshare (util-linux)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let worker_log = std::fs::read_to_string(&log).unwrap_or_default();
    let ctx = format!(
        "status={:?}\nstdout:\n{stdout}\nstderr:\n{}\nworker log:\n{worker_log}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "namespace run failed\n{ctx}");

    let exe = stdout
        .lines()
        .find_map(|l| l.strip_prefix("EXE="))
        .unwrap_or_default();
    assert_eq!(
        Path::new(exe).file_name().and_then(|n| n.to_str()),
        Some("sovereign-pod-worker"),
        "the verb must exec the pod-worker binary\n{ctx}"
    );

    let health = stdout
        .lines()
        .find_map(|l| l.strip_prefix("HEALTH="))
        .unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(health)
        .unwrap_or_else(|e| panic!("health did not answer JSON ({e})\n{ctx}"));
    assert_eq!(v["job_id"], "pb-pods-worker-e2e", "{ctx}");
    assert_eq!(v["uploads_expected"], 0, "{ctx}");
}
