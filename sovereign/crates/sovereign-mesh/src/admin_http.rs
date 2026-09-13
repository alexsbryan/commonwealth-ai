// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP admin surface — `POST /v1/admin/reload`,
//! `GET /v1/admin/context-window` and `GET /v1/admin/chat-activity`.
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

use std::sync::Arc;

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_contracts::daemon_wire::{ChatActivitySummary, ContextWindow};
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;

use crate::daemon::EmbeddedDaemon;
use crate::http_response::{json_error, service_unavailable};
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

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
        .route("/v1/admin/context-window", get(context_window))
        .route("/v1/admin/chat-activity", get(chat_activity))
        .localhost_only_with(daemon)
}

/// Query of `GET /v1/admin/chat-activity`. The window is the CALLER's
/// choice (the Mesh Health pane offers 7 / 30 / 90 days) and the default is
/// a week, matching what the desktop passed.
#[derive(Debug, Deserialize)]
pub struct ChatActivityQuery {
    /// Window in seconds, counted back from now. Clamped to at least one
    /// day — a zero or negative window would summarise nothing and render
    /// as "no activity", which is a different claim.
    #[serde(default)]
    pub window_secs: Option<i64>,
}

/// `GET /v1/admin/chat-activity?window_secs=N` — the user's own chat usage,
/// rolled up from the store THIS daemon serves turns against.
///
/// Beside `context-window` for the same reason that route is here: both are
/// a read of the serving process's own state that a Settings-style panel
/// renders, and both were computed inside the desktop from a handle on
/// something that no longer answers a turn. The desktop called
/// `SqliteStateStore::summarize_chat_activity` on the `sovereign.db` IT
/// opened; every turn has been the daemon's since sv-surface R5, so on an
/// attached boot the pane summarised a file with no turns in it and
/// reported real-looking zeros.
///
/// Absence is reported: a daemon with no store answers 503 and a store that
/// keeps no message metadata answers 501 with the method named
/// (`StateStore::summarize_chat_activity`'s default). Neither is an
/// all-zero summary, because "there is nothing to ask" and "you ran no
/// turns this week" render differently (ARCH principle 6).
async fn chat_activity(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    axum::extract::Query(q): axum::extract::Query<ChatActivityQuery>,
) -> Response {
    let Some(store) = daemon.state_store() else {
        return service_unavailable("this daemon holds no conversation store (mesh-admin)");
    };
    let window_secs = q.window_secs.unwrap_or(7 * 86_400).max(86_400);
    match store.summarize_chat_activity(window_secs).await {
        Ok(summary) => {
            tracing::debug!(
                window_secs,
                turns = summary.turns,
                tokens_generated = summary.tokens_generated,
                "admin_http: chat activity served",
            );
            Json::<ChatActivitySummary>(summary).into_response()
        }
        Err(sovereign_core::error::Error::NotImplemented(msg)) => {
            json_error(StatusCode::NOT_IMPLEMENTED, format!("chat-activity: {msg}"))
        }
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("chat-activity: {e}"),
        ),
    }
}

/// `GET /v1/admin/context-window` — the chat slot's context window, from
/// the daemon that owns the slot.
///
/// It lives beside `admin_reload` because the two are the same subject
/// read and written: `models.context_size` is in that reload's diff
/// precisely because the provider factory rebuilds every slot from
/// `effective_context_size()`, and this route reports what the rebuild
/// landed on.
///
/// The desktop's `get_setup_context_size` read its OWN provider for
/// `effective` and `n_ctx_train` until 2026-09-12 — a Settings panel
/// describing the window a slot in the app was budgeting against, while
/// every turn was budgeted by the daemon's. `configured` was already
/// right there (both processes read the same `config.toml`), which is
/// what made the wrong two easy to miss.
///
/// A daemon with no provider installed answers `None` for both, not a
/// copy of `configured`: "there is no slot to ask" is not the same fact
/// as "the slot agrees with the config" (ARCH principle 6).
async fn context_window(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Json<ContextWindow> {
    let configured = daemon.configured_context_size().await;
    let (effective, n_ctx_train) = match daemon.inference_provider().await {
        Some(inf) => (inf.effective_context_size(), inf.n_ctx_train_for_primary()),
        None => (None, None),
    };
    tracing::debug!(
        configured,
        effective = ?effective,
        n_ctx_train = ?n_ctx_train,
        "admin_http: context window served",
    );
    Json(ContextWindow {
        configured,
        effective,
        n_ctx_train,
    })
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    body: Option<Json<ReloadRequest>>,
) -> impl IntoResponse {
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
        if old_models.extra != new_models.extra {
            // Same hole as context_size and code: `svrn model set-extra`
            // wrote `[models.extra]`, this diff never read it, and the CLI
            // reported "no config changes detected" while `/v1/models` kept
            // not advertising the slot — so a pin on that name 503'd as
            // "no node in this mesh advertises model" until a restart
            // (2026-09-04, the seat's review engine). The factory rebuilds
            // every slot from cfg, extras included.
            d.models_changed.push("models.extra");
        }
        if old_models.max_extras_memory_gb != new_models.max_extras_memory_gb {
            d.models_changed.push("models.max_extras_memory_gb");
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
        // The three below reach the acceptor the same way `iroh.enabled` does
        // — read once while it is constructed, never re-read — and until
        // 2026-09-12 none of them was compared here. A change to any one made
        // `is_noop()` true, so `svrn daemon reload` printed "no config changes
        // detected" over a config that had demonstrably changed and the daemon
        // silently kept the old value (ARCH §18.3: absence is reported, never
        // defaulted). Observed setting `media_origin` on this host: the verb
        // said stored, reload said nothing changed, the fanout still 401'd.
        if old.iroh.media_origin != new.iroh.media_origin {
            d.restart_required.push("iroh.media_origin");
        }
        if old.iroh.media_allow != new.iroh.media_allow {
            d.restart_required.push("iroh.media_allow");
        }
        if old.iroh.apps != new.iroh.apps {
            // `[iroh.apps]` is the durable publish tier; the ephemeral one
            // (`svrn run`) goes through `PublishedApps` and needs no restart.
            d.restart_required.push("iroh.apps");
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
    use crate::loopback_guard::enforce_localhost;
    use crate::EmbeddedDaemon;
    use async_trait::async_trait;
    use sovereign_core::setup_config::{DaemonSection, DataSection, ModelsSection};
    use std::net::SocketAddr;
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
            engine: Default::default(),
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
            engine: Default::default(),
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

    /// The failing input this exists for, and it was a live one: adding
    /// `[iroh] media_origin` and running `svrn daemon reload` printed
    /// "✓ no config changes detected — nothing to reload" while the daemon
    /// went on serving the old value, because none of these three fields was
    /// compared. A house is converted along exactly this path — "one config
    /// line and a join" — so the reload saying nothing happened is the
    /// difference between a working demo and a silent one.
    ///
    /// Each is asserted ALONE. A single config differing in all three would
    /// pass even if only one comparison existed.
    #[test]
    fn a_media_or_app_config_change_is_never_reported_as_no_change() {
        let base = SetupConfig {
            engine: Default::default(),
            compute: Default::default(),
            search: Default::default(),
            models: None,
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

        let mut origin_set = base.clone();
        origin_set.iroh.media_origin = Some("127.0.0.1:8096".into());
        let d = ConfigDiff::diff(&base, &origin_set);
        assert_eq!(d.restart_required, vec!["iroh.media_origin"]);
        assert!(!d.is_noop(), "a changed config must never read as a no-op");

        let mut allow_set = base.clone();
        allow_set.iroh.media_allow = vec!["LittleMac".into()];
        let d = ConfigDiff::diff(&base, &allow_set);
        assert_eq!(d.restart_required, vec!["iroh.media_allow"]);
        assert!(!d.is_noop());

        let mut app_published = base.clone();
        app_published
            .iroh
            .apps
            .insert("chores".into(), "127.0.0.1:5000".into());
        let d = ConfigDiff::diff(&base, &app_published);
        assert_eq!(d.restart_required, vec!["iroh.apps"]);
        assert!(!d.is_noop());

        // The other half of the claim: an unchanged config still reads as one.
        assert!(ConfigDiff::diff(&base, &base.clone()).is_noop());
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

    /// A provider that OWNS a local slot, so the two `Option` fields can
    /// be something other than the trait's `None` default. Without this
    /// the no-slot case and the has-slot case are indistinguishable and
    /// the test below would pass against a handler that always answered
    /// `None`.
    struct SlotProvider;

    #[async_trait]
    impl InferenceProvider for SlotProvider {
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
                max_context_tokens: 32_768,
                supports_structured_output: false,
                relative_speed: sovereign_core::types::Speed::Fast,
                relative_reasoning: sovereign_core::types::Depth::Shallow,
            }
        }

        fn effective_context_size(&self) -> Option<u32> {
            Some(32_768)
        }

        fn n_ctx_train_for_primary(&self) -> Option<u32> {
            Some(131_072)
        }
    }

    /// The window comes from the slot the DAEMON is serving on, and
    /// `configured` from its config — three numbers that are allowed to
    /// disagree, which is why the route reports all three.
    #[tokio::test]
    async fn context_window_reports_the_daemons_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();
        let daemon = EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            initial,
            crate::daemon_services::fixtures::headless_with_provider(Arc::new(SlotProvider)),
        );

        let base = spawn(Arc::clone(&daemon)).await;
        let resp = reqwest::Client::new()
            .get(format!("{base}/v1/admin/context-window"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: ContextWindow = resp.json().await.unwrap();
        assert_eq!(body.effective, Some(32_768));
        assert_eq!(body.n_ctx_train, Some(131_072));
    }

    /// A provider with NO local slot — the remote-only case — answers
    /// `None` for both, and neither is quietly filled in from
    /// `configured`. "There is no slot to ask" and "the slot agrees with
    /// the config" are different answers, and a Settings panel renders
    /// them differently (ARCH principle 6).
    ///
    /// The fixture is deliberately `NullProvider`, which does not
    /// override either method and so answers the trait's `None`. That
    /// makes this test's pass condition weak ON ITS OWN — a handler
    /// hard-coding `None` would satisfy it — which is exactly why it is
    /// paired with the test above, where a provider that DOES own a slot
    /// must come back with that slot's numbers. Neither test is a gate
    /// without the other.
    #[tokio::test]
    async fn context_window_reports_absence_rather_than_echoing_configured() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();
        let initial_ctx = initial.effective_context_size();
        let daemon = EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            initial,
            crate::daemon_services::fixtures::headless(),
        );

        let base = spawn(Arc::clone(&daemon)).await;
        let body: ContextWindow = reqwest::Client::new()
            .get(format!("{base}/v1/admin/context-window"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body.effective, None, "no slot is not a number");
        assert_eq!(body.n_ctx_train, None, "no model is not a ceiling");
        assert_ne!(
            body.effective,
            Some(body.configured),
            "absence must not be reported as agreement with the config"
        );
        // And `configured` is the DAEMON's own, from the config it was
        // commissioned with — not a re-read of the serving process's
        // `~/.svrnmesh/config.toml`.
        assert_eq!(body.configured, initial_ctx);
    }

    /// Seed one assistant message whose metadata carries the provenance the
    /// rollup reads, plus one that carries none, and assert the route serves
    /// the DAEMON's store's numbers.
    ///
    /// The positive control is the point. Its partner below can only reach
    /// the trait's `Err(NotImplemented)` default, which a handler
    /// hard-coding a 501 would satisfy; this one cannot pass unless the
    /// route actually asked a store that counted something (ARCH principle
    /// 5, and the exact pairing `context_window`'s two tests carry).
    ///
    /// WATCHED TO FAIL: with the handler's
    /// `store.summarize_chat_activity(window_secs)` replaced by an
    /// all-zero `ChatActivitySummary`, this test fails on `turns`
    /// (`left: 0, right: 1`) while
    /// `chat_activity_reports_a_store_that_declines_the_rollup` still
    /// passes. Restored after.
    #[tokio::test]
    async fn chat_activity_reports_the_daemons_own_store() {
        use sovereign_core::traits::ConversationStore;

        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();

        let store = Arc::new(
            sovereign_store::sqlite::SqliteStateStore::open(&tmp.path().join("sovereign.db"))
                .expect("open sqlite state store"),
        );
        let now = sovereign_core::time::unix_now();
        // The shape the runtime persists: provenance nested under
        // `metadata["provenance"]`, `completion_tokens` preferred over
        // `tokens_used`, one retrieved source per corpus.
        //
        // Built from the TYPE, not from hand-written JSON, and that is not
        // style. My first draft spelled the object by hand with four of the
        // eleven fields; `ResponseProvenance` requires `intent`,
        // `search_method`, `oicp_match` and `total_latency_ms` with no serde
        // default, so `from_value` failed, the rollup SKIPPED the message
        // (it is best-effort by contract), and the route answered
        // `turns: 0`. The test caught it — that is the positive control
        // doing its job — but a fixture that cannot drift is better than one
        // that is checked once (ARCH principle 10, and §18.4: validate the
        // instrument before the result).
        let provenance = sovereign_contracts::types::ResponseProvenance {
            intent: "DeepQuery".into(),
            search_method: Some("CorpusEngine".into()),
            sources: vec![sovereign_contracts::types::SourceSummary {
                origin: "wikipedia".into(),
                count: 3,
                from_peer: None,
                display_name: None,
            }],
            inference_backend: "primary-122b".into(),
            oicp_match: None,
            total_latency_ms: 42,
            tokens_used: 999,
            coarse_intent: None,
            router: None,
            self_assessment: None,
            routing_trigger: None,
            coverage: None,
            finish_reason: None,
            max_tokens_budget: None,
            completion_tokens: Some(64),
            context_window: None,
        };
        store
            .save_message(&sovereign_contracts::types::Message {
                id: "m-1".into(),
                conversation_id: "c-1".into(),
                role: sovereign_contracts::types::Role::Assistant,
                content: "an answer".into(),
                created_at: now,
                metadata: Some(serde_json::json!({
                    "provenance": serde_json::to_value(&provenance).unwrap(),
                })),
                version: now,
            })
            .await
            .unwrap();
        // A message with no provenance is SKIPPED, not counted as a turn —
        // so a route that merely counted rows would over-report and this
        // asserts the fold, not the SELECT.
        store
            .save_message(&sovereign_contracts::types::Message {
                id: "m-2".into(),
                conversation_id: "c-1".into(),
                role: sovereign_contracts::types::Role::Assistant,
                content: "a pre-provenance answer".into(),
                created_at: now,
                metadata: None,
                version: now,
            })
            .await
            .unwrap();

        let daemon = EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            initial,
            crate::daemon_services::fixtures::headless_with_store(store),
        );
        let base = spawn(Arc::clone(&daemon)).await;
        let resp = reqwest::Client::new()
            .get(format!("{base}/v1/admin/chat-activity?window_secs=86400"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: ChatActivitySummary = resp.json().await.unwrap();
        assert_eq!(body.turns, 1, "one message carried provenance, not two");
        assert_eq!(
            body.tokens_generated, 64,
            "completion_tokens wins over tokens_used"
        );
        assert_eq!(body.chunks_retrieved, 3);
        assert_eq!(body.window_days, 1, "the window the caller asked for");
        assert_eq!(body.by_model.len(), 1);
        assert_eq!(body.by_model[0].model, "primary-122b");
        assert_eq!(body.by_corpus.len(), 1);
        assert_eq!(body.by_corpus[0].origin, "wikipedia");
        assert!(!body.by_corpus[0].from_peer);
    }

    /// A store that keeps no message metadata REFUSES, by name — it does
    /// not answer an all-zero summary. "There is nothing to ask" and "you
    /// ran no turns this week" render differently in a usage pane
    /// (principle 6), and the in-memory store is exactly the case: it
    /// inherits `ConversationStore::summarize_chat_activity`'s
    /// `Err(NotImplemented)` default.
    #[tokio::test]
    async fn chat_activity_reports_a_store_that_declines_the_rollup() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();
        let daemon = EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            initial,
            crate::daemon_services::fixtures::headless(),
        );

        let base = spawn(Arc::clone(&daemon)).await;
        let resp = reqwest::Client::new()
            .get(format!("{base}/v1/admin/chat-activity"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            501,
            "a declined rollup is Not Implemented, not 200 with zeros"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        let err = body["error"].as_str().unwrap_or_default();
        assert!(
            err.contains("summarize_chat_activity"),
            "the refusal must name the method that declined; got {err}"
        );
    }

    /// The window is the HOST's to clamp: a caller asking for zero (or a
    /// negative) window would summarise nothing and the pane would render
    /// "no activity", which is a claim about the user rather than about the
    /// request.
    #[tokio::test]
    async fn chat_activity_clamps_the_window_to_at_least_a_day() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();
        let store = Arc::new(
            sovereign_store::sqlite::SqliteStateStore::open(&tmp.path().join("sovereign.db"))
                .expect("open sqlite state store"),
        );
        let daemon = EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            initial,
            crate::daemon_services::fixtures::headless_with_store(store),
        );
        let base = spawn(Arc::clone(&daemon)).await;
        let body: ChatActivitySummary = reqwest::Client::new()
            .get(format!("{base}/v1/admin/chat-activity?window_secs=0"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body.window_days, 1, "a zero window is clamped to one day");
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

    /// RED before the `models.extra` arm was added to `ConfigDiff`.
    ///
    /// `svrn model set-extra Qwen3.8-27B <file>` wrote `[models.extra]`, the
    /// CLI reported "no config changes detected — nothing to reload", and
    /// `/v1/models` never advertised the name — so every call pinned to it
    /// answered 503 "no node in this mesh advertises model" until a restart.
    /// The failing input is the writer's own output: a config that differs
    /// from the running one ONLY in the extras map.
    #[tokio::test]
    async fn reload_applies_an_extra_slot_without_a_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();
        assert!(
            initial.models().unwrap().extra.is_empty(),
            "fixture starts with no extras"
        );

        let counter = Arc::new(AtomicUsize::new(0));
        let daemon = EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            initial.clone(),
            crate::daemon_services::fixtures::headless_with_factory(Arc::new(StubFactory {
                build_count: Arc::clone(&counter),
            })),
        );

        let mut modified = initial;
        modified
            .models
            .as_mut()
            .unwrap()
            .extra
            .insert("judge-27b".to_string(), PathBuf::from("/m/judge-27b.gguf"));
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
            vec!["models.extra".to_string()],
            "an added extra slot must be REPORTED as reloaded, not silently ignored"
        );
        assert!(
            !body.restart_required,
            "extras are built by the same factory — no restart"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "an extras change must actually rebuild the provider"
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
