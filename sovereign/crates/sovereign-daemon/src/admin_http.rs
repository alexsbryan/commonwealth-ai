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
    /// `[iroh] media_origin` / `media_allow`: applied by swapping the live
    /// `MediaRoute`, no restart.
    pub media_changed: Vec<&'static str>,
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
        // The FIVE below reached the acceptor the same way `iroh.enabled` does
        // — read once while it is constructed, never re-read — and until
        // 2026-09-12 none of them was compared here. A change to any one made
        // `is_noop()` true, so `svrn daemon reload` printed "no config changes
        // detected" over a config that had demonstrably changed and the daemon
        // silently kept the old value (ARCH §18.3: absence is reported, never
        // defaulted). Observed setting `media_origin` on this host: the verb
        // said stored, reload said nothing changed, the fanout still 401'd.
        // The media two are live since ring-room (`MediaRoute`): reload
        // applies them, so they are compared here and restart nothing.
        if old.iroh.media_origin != new.iroh.media_origin {
            d.media_changed.push("iroh.media_origin");
        }
        if old.iroh.media_allow != new.iroh.media_allow {
            d.media_changed.push("iroh.media_allow");
        }
        if old.iroh.apps != new.iroh.apps {
            // `[iroh.apps]` is the durable publish tier; the ephemeral one
            // (`svrn run`) goes through `PublishedApps` and needs no restart.
            d.restart_required.push("iroh.apps");
        }
        // The offer origin is read at acceptor construction exactly as the
        // media origin is, so it is compared here IN THE SAME COMMIT that
        // adds it. Adding the key and not the comparison is how the media
        // defect above happened: the operator writes one config line, reload
        // says nothing changed, and `svrn mesh offers` reports the node
        // publishes none — three affirmative messages and nothing applied.
        if old.iroh.offer_origin != new.iroh.offer_origin {
            d.restart_required.push("iroh.offer_origin");
        }
        if old.iroh.offer_allow != new.iroh.offer_allow {
            d.restart_required.push("iroh.offer_allow");
        }
        d
    }

    pub(crate) fn is_noop(&self) -> bool {
        self.models_changed.is_empty()
            && self.media_changed.is_empty()
            && self.restart_required.is_empty()
    }
}

#[cfg(test)]
mod tests;
