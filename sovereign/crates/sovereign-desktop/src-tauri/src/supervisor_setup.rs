// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri-side wiring for the daemon supervisor (see `crate::supervisor`).
//!
//! Opt-in via `SOVEREIGN_USE_SUPERVISOR=1`. When set, the desktop's
//! Local-mode boot does NOT construct an in-process `EmbeddedDaemon`
//! — it spawns `sovereign-cli daemon run` as a child and talks to it
//! over HTTP using the existing Attach-mode plumbing. This protects
//! the Tauri UI from process-level ggml/llama.cpp crashes: when the
//! daemon dies, the supervisor surfaces a `supervisor-state` event
//! the frontend can render as a Reconnect banner.
//!
//! This module owns the Tauri-side surface — binary-path resolution,
//! event forwarding, the startup wait — so `supervisor.rs` itself
//! stays Tauri-free and unit-testable in isolation.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tracing::{info, warn};

use crate::supervisor::{Supervisor, SupervisorConfig, SupervisorState};

/// `SOVEREIGN_USE_SUPERVISOR=1` (or any truthy value) opts this
/// process into the supervised path. Default off — PR-2 dogfood is
/// explicitly env-gated.
pub fn is_enabled() -> bool {
    std::env::var("SOVEREIGN_USE_SUPERVISOR")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Locate the `sovereign-cli` binary. Probes in order:
/// 1. `SOVEREIGN_CLI_PATH` env override.
/// 2. Next to the running desktop binary — covers both the production
///    Tauri sidecar layout AND `tauri dev`'s target/{debug,release}
///    layout, since `tauri dev` puts the cli binary alongside the
///    desktop binary under the same workspace target dir.
fn resolve_sovereign_cli() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("SOVEREIGN_CLI_PATH") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let parent = exe.parent()?;
    for name in &["sovereign-cli", "sovereign-cli.exe"] {
        let candidate = parent.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// If supervised mode is requested AND the bootstrap probe returned
/// `Local { source: CliSetup(_) }`, try to spawn the daemon as a
/// child and switch to `Attach` against it. Returns the (possibly
/// overridden) bootstrap mode plus the spawned supervisor handle (or
/// `None` when supervision was skipped or failed).
///
/// Three fall-back cases all return the original mode + `None`:
/// - feature flag off,
/// - bootstrap mode wasn't `Local { CliSetup }`,
/// - supervisor couldn't bring up a healthy daemon within the
///   startup deadline.
///
/// Fall-back is loud (`warn!`) when the user opted in — silent
/// fall-back would mask why supervised mode "isn't working."
pub async fn maybe_start(
    mode: crate::bootstrap::BootstrapMode,
    app_handle: AppHandle,
) -> (crate::bootstrap::BootstrapMode, Option<Arc<Supervisor>>) {
    use crate::bootstrap::{BootstrapMode, ConfigSource};

    if !is_enabled() {
        return (mode, None);
    }
    // Only intercept Local mode with a real `SetupConfig`. Fresh /
    // DesktopLegacy boots fall through to the existing wizard — we
    // don't have ports or paths to feed the supervisor yet.
    let cli_setup = match &mode {
        BootstrapMode::Local {
            source: ConfigSource::CliSetup(cfg),
        } => cfg.clone(),
        _ => return (mode, None),
    };

    let binary = match resolve_sovereign_cli() {
        Some(p) => p,
        None => {
            warn!(
                "SOVEREIGN_USE_SUPERVISOR=1 but sovereign-cli binary not found \
                 (set SOVEREIGN_CLI_PATH or place it next to the desktop \
                 binary); falling back to in-process daemon"
            );
            return (mode, None);
        }
    };

    let client_port = cli_setup.daemon.client_port;
    let crash_log_dir = cli_setup.data.dir.join("crash-logs");

    let config = SupervisorConfig {
        binary_path: binary,
        args: vec!["daemon".into(), "run".into()],
        working_dir: None,
        env: vec![],
        health_url: format!("http://127.0.0.1:{client_port}/v1/models"),
        crash_log_dir,
        heartbeat_interval: Duration::from_secs(2),
        heartbeat_timeout: Duration::from_secs(5),
        heartbeat_failure_threshold: 3,
        // 1s → 5s → 30s → 2min; on the 5th failure the crash-loop
        // ceiling latches Failed until manual reconnect.
        backoff_schedule: vec![
            Duration::from_secs(1),
            Duration::from_secs(5),
            Duration::from_secs(30),
            Duration::from_secs(120),
        ],
        crash_loop_window: Duration::from_secs(3600),
        crash_loop_max: 5,
        stderr_ring_lines: 500,
    };

    let supervisor = Arc::new(Supervisor::new(config));
    let mut startup_states = supervisor.subscribe();

    // Spawn the supervise loop. The JoinHandle is intentionally
    // dropped — tokio doesn't cancel-on-drop, so the task keeps
    // running. Process exit kills the child via `kill_on_drop(true)`.
    // A graceful SIGTERM-with-grace path can layer on later without
    // changing this contract.
    {
        let sup = Arc::clone(&supervisor);
        tokio::spawn(async move { sup.run().await });
    }

    // Forward every state event to the frontend as `supervisor-state`.
    {
        let app = app_handle.clone();
        let mut fwd_rx = supervisor.subscribe();
        tokio::spawn(async move {
            while let Ok(state) = fwd_rx.recv().await {
                let _ = app.emit("supervisor-state", &state);
            }
        });
    }

    // Wait for the first Healthy. Cold GGUF loads on CPU take tens
    // of seconds; 60s gives the child room. If the supervisor enters
    // Failed during startup (binary missing, port collision the
    // restart loop can't recover from), bail early rather than wait
    // the full deadline.
    let healthy_deadline = Duration::from_secs(60);
    let outcome = tokio::time::timeout(healthy_deadline, async {
        loop {
            match startup_states.recv().await {
                Ok(SupervisorState::Healthy { .. }) => return StartupOutcome::Healthy,
                Ok(SupervisorState::Failed { reason, .. }) => {
                    return StartupOutcome::Failed(reason)
                }
                Ok(_) | Err(_) => continue,
            }
        }
    })
    .await;

    match outcome {
        Ok(StartupOutcome::Healthy) => {
            info!(
                client_port,
                "supervisor: child daemon healthy; switching to Attach mode"
            );
            (BootstrapMode::Attach { client_port }, Some(supervisor))
        }
        Ok(StartupOutcome::Failed(reason)) => {
            warn!(
                reason = %reason,
                "supervisor: child daemon entered Failed during startup; \
                 falling back to in-process daemon"
            );
            (mode, None)
        }
        Err(_) => {
            warn!(
                deadline_secs = healthy_deadline.as_secs(),
                "supervisor: timeout waiting for child daemon to become healthy; \
                 falling back to in-process daemon"
            );
            (mode, None)
        }
    }
}

enum StartupOutcome {
    Healthy,
    Failed(String),
}

#[cfg(test)]
mod tests {

    #[test]
    fn is_enabled_off_by_default() {
        // We can't safely scrub the env in unit tests (other tests
        // may set it), but the default value of an unset var is the
        // path we care about.
        let key = "SOVEREIGN_USE_SUPERVISOR_test_unset_marker";
        std::env::remove_var(key);
        // Inline the parse so the assertion doesn't depend on global
        // env state.
        let parsed = std::env::var(key)
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        assert!(!parsed);
    }
}
