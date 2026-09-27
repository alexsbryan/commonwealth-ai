// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hot reload in serve: the provider every route answers from sits in one
//! cell, and [`RELOAD_PATH`] rebuilds it from the config on disk through the
//! same `ReloadFactory` cold start assembled with (pb-serving-assembly), then
//! swaps it. In-flight requests finish on the provider they cloned; new ones
//! see the new one. The svrn daemon's `/v1/admin/reload` forwards here when
//! it dials serve (pb-svrn-dials-serve).

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use host_kit::shell::guard::LocalOnly;
use host_kit::shell::RouteBundle;
use sovereign_compute::assembly::ReloadFactory;
use sovereign_compute::server::openai_refusal;
use sovereign_contracts::engine_state::{EngineReloaded, RELOAD_PATH};
use sovereign_contracts::setup_config::SetupConfig;
use sovereign_contracts::*;
use tracing::{info, warn};

pub use sovereign_contracts::reloadable_provider::ReloadableProvider;

/// What a reload needs: the cell it swaps, the factory it rebuilds through,
/// and the config file it re-reads.
#[derive(Clone)]
struct Reload {
    cell: Arc<ReloadableProvider>,
    factory: Arc<ReloadFactory>,
    config_path: PathBuf,
}

/// The reload route, loopback-only: a reload tears down and rebuilds every
/// slot, which is the operator's act, never a peer's.
pub fn bundle(
    cell: Arc<ReloadableProvider>,
    factory: Arc<ReloadFactory>,
    config_path: PathBuf,
) -> RouteBundle {
    RouteBundle::new("serve_reload")
        .route(
            RELOAD_PATH,
            post(reload).layer(axum::middleware::from_fn(
                host_kit::shell::guard::loopback_only,
            )),
        )
        .with_state(Reload {
            cell,
            factory,
            config_path,
        })
}

/// Rebuild from the config on disk and swap. Every failure is a 503 naming
/// what failed; the old provider keeps serving, so a failed reload is a
/// retry, never an outage.
async fn reload(_: LocalOnly, State(r): State<Reload>) -> Response {
    let config = match SetupConfig::load_from(&r.config_path) {
        Ok(c) => c,
        Err(e) => return refused(format!("cannot read {}: {e}", r.config_path.display())),
    };
    let factory = Arc::clone(&r.factory);
    let parts = match tokio::task::spawn_blocking(move || factory.build(&config)).await {
        Ok(Ok(parts)) => parts,
        Ok(Err(e)) => return refused(format!("the serving assembly refused: {e}")),
        Err(e) => return refused(format!("the serving assembly panicked: {e}")),
    };
    r.cell.swap(parts.provider, parts.embed_family.clone());
    let resident_models: Vec<String> = r
        .cell
        .resident_slots()
        .into_iter()
        .map(|s| s.model_id)
        .collect();
    info!(target: "serve", plan = ?parts.plan, resident = ?resident_models, "reload: provider rebuilt and swapped");
    Json(EngineReloaded { resident_models }).into_response()
}

fn refused(why: String) -> Response {
    warn!(target: "serve", reason = %why, "reload refused; the previous provider keeps serving");
    openai_refusal(
        StatusCode::SERVICE_UNAVAILABLE,
        format!("reload: {why}"),
        "reload_failed",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_compute::mock::{MockProvider, MOCK_ENGINE, MOCK_MODEL};
    use sovereign_contracts::model_family::ModelFamily;

    /// A cell over a mock, the reload route on a free loopback port, and the
    /// config file it re-reads.
    async fn serving(
        config_text: Option<&str>,
    ) -> (
        Arc<ReloadableProvider>,
        Arc<dyn InferenceProvider>,
        String,
        tempfile::TempDir,
    ) {
        let _ = sovereign_inference::engine_factory::register_engine(
            MOCK_ENGINE,
            Arc::new(sovereign_compute::mock::MockEngine),
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        if let Some(text) = config_text {
            std::fs::write(&config_path, text).expect("write config");
        }
        let before: Arc<dyn InferenceProvider> = Arc::new(MockProvider {
            tokens: 1,
            delay: std::time::Duration::ZERO,
        });
        let cell = Arc::new(ReloadableProvider::new(
            Arc::clone(&before),
            ModelFamily::Unknown,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let routes = vec![bundle(Arc::clone(&cell), Arc::default(), config_path)];
        tokio::spawn(host_kit::shell::serve(
            [listener],
            routes,
            std::future::pending(),
        ));
        (cell, before, base, dir)
    }

    #[tokio::test]
    async fn a_reload_rebuilds_through_the_assembly_and_swaps_the_cell() {
        let config = "[engine]\nkind = \"mock\"\n\n[models]\nprimary = \"/nonexistent/mock.gguf\"\nembed = \"/nonexistent/mock-embed.gguf\"\n";
        let (cell, before, base, _dir) = serving(Some(config)).await;
        let resp = reqwest::Client::new()
            .post(format!("{base}{RELOAD_PATH}"))
            .send()
            .await
            .expect("reload answered");
        assert_eq!(resp.status(), 200, "{:?}", resp.text().await);
        let body: EngineReloaded = resp.json().await.expect("body");
        assert_eq!(body.resident_models, vec![MOCK_MODEL.to_string()]);
        assert!(
            !Arc::ptr_eq(&cell.current(), &before),
            "the cell still holds the pre-reload provider"
        );
    }

    #[tokio::test]
    async fn a_reload_that_cannot_read_its_config_refuses_and_keeps_serving() {
        let (cell, before, base, _dir) = serving(None).await;
        let resp = reqwest::Client::new()
            .post(format!("{base}{RELOAD_PATH}"))
            .send()
            .await
            .expect("reload answered");
        assert_eq!(resp.status(), 503);
        assert!(resp
            .text()
            .await
            .unwrap_or_default()
            .contains("cannot read"));
        assert!(
            Arc::ptr_eq(&cell.current(), &before),
            "a refused reload swapped the provider"
        );
    }
}
