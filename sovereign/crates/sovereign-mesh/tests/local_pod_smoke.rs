// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local-podman smoke test for the ephemeral worker daemon.
//!
//! Builds the `sovereign-worker-local:test` image (built ahead of
//! time via `scripts/pod build`), runs it with a freshly-minted
//! bootstrap blob in env, then drives the full owner-side wire
//! protocol — wait_for_health, dispatch, poll, destroy.
//!
//! This catches every failure mode we'd otherwise discover only
//! after paying for a Vast offer:
//!
//! - Container image doesn't have `sovereign-cli` on PATH
//! - `entrypoint.sh` rejects a valid blob
//! - `daemon run --worker-mode` fails to parse the env var
//! - Daemon doesn't bind `:9742` cleanly
//! - TLS handshake fails between the seed-derived cert and the
//!   owner's pinned reqwest client over a real loopback socket
//! - The four owner-only routes don't survive the container
//!   network boundary
//!
//! Gated `#[ignore]` because (a) it requires podman + the wrapper
//! at `scripts/pod`, and (b) it spins up a real container each run.
//! Trigger manually:
//!
//!     cargo test --package sovereign-mesh --test local_pod_smoke -- --ignored --nocapture
//!
//! Prereqs:
//! - `target/release/sovereign-cli` must exist (the image COPYs it)
//! - The `sovereign-worker-local:test` image must be built
//!   (run `scripts/local-pod-smoke.sh build` first, or the test
//!   builds it as its first step)

use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use sovereign_mesh::worker_controller::{
    ControllerConfig, JobSpec, ProviderError, ProviderInstance, ProviderResult, PublicAddress,
    WorkerController, WorkerProvider,
};
use sovereign_mesh::worker_pod::{encode_bootstrap, mint_bootstrap, BootstrapInputs};

const IMAGE_TAG: &str = "sovereign-worker-local:test";
const CONTAINER_NAME: &str = "sovereign-worker-local-test-instance";

/// Resolve workspace-relative paths. `cargo test` runs from the crate
/// directory, not the workspace root, so we walk up from
/// `CARGO_MANIFEST_DIR` until we find the workspace-root marker
/// (`scripts/pod`).
fn workspace_path(relative: &str) -> std::path::PathBuf {
    let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..6 {
        if dir.join("scripts/pod").exists() {
            return dir.join(relative);
        }
        if !dir.pop() {
            break;
        }
    }
    std::path::PathBuf::from(relative)
}

fn pod_wrapper() -> std::path::PathBuf {
    workspace_path("scripts/pod")
}

fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// Pre-flight: is the wrapper executable and podman responsive?
/// Returns Some(reason) to skip the test, None to continue.
fn skip_reason() -> Option<String> {
    if !pod_wrapper().exists() {
        return Some(format!(
            "{} not found — run from workspace root",
            pod_wrapper().display()
        ));
    }
    let out = Command::new(pod_wrapper()).arg("info").output();
    match out {
        Ok(o) if o.status.success() => None,
        Ok(o) => Some(format!(
            "podman wrapper failed: {}",
            String::from_utf8_lossy(&o.stderr)
                .chars()
                .take(200)
                .collect::<String>()
        )),
        Err(e) => Some(format!("podman wrapper not runnable: {e}")),
    }
}

/// Ensure the image exists. If not, build it. Returns `Err` if the
/// build fails (caller will skip the test with a clear message).
fn ensure_image() -> Result<(), String> {
    let inspect = Command::new(pod_wrapper())
        .args(["image", "exists", IMAGE_TAG])
        .status()
        .map_err(|e| format!("podman image exists: {e}"))?;
    if inspect.success() {
        return Ok(());
    }
    eprintln!("[local-pod-smoke] image {IMAGE_TAG} not present — building…");
    let build = Command::new(pod_wrapper())
        .args([
            "build",
            "-t",
            IMAGE_TAG,
            "-f",
            "sovereign/container/Containerfile.local-test",
            "--ignorefile",
            ".containerignore.local-test",
            ".",
        ])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("podman build: {e}"))?;
    if !build.success() {
        return Err(format!("podman build exited with {build}"));
    }
    Ok(())
}

/// Force-remove any lingering container from a prior run.
fn force_rm(name: &str) {
    let _ = Command::new(pod_wrapper())
        .args(["rm", "-f", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Build the `podman run` argument list for a smoke test.
///
/// Two non-obvious choices:
///
/// 1. **Skip `entrypoint.sh`** — the production entrypoint does a
///    clock-sync via `curl https://www.cloudflare.com` to fix Vast
///    hosts whose clocks have skewed by hours. Our local box's
///    clock is fine, and the minimal test image doesn't have curl.
///    `--entrypoint /usr/local/bin/sovereign-cli` exec's the binary
///    directly. The wire protocol is what we're validating, not the
///    shell wrapper (which is exercised by the production deploy).
///
/// 2. **Bind-mount `libvulkan.so.1`** — the host build of
///    `sovereign-cli` (built in the `dev-toolbox` toolbox)
///    dynamic-links `libvulkan.so.1`. Production CUDA/ROCm images
///    build their own binary against their native GPU stack and
///    don't have this problem; locally we need the host's
///    Vulkan loader to satisfy the linker before `main()` runs.
fn run_args<'a>(container_name: &'a str, host_port: u16, bootstrap_b64: &'a str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "run".into(),
        "-d".into(),
        "--rm".into(),
        "--name".into(),
        container_name.into(),
        "-p".into(),
        format!("127.0.0.1:{host_port}:9742"),
        "-e".into(),
        "SOVEREIGN_WORKER_RUNNER=echo".into(),
        "-e".into(),
        format!("SOVEREIGN_BOOTSTRAP={bootstrap_b64}"),
        // Skip the production entrypoint — see fn docs above.
        "--entrypoint".into(),
        "/usr/local/bin/sovereign-cli".into(),
    ];
    // Bind-mount the host's /lib64 read-only as /host-lib64 and
    // prepend it to LD_LIBRARY_PATH. The host binary was built in
    // the dev-toolbox toolbox and links against the host's
    // OpenSSL 3, libgomp, libvulkan, etc. Trying to bind each one
    // individually devolves into whack-a-mole as the binary's
    // transitive deps shift; mounting the whole directory is the
    // cleanest local-test plumbing.
    //
    // SAFETY note: production CUDA/ROCm images build the binary
    // inside the container against the matching system libs, so
    // none of this run-time fixup applies to Vast deploys.
    if std::path::Path::new("/lib64").exists() {
        args.push("-v".into());
        args.push("/lib64:/host-lib64:ro".into());
        args.push("-e".into());
        args.push("LD_LIBRARY_PATH=/host-lib64".into());
    }
    args.push(IMAGE_TAG.into());
    // Args to sovereign-cli itself (now that we bypassed
    // entrypoint.sh).
    args.push("daemon".into());
    args.push("run".into());
    args.push(sovereign_contracts::launch::WORKER_MODE_FLAG.into());
    args
}

/// Pick an unused TCP port by binding `:0` and reading the local
/// addr. Drop the listener immediately. Small race window but fine
/// for a single-process test.
fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind :0");
    let port = l.local_addr().expect("local_addr").port();
    drop(l);
    port
}

/// Tail the container logs and surface them on test failure so we
/// can debug entrypoint.sh issues without re-running with --nocapture.
fn dump_container_logs(name: &str, label: &str) {
    eprintln!("\n=== {label}: container logs for {name} ===");
    let _ = Command::new(pod_wrapper())
        .args(["logs", name])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
    eprintln!("=== end logs ===\n");
}

/// Trivial provider — the test pre-creates the container via raw
/// `podman run`, so the controller's provider machinery just hands
/// out the address we already know.
struct PreboundProvider {
    address: PublicAddress,
}
impl WorkerProvider for PreboundProvider {
    fn create(&self, _bootstrap_b64: &str, _spec: &JobSpec) -> ProviderResult<ProviderInstance> {
        Ok(ProviderInstance {
            instance_id: CONTAINER_NAME.to_string(),
            gpu_name: "Local".into(),
            cost_per_hour: 0.0,
        })
    }
    fn address(&self, _instance_id: &str) -> ProviderResult<Option<PublicAddress>> {
        Ok(Some(self.address.clone()))
    }
    fn destroy(&self, _instance_id: &str) -> ProviderResult<()> {
        // The test does its own podman rm; provider.destroy is a no-op.
        Ok(())
    }
}

/// Stub provider that returns a hard error on `create`. Used by tests
/// that pre-create their own pod and don't want a phantom controller
/// trying to spawn another one.
struct FailingProvider;
impl WorkerProvider for FailingProvider {
    fn create(&self, _b: &str, _s: &JobSpec) -> ProviderResult<ProviderInstance> {
        Err(ProviderError::Other(
            "must not be called in this test".into(),
        ))
    }
    fn address(&self, _: &str) -> ProviderResult<Option<PublicAddress>> {
        Ok(None)
    }
    fn destroy(&self, _: &str) -> ProviderResult<()> {
        Ok(())
    }
}

#[tokio::test]
#[ignore]
async fn local_pod_smoke_full_lifecycle() {
    install_crypto_provider();
    if let Some(reason) = skip_reason() {
        eprintln!("[local-pod-smoke] SKIP — {reason}");
        return;
    }
    if let Err(e) = ensure_image() {
        panic!("could not prepare image: {e}");
    }

    // Clean state from prior runs.
    force_rm(CONTAINER_NAME);

    // ── Mint bootstrap blob ────────────────────────────────────────
    let owner = SigningKey::from_bytes(&[42u8; 32]);
    let (blob, _) = mint_bootstrap(BootstrapInputs {
        job_id: "local-smoke".into(),
        owner_signing: &owner,
        expected_uploads: BTreeMap::new(),
        ttl_seconds: 600,
        seed_override: Some([7u8; 32]),
    })
    .expect("mint_bootstrap");
    let encoded = encode_bootstrap(&blob).expect("encode_bootstrap");

    // ── Run the container ──────────────────────────────────────────
    let host_port = pick_free_port();
    eprintln!(
        "[local-pod-smoke] starting container {CONTAINER_NAME} \
         (host port {host_port} → :9742); echo-runner mode"
    );
    let run = Command::new(pod_wrapper())
        .args(run_args(CONTAINER_NAME, host_port, &encoded))
        .output()
        .expect("podman run");
    if !run.status.success() {
        panic!(
            "podman run failed:\n  stdout: {}\n  stderr: {}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr),
        );
    }

    // Ensure cleanup even on panic. Drop guard pattern.
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            force_rm(CONTAINER_NAME);
        }
    }
    let _guard = Cleanup;

    // ── Drive the owner-side wire protocol ────────────────────────
    let provider = Arc::new(PreboundProvider {
        address: PublicAddress {
            host: "127.0.0.1".to_string(),
            port: host_port,
        },
    });
    let mut config = ControllerConfig::default();
    // Container startup includes the entrypoint's clock-sync probe
    // which can take 5-10 s on a cold pull. Allow generous health
    // poll so a slow boot doesn't false-fail.
    config.health_poll_interval = Duration::from_millis(250);
    config.health_poll_timeout = Duration::from_secs(60);
    let controller = WorkerController::new(provider, owner.clone(), config);

    let client = sovereign_mesh::worker_controller::build_pinned_client_for(&blob)
        .expect("build_pinned_client_for");
    let handle = sovereign_mesh::worker_pod::WorkerHandle::new(
        "127.0.0.1".to_string(),
        host_port,
        blob.pod_pubkey_thumbprint(),
        blob.worker_token.clone(),
        blob.job_id.clone(),
        owner.clone(),
    );

    // 1. Health
    if let Err(e) = controller.wait_for_health(&handle, &client).await {
        dump_container_logs(CONTAINER_NAME, "health-check failed");
        panic!("wait_for_health: {e}");
    }
    eprintln!("[local-pod-smoke] /health: OK");

    // 2. Dispatch a 3-unit echo job (unit_ids start at 1).
    let spec = JobSpec {
        job_id: "local-smoke".into(),
        image: "ignored".into(),
        disk_gb: 0,
        gpu_name: "Local".into(),
        max_price_per_hour: 0.0,
        label: "smoke".into(),
        uploads: BTreeMap::new(),
        units: vec![
            sovereign_mesh::worker_http::WorkUnit {
                unit_id: 1,
                kind: "u1".into(),
                payload: serde_json::json!({"q": "first"}),
            },
            sovereign_mesh::worker_http::WorkUnit {
                unit_id: 2,
                kind: "u2".into(),
                payload: serde_json::json!({"q": "second"}),
            },
            sovereign_mesh::worker_http::WorkUnit {
                unit_id: 3,
                kind: "u3".into(),
                payload: serde_json::json!({"q": "third"}),
            },
        ],
        runner_config: serde_json::json!({}),
    };
    if let Err(e) = controller.dispatch_job(&handle, &client, &spec).await {
        dump_container_logs(CONTAINER_NAME, "dispatch failed");
        panic!("dispatch_job: {e}");
    }
    eprintln!("[local-pod-smoke] dispatch: 3 units accepted");

    // 3. Poll until all 3 echo back, with a 15 s ceiling.
    let mut total = 0usize;
    let started = std::time::Instant::now();
    while total < 3 && started.elapsed() < Duration::from_secs(15) {
        let batch = controller
            .poll_completed(&handle, &client)
            .await
            .expect("poll_completed");
        total = batch.total_completed;
        if total < 3 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    if total != 3 {
        dump_container_logs(CONTAINER_NAME, "poll drain incomplete");
        panic!("expected 3 completed units, got {total}");
    }
    eprintln!("[local-pod-smoke] poll: 3/3 units echoed");

    // 4. DELETE → shutdown flag
    controller
        .destroy(&handle, &client, CONTAINER_NAME)
        .await
        .expect("destroy");
    eprintln!("[local-pod-smoke] destroy: OK");

    eprintln!("\n[local-pod-smoke] ✓ full lifecycle PASS");
}

/// Validates the impostor rejection path against a real container —
/// same as the unit-level e2e test, but proves the rejection survives
/// the network boundary the actual Vast deploy will cross.
#[tokio::test]
#[ignore]
async fn local_pod_rejects_impostor_owner() {
    install_crypto_provider();
    if let Some(reason) = skip_reason() {
        eprintln!("[local-pod-impostor] SKIP — {reason}");
        return;
    }
    if let Err(e) = ensure_image() {
        panic!("could not prepare image: {e}");
    }

    let container_name = "sovereign-worker-local-impostor";
    force_rm(container_name);

    let owner_a = SigningKey::from_bytes(&[1u8; 32]);
    let owner_b = SigningKey::from_bytes(&[2u8; 32]);
    let (blob, _) = mint_bootstrap(BootstrapInputs {
        job_id: "impostor-test".into(),
        owner_signing: &owner_a,
        expected_uploads: BTreeMap::new(),
        ttl_seconds: 600,
        seed_override: Some([99u8; 32]),
    })
    .unwrap();
    let encoded = encode_bootstrap(&blob).unwrap();

    let host_port = pick_free_port();
    let run = Command::new(pod_wrapper())
        .args(run_args(container_name, host_port, &encoded))
        .output()
        .expect("podman run");
    if !run.status.success() {
        panic!(
            "podman run failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );
    }

    struct Cleanup<'a>(&'a str);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            force_rm(self.0);
        }
    }
    let _g = Cleanup(container_name);

    // Mint a token signed by owner B — pinned cert still works (seed
    // is shared), but the bearer signature won't verify against the
    // owner verifying key embedded in the blob.
    let claims = sovereign_mesh::worker_pod::TokenClaims {
        job_id: "impostor-test".into(),
        owner_pubkey_thumbprint: [0u8; 32],
        pod_pubkey_thumbprint: blob.pod_pubkey_thumbprint(),
        expires_unix: u64::MAX / 2,
    };
    let bad_token = sovereign_mesh::worker_pod::sign_worker_token(&owner_b, &claims).unwrap();

    let client = sovereign_mesh::worker_controller::build_pinned_client_for(&blob).unwrap();
    let handle = sovereign_mesh::worker_pod::WorkerHandle::new(
        "127.0.0.1".to_string(),
        host_port,
        blob.pod_pubkey_thumbprint(),
        bad_token,
        "impostor-test",
        owner_b.clone(),
    );

    let mut config = ControllerConfig::default();
    config.health_poll_interval = Duration::from_millis(250);
    // Short timeout — we want the rejection to bubble up fast, not
    // wait for the default 5-min poll deadline.
    config.health_poll_timeout = Duration::from_secs(15);
    let controller = WorkerController::new(Arc::new(FailingProvider), owner_b.clone(), config);

    let err = controller
        .wait_for_health(&handle, &client)
        .await
        .expect_err("impostor must be rejected");
    let msg = err.to_string();
    eprintln!("[local-pod-impostor] rejection (expected): {msg}");
    assert!(
        msg.contains("401") || msg.to_lowercase().contains("unauth") || msg.contains("timed out"),
        "expected auth rejection, got: {msg}"
    );
    eprintln!("\n[local-pod-impostor] ✓ impostor rejected at container boundary");
}

/// Multi-pod local smoke — spins up THREE real containers (each with
/// distinct seeds + tokens), drives them via [`PoolHandle`], and
/// validates the fan-in poll drains units correctly across real TLS
/// boundaries. This is the highest-fidelity test we can run before
/// paying for Vast.
#[tokio::test]
#[ignore]
async fn local_pod_pool_three_containers_drain() {
    use sovereign_mesh::multi_pod_coordinator::{
        partition_units, CoordinatorConfig, PoolHandle, PoolPod,
    };
    use tokio::sync::Mutex as AsyncMutex;

    install_crypto_provider();
    if let Some(reason) = skip_reason() {
        eprintln!("[local-pod-pool] SKIP — {reason}");
        return;
    }
    if let Err(e) = ensure_image() {
        panic!("could not prepare image: {e}");
    }

    let pod_count = 3;
    let names: Vec<String> = (0..pod_count)
        .map(|i| format!("sovereign-worker-pool-p{i}"))
        .collect();
    for n in &names {
        force_rm(n);
    }
    struct Cleanup(Vec<String>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for n in &self.0 {
                force_rm(n);
            }
        }
    }
    let _g = Cleanup(names.clone());

    // Build the partition of 7 units across 3 pods (matching the
    // mesh-level e2e test pattern). unit_ids must start at 1 — the
    // /completed cursor uses `> since` watermark semantics.
    let total_units = 7usize;
    let units: Vec<sovereign_mesh::worker_http::WorkUnit> = (1..=total_units)
        .map(|i| sovereign_mesh::worker_http::WorkUnit {
            unit_id: i as u64,
            kind: format!("u{i}"),
            payload: serde_json::json!({"i": i}),
        })
        .collect();
    let partitions = partition_units(units.clone(), pod_count);

    // Per pod: distinct seed, distinct job_id, distinct container,
    // distinct host port. Build the PoolPod set manually (rather than
    // going through MultiPodCoordinator::launch) so each daemon's
    // bootstrap blob can be picked up by its own container.
    let owner = SigningKey::from_bytes(&[123u8; 32]);
    let mut pool_pods: Vec<Arc<PoolPod>> = Vec::with_capacity(pod_count);
    for i in 0..pod_count {
        let seed_bytes = [200u8 + i as u8; 32];
        let job_id = format!("pool-smoke-p{i}");
        let (blob, _) = mint_bootstrap(BootstrapInputs {
            job_id: job_id.clone(),
            owner_signing: &owner,
            expected_uploads: BTreeMap::new(),
            ttl_seconds: 600,
            seed_override: Some(seed_bytes),
        })
        .unwrap();
        let encoded = encode_bootstrap(&blob).unwrap();
        let host_port = pick_free_port();
        eprintln!(
            "[local-pod-pool] starting pod {i}: {} → :9742 ({})",
            host_port, names[i]
        );
        let run = Command::new(pod_wrapper())
            .args(run_args(&names[i], host_port, &encoded))
            .output()
            .expect("podman run");
        if !run.status.success() {
            panic!(
                "podman run for pod {i} failed: {}",
                String::from_utf8_lossy(&run.stderr)
            );
        }
        let client = sovereign_mesh::worker_controller::build_pinned_client_for(&blob).unwrap();
        let handle = sovereign_mesh::worker_pod::WorkerHandle::new(
            "127.0.0.1".to_string(),
            host_port,
            blob.pod_pubkey_thumbprint(),
            blob.worker_token.clone(),
            job_id.clone(),
            owner.clone(),
        );
        let instance = sovereign_mesh::worker_controller::ProviderInstance {
            instance_id: names[i].clone(),
            gpu_name: "Local-Container".into(),
            cost_per_hour: 0.0,
        };
        pool_pods.push(Arc::new(PoolPod {
            handle,
            instance,
            blob,
            client,
            assigned_units: partitions[i].len(),
            received_units: AsyncMutex::new(0),
        }));
    }

    // Wait for each container's /health to come up, then dispatch its
    // partition. Same controller for all (the owner key is shared).
    let mut config = sovereign_mesh::worker_controller::ControllerConfig::default();
    config.health_poll_interval = Duration::from_millis(250);
    config.health_poll_timeout = Duration::from_secs(60);
    let provider = Arc::new(FailingProvider);
    let controller =
        sovereign_mesh::worker_controller::WorkerController::new(provider, owner.clone(), config);

    for (i, pp) in pool_pods.iter().enumerate() {
        if let Err(e) = controller.wait_for_health(&pp.handle, &pp.client).await {
            dump_container_logs(&names[i], &format!("pod {i} health failed"));
            panic!("pod {i} health: {e}");
        }
        let spec = JobSpec {
            job_id: pp.handle.job_id().to_string(),
            image: "ignored".into(),
            disk_gb: 0,
            gpu_name: "Local".into(),
            max_price_per_hour: 0.0,
            label: format!("pool-p{i}"),
            uploads: BTreeMap::new(),
            units: partitions[i].clone(),
            runner_config: serde_json::json!({}),
        };
        if let Err(e) = controller.dispatch_job(&pp.handle, &pp.client, &spec).await {
            dump_container_logs(&names[i], &format!("pod {i} dispatch failed"));
            panic!("pod {i} dispatch: {e}");
        }
        eprintln!(
            "[local-pod-pool] pod {i}: {} units dispatched",
            partitions[i].len()
        );
    }

    let pool = PoolHandle {
        pods: pool_pods.clone(),
        expected_total_units: total_units,
        config: CoordinatorConfig {
            poll_interval: Duration::from_millis(100),
            stall_timeout: Duration::from_secs(15),
            total_timeout: Duration::from_secs(60),
        },
        base_job_id: "pool-smoke".into(),
    };
    let observations: std::sync::Arc<std::sync::Mutex<Vec<(usize, u64)>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let obs_h = observations.clone();
    let summary = pool
        .poll_until_complete(&controller, |idx, unit| {
            obs_h.lock().unwrap().push((idx, unit.unit_id));
        })
        .await
        .expect("poll_until_complete");
    assert!(!summary.timed_out, "poll loop should not have timed out");
    assert_eq!(summary.total_received, total_units);

    // Verify each pod received exactly its partition.
    let obs = observations.lock().unwrap().clone();
    let mut got_per_pod: Vec<Vec<u64>> = vec![Vec::new(); pod_count];
    for (idx, uid) in obs {
        got_per_pod[idx].push(uid);
    }
    for i in 0..pod_count {
        let mut expected: Vec<u64> = partitions[i].iter().map(|u| u.unit_id).collect();
        let mut got = got_per_pod[i].clone();
        expected.sort();
        got.sort();
        assert_eq!(got, expected, "pod {i} partition mismatch");
    }
    eprintln!(
        "[local-pod-pool] ✓ 3 containers drained {} units cleanly in {:.1}s",
        summary.total_received,
        summary.elapsed.as_secs_f64()
    );
}
