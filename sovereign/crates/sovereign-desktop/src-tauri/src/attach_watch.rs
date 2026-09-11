// SPDX-License-Identifier: AGPL-3.0-or-later
//! Attach-mode daemon health watch (DAEMON_RESILIENCE.md P0.2).
//!
//! In Attach mode the daemon is externally owned (CLI / launchd /
//! systemd) and the desktop deliberately runs no supervisor — but
//! until this module it ran no health monitoring either: attach was
//! explicitly fire-and-forget ("inference 503s will surface it through
//! the chat UI"), so a dead daemon degraded to per-turn error bubbles
//! with no global surface. This poller closes that: ask the client
//! family whether a host is serving ([`ServingHost::is_serving`] — a
//! `/v1/models` probe, any 2xx = healthy, the same contract the
//! supervisor's heartbeat and `daemon start`'s readiness wait use) and
//! emit `attach-daemon-state` events the ReconnectBanner renders.
//!
//! The probe itself is NOT written here (sv-surface, 2026-09-11). "Is a
//! backend reachable" belongs to `sovereign-turn-client`, where it is
//! written once for the desktop, the CLI and the phone; this module's
//! own subject is the BANNER — how many consecutive misses raise it and
//! what the UI is told. It holds no process and starts nothing: when the
//! daemon is down, the affordance is `attach_restart_daemon`
//! (service-manager kickstart, `commands/supervisor_ctl.rs`), not a
//! supervisor here.
//!
//! Recovery is automatic by construction — attach-mode calls are
//! stateless HTTP, so the moment the daemon answers again everything
//! works; the banner clears on the healthy transition. The manual
//! affordance is `attach_restart_daemon` (service-manager kickstart,
//! `commands/supervisor_ctl.rs`), not a supervisor.
//!
//! NOT spawned for the supervised child (it has its own 2s heartbeat)
//! or the in-process daemon (nothing to poll) — see the `main.rs`
//! call site.

use serde::Serialize;
use sovereign_turn_client::ServingHost;
use tauri::{AppHandle, Emitter};

const PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
/// Consecutive misses before the banner raises — 36 × 5s = 180s.
///
/// It was 3 (15s), and at 15s the banner fired during every ORDINARY
/// restart and said "down" about a daemon that was starting normally. The
/// threshold has to clear both clocks that a restart runs through before
/// the daemon can answer at all:
///
/// * the service manager's own relaunch delay — launchd `ThrottleInterval`
///   10s (`contrib/launchd/com.svrnmesh.daemon.plist`, the KeepAlive note),
///   systemd `RestartSec=10` (`contrib/systemd/svrnmesh.service`), and
///   Task Scheduler `RestartOnFailure/Interval` `PT1M`
///   (`contrib/windows/SvrnmeshDaemon.xml`, whose minimum IS one minute).
///   Worst case across the three: 60s.
/// * the daemon's own readiness budget — 120s by default
///   (`sovereign-cli-daemon` `daemon_cmd/lifecycle.rs::parse_ready_timeout`,
///   `DEFAULT_SECS = 120`), which is how long a starting daemon is
///   legitimately not answering `/v1/models` while it loads a model.
///
/// 60 + 120 = 180s is the longest an ordinary restart takes on the slowest
/// platform; anything below it makes the banner a guess. The cost is that a
/// daemon which is genuinely gone goes unannounced for three minutes — paid
/// deliberately, because a banner that cries wolf on every restart is one
/// users learn to ignore, and the recovery this banner offers
/// (`attach_restart_daemon`) is useless against a daemon that was already
/// coming back on its own.
const FAILURES_TO_RAISE: u32 = 36;

/// Mirrors over the `attach-daemon-state` event; `kind` is the
/// discriminant, matching the `supervisor-state` convention.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttachDaemonState {
    Healthy {
        client_port: u16,
    },
    Down {
        client_port: u16,
        consecutive_failures: u32,
    },
}

/// Spawn the poll loop for the externally-owned daemon on
/// `client_port`. Detached for the app's lifetime.
pub fn spawn(app_handle: AppHandle, client_port: u16) {
    tauri::async_runtime::spawn(async move {
        let host = ServingHost::at(format!("http://127.0.0.1:{client_port}"));
        tracing::info!(client_port, "attach-watch: armed");
        let mut consecutive: u32 = 0;
        let mut raised = false;
        loop {
            tokio::time::sleep(PROBE_INTERVAL).await;
            let ok = host.is_serving().await;
            if ok {
                if raised {
                    tracing::info!(client_port, "attach-watch: daemon is back");
                    let _ = app_handle.emit(
                        "attach-daemon-state",
                        AttachDaemonState::Healthy { client_port },
                    );
                }
                consecutive = 0;
                raised = false;
            } else {
                consecutive += 1;
                // The banner deliberately waits out the OS's recovery window,
                // which leaves three minutes where the only global signal is a
                // per-turn error. One line at the first miss so the log tells
                // the story at default levels, not only at debug.
                if consecutive == 1 {
                    tracing::info!(
                        client_port,
                        raise_after_secs = (FAILURES_TO_RAISE as u64) * PROBE_INTERVAL.as_secs(),
                        "attach-watch: daemon stopped answering — watching"
                    );
                }
                if consecutive >= FAILURES_TO_RAISE {
                    if !raised {
                        tracing::warn!(
                            client_port,
                            consecutive,
                            "attach-watch: external daemon not answering — raising banner"
                        );
                    }
                    raised = true;
                    let _ = app_handle.emit(
                        "attach-daemon-state",
                        AttachDaemonState::Down {
                            client_port,
                            consecutive_failures: consecutive,
                        },
                    );
                }
            }
        }
    });
}
