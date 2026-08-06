// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri-side wiring for the daemon supervisor (see `crate::supervisor`).
//!
//! **Default ON since the W1 flip** (DAEMON_RESILIENCE.md P0.1,
//! 2026-07-18; formerly opt-in via `SOVEREIGN_USE_SUPERVISOR=1`).
//! When engaged, the desktop's Local-mode boot does NOT construct an
//! in-process `EmbeddedDaemon` — it spawns the daemon as a child
//! (`current_exe() --daemon-child`, the desktop binary re-entering as a
//! headless daemon) and talks to it over HTTP using the existing
//! Attach-mode plumbing. This protects the Tauri UI from process-level
//! ggml/llama.cpp crashes: when the daemon dies, the supervisor
//! surfaces a `supervisor-state` event the frontend renders as a
//! Reconnect banner, and restarts the child behind it. Opt-outs in
//! [`is_enabled`].
//!
//! This module owns the Tauri-side surface — binary-path resolution,
//! event forwarding, the startup wait — so `supervisor.rs` itself
//! stays Tauri-free and unit-testable in isolation.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tracing::{info, warn};

use crate::supervisor::{HealthTarget, Supervisor, SupervisorConfig, SupervisorState};

/// Supervised child-process mode is the DEFAULT (DAEMON_RESILIENCE.md
/// P0.1 — the W1 flip, 2026-07-18). Two opt-outs:
///
/// - `SOVEREIGN_USE_SUPERVISOR=0` (or `false`) — the kill-switch back
///   to the in-process `EmbeddedDaemon`. (`=1`/`true`, the old opt-IN
///   spelling, is accepted and redundant.)
/// - `SOVEREIGN_FORCE_LOCAL=1` — its documented meaning is "THIS
///   process runs the weights" (the real-mode desktop harnesses and
///   the run-local-while-a-daemon-is-up power case), which a child
///   daemon would contradict.
pub fn is_enabled() -> bool {
    if std::env::var("SOVEREIGN_FORCE_LOCAL").is_ok_and(|v| v == "1") {
        return false;
    }
    !std::env::var("SOVEREIGN_USE_SUPERVISOR")
        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

/// What to spawn as the daemon child.
struct SpawnSpec {
    binary: PathBuf,
    args: Vec<String>,
}

/// Resolve the daemon child to spawn. Probes in order:
/// 1. `SOVEREIGN_CLI_PATH` env override → `<path> daemon run`
///    (dev/dogfood: point at a CLI build).
/// 2. **This very binary** with `--daemon-child` — the packaged-app
///    path. `main.rs` detects the flag before Tauri init and calls
///    `sovereign_cli_daemon::daemon_child_main()`, so the child is the
///    real daemon with zero extra bundle bytes (no sidecar).
///
/// `None` only when `current_exe()` itself fails — effectively never;
/// the caller surfaces it loudly rather than silently degrading.
fn resolve_daemon_child() -> Option<SpawnSpec> {
    if let Ok(env_path) = std::env::var("SOVEREIGN_CLI_PATH") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Some(SpawnSpec {
                binary: p,
                args: vec!["daemon".into(), "run".into()],
            });
        }
    }
    let exe = std::env::current_exe().ok()?;
    Some(SpawnSpec {
        binary: exe,
        args: vec!["--daemon-child".into()],
    })
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

    // Only intercept Local mode with a real `SetupConfig` — we need its
    // ports/paths to feed the supervisor. Fresh / DesktopLegacy boots fall
    // through to the existing wizard.
    let cli_setup = match &mode {
        BootstrapMode::Local {
            source: ConfigSource::CliSetup(cfg),
        } => cfg.clone(),
        _ => return (mode, None),
    };
    if !is_enabled() {
        return (mode, None);
    }

    let spec = match resolve_daemon_child() {
        Some(s) => s,
        None => {
            surface_fallback(
                &app_handle,
                "cannot resolve the daemon child binary (current_exe failed)",
            );
            return (mode, None);
        }
    };

    let client_port = cli_setup.daemon.client_port;
    let crash_log_dir = cli_setup.data.dir.join("crash-logs");

    let config = daemon_supervisor_config(spec, client_port, crash_log_dir);

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
            (
                BootstrapMode::Attach {
                    client_port,
                    internal_port: cli_setup.daemon.internal_port,
                },
                Some(supervisor),
            )
        }
        Ok(StartupOutcome::Failed(reason)) => {
            surface_fallback(
                &app_handle,
                &format!("child daemon entered Failed during startup: {reason}"),
            );
            (mode, None)
        }
        Err(_) => {
            surface_fallback(
                &app_handle,
                &format!(
                    "child daemon not healthy within {}s",
                    healthy_deadline.as_secs()
                ),
            );
            (mode, None)
        }
    }
}

/// First-post-wizard-session fix (DAEMON_RESILIENCE.md P0.1).
///
/// The supervisor decision happens exactly once, at app startup — so
/// the session that RAN the wizard would otherwise finish its life
/// in-process (no crash isolation) and only pick up the supervised
/// child on the NEXT launch. Instead of hot-switching a live AppState
/// into attach mode (the mode is baked at construction), relaunch the
/// app: the wizard session has never bound `:9741` (the embedded
/// daemon only starts at `state::bootstrap`, which the caller skips
/// when this returns true), so the fresh instance's `detect()` finds
/// the just-written `SetupConfig`, spawns the supervised child, and
/// attaches — isolation from minute one.
///
/// Returns `true` when the relaunch was initiated — the process is on
/// its way out and the caller must NOT bootstrap in-process. Returns
/// `false` when supervision is disabled (`SOVEREIGN_FORCE_LOCAL=1`
/// harnesses, the kill-switch) or the spawn failed — the caller keeps
/// the legacy in-process completion, which is exactly the pre-flip
/// behavior.
pub async fn maybe_restart_into_supervised(app_handle: &AppHandle) -> bool {
    if !is_enabled() {
        return false;
    }
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "setup-restart: current_exe failed — finishing in-process");
            return false;
        }
    };
    // Let the wizard UI say why the window is about to close, and give
    // the webview a beat to paint it.
    let _ = app_handle.emit(
        "setup-restarting",
        serde_json::json!({ "reason": "enabling crash protection" }),
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    match std::process::Command::new(&exe).spawn() {
        Ok(_) => {
            info!("setup complete — relaunching into the supervised topology");
            app_handle.exit(0);
            true
        }
        Err(e) => {
            warn!(error = %e, "setup-restart: relaunch spawn failed — finishing in-process");
            false
        }
    }
}

/// Falling back to the in-process daemon must be VISIBLE, not silent
/// (DAEMON_RESILIENCE.md P0.1): the user keeps a working app, but
/// crash isolation is off — a ggml crash would take the window down —
/// and support triage needs to know which mode a session ran in. The
/// frontend renders `supervisor-fallback` as a dismissible notice.
/// The daemon child's supervision policy.
///
/// Split out of `install` purely so it is reachable from a test: the
/// timing fields below are a policy that has already been wrong once in
/// a way no type can catch, and prose in a comment is not a gate. See
/// `startup_grace_outlasts_the_heartbeat_kill_window`.
fn daemon_supervisor_config(
    spec: SpawnSpec,
    client_port: u16,
    crash_log_dir: PathBuf,
) -> SupervisorConfig {
    SupervisorConfig {
        binary_path: spec.binary,
        args: spec.args,
        working_dir: None,
        env: vec![],
        health: HealthTarget::Fixed(format!("http://127.0.0.1:{client_port}/v1/models")),
        crash_log_dir,
        heartbeat_interval: Duration::from_secs(2),
        heartbeat_timeout: Duration::from_secs(5),
        heartbeat_failure_threshold: 3,
        // Startup grace for the cold model load.
        //
        // This was `Duration::ZERO`, justified by "the desktop daemon
        // binds its client port early, so failures count from spawn".
        // That premise is false, and it cost users a boot loop. The
        // daemon loads its eager slots FIRST — `build::inference::
        // load_provider` at `sovereign-cli-daemon/src/daemon_cmd/mod.rs:411`
        // — and only then calls `try_resume()` (:780), which binds
        // :9741. So the health probe above (`GET /v1/models`) cannot
        // answer until fast + embed are resident, and with
        // `interval 2s × threshold 3` the supervisor started killing
        // the child at ~6s.
        //
        // Measured 2026-08-05 on a MacBookPro16,1, two 0.6B slots,
        // CPU-only: listener bound 5.9-7.7s after spawn. Five kills
        // observed in one morning, each with the child's last log line
        // still `loading slot slot="fast"`, each restart re-paying the
        // whole load from cold. The user sees "stuck initializing".
        //
        // `mobile_host_setup` already solved the same shape correctly:
        // sovereign-server also binds last, and it buys ~60s via
        // `heartbeat_failure_threshold: 12`. This path uses the
        // purpose-built grace instead, so post-startup hang detection
        // stays fast (3 × 2s) rather than being slowed to 120s.
        //
        // 120s, not 60s: eager-slot cost scales with the user's models
        // and disk, not ours — a large embed gguf on a cold external
        // volume is minutes, not seconds. The grace ends at the FIRST
        // successful probe (`first_healthy.is_none()` in
        // `supervisor.rs`), so it costs nothing once serving, and a
        // child that never answers was never going to be fixed by
        // killing it every 6s and re-loading its models.
        ready_deadline: Duration::from_secs(120),
        // 1s → 5s → 30s → 2min; on the 5th failure the crash-loop
        // ceiling latches Failed until manual reconnect.
        backoff_schedule: vec![
            Duration::from_secs(1),
            Duration::from_secs(5),
            Duration::from_secs(30),
            Duration::from_secs(120),
        ],
        // A generation that serves a full minute proves the restart worked.
        // Reset condition for the crash breaker — see the field docs.
        healthy_reset_after: Duration::from_secs(60),
        crash_loop_max: 5,
        stderr_ring_lines: 500,
    }
}

fn surface_fallback(app_handle: &AppHandle, reason: &str) {
    warn!(
        reason,
        "supervisor: falling back to IN-PROCESS daemon (crash isolation off)"
    );
    let _ = app_handle.emit(
        "supervisor-fallback",
        serde_json::json!({ "reason": reason }),
    );
}

enum StartupOutcome {
    Healthy,
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::{daemon_supervisor_config, SpawnSpec};
    use std::path::PathBuf;
    use std::time::Duration;

    fn config() -> crate::supervisor::SupervisorConfig {
        daemon_supervisor_config(
            SpawnSpec {
                binary: PathBuf::from("/nonexistent/sovereign-desktop"),
                args: vec!["--daemon-child".into()],
            },
            9741,
            PathBuf::from("/nonexistent/crash-logs"),
        )
    }

    /// The regression that shipped: `ready_deadline: ZERO` while the
    /// daemon binds :9741 only AFTER loading its eager slots, so the
    /// supervisor killed the child mid-load at `interval × threshold`
    /// (~6s) and restarted it into the same wall, forever.
    ///
    /// The invariant is a comparison, not a magic number: whatever the
    /// heartbeat cadence becomes, the startup grace must outlast the
    /// window in which the supervisor would kill an un-answered child.
    /// Anything else is a boot loop on a cold cache.
    #[test]
    fn startup_grace_outlasts_the_heartbeat_kill_window() {
        let c = config();
        let kill_window = c.heartbeat_interval * c.heartbeat_failure_threshold;
        assert!(
            c.ready_deadline > kill_window,
            "startup grace ({:?}) must exceed the heartbeat kill window ({:?}), \
             or a cold model load is killed before it can answer a probe",
            c.ready_deadline,
            kill_window
        );
    }

    /// The grace has to cover a real cold load, not merely clear the
    /// kill window by a hair. 5.9-7.7s was one Intel laptop with two
    /// 0.6B slots; a user's embed gguf on a cold external disk is the
    /// case that actually needs the headroom.
    #[test]
    fn startup_grace_covers_a_slow_cold_load() {
        assert!(
            config().ready_deadline >= Duration::from_secs(60),
            "grace must tolerate a slow cold load, not just the 6s kill window"
        );
    }

    /// Post-startup detection must stay responsive: the grace buys time
    /// for the FIRST probe only, so a daemon that hangs after serving is
    /// still caught in seconds, not minutes.
    #[test]
    fn hang_detection_after_first_probe_stays_fast() {
        let c = config();
        assert!(
            c.heartbeat_interval * c.heartbeat_failure_threshold <= Duration::from_secs(10),
            "a hang after the child is serving must be caught within ~10s"
        );
    }

    /// The W1 flip: supervised mode is ON by default; only an explicit
    /// `0`/`false` disables it. Inline the parse (mirroring
    /// `is_enabled`) so the assertion doesn't depend on global env
    /// state, which other tests may mutate.
    #[test]
    fn is_enabled_on_by_default_with_explicit_kill_switch() {
        let parse = |v: Option<&str>| -> bool {
            !v.map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
                .unwrap_or(false)
        };
        assert!(parse(None), "unset must mean supervised (default ON)");
        assert!(parse(Some("1")), "legacy opt-in spelling stays enabled");
        assert!(parse(Some("true")));
        assert!(!parse(Some("0")), "0 is the kill-switch");
        assert!(!parse(Some("false")));
        assert!(!parse(Some("FALSE")));
    }
}
