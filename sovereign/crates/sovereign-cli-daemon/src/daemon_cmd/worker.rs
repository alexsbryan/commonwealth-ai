// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ephemeral worker-pod daemon entry — extracted from `daemon_cmd`
//! (§3.2). Runs the stripped-down pod daemon (no config/models/mesh;
//! owner-only routes) triggered by `daemon run --worker-mode`.

use std::sync::Arc;

/// Worker-mode entry — runs the ephemeral pod daemon. Skips every
/// persistent-peer surface (no config, no models, no mesh) and serves
/// only the four owner-only routes documented in
/// `sovereign/docs/EPHEMERAL_WORKER_PODS.md`.
///
/// Triggered by `sovereign daemon run --worker-mode`. The bootstrap
/// blob is read from `SOVEREIGN_BOOTSTRAP` env or `--bootstrap-blob
/// <file>`. Falls through to the foreground worker daemon; exits when
/// the owner sends `DELETE /internal/worker/job` (TBD: wire shutdown
/// from the worker state's flag) or the process receives SIGTERM.
pub(super) async fn run_worker_daemon(args: &[String]) -> i32 {
    // Parse `--bootstrap-blob <path>` if supplied. The env-var path
    // is the production default (Vast injects it via `onstart_cmd`);
    // the file-path mode is for local testing where shell quoting a
    // 500-byte blob is painful.
    let mut blob_path: Option<std::path::PathBuf> = None;
    let mut iter = args.iter().enumerate();
    while let Some((_, a)) = iter.next() {
        if a == "--bootstrap-blob" {
            if let Some((_, p)) = iter.next() {
                blob_path = Some(std::path::PathBuf::from(p));
            }
        }
    }

    let (blob, source) = match sovereign_mesh::worker_daemon::load_bootstrap_blob(
        "SOVEREIGN_BOOTSTRAP",
        blob_path.as_deref(),
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("worker daemon: {e}");
            return 2;
        }
    };
    eprintln!(
        "[worker-daemon] bootstrap loaded from {source}; job_id={} expected_uploads={}",
        blob.job_id,
        blob.expected_uploads.len()
    );

    // Pod models dir — the disk-dump watcher writes uploaded bytes
    // here, and the SubprocessRunner spawns a child daemon against
    // the config the watcher writes one level up.
    let models_dir = std::env::var("SOVEREIGN_MODELS_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/workspace/models"));
    let config_path = models_dir
        .parent()
        .map(|p| p.join("config.toml"))
        .unwrap_or_else(|| models_dir.join("config.toml"));

    // Pre-build the disk-dump signals so the runner can share them
    // with the WorkerState. Without this the runner would never
    // observe the dump completing (it'd hold a different Notify
    // than the watcher fires).
    let signals = sovereign_mesh::worker_daemon::new_disk_dump_signals();

    // `SOVEREIGN_WORKER_RUNNER=echo` falls back to the stub used
    // during early integration testing — useful when validating the
    // wire protocol against a real Vast pod before the child daemon
    // is known-good. Production default is `subprocess`.
    let runner_kind =
        std::env::var("SOVEREIGN_WORKER_RUNNER").unwrap_or_else(|_| "subprocess".to_string());
    // Keep the runner as both `Arc<dyn WorkerRunner>` (for the worker
    // daemon entrypoint) and — in the subprocess case — as a typed
    // `Arc<SubprocessRunner>` so we can call `child_ready_signal()`
    // to wire the pod-side inference proxy. Without this typed
    // sibling the proxy can never be enabled because the trait
    // object hides the readiness flag.
    let (runner, inference_proxy): (
        Arc<dyn sovereign_mesh::worker_http::WorkerRunner>,
        Option<Arc<sovereign_mesh::worker_inference_proxy::InferenceProxyConfig>>,
    ) = match runner_kind.as_str() {
        "echo" => {
            eprintln!("[worker-daemon] runner: echo (stub — no inference will run)");
            // Echo runner has no child daemon — leave the proxy
            // disabled. The /v1/* routes return 404 and the wire
            // protocol stays at /internal/worker/*.
            (Arc::new(sovereign_mesh::worker_daemon::EchoRunner), None)
        }
        other => {
            if other != "subprocess" {
                eprintln!(
                    "[worker-daemon] unrecognised SOVEREIGN_WORKER_RUNNER={other:?}; \
                     falling back to subprocess"
                );
            }
            eprintln!(
                "[worker-daemon] runner: subprocess (child daemon will spawn against {})",
                config_path.display()
            );
            let cfg = sovereign_mesh::worker_subprocess_runner::SubprocessRunnerConfig {
                config_path: config_path.clone(),
                ..Default::default()
            };
            let child_port = cfg.child_client_port;
            let subprocess = Arc::new(
                sovereign_mesh::worker_subprocess_runner::SubprocessRunner::new(
                    cfg,
                    signals.0.clone(),
                    signals.1.clone(),
                ),
            );
            // Build the proxy config from the same readiness atomic
            // the subprocess runner flips when `/v1/models` first
            // returns 200. The proxy reads it on every request so
            // owner-side scheduler calls naturally 503 during the
            // ~90s model warmup instead of seeing ECONNREFUSED.
            let proxy = Arc::new(
                sovereign_mesh::worker_inference_proxy::InferenceProxyConfig::for_local_child(
                    format!("http://127.0.0.1:{child_port}"),
                    subprocess.child_ready_signal(),
                ),
            );
            eprintln!(
                "[worker-daemon] inference proxy enabled (→ http://127.0.0.1:{child_port}) — \
                 owner-side mesh scheduler can now route /v1/chat/completions to this pod"
            );
            let trait_obj: Arc<dyn sovereign_mesh::worker_http::WorkerRunner> = subprocess;
            (trait_obj, Some(proxy))
        }
    };

    if let Err(e) = sovereign_mesh::worker_daemon::run_worker_mode_with_signals(
        blob,
        runner,
        None,
        Some(models_dir),
        Some(signals),
        inference_proxy,
    )
    .await
    {
        eprintln!("worker daemon: serve failed: {e}");
        return 1;
    }
    0
}
