// SPDX-License-Identifier: AGPL-3.0-or-later
//! Attach-mode daemon health watch (DAEMON_RESILIENCE.md P0.2).
//!
//! In Attach mode the daemon is externally owned (CLI / launchd /
//! systemd) and the desktop deliberately runs no supervisor — but
//! until this module it ran no health monitoring either: attach was
//! explicitly fire-and-forget ("inference 503s will surface it through
//! the chat UI"), so a dead daemon degraded to per-turn error bubbles
//! with no global surface. This poller closes that: probe
//! `/v1/models` (any 2xx = healthy — the same contract the
//! supervisor's heartbeat and `daemon start`'s readiness wait use)
//! and emit `attach-daemon-state` events the ReconnectBanner renders.
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
use tauri::{AppHandle, Emitter};

const PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
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
        let url = format!("http://127.0.0.1:{client_port}/v1/models");
        let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "attach-watch: could not build probe client");
                return;
            }
        };
        tracing::info!(client_port, "attach-watch: armed");
        let mut consecutive: u32 = 0;
        let mut raised = false;
        loop {
            tokio::time::sleep(PROBE_INTERVAL).await;
            let ok = matches!(
                client.get(&url).send().await,
                Ok(resp) if resp.status().is_success()
            );
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
