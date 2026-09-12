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
//! stderr ring, all held by a window. Removing the supervisor left a smaller
//! version of the same shape: a `tokio::process::Command` with
//! `kill_on_drop(true)`, a `Child` held inside a task, and `child.wait()` to
//! learn it had exited. That is still a client deciding another process's
//! lifetime, and sv-surface's lifecycle census counted all four of those
//! sites.
//!
//! **Both halves now belong to somebody that owns them.**
//!
//! - **Up** is [`sovereign_turn_client::ServingHost`], the one sanctioned
//!   bring-up in this workspace. It probes first, starts only if nothing
//!   answers, detaches the child and drops the handle at the spawn site. The
//!   bar's line is *"a client may bring a backend up at a moment a USER
//!   ACTION asks for one"* — the toggle is that user action.
//! - **Down** is `POST /v1/admin/shutdown` on the host itself
//!   (`sovereign-server/src/shutdown.rs`), authorized with the same
//!   `sk-mobile-…` bearer token the phone uses. The host had no stop path
//!   before this commit, which is why the app was holding a `Child` to
//!   provide one; the gap was `sovereign-server`'s, and it is closed there.
//!
//! Nothing here holds a handle, waits on a process, or names the
//! process-control API. Two HTTP calls, and a user who can still turn the
//! toggle both ways.
//!
//! The behaviour that went with the supervisor is unchanged: a
//! `sovereign-server` that crashes is not restarted. The toggle is the only
//! lifecycle verb, and the Settings panel already re-polls [`pairing`] —
//! `iroh_dial` stays `None` while nothing is serving, which is the surface a
//! dead host shows up on.

use std::path::PathBuf;
use std::time::Duration;

use sovereign_core::mobile_host::{self, MobileHostConfig};
use sovereign_contracts::setup_config::SetupConfig;
use sovereign_turn_client::{BundledBackend, ServingHost};
use tauri::async_runtime::{self, JoinHandle};
use tracing::{info, warn};

/// Whole budget for a bring-up: the probe, the spawn and the readiness wait.
///
/// 30 s rather than the 60 s `serving_host::BRING_UP_WINDOW` spends on a
/// daemon, and the difference is the point: this host loads NO weights (it
/// forwards every completion and every embedding to the daemon), so it has
/// nothing to do that takes tens of seconds. A cold GGUF load is what sized
/// the daemon's window; there is no such load here. It is a CAP, not a
/// sleep — `ensure_reachable` polls at 250 ms and returns on the first
/// answer.
const BRING_UP_WINDOW: Duration = Duration::from_secs(30);

/// The door `sovereign-server` answers liveness on.
///
/// Not `/v1/models`, which this binary does not serve at all — it is
/// `/health`, mounted outside the auth layer precisely so a caller with no
/// tenant token can ask (`sovereign-server/src/main.rs:778`). Probing the
/// default would 404 forever against a host that is serving fine.
const MOBILE_READY_PATH: &str = "/health";

/// How long a stop may take before the toggle reports it did not land.
///
/// The host answers 202 as soon as it accepts, before it finishes draining,
/// so this covers the request and not the drain — the drain has its own
/// bound inside the host (`shutdown.rs::GRACE`).
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// `mobile_host::default_bind()` is `0.0.0.0:8080`, and this is its port —
/// what [`mobile_port`] assumes when a hand-edited `bind` carries none.
const DEFAULT_MOBILE_PORT: u16 = 8080;

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

/// Ensure the phone-facing host is serving, bringing it up if it is not.
///
/// Idempotent by construction: `ensure_reachable` probes before it starts,
/// so a second toggle-on, a relaunch while the host is already up, or a
/// host somebody started with `svrn mobile serve` all resolve to "already
/// serving" and spawn nothing.
///
/// An error string is the UI's message — the three refusals a user can act
/// on (no configured node, no `sovereign-server` binary, brought up but
/// silent) each name what to do.
pub async fn ensure_running() -> Result<(), String> {
    let setup = SetupConfig::load()
        .map_err(|e| format!("Mobile access needs a configured node (run setup first): {e}"))?;
    let mh = MobileHostConfig::load_or_create()?;
    let config_path = mobile_host::write_server_config(&setup, &mh)?;
    let binary = mobile_host::resolve_server_binary().ok_or_else(|| {
        "sovereign-server binary not found next to the app (or set SOVEREIGN_SERVER_PATH)"
            .to_string()
    })?;

    let port = mobile_port(&mh.bind);
    let log = log_file(&setup.data.dir);

    // The child's output is the only account of why a host did not come up,
    // and a detached process's stderr goes nowhere (ARCH principle 1). A
    // failure to create the log dir degrades to DISCARDED output rather than
    // failing the toggle — named here because it is a substitution
    // (principle 6).
    let mut backend = BundledBackend::at(&binary)
        .arg("--config")
        .arg(config_path.display().to_string());
    match log.as_ref() {
        Some(path) => backend = backend.log_to(path),
        None => warn!("mobile-access: no log file — the host's output will be DISCARDED"),
    }

    let reached = ServingHost::at(format!("http://127.0.0.1:{port}"))
        .ready_at(MOBILE_READY_PATH)
        .bringing_up(backend)
        .ensure_reachable(BRING_UP_WINDOW)
        .await
        .map_err(|e| format!("Mobile access could not start: {e}"))?;

    info!(
        port,
        binary = %binary.display(),
        log = log.as_ref().map(|p| p.display().to_string()),
        ?reached,
        "mobile-access: sovereign-server is serving (inference delegated to the daemon; \
         no models loaded, no restart policy — the toggle is the lifecycle)"
    );
    Ok(())
}

/// Ask the host to stop, over its own admin door.
///
/// Not a signal and not a handle: this process does not own that one, and
/// after this commit it may not even be its parent — a host already up when
/// the toggle was flipped was started by somebody else entirely.
///
/// A host that is already down answers nothing, and that is reported as
/// success: the user asked for "not serving" and "not serving" is the state.
/// Every other failure is returned, because a stop that did not land while
/// the toggle says off is exactly the silent substitution principle 6
/// refuses.
pub async fn stop() -> Result<(), String> {
    let mh = MobileHostConfig::load_or_create()?;
    let port = mobile_port(&mh.bind);
    let url = format!("http://127.0.0.1:{port}/v1/admin/shutdown");

    let client = reqwest::Client::builder()
        .timeout(STOP_TIMEOUT)
        .build()
        .map_err(|e| format!("mobile access: could not build an HTTP client: {e}"))?;

    // The same bearer the phone presents. One credential, so there is no
    // second secret to rotate, leak or disagree about (ARCH principle 8).
    let resp = match client.post(&url).bearer_auth(&mh.token).send().await {
        Ok(r) => r,
        Err(e) if e.is_connect() => {
            info!(port, "mobile-access: nothing was serving — already stopped");
            return Ok(());
        }
        Err(e) => return Err(format!("mobile access: stop request failed: {e}")),
    };

    let status = resp.status();
    if status.is_success() {
        info!(port, %status, "mobile-access: stop accepted by the host");
        return Ok(());
    }
    let body = resp.text().await.unwrap_or_default();
    warn!(port, %status, %body, "mobile-access: the host refused the stop");
    Err(format!(
        "the mobile host refused to stop ({status}). It is still serving on port {port}. {body}"
    ))
}

/// Launch-time entry point, for a user who left the toggle on.
///
/// **The returned handle is the BRING-UP, not the host's life.** Aborting it
/// cancels this process's wait for the host to answer and stops nothing —
/// [`stop`] is the only way to stop the host. It exists in this shape only
/// because `main.rs`'s call site still assigns it to
/// `AppState::mobile_host_supervisor`, a field that no longer means anything
/// and is removed with that call site. Nothing reads the handle.
pub fn start() -> Result<JoinHandle<()>, String> {
    Ok(async_runtime::spawn(async {
        if let Err(e) = ensure_running().await {
            warn!(error = %e, "mobile-access: could not bring the host up at launch");
        }
    }))
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

/// Pairing info for the Settings card. The iroh pairing code comes
/// from the live server (it's runtime state — endpoint identity +
/// the relay it settled on — not config), so it is `None` whenever
/// the server isn't up yet.
pub async fn pairing() -> Result<MobilePairing, String> {
    let mh = MobileHostConfig::load_or_create()?;
    let iroh_dial = if mh.iroh_enabled {
        fetch_iroh_dial(mobile_port(&mh.bind)).await
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

/// Best-effort read of `GET /status` → `iroh.dial` from the running
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

/// The port every caller in this module talks to the host on.
///
/// One decider (ARCH principle 8): `port_of(..).unwrap_or(8080)` was written
/// three times here, and the three now have to agree — the bring-up probes
/// this port, the stop POSTs to it, and the pairing card reads `/status` on
/// it. Three copies of a fallback is three chances to send the stop somewhere
/// the probe never looked.
///
/// The fallback itself is a SUBSTITUTION and says so on the trace rather than
/// happening quietly (principle 6). It is not a refusal because the port is
/// not this module's to decide: `mobile-host.toml` is the operator's file,
/// `write_server_config` hands `bind` to the server verbatim, and 8080 is the
/// default the whole feature is documented against. What a user must not get
/// is the silent version — a toggle that reports success while the stop went
/// to a port nothing is on.
fn mobile_port(bind: &str) -> u16 {
    match port_of(bind) {
        Some(p) => p,
        None => {
            warn!(
                bind,
                assumed = DEFAULT_MOBILE_PORT,
                "mobile-access: `bind` in mobile-host.toml carries no parseable port \
                 — assuming the default. The host binds what the generated config says, \
                 so if that is not this port the toggle will not find or stop it."
            );
            DEFAULT_MOBILE_PORT
        }
    }
}

fn port_of(bind: &str) -> Option<u16> {
    bind.rsplit_once(':').and_then(|(_, p)| p.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host's liveness door, pinned against the default.
    ///
    /// The regression is silent and total: `ServingHost`'s default asks
    /// `/v1/models`, `sovereign-server` does not serve it, and every
    /// bring-up would end in `SilentAfterLaunch` — the toggle reporting
    /// failure while a perfectly healthy host serves the user's phone.
    /// The mobile host is not probed on the daemon's door.
    ///
    /// The failing input is the one-character edit this exists to catch:
    /// drop the `.ready_at(MOBILE_READY_PATH)` call, or set the constant to
    /// `DEFAULT_READY_PATH`, and every bring-up ends in `SilentAfterLaunch`
    /// — the toggle reporting failure while a healthy host serves the user's
    /// phone. `sovereign-server` has no `/v1/models` route at all
    /// (`sovereign-server/src/main.rs:691-711` is its whole `/v1` set).
    #[test]
    fn the_mobile_host_is_not_probed_on_the_daemons_door() {
        assert_ne!(
            MOBILE_READY_PATH,
            sovereign_turn_client::reach::DEFAULT_READY_PATH,
            "the mobile host is being probed on the daemon's door"
        );
    }

    /// The bring-up, the stop and the pairing card read ONE port.
    ///
    /// They each spelled `port_of(..).unwrap_or(8080)` before, which is three
    /// chances to send the stop to a port the probe never looked at.
    #[test]
    fn every_caller_resolves_the_same_port_and_the_fallback_is_the_default() {
        assert_eq!(mobile_port("0.0.0.0:8080"), 8080);
        assert_eq!(mobile_port("[::]:9000"), 9000);
        assert_eq!(mobile_port("100.64.0.2:9000"), 9000);
        // The substitution, exercised: no port to parse.
        assert_eq!(mobile_port("localhost"), DEFAULT_MOBILE_PORT);
        assert_eq!(port_of("localhost"), None);
    }
}
