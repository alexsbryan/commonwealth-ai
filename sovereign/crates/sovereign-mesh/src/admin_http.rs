// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP admin surface — `POST /v1/admin/reload`.
//!
//! When the desktop writes a new model path into `SetupConfig` and
//! wants the running daemon to pick it up, it has two options:
//!
//! 1. `launchctl kickstart -k` the service. Hard stop, ~3s gap in
//!    inference availability — every open request gets torn down.
//! 2. POST here. The daemon re-reads `SetupConfig` from disk, diffs
//!    against what's in memory, and rebuilds only the subsystems that
//!    changed. An `InferenceProvider` swap is atomic at the
//!    `RwLock<Option<_>>` inside `EmbeddedDaemon`, so in-flight
//!    requests finish on the old provider while new ones see the new
//!    one — no visible gap.
//!
//! The daemon can't rebuild a provider on its own (that would couple
//! `sovereign-mesh` to `sovereign-inference` model-loading details
//! that live in the CLI/desktop bootstrap). It delegates via a
//! `ProviderFactory` trait: the CLI/desktop installs one at startup
//! that knows how to call `EmbeddedLlamaCpp::load_full_with_families`.
//!
//! Fields that need a full rebind (ports, data_dir) can't be hot-
//! reloaded because `TcpListener` is already bound and SQLite handles
//! are already open. The handler signals `restart_required: true` for
//! those; callers fall back to `launchctl kickstart` in that case.
//!
//! Local-only — same loopback guard as `mcp_router` and `mesh_http`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::enforce_localhost;

/// How the admin handler rebuilds an `InferenceProvider` from a new
/// `SetupConfig`. Implemented by whoever owns the model-loading code —
/// typically `sovereign-cli::daemon_cmd` (for the launchd daemon) or
/// the desktop bootstrap (for in-process). Keeps `sovereign-mesh` free
/// of llama.cpp loading details.
#[async_trait::async_trait]
pub trait ProviderFactory: Send + Sync {
    async fn build_provider(&self, cfg: &SetupConfig)
        -> Result<Arc<dyn InferenceProvider>, String>;
}

/// Build the admin HTTP router. Merged into the daemon's client router
/// next to `mcp_router` and `mesh_router`.
///
/// Two layers of loopback enforcement:
/// 1. Router-level middleware ([`crate::loopback_guard::loopback_only`])
///    rejects non-loopback callers before any handler runs — so a
///    future route added here inherits the guard for free.
/// 2. Per-handler `enforce_localhost` check — belt + suspenders in
///    case the middleware is ever stripped.
pub fn admin_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/admin/reload", post(admin_reload))
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
        .layer(Extension(daemon))
}

/// Request body for `POST /v1/admin/reload`. Empty body (`{}` or no
/// body at all) means "reload everything that changed in `SetupConfig`
/// since we last read it". A future extension can accept
/// `{fields: ["models.primary"]}` for surgical reloads; not needed for
/// the current Attach-mode MVP.
#[derive(Debug, Default, Deserialize)]
pub struct ReloadRequest {
    /// Override the path we read `SetupConfig` from. Only used by tests
    /// to avoid touching the real `~/.svrnmesh/config.toml`.
    /// Production callers omit this; the daemon's stored path is used.
    #[serde(default)]
    pub config_path: Option<std::path::PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReloadResponse {
    /// Fully-qualified config keys that successfully hot-reloaded,
    /// e.g. `["models.primary", "models.fast"]`. Empty on a no-op.
    pub reloaded_fields: Vec<String>,
    /// Keys that changed but cannot reload live — the caller must
    /// restart the daemon (launchctl kickstart / systemctl restart)
    /// to apply them.
    pub restart_required_fields: Vec<String>,
    /// Convenience flag — `true` iff `restart_required_fields` is
    /// non-empty. Clients can branch on this without inspecting the
    /// vector.
    pub restart_required: bool,
}

async fn admin_reload(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    body: Option<Json<ReloadRequest>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let req = body.map(|Json(b)| b).unwrap_or_default();

    match daemon
        .reload_from_setup_config(req.config_path.as_deref())
        .await
    {
        Ok(report) => (StatusCode::OK, Json(serde_json::to_value(report).unwrap())).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// What changed between the daemon's in-memory `SetupConfig` and the
/// fresh copy on disk. Used only by `EmbeddedDaemon::reload_from_setup_config`.
/// Exposed at `pub(crate)` so the daemon module can populate it.
#[derive(Debug, Default)]
pub(crate) struct ConfigDiff {
    pub models_changed: Vec<&'static str>,
    pub restart_required: Vec<&'static str>,
}

impl ConfigDiff {
    /// Compare an old and new `SetupConfig`. Returns which fields
    /// differ and which category they fall into (hot-reloadable vs.
    /// restart-required).
    pub(crate) fn diff(old: &SetupConfig, new: &SetupConfig) -> Self {
        let mut d = ConfigDiff::default();
        // `[models]` appearing or disappearing is a CLASS change (holder
        // <-> terminal), not a slot swap: the provider stops being an
        // embedded engine and becomes a forwarder, or the reverse. The
        // factory cannot hot-swap that, so it is restart-required and the
        // per-field comparisons below are skipped — comparing a slot path
        // against an absent section would report "primary changed" for a
        // node that no longer has slots at all.
        let (old_models, new_models) = match (old.models.as_ref(), new.models.as_ref()) {
            (Some(o), Some(n)) => (o, n),
            (None, None) => return Self::finish_non_model_fields(d, old, new),
            _ => {
                d.restart_required.push("models");
                return Self::finish_non_model_fields(d, old, new);
            }
        };
        if old_models.primary != new_models.primary {
            d.models_changed.push("models.primary");
        }
        if old_models.fast != new_models.fast {
            d.models_changed.push("models.fast");
        }
        if old_models.embed != new_models.embed {
            d.models_changed.push("models.embed");
        }
        if old_models.code != new_models.code {
            d.models_changed.push("models.code");
        }
        if old_models.context_size != new_models.context_size {
            // Hot-reloadable, NOT restart-required: the provider factory
            // reads `effective_context_size()` and rebuilds every slot from
            // scratch, so the swap picks the new window up. It was simply
            // absent from this diff, which made `is_noop()` true and the
            // rebuild never fire — so `svrn model context <n>` wrote the
            // config, reported "no config changes detected — nothing to
            // reload", and left the daemon serving the old window. Measured
            // 2026-08-23: a run raised to 65,536 kept refusing prompts at
            // 32,764 until the daemon was restarted by hand, and the CLI
            // said it had applied (§18.3 — a success message for work that
            // did not happen).
            d.models_changed.push("models.context_size");
        }
        Self::finish_non_model_fields(d, old, new)
    }

    /// The non-`[models]` half of the diff, shared by every arm above so a
    /// class change still reports a moved port or data dir. Splitting it out
    /// rather than duplicating: a second copy is how one arm quietly stops
    /// noticing `daemon.client_bind` (§10.6).
    fn finish_non_model_fields(mut d: Self, old: &SetupConfig, new: &SetupConfig) -> Self {
        // Compared through `binding()` so BOTH forms are covered by one test.
        // Reading `node.entry` alone stopped noticing the identity binding the
        // moment it existed, and a terminal re-pointed at a different entry
        // node would have kept serving from the old one until something else
        // restarted it.
        if old.node.binding() != new.node.binding() {
            // The entry node is where a terminal forwards every turn, and its
            // provider is built once, at boot.
            d.restart_required.push("node.entry");
        }
        if old.daemon.client_port != new.daemon.client_port {
            d.restart_required.push("daemon.client_port");
        }
        if old.daemon.client_bind != new.daemon.client_bind {
            // Changing the bind address re-opens the listener (and
            // re-runs token resolution for the new loopback/remote
            // posture) — can't hot-swap an already-bound TcpListener.
            // This is the field the desktop's "enable mesh sharing"
            // toggle flips (127.0.0.1 → 0.0.0.0).
            d.restart_required.push("daemon.client_bind");
        }
        if old.daemon.client_token != new.daemon.client_token {
            // The token is resolved + installed onto AppState during
            // start_daemon; restart re-runs that path.
            d.restart_required.push("daemon.client_token");
        }
        if old.daemon.internal_port != new.daemon.internal_port {
            d.restart_required.push("daemon.internal_port");
        }
        if old.data.dir != new.data.dir {
            d.restart_required.push("data.dir");
        }
        if old.iroh.enabled != new.iroh.enabled {
            // The iroh endpoint is bound (or not) during start_daemon;
            // the acceptor + RoutedTransport install can't be hot-swapped.
            d.restart_required.push("iroh.enabled");
        }
        if old.iroh.transport != new.iroh.transport {
            // Per-class routing is baked into the RoutedTransport
            // installed at startup.
            d.restart_required.push("iroh.transport");
        }
        d
    }

    pub(crate) fn is_noop(&self) -> bool {
        self.models_changed.is_empty() && self.restart_required.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EmbeddedDaemon;
    use async_trait::async_trait;
    use sovereign_core::setup_config::{DaemonSection, DataSection, ModelsSection};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    /// Stub provider that records a version so tests can assert the
    /// swap actually happened. Only `capabilities()` is exercised; the
    /// other trait methods would panic if called, but the admin
    /// handler never calls them during a reload test.
    struct StubProvider {
        #[allow(dead_code)]
        version: usize,
    }

    #[async_trait]
    impl InferenceProvider for StubProvider {
        async fn complete(
            &self,
            _request: &sovereign_core::types::CompletionRequest,
        ) -> sovereign_core::error::Result<sovereign_core::types::CompletionResponse> {
            unimplemented!("stub")
        }

        async fn complete_stream(
            &self,
            _request: &sovereign_core::types::CompletionRequest,
        ) -> sovereign_core::error::Result<
            std::pin::Pin<
                Box<dyn futures::Stream<Item = sovereign_core::error::Result<String>> + Send>,
            >,
        > {
            unimplemented!("stub")
        }

        async fn embed(&self, _text: &str) -> sovereign_core::error::Result<Vec<f32>> {
            unimplemented!("stub")
        }

        fn capabilities(&self) -> sovereign_core::types::ProviderCapabilities {
            sovereign_core::types::ProviderCapabilities {
                max_context_tokens: 0,
                supports_structured_output: false,
                relative_speed: sovereign_core::types::Speed::Fast,
                relative_reasoning: sovereign_core::types::Depth::Shallow,
            }
        }
    }

    struct StubFactory {
        build_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ProviderFactory for StubFactory {
        async fn build_provider(
            &self,
            _cfg: &SetupConfig,
        ) -> Result<Arc<dyn InferenceProvider>, String> {
            let v = self.build_count.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(Arc::new(StubProvider { version: v }))
        }
    }

    fn write_cfg(dir: &TempDir, primary: &str) -> PathBuf {
        let path = dir.path().join("config.toml");
        let cfg = SetupConfig {
            compute: Default::default(),
            search: Default::default(),
            models: Some(ModelsSection {
                primary: PathBuf::from(primary),
                fast: Some(PathBuf::from("/m/fast.gguf")),
                embed: PathBuf::from("/m/embed.gguf"),
                code: None,
                context_size: None,
                fast_context_size: None,
                max_extras_memory_gb: None,
                extra: std::collections::BTreeMap::new(),
                primary_pool: None,
                edit: None,
            }),
            node: Default::default(),
            daemon: DaemonSection::default(),
            data: DataSection::default(),
            watched_folders: Default::default(),
            memory: Default::default(),
            iroh: Default::default(),
            shared_model: Default::default(),
            discovery: Default::default(),
            mcp_servers: Vec::new(),
        };
        cfg.save_to(&path).unwrap();
        path
    }

    #[test]
    fn config_diff_flags_iroh_changes_as_restart_required() {
        let base = SetupConfig {
            compute: Default::default(),
            search: Default::default(),
            models: Some(ModelsSection {
                primary: PathBuf::from("/m/primary.gguf"),
                fast: None,
                embed: PathBuf::from("/m/embed.gguf"),
                code: None,
                context_size: None,
                fast_context_size: None,
                max_extras_memory_gb: None,
                extra: std::collections::BTreeMap::new(),
                primary_pool: None,
                edit: None,
            }),
            node: Default::default(),
            daemon: DaemonSection::default(),
            data: DataSection::default(),
            watched_folders: Default::default(),
            memory: Default::default(),
            iroh: Default::default(),
            shared_model: Default::default(),
            discovery: Default::default(),
            mcp_servers: Vec::new(),
        };

        let mut enabled_flipped = base.clone();
        enabled_flipped.iroh.enabled = Some(true);
        let d = ConfigDiff::diff(&base, &enabled_flipped);
        assert_eq!(d.restart_required, vec!["iroh.enabled"]);
        assert!(d.models_changed.is_empty());

        let mut class_pinned = base.clone();
        class_pinned.iroh.transport.inference = Some("ip".into());
        let d = ConfigDiff::diff(&base, &class_pinned);
        assert_eq!(d.restart_required, vec!["iroh.transport"]);

        let d = ConfigDiff::diff(&base, &base.clone());
        assert!(d.restart_required.is_empty());
    }

    async fn spawn(daemon: Arc<EmbeddedDaemon>) -> String {
        let app = admin_router(Arc::clone(&daemon));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .ok();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        format!("http://{addr}")
    }

    /// Direct unit test for the guard itself — independent of axum
    /// extraction. Guards are small; bugs here are quiet, so pin both
    /// directions (loopback passes, everything else rejected).
    #[test]
    fn enforce_localhost_rejects_non_loopback() {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

        let allowed = [
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9741),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 1, 2, 3)), 9741),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9741),
        ];
        for addr in allowed {
            assert!(
                enforce_localhost(&addr).is_ok(),
                "loopback {addr} must pass"
            );
        }

        // Covers the attack scenarios: LAN peer, Tailscale peer, and a
        // public IP — none should reach the admin handler.
        let denied = [
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7)), 9741),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2)), 9741),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 9741),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0x2606, 0, 0, 0, 0, 0, 0, 1)), 9741),
        ];
        for addr in denied {
            let Err(resp) = enforce_localhost(&addr) else {
                panic!("non-loopback {addr} must be rejected");
            };
            assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
        }
    }

    #[tokio::test]
    async fn reload_is_noop_when_nothing_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();

        let counter = Arc::new(AtomicUsize::new(0));
        let daemon = EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            initial,
            crate::daemon_services::fixtures::headless_with_factory(Arc::new(StubFactory {
                build_count: Arc::clone(&counter),
            })),
        );

        let base = spawn(Arc::clone(&daemon)).await;
        let resp = reqwest::Client::new()
            .post(format!("{base}/v1/admin/reload"))
            .json(&serde_json::json!({ "config_path": path }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: ReloadResponse = resp.json().await.unwrap();
        assert!(body.reloaded_fields.is_empty());
        assert!(!body.restart_required);
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "factory must not be called"
        );
    }

    #[tokio::test]
    async fn reload_swaps_inference_provider_when_models_change() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary-v1.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();

        let counter = Arc::new(AtomicUsize::new(0));
        // The headless profile carries the factory; the initial provider comes
        // in through the core ring, so there is no seeding step any more.
        let daemon = EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            initial,
            crate::daemon_services::fixtures::headless_with_factory(Arc::new(StubFactory {
                build_count: Arc::clone(&counter),
            })),
        );

        // Change models.primary on disk, then POST reload.
        let _ = write_cfg(&tmp, "/m/primary-v2.gguf");

        let base = spawn(Arc::clone(&daemon)).await;
        let resp = reqwest::Client::new()
            .post(format!("{base}/v1/admin/reload"))
            .json(&serde_json::json!({ "config_path": path }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: ReloadResponse = resp.json().await.unwrap();
        assert_eq!(body.reloaded_fields, vec!["models.primary".to_string()]);
        assert!(!body.restart_required);
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "factory must be invoked exactly once"
        );
    }

    /// RED before the `models.context_size` arm was added to `ConfigDiff`.
    ///
    /// `svrn model context 65536` wrote the config, then reported "no config
    /// changes detected — nothing to reload" while the daemon kept serving
    /// 32,764 — a success message for work that did not happen (§18.3).
    /// The window IS hot-reloadable: `build_provider` reads
    /// `effective_context_size()` and rebuilds every slot. The diff simply
    /// never looked at the field, so `is_noop()` short-circuited the rebuild.
    #[tokio::test]
    async fn reload_applies_a_context_size_change_without_a_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();
        assert_eq!(
            initial.models().unwrap().context_size,
            None,
            "fixture starts at auto"
        );

        // Commissioned through the total constructor, like every other test in
        // this file. These two tests arrived on main written against the
        // `set_*` builders daemon-convergence Phase 2 deleted; the merge took
        // both sides' text and only the compiler noticed.
        let counter = Arc::new(AtomicUsize::new(0));
        let daemon = EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            initial.clone(),
            crate::daemon_services::fixtures::headless_with_factory(Arc::new(StubFactory {
                build_count: Arc::clone(&counter),
            })),
        );

        let mut modified = initial;
        modified.models.as_mut().unwrap().context_size = Some(65_536);
        modified.save_to(&path).unwrap();

        let base = spawn(Arc::clone(&daemon)).await;
        let resp = reqwest::Client::new()
            .post(format!("{base}/v1/admin/reload"))
            .json(&serde_json::json!({ "config_path": path }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: ReloadResponse = resp.json().await.unwrap();
        assert_eq!(
            body.reloaded_fields,
            vec!["models.context_size".to_string()],
            "the window must be REPORTED as reloaded, not silently ignored"
        );
        assert!(
            !body.restart_required,
            "the factory rebuilds every slot from cfg — no restart is needed"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "a context change must actually rebuild the provider"
        );
    }

    /// The code slot had the same hole: `build_provider` passes
    /// `cfg.models.code` to the loader, but the diff never compared it, so
    /// `svrn model set code <file>` on a running daemon was a no-op that
    /// reported success.
    #[tokio::test]
    async fn reload_applies_a_code_slot_change_without_a_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();

        // Commissioned through the total constructor, like every other test in
        // this file. These two tests arrived on main written against the
        // `set_*` builders daemon-convergence Phase 2 deleted; the merge took
        // both sides' text and only the compiler noticed.
        let counter = Arc::new(AtomicUsize::new(0));
        let daemon = EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            initial.clone(),
            crate::daemon_services::fixtures::headless_with_factory(Arc::new(StubFactory {
                build_count: Arc::clone(&counter),
            })),
        );

        let mut modified = initial;
        modified.models.as_mut().unwrap().code = Some(PathBuf::from("/m/coder.gguf"));
        modified.save_to(&path).unwrap();

        let base = spawn(Arc::clone(&daemon)).await;
        let resp = reqwest::Client::new()
            .post(format!("{base}/v1/admin/reload"))
            .json(&serde_json::json!({ "config_path": path }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: ReloadResponse = resp.json().await.unwrap();
        assert_eq!(body.reloaded_fields, vec!["models.code".to_string()]);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reload_port_change_requires_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();

        let counter = Arc::new(AtomicUsize::new(0));
        let daemon = EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            initial.clone(),
            crate::daemon_services::fixtures::headless_with_factory(Arc::new(StubFactory {
                build_count: Arc::clone(&counter),
            })),
        );

        // Rewrite config with a different client_port.
        let mut modified = initial;
        modified.daemon.client_port = 19741;
        modified.save_to(&path).unwrap();

        let base = spawn(Arc::clone(&daemon)).await;
        let resp = reqwest::Client::new()
            .post(format!("{base}/v1/admin/reload"))
            .json(&serde_json::json!({ "config_path": path }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: ReloadResponse = resp.json().await.unwrap();
        assert!(body.reloaded_fields.is_empty());
        assert!(body.restart_required);
        assert_eq!(
            body.restart_required_fields,
            vec!["daemon.client_port".to_string()]
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "port-only change must not rebuild provider"
        );
    }

    /// Regression test for the production listener shape.
    ///
    /// The daemon's real client listener uses
    /// `router.into_make_service_with_connect_info::<SocketAddr>()`
    /// so that `ConnectInfo<SocketAddr>` extractors in mesh_http,
    /// admin_http, and mcp_router can read the peer address and
    /// enforce the loopback-only guard. An earlier version of
    /// `daemon.rs` used bare `axum::serve(listener, router)` which
    /// made ConnectInfo extraction fail with 500 "Missing request
    /// extension" — breaking the guards for legitimate localhost
    /// callers (and, more subtly, defeating them for remote callers).
    ///
    /// This test pins the correct shape so a future refactor can't
    /// silently revert to the bare-serve pattern.
    #[tokio::test]
    async fn loopback_guard_works_under_production_listener_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();

        let daemon = EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            initial,
            crate::daemon_services::fixtures::headless(),
        );

        let app = admin_router(Arc::clone(&daemon));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // Exact shape `daemon::start_daemon` uses — if this line
            // ever drifts from the production call site, the admin
            // surface breaks for localhost and this test must fail.
            let service = app.into_make_service_with_connect_info::<SocketAddr>();
            axum::serve(listener, service).await.ok();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/v1/admin/reload"))
            .json(&serde_json::json!({ "config_path": path }))
            .send()
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            reqwest::StatusCode::OK,
            "loopback must pass the guard; got body: {}",
            resp.text().await.unwrap_or_default()
        );
    }

    #[tokio::test]
    async fn reload_without_factory_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary-v1.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();

        // The DESKTOP profile, which declares it carries no ProviderFactory.
        // The refusal must name the profile rather than report a missing
        // installation — nothing is missing, this shape has no factory.
        let daemon = EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            initial,
            crate::daemon_services::fixtures::desktop(),
        );

        let _ = write_cfg(&tmp, "/m/primary-v2.gguf");

        let base = spawn(Arc::clone(&daemon)).await;
        let resp = reqwest::Client::new()
            .post(format!("{base}/v1/admin/reload"))
            .json(&serde_json::json!({ "config_path": path }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 500);
    }
}
