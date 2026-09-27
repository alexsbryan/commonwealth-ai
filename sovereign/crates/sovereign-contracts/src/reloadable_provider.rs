// SPDX-License-Identifier: AGPL-3.0-or-later
//! The swappable provider cell: serve's routes answer from it, and on the
//! dialing path the svrn daemon's readers share one, so a reload is seen by
//! every reader at once (pb-svrn-dials-serve, phase-b-28).

use std::pin::Pin;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use futures::Stream;

use crate::error::Result;
use crate::model_family::ModelFamily;
use crate::*;

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
    pub fn current(&self) -> Arc<dyn InferenceProvider> {
        Arc::clone(&self.current.read().unwrap_or_else(|p| p.into_inner()))
    }

    /// Store a rebuilt provider and its embed family: in-flight requests
    /// finish on the provider they cloned, new ones see this one.
    pub fn swap(&self, provider: Arc<dyn InferenceProvider>, embed_family: ModelFamily) {
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
