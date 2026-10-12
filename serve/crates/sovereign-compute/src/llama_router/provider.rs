// SPDX-License-Identifier: AGPL-3.0-or-later
//! The provider a `kind = "llama-server"` node serves: the remote half built
//! against the live router, forwarding every method, with the answers only
//! the router's owner can give: whose weights these are, the router's own
//! lifecycle for `/status`, and the host facts the remote half could only
//! assume (each model's residency, the code model, the trained context;
//! svrn-docs ENGINE_SWAP_PLAN.md A1).

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use sovereign_contracts::traits::{InferenceProvider, ResidentSlot, ServingLocus};
use sovereign_contracts::types::*;
use sovereign_contracts::{oicp, Result};

use super::live::LiveRouter;
use super::RouterPreset;

/// The remote half, the router it talks to, and the preset this build asked
/// it to serve.
pub struct RouterProvider {
    inner: Arc<dyn InferenceProvider>,
    router: Arc<LiveRouter>,
    preset: RouterPreset,
    n_ctx_train: Option<u32>,
}

impl RouterProvider {
    /// Wrap `inner`, the remote provider built from `preset`'s section of
    /// `router`. Reads the primary GGUF's trained context from its header;
    /// one that cannot be read is reported as unknown, never guessed.
    pub fn new(
        inner: Arc<dyn InferenceProvider>,
        router: Arc<LiveRouter>,
        preset: RouterPreset,
    ) -> Self {
        let primary = preset.primary().to_string();
        let n_ctx_train = match preset.path_of(&primary) {
            Some(path) => match sovereign_inference::gguf_meta::read_context_length(path) {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(target: "llama_router", model = %primary, error = %e, "n_ctx_train unknown: the GGUF header is unreadable");
                    None
                }
            },
            None => None,
        };
        tracing::debug!(target: "llama_router", model = %primary, ?n_ctx_train, "router provider: primary's trained context");
        Self {
            inner,
            router,
            preset,
            n_ctx_train,
        }
    }

    /// The router loads only the models its preset names, and the slots
    /// `/internal/models/load` manages are GGUFs outside it; the inner
    /// refusal would say this node holds no weights, which is not so here.
    fn extras_refusal(&self, verb: &str, slot: &str) -> sovereign_contracts::Error {
        tracing::debug!(target: "llama_router", verb, slot, "router provider: extra slot refused");
        sovereign_contracts::Error::Inference(format!(
            "runtime slot {verb} of {slot:?} is refused: the llama-server router serves only its \
             preset ({}); add the GGUF to [models] and restart the daemon",
            self.preset.warm_models().join(", ")
        ))
    }

    fn model_for(&self, role: &str) -> Option<&str> {
        self.preset
            .roles()
            .into_iter()
            .find(|(r, _)| *r == role)
            .map(|(_, m)| m)
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

    /// The primary GGUF's `<arch>.context_length`, read at build.
    fn n_ctx_train_for_primary(&self) -> Option<u32> {
        self.n_ctx_train
    }

    /// An estimate: the remote half's chars/4. The exact count is
    /// [`Self::count_tokens_exact`], which asks the router.
    fn count_tokens(&self, text: &str) -> u32 {
        self.inner.count_tokens(text)
    }

    /// `POST /tokenize` on the router, with the model a fast turn goes to
    /// (the fast model, else the primary), as the embedded engine counts on
    /// its fast slot.
    async fn count_tokens_exact(&self, text: &str) -> Result<u32> {
        let model = self
            .model_for("fast")
            .unwrap_or_else(|| self.preset.primary());
        self.router
            .token_count(model, text)
            .await
            .map_err(sovereign_contracts::Error::Inference)
    }

    /// The preset's `[models] code` entry, which the router serves by name.
    fn code_model_id(&self) -> Option<String> {
        self.model_for("code").map(str::to_string)
    }

    fn edit_slot_info(&self) -> Option<EditSlotInfo> {
        self.inner.edit_slot_info()
    }

    /// Load the primary through the router's `POST /models/load` and wait
    /// for `loaded`; a load that fails is an error, never Ok.
    async fn warmup_primary(&self) -> Result<()> {
        let primary = self.preset.primary();
        let result = self.router.load(primary).await;
        tracing::debug!(target: "llama_router", model = primary, ok = result.is_ok(), "router provider: warmup");
        result.map_err(sovereign_contracts::Error::Inference)
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
        let _ = (path, context_size);
        Err(self.extras_refusal("load", &slot_name))
    }

    fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>> {
        Err(self.extras_refusal("unload", slot_name))
    }

    fn extras_inventory(&self) -> Vec<(String, String)> {
        self.inner.extras_inventory()
    }

    /// Every role the preset fills, embed and rerank included, resident as
    /// the router last listed its model in `GET /models`: `loaded` is
    /// resident, `loading` is transitioning, anything else (or no listing
    /// yet) is not resident. Sizes are not reported by the router.
    fn resident_slots(&self) -> Vec<ResidentSlot> {
        self.preset
            .roles()
            .into_iter()
            .map(|(role, model)| {
                let status = self.router.model_status(model);
                ResidentSlot {
                    role: role.to_string(),
                    model_id: model.to_string(),
                    resident: status.as_deref() == Some("loaded"),
                    size_bytes: None,
                    transitioning: status.as_deref() == Some("loading"),
                    placement: None,
                }
            })
            .collect()
    }

    fn decode_evidence(&self) -> Vec<oicp::SlotDecodeEvidence> {
        self.inner.decode_evidence()
    }

    async fn primary_slot_status(&self) -> Option<ResidentSlot> {
        self.resident_slots()
            .into_iter()
            .find(|s| s.role == "primary")
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
    /// The host facts the router answers itself instead of the remote half
    /// (ENGINE_SWAP_PLAN A1, A5), in trait order.
    const ANSWERED_HERE: [&str; 7] = [
        "n_ctx_train_for_primary",
        "count_tokens_exact",
        "code_model_id",
        "warmup_primary",
        "load_extra_slot",
        "unload_extra_slot",
        "primary_slot_status",
    ];

    #[test]
    fn the_router_provider_forwards_every_method_it_does_not_answer_itself() {
        let unforwarded = sovereign_contracts::reloadable_provider::unforwarded_methods(
            include_str!("provider.rs"),
            "impl InferenceProvider for RouterProvider",
        );
        assert_eq!(
            unforwarded, ANSWERED_HERE,
            "RouterProvider leaves a default in place, or forwards a fact it should answer"
        );
    }
}
