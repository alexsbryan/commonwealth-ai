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
use tokio::task::JoinHandle;
use tracing::info;

use crate::supervisor::{Supervisor, SupervisorConfig};

/// Pairing card the Settings panel renders. `address` is already dialable (a
/// wildcard bind is resolved to this node's tailnet IP).
#[derive(Debug, Clone, serde::Serialize)]
pub struct MobilePairing {
    pub address: String,
    pub tenant: String,
    pub token: String,
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
        health_url: format!("http://127.0.0.1:{port}/health"),
        crash_log_dir,
        heartbeat_interval: Duration::from_secs(5),
        heartbeat_timeout: Duration::from_secs(5),
        heartbeat_failure_threshold: 3,
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
    let handle = tokio::spawn(async move { supervisor.run().await });
    info!(
        port,
        "mobile-access: supervised sovereign-server started (inference delegated to the daemon; no models loaded)"
    );
    Ok(handle)
}

/// Pairing info for the Settings card.
pub fn pairing() -> Result<MobilePairing, String> {
    let mh = MobileHostConfig::load_or_create()?;
    Ok(MobilePairing {
        address: mobile_host::dialable_address(&mh.bind),
        tenant: mh.tenant,
        token: mh.token,
    })
}

fn port_of(bind: &str) -> Option<u16> {
    bind.rsplit_once(':').and_then(|(_, p)| p.parse().ok())
}
