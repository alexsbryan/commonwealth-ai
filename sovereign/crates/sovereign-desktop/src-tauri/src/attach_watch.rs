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
/// Consecutive failures before the banner raises. 3×5s rides out a
/// hot-reload blip; a restarting daemon (30–60s model load) correctly
/// shows as down until it answers again.
const FAILURES_TO_RAISE: u32 = 3;

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
