// SPDX-License-Identifier: AGPL-3.0-or-later
//! The provider a `kind = "llama-server"` node serves: the remote half built
//! against the live router, forwarding every method, with the two answers
//! only the router's owner can give — whose weights these are, and the
//! router's own lifecycle for `/status`.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use sovereign_contracts::traits::{InferenceProvider, ResidentSlot, ServingLocus};
use sovereign_contracts::types::*;
use sovereign_contracts::{oicp, Result};

use super::live::LiveRouter;

/// The remote half, and the router it talks to.
pub struct RouterProvider {
    inner: Arc<dyn InferenceProvider>,
    router: Arc<LiveRouter>,
}

impl RouterProvider {
    /// Wrap `inner`, the remote provider built from the router's section.
    pub fn new(inner: Arc<dyn InferenceProvider>, router: Arc<LiveRouter>) -> Self {
        Self { inner, router }
    }
}

/// Every method forwards, defaults included: a default left in place here
/// would answer for the wrapper instead of the engine behind it.
#[async_trait]
impl InferenceProvider for RouterProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        self.inner.complete(request).await
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        self.inner.complete_stream(request).await
    }

    async fn complete_stream_with_id(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = Result<String>> + Send>>, String)> {
        self.inner.complete_stream_with_id(request).await
    }

    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        self.inner.complete_stream_with_finish(request).await
    }

    async fn complete_stream_with_id_and_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, String)> {
        self.inner.complete_stream_with_id_and_finish(request).await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.inner.embed(text).await
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.inner.embed_batch(texts).await
    }

    async fn complete_batch(
        &self,
        requests: &[CompletionRequest],
    ) -> Result<Vec<CompletionResponse>> {
        self.inner.complete_batch(requests).await
    }

    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.inner.embed_query(query).await
    }

    async fn rerank_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        self.inner.rerank_batch(query, docs).await
    }

    fn model_id_for(&self, speed: Speed) -> String {
        self.inner.model_id_for(speed)
    }

    fn embed_model_id(&self) -> String {
        self.inner.embed_model_id()
    }

    /// The remote half sees a loopback server and says it forwards on the
    /// box; that server is this process's own supervised child, so the
    /// weights are this node's (decided 2026-10-10, host.serving-locus).
    fn serving_locus(&self) -> ServingLocus {
        match self.inner.serving_locus() {
            ServingLocus::ForwardsOnBox => ServingLocus::OwnWeights,
            other => other,
        }
    }

    fn effective_context_size(&self) -> Option<u32> {
        self.inner.effective_context_size()
    }

    fn n_ctx_train_for_primary(&self) -> Option<u32> {
        self.inner.n_ctx_train_for_primary()
    }

    fn count_tokens(&self, text: &str) -> u32 {
        self.inner.count_tokens(text)
    }

    fn code_model_id(&self) -> Option<String> {
        self.inner.code_model_id()
    }

    fn edit_slot_info(&self) -> Option<EditSlotInfo> {
        self.inner.edit_slot_info()
    }

    async fn warmup_primary(&self) -> Result<()> {
        self.inner.warmup_primary().await
    }

    async fn probe_logprobs(
        &self,
        probe: &sovereign_contracts::engine_observe::ProbeRequest,
    ) -> Result<sovereign_contracts::engine_observe::ProbeAnswer> {
        self.inner.probe_logprobs(probe).await
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }

    fn load_extra_slot(
        &self,
        slot_name: String,
        path: std::path::PathBuf,
        context_size: u32,
    ) -> Result<String> {
        self.inner.load_extra_slot(slot_name, path, context_size)
    }

    fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>> {
        self.inner.unload_extra_slot(slot_name)
    }

    fn extras_inventory(&self) -> Vec<(String, String)> {
        self.inner.extras_inventory()
    }

    fn resident_slots(&self) -> Vec<ResidentSlot> {
        self.inner.resident_slots()
    }

    fn decode_evidence(&self) -> Vec<oicp::SlotDecodeEvidence> {
        self.inner.decode_evidence()
    }

    async fn primary_slot_status(&self) -> Option<ResidentSlot> {
        self.inner.primary_slot_status().await
    }

    /// The router and each of its models, after whatever the inner
    /// provider reports.
    fn compute_children(&self) -> Vec<oicp::ComputeChildStatus> {
        let mut out = self.inner.compute_children();
        out.extend(self.router.statuses());
        out
    }

    async fn peer_manifests(&self) -> Vec<(String, oicp::ProviderManifest)> {
        self.inner.peer_manifests().await
    }

    async fn lender_manifest(&self) -> Option<(String, Vec<String>)> {
        self.inner.lender_manifest().await
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_router_provider_forwards_every_inference_provider_method() {
        let missing = sovereign_contracts::reloadable_provider::unforwarded_methods(
            include_str!("provider.rs"),
            "impl InferenceProvider for RouterProvider",
        );
        assert!(
            missing.is_empty(),
            "RouterProvider does not forward: {missing:?}"
        );
    }
}
