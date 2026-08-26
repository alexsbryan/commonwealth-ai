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

/// What [`load_from_env`] decided about the rerank slot.
///
/// Four arms rather than an `Option`, because the three ways to end up without
/// a reranker are not the same fact and a caller has different things to say
/// about each (ARCH §18.3 — absence is reported, never defaulted). "Nobody
/// asked for one" and "one was asked for and refused because it does not fit"
/// read identically through an `Option`, and the second is the one an operator
/// has to see.
pub enum RerankLoad {
    /// `SOVEREIGN_RERANK_MODEL_PATH` is unset. A reranker is opt-in.
    NotConfigured,
    /// Loaded and ready.
    Loaded(Arc<dyn InferenceProvider>),
    /// **Refused before allocating.** The pre-flight said the slot does not
    /// fit, so the allocation never happened — the point of a pre-flight gate.
    Refused {
        /// The full capacity report, for an operator to act on.
        message: String,
    },
    /// Configured, attempted, and the load itself failed.
    Failed {
        /// Why.
        message: String,
    },
}

impl RerankLoad {
    /// The provider, if there is one. For a caller that genuinely has nothing
    /// different to do about the three absences.
    pub fn provider(self) -> Option<Arc<dyn InferenceProvider>> {
        match self {
            Self::Loaded(p) => Some(p),
            _ => None,
        }
    }
}

/// Load the reranker named by `SOVEREIGN_RERANK_MODEL_PATH`, if any — and
/// refuse it up front when it does not fit.
///
/// **One loader for every shipping surface.** The CLI, the desktop and the hub
/// server all reach the reranker through here (ARCH §10.6).
///
/// That sentence was in this doc comment before 2026-08-25 and was FALSE: the
/// CLI never called this function. It carried its own load path in
/// `chat_cmd/bootstrap.rs` — and that path had the capacity pre-flight this
/// one did not, so of the three shipping surfaces exactly one honoured note
/// `b57b0cd5`, the 64 GB SIGTERM incident whose whole lesson is that a slot
/// which does not fit must be refused UP FRONT rather than discovered by the
/// OOM killer mid-turn. A comment asserting a mirror that is not one is
/// ARCH §7.2's smell, and this is what it cost: the desktop and the hub could
/// each load a rerank slot alongside a resident primary with nothing checking
/// whether the two fit together.
///
/// The pre-flight is now HERE, so all three inherit it from one place.
/// `SOVEREIGN_SKIP_VRAM_CHECK` still overrides it — loudly, as before.
///
/// Soft-fail by contract: none of the three absences may block startup —
/// retrieval simply runs the baseline fusion path. Each is logged and each is
/// a distinct arm of [`RerankLoad`], so a caller with a banner can say which
/// one happened.
pub fn load_from_env() -> RerankLoad {
    let Ok(raw) = std::env::var("SOVEREIGN_RERANK_MODEL_PATH") else {
        return RerankLoad::NotConfigured;
    };
    let path = std::path::PathBuf::from(&raw);

    // NATIVE_GROUNDING.md §8 residency plan — the fit check BEFORE the slot
    // loads. The rerank slot is process-local additional weight alongside
    // whatever primary is already resident.
    let hw = crate::hardware::HardwareProfile::detect();
    let plan = [crate::capacity::SlotPlan {
        role: "rerank".into(),
        path: path.clone(),
        // Matches the pool H1 scores in one batch (k <= 8) and the reranker's
        // own batch shape during retrieval.
        n_seq_max: 8,
        n_ctx: 8192,
    }];
    let report = crate::capacity::check_fit(&plan, &hw);
    if !report.fits {
        if crate::capacity::check_skipped_by_env() {
            tracing::warn!(
                path = %path.display(),
                "rerank capacity check FAILED but is disabled by \
                 SOVEREIGN_SKIP_VRAM_CHECK — loading as instructed"
            );
        } else {
            let message = format!(
                "the rerank slot does not fit ({} MiB required, {} MiB \
                 available after {} MiB reserved). Running baseline retrieval; \
                 native grounding will report no instrument.\n{}",
                report.total_required_mb,
                report.available_mb,
                report.safety_reserved_mb,
                report.refuse_message()
            );
            tracing::warn!(path = %path.display(), "{message}");
            // The allocation never happens — that is the point of the gate.
            return RerankLoad::Refused { message };
        }
    }

    match StandaloneReranker::load(&path, ModelFamily::Reranker, None) {
        Ok(reranker) => {
            tracing::info!(
                path = %path.display(),
                "reranker loaded from SOVEREIGN_RERANK_MODEL_PATH"
            );
            RerankLoad::Loaded(Arc::new(reranker) as Arc<dyn InferenceProvider>)
        }
        Err(e) => {
            let message = format!(
                "SOVEREIGN_RERANK_MODEL_PATH is set but the reranker failed to \
                 load ({e}) — running baseline retrieval without rerank"
            );
            tracing::warn!(path = %path.display(), "{message}");
            RerankLoad::Failed { message }
        }
    }
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
                crate::llama_logs::LlamaLogs::from_env().install(&mut b);
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

    /// Reference-path scoring (see [`RerankSlot::score_sequential`]):
    /// fully independent per-pair decode, no shared-prefix reuse and no
    /// multi-sequence batching. Exists so the `rerank_batch_check`
    /// example can hold the production `rerank_batch` against a
    /// machinery-free oracle — a correct batched decode of independent
    /// sequences must reproduce these scores.
    pub async fn rerank_sequential(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        let slot = Arc::clone(&self.slot);
        let query = query.to_string();
        let docs = docs.to_vec();
        tokio::task::spawn_blocking(move || slot.score_sequential(&query, &docs))
            .await
            .map_err(|e| Error::Inference(format!("Rerank seq task failed: {e}")))?
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
