// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hot reload in serve: the provider every route answers from sits in one
//! cell, and [`RELOAD_PATH`] rebuilds it from the config on disk through the
//! same `ReloadFactory` cold start assembled with (pb-serving-assembly), then
//! swaps it. In-flight requests finish on the provider they cloned; new ones
//! see the new one. The svrn daemon's `/v1/admin/reload` forwards here when
//! it dials serve (pb-svrn-dials-serve).

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use futures::Stream;
use host_kit::shell::guard::LocalOnly;
use host_kit::shell::RouteBundle;
use sovereign_compute::assembly::ReloadFactory;
use sovereign_compute::server::openai_refusal;
use sovereign_contracts::engine_state::{EngineReloaded, RELOAD_PATH};
use sovereign_contracts::error::Result;
use sovereign_contracts::model_family::ModelFamily;
use sovereign_contracts::setup_config::SetupConfig;
use sovereign_contracts::*;
use tracing::{info, warn};

/// The provider serve's routes answer from, swappable by a reload.
pub struct ReloadableProvider {
    current: RwLock<Arc<dyn InferenceProvider>>,
    /// The embed slot's family as the assembly resolved it, swapped with the
    /// provider: it decides the query-instruction prefix a client applies.
    embed_family: RwLock<ModelFamily>,
}

impl ReloadableProvider {
    /// A cell holding the provider cold start built.
    pub fn new(provider: Arc<dyn InferenceProvider>, embed_family: ModelFamily) -> Self {
        Self {
            current: RwLock::new(provider),
            embed_family: RwLock::new(embed_family),
        }
    }

    /// The embed family of the provider this cell holds now.
    pub fn embed_family(&self) -> ModelFamily {
        self.embed_family
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The provider this request runs on. The guard is only ever held for a
    /// clone, so a poisoned lock is recovered rather than panicked on.
    fn current(&self) -> Arc<dyn InferenceProvider> {
        Arc::clone(&self.current.read().unwrap_or_else(|p| p.into_inner()))
    }

    fn swap(&self, provider: Arc<dyn InferenceProvider>, embed_family: ModelFamily) {
        *self.current.write().unwrap_or_else(|p| p.into_inner()) = provider;
        *self.embed_family.write().unwrap_or_else(|p| p.into_inner()) = embed_family;
    }
}

/// Every method delegates, defaults included: a default left in place here
/// would answer for the wrapper instead of the engine behind it.
#[async_trait]
impl InferenceProvider for ReloadableProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        self.current().complete(request).await
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        self.current().complete_stream(request).await
    }

    async fn complete_stream_with_id(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = Result<String>> + Send>>, String)> {
        self.current().complete_stream_with_id(request).await
    }

    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        self.current().complete_stream_with_finish(request).await
    }

    async fn complete_stream_with_id_and_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, String)> {
        self.current()
            .complete_stream_with_id_and_finish(request)
            .await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.current().embed(text).await
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.current().embed_batch(texts).await
    }

    async fn complete_batch(
        &self,
        requests: &[CompletionRequest],
    ) -> Result<Vec<CompletionResponse>> {
        self.current().complete_batch(requests).await
    }

    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.current().embed_query(query).await
    }

    async fn rerank_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        self.current().rerank_batch(query, docs).await
    }

    fn model_id_for(&self, speed: Speed) -> String {
        self.current().model_id_for(speed)
    }

    fn embed_model_id(&self) -> String {
        self.current().embed_model_id()
    }

    fn serving_locus(&self) -> ServingLocus {
        self.current().serving_locus()
    }

    fn effective_context_size(&self) -> Option<u32> {
        self.current().effective_context_size()
    }

    fn n_ctx_train_for_primary(&self) -> Option<u32> {
        self.current().n_ctx_train_for_primary()
    }

    fn count_tokens(&self, text: &str) -> u32 {
        self.current().count_tokens(text)
    }

    fn code_model_id(&self) -> Option<String> {
        self.current().code_model_id()
    }

    fn edit_slot_info(&self) -> Option<EditSlotInfo> {
        self.current().edit_slot_info()
    }

    async fn warmup_primary(&self) -> Result<()> {
        self.current().warmup_primary().await
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.current().capabilities()
    }

    fn load_extra_slot(
        &self,
        slot_name: String,
        path: std::path::PathBuf,
        context_size: u32,
    ) -> Result<String> {
        self.current()
            .load_extra_slot(slot_name, path, context_size)
    }

    fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>> {
        self.current().unload_extra_slot(slot_name)
    }

    fn extras_inventory(&self) -> Vec<(String, String)> {
        self.current().extras_inventory()
    }

    fn resident_slots(&self) -> Vec<ResidentSlot> {
        self.current().resident_slots()
    }

    fn decode_evidence(&self) -> Vec<oicp::SlotDecodeEvidence> {
        self.current().decode_evidence()
    }

    async fn primary_slot_status(&self) -> Option<ResidentSlot> {
        self.current().primary_slot_status().await
    }

    fn compute_children(&self) -> Vec<ComputeChildStatus> {
        self.current().compute_children()
    }

    async fn peer_manifests(&self) -> Vec<(String, oicp::ProviderManifest)> {
        self.current().peer_manifests().await
    }
}

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
