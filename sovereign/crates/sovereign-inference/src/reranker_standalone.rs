// SPDX-License-Identifier: AGPL-3.0-or-later
//! Standalone cross-encoder reranker — wires a `RerankSlot` into an
//! `InferenceProvider` without touching the chat/embed/code slots.
//!
//! Why this exists
//! ---------------
//! The daemon-attached chat path (`sovereign chat`, `sovereign eval
//! run`) talks to the long-running daemon over HTTP for chat +
//! embeddings via `SplitInferenceProvider`. That provider doesn't
//! implement `rerank_batch` — it's a network shim. But the Runtime
//! fans out to `CorpusIndex::search_with_rerank` in the *local* eval
//! process, so the reranker has to be locally callable there.
//!
//! `StandaloneReranker` is the minimal box for that: load one
//! reranker GGUF directly into a `RerankSlot`, expose it as an
//! `InferenceProvider` whose only meaningful method is
//! `rerank_batch`. No chat slot, no embed slot, no host machinery
//! loaded.
//!
//! Pairs with `sovereign_tools::corpus::inference_to_rerank_fn` —
//! that wrapper is provider-agnostic, so the same closure type
//! plumbs through the runtime whether the reranker lives inside
//! `EmbeddedLlamaCpp` (daemon-local) or `StandaloneReranker`
//! (eval-local).

use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use crate::llama::cpp::llama_backend::LlamaBackend;
use async_trait::async_trait;
use futures::Stream;

use sovereign_core::error::{Error, Result};
use sovereign_core::model_family::ModelFamily;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
};

use crate::embedded::RerankSlot;

/// An `InferenceProvider` whose only implemented method is
/// `rerank_batch`. Holds a dedicated `LlamaBackend` + `RerankSlot`
/// directly — no chat/embed slots loaded.
///
/// Constructed via [`load`]. The created backend is independent of
/// any other `EmbeddedLlamaCpp` instance in the same process, so
/// you can run this alongside a daemon-backed `SplitInferenceProvider`
/// without coupling backends.
pub struct StandaloneReranker {
    // Held to keep the backend alive for the lifetime of the slot.
    _backend: Arc<LlamaBackend>,
    slot: Arc<RerankSlot>,
}

impl StandaloneReranker {
    /// Load a reranker GGUF and wrap it in an `InferenceProvider`.
    ///
    /// `family` selects the rerank quirks (default `Reranker` works
    /// for jina-reranker-v3, bge-reranker-v2-m3, and other BERT/
    /// Qwen-based cross-encoders that support `pooling_type = Rank`).
    /// `gpu_layers = None` → auto-detect (`999` for full offload on
    /// every GPU platform we support, `0` otherwise).
    pub fn load(
        reranker_path: &Path,
        family: ModelFamily,
        gpu_layers: Option<u32>,
    ) -> Result<Self> {
        let backend = match LlamaBackend::init() {
            Ok(mut b) => {
                // Honour the same llama.cpp-log-suppression policy the
                // host daemon uses — SOVEREIGN_LLAMA_LOGS=1 to see ggml
                // output.
                if std::env::var("SOVEREIGN_LLAMA_LOGS").ok().as_deref() != Some("1") {
                    b.void_logs();
                }
                Arc::new(b)
            }
            Err(crate::llama::cpp::LLamaCppError::BackendAlreadyInitialized) => {
                // Another in-process component (router classifiers,
                // an embedded engine) already owns the process-global
                // backend — `LlamaBackend` is only a zero-sized proof
                // of initialization, so piggyback on it with a shadow
                // handle. The Arc is deliberately leaked (strong count
                // pinned above zero) so OUR handle can never run
                // `Drop` and free the backend out from under the real
                // owner; the shadow is inert for the process lifetime.
                let shadow = Arc::new(LlamaBackend {});
                std::mem::forget(Arc::clone(&shadow));
                shadow
            }
            Err(e) => {
                return Err(Error::Inference(format!("init backend: {e}")));
            }
        };
        let rerank_quirks = family.default_quirks().rerank;
        let slot = RerankSlot::load(
            &backend,
            reranker_path,
            gpu_layers.unwrap_or(0),
            rerank_quirks,
        )?;
        Ok(Self {
            _backend: backend,
            slot: Arc::new(slot),
        })
    }
}

#[async_trait]
impl InferenceProvider for StandaloneReranker {
    async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
        Err(Error::NotImplemented(
            "StandaloneReranker has no chat capability".to_string(),
        ))
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented(
            "StandaloneReranker has no chat capability".to_string(),
        ))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Err(Error::NotImplemented(
            "StandaloneReranker has no embed capability".to_string(),
        ))
    }

    async fn rerank_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        let slot = Arc::clone(&self.slot);
        let query = query.to_string();
        let docs = docs.to_vec();
        tokio::task::spawn_blocking(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                slot.score_batch(&query, &docs)
            }));
            match result {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(Error::Inference(
                    "Standalone rerank inference panicked".to_string(),
                )),
            }
        })
        .await
        .map_err(|e| Error::Inference(format!("Rerank task failed: {e}")))?
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 8192,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}
