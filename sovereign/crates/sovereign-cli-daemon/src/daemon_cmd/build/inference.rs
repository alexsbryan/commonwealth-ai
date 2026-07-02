//! Inference provider construction — extracted from `run_daemon` (§3.3).
//!
//! Builds the embedded llama.cpp provider from the three GGUF slots.
//! Synchronous — load happens inline; model files are mmapped so
//! cold-start latency is dominated by disk I/O on first reference.
//!
//! **Family resolution.** The embed slot's family identity decides its
//! app-side pooling strategy, normalisation, and the document / query
//! instruction prefixes via `EmbedQuirks`. After the llama-cpp-4 0.2.x
//! migration the C-side pooling type is forced to `None` in
//! `EmbedSlot::load` (the binding returns null from `embeddings_seq_ith`
//! on every gguf whose header says NONE, and setting any other type
//! ggml_aborts the context constructor for Qwen3-Embedding); pooling
//! moved into Rust against the per-token `embeddings_ith` reads. The
//! family lookup is therefore what selects the right strategy (Last for
//! Qwen3-Embedding, Mean for BERT-style) and the right text prep on the
//! input — keeping it resolved here means the slot loader and the
//! mesh-advertisement path read from a single source of truth.

use std::path::PathBuf;
use std::sync::Arc;

use sovereign_core::model_family::ModelFamily;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::embedded::EmbeddedLlamaCpp;

/// Returns `(provider, engine, embed_family)` on success, or `Err(())`
/// when a slot fails to load or configure (the caller returns 1; the
/// operator-facing diagnostics are printed here).
///
/// - `provider` — the `dyn`-erased view the daemon installs + advertises.
/// - `engine` — the same object kept concrete, captured for the
///   RPC-worker auto-reload path (the mesh discovery task force-reloads
///   the primary when the worker set grows).
/// - `embed_family` — the manifest-resolved embed slot family; drives
///   app-side pooling + the mesh advertisement's embed-model info.
pub(crate) fn load_provider(
    config: &SetupConfig,
) -> Result<
    (
        Arc<dyn InferenceProvider>,
        Arc<EmbeddedLlamaCpp>,
        ModelFamily,
    ),
    (),
> {
    let resolved_embed_family = config
        .models
        .embed
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(|name| {
            sovereign_core::models_manifest::DEFAULT_MANIFEST.embed_family_for_file(name)
        })
        .unwrap_or(ModelFamily::Unknown);

    let arc = match EmbeddedLlamaCpp::load_full_with_families(
        config.models.fast_path(),
        Some(&config.models.primary),
        Some(&config.models.embed),
        // PR-E2: optional Code specialist. When set, `code`-hinted
        // requests hot-swap into the lazy slot (shared with primary).
        // None = pre-E2 two-slot behaviour — all substantive work on
        // the Main responder.
        config.models.code.as_deref(),
        // context_size — sourced from `[models].context_size` so a batch
        // host (Strix Halo, 128 GB unified) can opt into 32k without
        // touching code, while a 64 GB Mac stays at the safe 16384
        // default. 16384 halves KV to ~8 GB and keeps headroom on a Mac;
        // the atlas Phase 1 pipeline benefits from 32k on Strix Halo.
        config.models.effective_context_size(),
        None, // gpu_layers — auto-detect
        ModelFamily::Unknown,
        ModelFamily::Unknown,
        // Manifest-resolved embed family: drives app-side pooling +
        // document/query instruction prefixes. C-side pooling is fixed to
        // `None` inside `EmbedSlot::load`, so a non-Unknown family here no
        // longer triggers the ggml_abort that motivated the earlier
        // hard-coded `Unknown`.
        resolved_embed_family.clone(),
        // code slot is Qwen3-Coder-30B-A3B-Instruct (the only code GGUF we
        // ship today). Pinning the family to Qwen3 picks up Qwen's
        // recommended sampling defaults — top_k=20, top_p=0.95,
        // presence_penalty=1.5 — and the SystemPromptToken thinking
        // control; the Unknown defaults left the sampler too permissive on
        // long Rust emissions (the `f3 2` / `Lat encyClass` char-drop).
        ModelFamily::Qwen3,
    ) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            eprintln!("error: failed to load models: {e}");
            eprintln!(
                "hint: verify paths in {}",
                SetupConfig::default_path().display()
            );
            return Err(());
        }
    };

    // Wire the optional LRU memory budget BEFORE installing extras. With a
    // budget set, each `load_extra` call (including the eager startup loads
    // from `[models.extra]`) checks against it and evicts cold slots if
    // needed. Without a budget, eviction is disabled and slots persist.
    if let Err(e) = arc.set_extras_memory_budget(config.models.max_extras_memory_bytes()) {
        eprintln!("error: failed to set extras memory budget: {e}");
        return Err(());
    }
    // Idle-unload monitor for extras slots. Default 0 = disabled.
    arc.start_extras_idle_monitor(config.daemon.extras_idle_secs);
    // Operator-declared additional chat slots. Each `[models.extra]` entry
    // is loaded eagerly here; failures fail the daemon. Routing kicks in
    // when `/v1/chat/completions` arrives with a matching `model` field.
    if !config.models.extra.is_empty() {
        if let Err(e) = arc.install_extras(
            config.models.extra.clone(),
            config.models.effective_context_size(),
        ) {
            eprintln!("error: failed to install extras slots: {e}");
            return Err(());
        }
    }
    // Sourced from `[daemon].primary_idle_secs`. Default 300s suits a
    // desktop; batch workloads (atlas enrich) want 1800+ to skip the
    // 3–4 s reload tax between back-to-back short LLM calls.
    arc.start_idle_monitor(config.daemon.primary_idle_secs);
    // Optional cross-encoder reranker from `SOVEREIGN_RERANK_MODEL_PATH`.
    // Soft-fail: a missing/broken reranker file must not block startup —
    // retrieval simply runs the baseline path.
    if let Ok(rerank_path) = std::env::var("SOVEREIGN_RERANK_MODEL_PATH") {
        let path = PathBuf::from(&rerank_path);
        match arc.install_rerank_slot(path, ModelFamily::Reranker) {
            Ok(model_id) => {
                tracing::info!(
                    slot = "rerank",
                    model_id = %model_id,
                    "rerank slot installed from SOVEREIGN_RERANK_MODEL_PATH"
                );
            }
            Err(e) => {
                tracing::warn!(
                    path = %rerank_path,
                    error = %e,
                    "rerank slot install failed — running without reranker"
                );
            }
        }
    }

    let provider: Arc<dyn InferenceProvider> = Arc::clone(&arc) as Arc<dyn InferenceProvider>;
    Ok((provider, arc, resolved_embed_family))
}
