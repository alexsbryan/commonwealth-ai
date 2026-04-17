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

/// How the admin handler rebuilds an `InferenceProvider` from a new
/// `SetupConfig`. Implemented by whoever owns the model-loading code —
/// typically `sovereign-cli::daemon_cmd` (for the launchd daemon) or
/// the desktop bootstrap (for in-process). Keeps `sovereign-mesh` free
/// of llama.cpp loading details.
#[async_trait::async_trait]
pub trait ProviderFactory: Send + Sync {
    async fn build_provider(
        &self,
        cfg: &SetupConfig,
    ) -> Result<Arc<dyn InferenceProvider>, String>;
}

/// Build the admin HTTP router. Merged into the daemon's client router
/// next to `mcp_router` and `mesh_router`.
pub fn admin_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/admin/reload", post(admin_reload))
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
    /// to avoid touching the real `~/.config/sovereign/config.toml`.
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

fn enforce_localhost(
    addr: &SocketAddr,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "local-only" })),
        ))
    }
}

async fn admin_reload(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    body: Option<Json<ReloadRequest>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r.into_response();
    }
    let req = body.map(|Json(b)| b).unwrap_or_default();

    match daemon.reload_from_setup_config(req.config_path.as_deref()).await {
        Ok(report) => (
            StatusCode::OK,
            Json(serde_json::to_value(report).unwrap()),
        )
            .into_response(),
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
        if old.models.primary != new.models.primary {
            d.models_changed.push("models.primary");
        }
        if old.models.fast != new.models.fast {
            d.models_changed.push("models.fast");
        }
        if old.models.embed != new.models.embed {
            d.models_changed.push("models.embed");
        }
        if old.daemon.client_port != new.daemon.client_port {
            d.restart_required.push("daemon.client_port");
        }
        if old.daemon.internal_port != new.daemon.internal_port {
            d.restart_required.push("daemon.internal_port");
        }
        if old.data.dir != new.data.dir {
            d.restart_required.push("data.dir");
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
                Box<
                    dyn futures::Stream<Item = sovereign_core::error::Result<String>>
                        + Send,
                >,
            >,
        > {
            unimplemented!("stub")
        }

        async fn embed(
            &self,
            _text: &str,
        ) -> sovereign_core::error::Result<Vec<f32>> {
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
            models: ModelsSection {
                primary: PathBuf::from(primary),
                fast: PathBuf::from("/m/fast.gguf"),
                embed: PathBuf::from("/m/embed.gguf"),
            },
            daemon: DaemonSection::default(),
            data: DataSection::default(),
        };
        cfg.save_to(&path).unwrap();
        path
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

    #[tokio::test]
    async fn reload_is_noop_when_nothing_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();

        let daemon = Arc::new(EmbeddedDaemon::new(tmp.path().to_path_buf()));
        daemon.set_setup_config(initial).await;
        let counter = Arc::new(AtomicUsize::new(0));
        daemon
            .set_provider_factory(Arc::new(StubFactory {
                build_count: Arc::clone(&counter),
            }))
            .await;

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
        assert_eq!(counter.load(Ordering::SeqCst), 0, "factory must not be called");
    }

    #[tokio::test]
    async fn reload_swaps_inference_provider_when_models_change() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary-v1.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();

        let daemon = Arc::new(EmbeddedDaemon::new(tmp.path().to_path_buf()));
        daemon.set_setup_config(initial).await;
        let counter = Arc::new(AtomicUsize::new(0));
        daemon
            .set_provider_factory(Arc::new(StubFactory {
                build_count: Arc::clone(&counter),
            }))
            .await;
        // Seed an initial provider so we can observe the swap.
        daemon
            .set_inference_provider(Arc::new(StubProvider { version: 0 }))
            .await;

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

    #[tokio::test]
    async fn reload_port_change_requires_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();

        let daemon = Arc::new(EmbeddedDaemon::new(tmp.path().to_path_buf()));
        daemon.set_setup_config(initial.clone()).await;
        let counter = Arc::new(AtomicUsize::new(0));
        daemon
            .set_provider_factory(Arc::new(StubFactory {
                build_count: Arc::clone(&counter),
            }))
            .await;

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

    #[tokio::test]
    async fn reload_without_factory_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_cfg(&tmp, "/m/primary-v1.gguf");
        let initial = SetupConfig::load_from(&path).unwrap();

        let daemon = Arc::new(EmbeddedDaemon::new(tmp.path().to_path_buf()));
        daemon.set_setup_config(initial).await;
        // No factory installed on purpose.

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
