// SPDX-License-Identifier: AGPL-3.0-or-later
//! Desktop wiring for the opt-in **Mobile access** host.
//!
//! Reuses the shared [`sovereign_core::mobile_host`] core (config generation +
//! token + binary resolution) to run `sovereign-server`, the phone-facing API.
//! That host delegates ALL inference to the local daemon, so it loads no
//! models of its own — see the core module for the "no second model load"
//! details.
//!
//! # What this module stopped doing (sv-surface svt-2)
//!
//! It used to run `sovereign-server` under `crate::supervisor` — a restart
//! policy, a backoff schedule, a health heartbeat, a crash-loop breaker and a
//! stderr ring, all held by a window. That is one component owning another's
//! recovery, which is the line ARCH principle 12 draws: the desktop owns the
//! TOGGLE (Settings → Mobile access starts and stops the host) and nothing
//! else about the host's life.
//!
//! The behaviour that goes with the supervisor: a `sovereign-server` that
//! crashes is not restarted. The toggle is the only lifecycle verb, and the
//! Settings panel already re-polls `pairing()` — `iroh_dial` stays `None`
//! while nothing is serving, which is the surface a dead host shows up on.
//!
//! Lifecycle: [`start`] spawns the child and returns a `JoinHandle` that
//! resolves when it exits. Aborting the handle drops the in-flight `Child`,
//! whose `kill_on_drop(true)` SIGKILLs `sovereign-server` — that is toggle-off,
//! unchanged.

use std::path::PathBuf;
use std::time::Duration;

use sovereign_core::mobile_host::{self, MobileHostConfig};
use sovereign_core::setup_config::SetupConfig;
// `tauri::async_runtime::spawn` (NOT `tokio::spawn`): `start` is called from the
// Tauri `setup()` closure, which runs in the app-delegate's
// `did_finish_launching` with no ambient Tokio runtime on that thread —
// `tokio::spawn` panics there ("no reactor running"). Tauri's handle works from
// any context. (Same reasoning as the mobile crate's `connectivity::monitor`.)
use tauri::async_runtime::{self, JoinHandle};
use tracing::{info, warn};

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

/// Generate the remote-backed `sovereign-server` config and spawn the host.
///
/// Returns a handle that resolves when the child exits; abort it to stop the
/// host. An error string is the UI's message — the two refusals a user can act
/// on (no configured node, no `sovereign-server` binary) name what to do.
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
    let log = log_file(&setup.data.dir);

    let mut cmd = tokio::process::Command::new(&binary);
    cmd.arg("--config").arg(&config_path);
    // The child's output is the only account of why a host did not come up,
    // and a detached process's stderr goes nowhere (ARCH principle 1). Both
    // streams to one appended file beside the daemon's own log. A failure to
    // open it degrades to inherited stdio rather than failing the toggle —
    // named here because it is a substitution (principle 6).
    match log.as_ref().and_then(|p| open_append(p)) {
        Some((out, err)) => {
            cmd.stdout(out).stderr(err);
        }
        None => {
            warn!("mobile-access: no log file — the host's output follows this process's stdio")
        }
    }
    // Toggle-off is `handle.abort()`, which drops the future holding `Child`.
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", binary.display()))?;

    let handle = async_runtime::spawn(async move {
        match child.wait().await {
            Ok(status) => info!(%status, "mobile-access: sovereign-server exited"),
            Err(e) => warn!(error = %e, "mobile-access: could not wait on sovereign-server"),
        }
    });
    info!(
        port,
        binary = %binary.display(),
        log = log.as_ref().map(|p| p.display().to_string()),
        "mobile-access: sovereign-server started (inference delegated to the daemon; \
         no models loaded, no restart policy — the toggle is the lifecycle)"
    );
    Ok(handle)
}

/// `<data_dir>/logs/mobile-host.err` — beside the daemon's `daemon.err`, so
/// one directory holds every local process's account of itself.
fn log_file(data_dir: &std::path::Path) -> Option<PathBuf> {
    let dir = data_dir.join("logs");
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir.join("mobile-host.err")),
        Err(e) => {
            warn!(dir = %dir.display(), error = %e, "mobile-access: cannot create the log dir");
            None
        }
    }
}

/// Two independent append handles on one path — `Stdio` consumes the `File`,
/// and stdout and stderr each need their own.
fn open_append(path: &std::path::Path) -> Option<(std::fs::File, std::fs::File)> {
    let open = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    };
    match (open(), open()) {
        (Ok(a), Ok(b)) => Some((a, b)),
        (a, b) => {
            let e = a.err().or(b.err());
            warn!(path = %path.display(), error = ?e, "mobile-access: cannot open the log file");
            None
        }
    }
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
