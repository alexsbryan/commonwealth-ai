// SPDX-License-Identifier: AGPL-3.0-or-later
//! Desktop wiring for the opt-in **Mobile access** host.
//!
//! Reuses the shared [`sovereign_core::mobile_host`] core (config generation +
//! token + binary resolution) and the desktop's own [`crate::supervisor`] to
//! run `sovereign-server` as a supervised child. The host delegates ALL
//! inference to the local daemon, so it loads no models of its own — the
//! desktop's own models stay the single resident copy. See the core module for
//! the "no second model load" details.
//!
//! Lifecycle: [`start`] spawns the supervise loop and returns its
//! `JoinHandle`; the desktop holds it in `AppState.mobile_host_supervisor`.
//! Aborting the handle drops the run future and the in-flight child's
//! `kill_on_drop(true)` SIGKILLs `sovereign-server` — that's toggle-off.

use std::sync::Arc;
use std::time::Duration;

use sovereign_core::mobile_host::{self, MobileHostConfig};
use sovereign_core::setup_config::SetupConfig;
// `tauri::async_runtime::spawn` (NOT `tokio::spawn`): `start` is called from the
// Tauri `setup()` closure, which runs in the app-delegate's
// `did_finish_launching` with no ambient Tokio runtime on that thread —
// `tokio::spawn` panics there ("no reactor running"). Tauri's handle works from
// any context. (Same reasoning as the mobile crate's `connectivity::monitor`.)
use tauri::async_runtime::{self, JoinHandle};
use tracing::info;

use crate::supervisor::{HealthTarget, Supervisor, SupervisorConfig};

/// Pairing card the Settings panel renders. `address` is already dialable (a
/// wildcard bind is resolved to this node's tailnet IP). `iroh_dial` is the
/// no-VPN pairing code (`<endpoint-id-hex>@<relay-url>`) read live from the
/// running server's `GET /status` — `None` while the server is starting, the
/// relay isn't connected yet, or `[iroh]` is disabled; the panel re-polls.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MobilePairing {
    pub address: String,
    pub tenant: String,
    pub token: String,
    pub iroh_dial: Option<String>,
}

/// Generate the remote-backed `sovereign-server` config and spawn a supervised
/// child. Returns the supervise-task handle (abort to stop) or an error string
/// for the UI to surface.
pub fn start() -> Result<JoinHandle<()>, String> {
    let setup = SetupConfig::load()
        .map_err(|e| format!("Mobile access needs a configured node (run setup first): {e}"))?;
    let mh = MobileHostConfig::load_or_create()?;
    let config_path = mobile_host::write_server_config(&setup, &mh)?;
    let binary = mobile_host::resolve_server_binary().ok_or_else(|| {
        "sovereign-server binary not found next to the app (or set SOVEREIGN_SERVER_PATH)"
            .to_string()
    })?;

    let port = port_of(&mh.bind).unwrap_or(8080);
    let crash_log_dir = setup.data.dir.join("crash-logs");

    let config = SupervisorConfig {
        binary_path: binary,
        args: vec!["--config".into(), config_path.display().to_string()],
        working_dir: None,
        env: vec![],
        // Unauthenticated liveness route (added to sovereign-server's router
        // outside the /v1 auth layer) — the heartbeat needs a 200 without a
        // tenant token.
        health: HealthTarget::Fixed(format!("http://127.0.0.1:{port}/health")),
        crash_log_dir,
        heartbeat_interval: Duration::from_secs(5),
        heartbeat_timeout: Duration::from_secs(5),
        // sovereign-server binds its listener LAST — only after loading the
        // meta-atlas (~18s). Its `heartbeat_failure_threshold: 12` (12 × 5s =
        // 60s) already tolerates a cold load; keep `ready_deadline: ZERO` so
        // this path's behaviour is unchanged by the compute-child grace.
        heartbeat_failure_threshold: 12,
        ready_deadline: Duration::ZERO,
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
    };

    let supervisor = Arc::new(Supervisor::new(config));
    let handle = async_runtime::spawn(async move { supervisor.run().await });
    info!(
        port,
        "mobile-access: supervised sovereign-server started (inference delegated to the daemon; no models loaded)"
    );
    Ok(handle)
}

/// Pairing info for the Settings card. The iroh pairing code comes
/// from the live server (it's runtime state — endpoint identity +
/// the relay it settled on — not config), so it is `None` whenever
/// the server isn't up yet.
pub async fn pairing() -> Result<MobilePairing, String> {
    let mh = MobileHostConfig::load_or_create()?;
    let iroh_dial = if mh.iroh_enabled {
        fetch_iroh_dial(port_of(&mh.bind).unwrap_or(8080)).await
    } else {
        None
    };
    Ok(MobilePairing {
        address: mobile_host::dialable_address(&mh.bind),
        tenant: mh.tenant,
        token: mh.token,
        iroh_dial,
    })
}

/// Best-effort read of `GET /status` → `iroh.dial` from the supervised
/// server. One short-timeout attempt — the Settings panel polls while
/// the value is null, so there's no point blocking the card here.
async fn fetch_iroh_dial(port: u16) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    let status: serde_json::Value = client
        .get(format!("http://127.0.0.1:{port}/status"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    status
        .pointer("/iroh/dial")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn port_of(bind: &str) -> Option<u16> {
    bind.rsplit_once(':').and_then(|(_, p)| p.parse().ok())
}
