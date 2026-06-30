// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decide at app start whether the desktop should attach to an
//! already-running CLI daemon, or bring up its own `EmbeddedDaemon`
//! in-process.
//!
//! Before this module existed, the desktop always started its own
//! daemon. That collided with anything the CLI had registered under
//! launchd/systemd — `TcpListener::bind(:9741)` silently failed and
//! the user got a blank chat surface with no hint of the reason.
//!
//! Detection order:
//! 1. Probe `http://localhost:9741/v1/models`. If the response
//!    succeeds we know a daemon (CLI-started or otherwise) is
//!    already serving. Return `Attach`.
//! 2. Fall back to config on disk. Prefer the CLI's `SetupConfig`
//!    (`~/.sovereign/config.toml`) — if present, the user has run
//!    `sovereign setup` before and we should use those model paths
//!    rather than asking again in the wizard.
//! 3. If `SetupConfig` is absent but `DesktopConfig` says
//!    `setup_complete`, we're an older desktop-only install —
//!    `DesktopLegacy` keeps today's behaviour unchanged.
//! 4. Nothing persisted → `Fresh`. Run the full wizard.

use std::time::Duration;

use serde::Serialize;
use sovereign_core::setup_config::SetupConfig;

use crate::state::DesktopConfig;

/// Outcome of the bootstrap probe. Consumed by `AppState::new_with_mode`
/// to pick between Attach (HTTP client) and Local (in-process daemon).
#[derive(Debug, Clone)]
pub enum BootstrapMode {
    /// A daemon is already answering on the configured client port.
    /// The desktop should proxy inference through `RemoteApiProvider`
    /// and mesh mutations through the daemon's `/v1/mesh/*` HTTP API
    /// (see the mesh_http module in sovereign-mesh, when landed).
    Attach { client_port: u16, internal_port: u16 },
    /// Nothing is listening. The desktop should start its own
    /// `EmbeddedDaemon` and load models from `source`.
    Local { source: ConfigSource },
}

/// Where Local mode should read model paths + ports from.
#[derive(Debug, Clone)]
pub enum ConfigSource {
    /// The CLI has run setup. Reuse `SetupConfig.models.*` and skip
    /// the model-selection screens in the wizard.
    CliSetup(SetupConfig),
    /// Desktop-only install pre-dating the shared config work. The
    /// presence of this variant is what's load-bearing — the
    /// concrete `DesktopConfig` is read at probe time only to decide
    /// whether `setup_complete=true`, then discarded. The setup
    /// wizard reads `DesktopConfig` directly from disk again when it
    /// needs settings; threading the value through here adds no
    /// information the wizard couldn't recompute.
    DesktopLegacy,
    /// First launch on a clean machine. Run the full wizard.
    Fresh,
}

/// Snapshot the frontend receives to decide whether to render the
/// setup wizard, and which screens to show. Flattening the mode into
/// booleans keeps the Tauri IPC payload simple and stable.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct BootstrapSnapshot {
    pub daemon_running: bool,
    pub cli_config_present: bool,
    pub desktop_setup_complete: bool,
    /// The port the desktop will use for `/v1/*` calls. 9741 unless
    /// the user overrode it in `SetupConfig` or `DesktopConfig`.
    pub client_port: u16,
}

impl From<&BootstrapMode> for BootstrapSnapshot {
    fn from(mode: &BootstrapMode) -> Self {
        match mode {
            BootstrapMode::Attach { client_port, .. } => Self {
                daemon_running: true,
                cli_config_present: SetupConfig::exists(),
                desktop_setup_complete: false, // unused in Attach path
                client_port: *client_port,
            },
            BootstrapMode::Local {
                source: ConfigSource::CliSetup(c),
            } => Self {
                daemon_running: false,
                cli_config_present: true,
                desktop_setup_complete: false,
                client_port: c.daemon.client_port,
            },
            BootstrapMode::Local {
                source: ConfigSource::DesktopLegacy,
            } => Self {
                daemon_running: false,
                cli_config_present: false,
                desktop_setup_complete: true,
                client_port: 9741,
            },
            BootstrapMode::Local {
                source: ConfigSource::Fresh,
            } => Self {
                daemon_running: false,
                cli_config_present: false,
                desktop_setup_complete: false,
                client_port: 9741,
            },
        }
    }
}

/// Top-level probe. Called once at app startup from
/// `main::setup`, before `AppState::new`.
pub async fn detect() -> BootstrapMode {
    // Probe the canonical client port first. A successful `/v1/models`
    // response means _something_ is a valid sovereign daemon — we
    // don't care whether it's CLI-started, a prior desktop instance
    // that somehow survived, or a user-launched `sovereign daemon run`.
    // Either way, using it is safer than trying to bind on top.
    // Load the CLI `SetupConfig` ONCE; both the Attach-mode port
    // resolution and the Local/CliSetup branch below reuse it (no
    // second `SetupConfig::load()` round-trip).
    let cfg = SetupConfig::load().ok();
    let port = cfg.as_ref().map(|c| c.daemon.client_port).unwrap_or(9741);
    let internal_port = cfg.as_ref().map(|c| c.daemon.internal_port).unwrap_or(9742);

    if is_daemon_live(port).await {
        tracing::info!(
            target: "bootstrap",
            port,
            internal_port,
            "bootstrap: daemon answered on client port — Attach mode \
             (Members + activity read from the running daemon)"
        );
        return BootstrapMode::Attach {
            client_port: port,
            internal_port,
        };
    }

    // Nothing listening. Pick the best available config source.
    let (mode, label) = if let Some(cfg) = cfg {
        (
            BootstrapMode::Local {
                source: ConfigSource::CliSetup(cfg),
            },
            "CliSetup",
        )
    } else if DesktopConfig::load().setup_complete {
        (
            BootstrapMode::Local {
                source: ConfigSource::DesktopLegacy,
            },
            "DesktopLegacy",
        )
    } else {
        (
            BootstrapMode::Local {
                source: ConfigSource::Fresh,
            },
            "Fresh",
        )
    };
    // Glassbox: Local mode means this desktop runs its OWN embedded
    // daemon. If a separate daemon is in fact serving the mesh on this
    // port, that's the "empty Members list" failure mode — this log
    // line plus the `is_daemon_live` warning below name it directly.
    tracing::info!(
        target: "bootstrap",
        port,
        source = label,
        "bootstrap: no live daemon on client port — Local mode \
         (this desktop will run its own in-process EmbeddedDaemon)"
    );
    mode
}

/// TCP-connect + `GET /v1/models` probe. We need both checks:
/// - TCP connect rules out "nothing on the port" fast (≤2s).
/// - `/v1/models` rules out "some other service accidentally owns this
///   port" — a stranger on :9741 won't answer that endpoint.
///
/// Total worst-case latency: ~4 seconds. Runs once at app start; not
/// on a hot path.
async fn is_daemon_live(port: u16) -> bool {
    // Quick TCP probe. Short timeout — if nothing's listening we want
    // to fall through fast.
    let tcp_ok = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false);

    if !tcp_ok {
        // Nothing on the port — definitively no daemon. Fast path; a
        // closed port refuses immediately, so this costs ~nothing and
        // we must NOT retry (it's the common fresh-install case).
        return false;
    }

    // Something's listening, but it may still be booting: a cold model
    // load can push the first `/v1/models` response past a single 2s
    // window. Retry a few times before concluding "not a daemon."
    // This closes the startup race where the desktop probes while a
    // CLI/headless daemon is mid-boot and would otherwise fall through
    // to its OWN embedded daemon — which then shows an empty Members
    // list even though the real daemon (with peers) is right there.
    // Cheap: only reached when the port is already occupied.
    let url = format!("http://127.0.0.1:{port}/v1/models");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    for attempt in 1..=3u32 {
        match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => {
                tracing::info!(
                    target: "bootstrap",
                    port,
                    attempt,
                    "bootstrap: /v1/models answered — daemon is live"
                );
                return true;
            }
            Ok(r) => tracing::debug!(
                target: "bootstrap",
                port,
                attempt,
                status = %r.status(),
                "bootstrap: port occupied, /v1/models non-2xx — retrying"
            ),
            Err(e) => tracing::debug!(
                target: "bootstrap",
                port,
                attempt,
                error = %e,
                "bootstrap: port occupied, /v1/models errored — retrying"
            ),
        }
        if attempt < 3 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    tracing::warn!(
        target: "bootstrap",
        port,
        "bootstrap: port occupied but /v1/models never answered after 3 \
         tries — NOT attaching. Either a non-sovereign service holds the \
         port, or the daemon is still booting. If Members looks empty, \
         this is the most likely cause."
    );
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn is_daemon_live_false_when_nothing_listens() {
        // Pick a port unlikely to be in use. We just want to confirm
        // the function returns `false` quickly rather than hanging.
        let port: u16 = 59_741;
        assert!(!is_daemon_live(port).await);
    }

    #[tokio::test]
    async fn is_daemon_live_true_when_v1_models_answers() {
        use axum::{routing::get, Json, Router};

        // Stand up a tiny axum server that answers GET /v1/models.
        // Bind to a kernel-assigned port so parallel test runs don't
        // collide.
        let app = Router::new().route(
            "/v1/models",
            get(|| async { Json(serde_json::json!({"data": []})) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        // Give axum a beat to come up.
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(is_daemon_live(port).await);
    }

    #[tokio::test]
    async fn is_daemon_live_false_when_port_answers_but_path_404s() {
        // Defensive: someone else owns :9741 (e.g. webpack dev server).
        // It responds to TCP connect but 404s /v1/models. We must not
        // decide that's a sovereign daemon.
        use axum::{routing::get, Router};
        let app = Router::new().route("/other", get(|| async { "hi" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(!is_daemon_live(port).await);
    }

    #[test]
    fn snapshot_from_attach_signals_daemon_running() {
        let mode = BootstrapMode::Attach {
            client_port: 9741,
            internal_port: 9742,
        };
        let snap = BootstrapSnapshot::from(&mode);
        assert!(snap.daemon_running);
        assert_eq!(snap.client_port, 9741);
    }

    #[test]
    fn snapshot_from_cli_setup_signals_cli_config_present() {
        let cfg = SetupConfig {
            models: sovereign_core::setup_config::ModelsSection {
                primary: "/p".into(),
                fast: Some("/f".into()),
                embed: "/e".into(),
                code: None,
                context_size: None,
                max_extras_memory_gb: None,
                extra: std::collections::BTreeMap::new(),
                primary_pool: None,
            },
            daemon: sovereign_core::setup_config::DaemonSection {
                client_port: 19_741,
                internal_port: 19_742,
                ..Default::default()
            },
            data: sovereign_core::setup_config::DataSection::default(),
            watched_folders: Default::default(),
            memory: Default::default(),
            iroh: Default::default(),
            shared_model: Default::default(),
            discovery: Default::default(),
            mcp_servers: Vec::new(),
        };
        let mode = BootstrapMode::Local {
            source: ConfigSource::CliSetup(cfg),
        };
        let snap = BootstrapSnapshot::from(&mode);
        assert!(!snap.daemon_running);
        assert!(snap.cli_config_present);
        assert_eq!(snap.client_port, 19_741);
    }

    #[test]
    fn snapshot_from_fresh_signals_no_state() {
        let mode = BootstrapMode::Local {
            source: ConfigSource::Fresh,
        };
        let snap = BootstrapSnapshot::from(&mode);
        assert!(!snap.daemon_running);
        assert!(!snap.cli_config_present);
        assert!(!snap.desktop_setup_complete);
    }
}
