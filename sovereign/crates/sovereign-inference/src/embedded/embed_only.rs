// SPDX-License-Identifier: AGPL-3.0-or-later
//! Embed-only `InferenceProvider` — loads ONLY an embedding slot, no chat
//! models. Purpose-built for `sovereign router-cache rebuild`: it drives
//! `build_llm_router` (which only ever calls `embed_query`/`embed`) against the
//! prescribed embed model, so the regenerated cache is byte-identical to what
//! the shipped runtime produces — same `EmbedSlot`, same quirks, same code
//! path. `complete*` are unsupported and error: routing never runs here.

use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;

use sovereign_core::error::{Error, Result};
use sovereign_core::model_family::ModelFamily;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
};

use crate::hardware::HardwareProfile;
use crate::llama::cpp::llama_backend::LlamaBackend;

use super::EmbedSlot;

/// An `InferenceProvider` that loads only an embedding slot.
pub struct EmbedOnlyProvider {
    slot: Arc<EmbedSlot>,
    /// Held for the lifetime of the slot — its contexts hold raw pointers into
    /// backend state, so the backend must outlive them.
    _backend: Arc<LlamaBackend>,
}

impl EmbedOnlyProvider {
    /// Load only the embed slot for `embed_model_path` with `embed_family`'s
    /// quirks. When `embed_family` is `Unknown`, `EmbedSlot::load` still
    /// gguf-auto-detects Qwen3-Embedding — so a correctly-prescribed model
    /// under any filename gets the right prefix/EOS here too.
    pub fn load(embed_model_path: &Path, embed_family: ModelFamily) -> Result<Self> {
        // Tolerates an already-initialized process-global backend — this
        // provider is routinely a SECOND slot in a process that already
        // has one (see `llama::shared_backend`).
        let backend = crate::llama::shared_backend().map_err(Error::Inference)?;
        let n_gpu_layers = HardwareProfile::detect().recommended_gpu_layers;
        let embed_quirks = embed_family.default_quirks().embed;
        let slot = EmbedSlot::load(&backend, embed_model_path, n_gpu_layers, embed_quirks)?;
        Ok(Self {
            slot: Arc::new(slot),
            _backend: backend,
        })
    }
}

#[async_trait]
impl InferenceProvider for EmbedOnlyProvider {
    async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
        Err(Error::NotImplemented(
            "EmbedOnlyProvider supports embeddings only (no text completion)".into(),
        ))
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented(
            "EmbedOnlyProvider supports embeddings only (no streaming)".into(),
        ))
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let slot = Arc::clone(&self.slot);
        let text = text.to_string();
        tokio::task::spawn_blocking(move || {
            let mut ctx_lock = slot.contexts[0].blocking_lock();
            EmbedSlot::embed_sync(
                &slot.model,
                &mut ctx_lock.ctx,
                &text,
                slot.n_embd,
                slot.max_input_tokens,
                slot.embed_quirks.as_ref(),
            )
        })
        .await
        .map_err(|e| Error::Inference(format!("embed task failed: {e}")))?
    }

    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let slot = Arc::clone(&self.slot);
        let query = query.to_string();
        tokio::task::spawn_blocking(move || {
            let mut ctx_lock = slot.contexts[0].blocking_lock();
            EmbedSlot::embed_query_sync(
                &slot.model,
                &mut ctx_lock.ctx,
                &query,
                slot.n_embd,
                slot.max_input_tokens,
                slot.embed_quirks.as_ref(),
            )
        })
        .await
        .map_err(|e| Error::Inference(format!("embed task failed: {e}")))?
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 0,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}
