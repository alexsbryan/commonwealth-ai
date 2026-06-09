// SPDX-License-Identifier: AGPL-3.0-or-later
//! Inference-provider construction — extracted verbatim from
//! `bootstrap_with_progress` (§3.3). Loads the embedded llama.cpp slots
//! (after a crash-isolated GPU smoke test) and, in Local mode, wraps the
//! raw provider in a `MeshInferenceProvider` for Slow-slot peer routing.
//! Narrowed to the inference slot (not `&AppState`, ARCH_PRINCIPLES §5.2).
//!
//! Reuses an already-loaded provider when `inference_slot` is populated
//! (a Runtime rebuild — or a test that pre-seeds a mock). That reuse seam
//! is what lets the rest of bootstrap be exercised in CI without a GGUF;
//! only the in-process model load below is genuinely untestable.

use std::sync::Arc;

use sovereign_core::model_family::ModelFamily;
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::embedded::EmbeddedLlamaCpp;
use tokio::sync::RwLock;

use crate::state::{BootstrapPhase, DesktopConfig};

/// Returns `(raw_inference, inference)`:
/// - `raw_inference` — the plain local `EmbeddedLlamaCpp` (handed to the
///   corpus engine + advertised to the mesh as this node's provider).
/// - `inference` — `raw_inference` wrapped in a `MeshInferenceProvider`
///   in Local mode (`mesh = Some`, routes Slow-slot work to a beefier
///   peer when one is online), or the raw provider unchanged in Attach
///   mode (`mesh = None`, the CLI daemon already owns peer routing).
///
/// Both share the same underlying weights — the wrapper is a thin router
/// over an Arc clone, no double-load.
pub(crate) async fn load_inference(
    inference_slot: &RwLock<Option<Arc<dyn InferenceProvider>>>,
    mesh: Option<&Arc<sovereign_mesh::EmbeddedDaemon>>,
    config: &DesktopConfig,
    emit: impl Fn(BootstrapPhase),
) -> Result<(Arc<dyn InferenceProvider>, Arc<dyn InferenceProvider>), String> {
    let raw_inference: Arc<dyn InferenceProvider> = {
        let existing = inference_slot.read().await;
        if let Some(inf) = existing.as_ref() {
            Arc::clone(inf)
        } else {
            drop(existing);
            tracing::info!("Loading fast model: {}", config.model_path.display());
            if let Some(ref ep) = config.embed_model_path {
                tracing::info!("Loading embed model: {}", ep.display());
            } else {
                tracing::warn!(
                    "No embedding model configured. Corpus install and RAG features \
                     will be unavailable until you set Settings → Embedding model."
                );
            }

            // Canonical chat-slot ctx lives in `~/.sovereign/config.toml`'s
            // `[models].context_size` (single source of truth). Read it so
            // the desktop-embedded `EmbeddedLlamaCpp` lines up with the
            // daemon. 16384 matches `setup_config::default_context_size`;
            // the daemon's `effective_context_size` wins for users who
            // actually have a SetupConfig file (the common case).
            let effective_ctx = sovereign_core::setup_config::SetupConfig::load()
                .map(|c| c.models.effective_context_size())
                .unwrap_or(16384);

            // Crash-isolated smoke test: spawn ourselves with `--smoketest`
            // and run a 1-token decode against the chat slot's GGUF before
            // loading it in-process. If the child SIGSEGVs (e.g., Gemma 4 on
            // Apple Metal in llama-cpp-2 0.1.145), set `SOVEREIGN_FORCE_CPU_CHAT=1`
            // for THIS process and continue — the in-process load below then
            // configures the chat slot with `n_gpu_layers=0`. Skipped when the
            // var is already set or when no GPU is configured anyway.
            let env_force_cpu = std::env::var("SOVEREIGN_FORCE_CPU_CHAT")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if !env_force_cpu {
                let smoke_gpu_layers =
                    sovereign_inference::hardware::HardwareProfile::detect().recommended_gpu_layers;
                if smoke_gpu_layers > 0 {
                    emit(BootstrapPhase::SmokeTesting);
                    let smoke_ctx = effective_ctx.min(2048);
                    tracing::info!(
                        model = %config.model_path.display(),
                        gpu_layers = smoke_gpu_layers,
                        n_ctx = smoke_ctx,
                        "smoketest: probing GPU compatibility before in-process load"
                    );
                    let outcome = crate::smoketest::run_in_subprocess(
                        &config.model_path,
                        smoke_gpu_layers,
                        smoke_ctx,
                        std::time::Duration::from_secs(60),
                    );
                    match &outcome {
                        crate::smoketest::SmokeResult::Ok => {
                            tracing::info!("smoketest: GPU path ok — proceeding");
                        }
                        other if other.suggests_cpu_fallback() => {
                            tracing::error!(
                                outcome = %other,
                                "smoketest: GPU path crashed — falling back to CPU. \
                                 Set SOVEREIGN_FORCE_CPU_CHAT=0 to disable this guard."
                            );
                            // SAFETY: bootstrap runs once, single task, no
                            // concurrent env mutation. The var is read by
                            // sovereign-inference's chat-slot loader below.
                            std::env::set_var("SOVEREIGN_FORCE_CPU_CHAT", "1");
                        }
                        other => {
                            tracing::warn!(
                                outcome = %other,
                                "smoketest: inconclusive — proceeding with GPU load. \
                                 The model may still load and run normally; this just \
                                 means we couldn't pre-confirm it."
                            );
                        }
                    }
                }
            }

            emit(BootstrapPhase::LoadingModel);
            let loaded = Arc::new(
                EmbeddedLlamaCpp::load_full_with_families(
                    &config.model_path,
                    config.primary_model_path.as_deref(),
                    config.embed_model_path.as_deref(),
                    config.code_model_path.as_deref(),
                    effective_ctx,
                    None,
                    ModelFamily::Unknown,        // fast slot
                    ModelFamily::Unknown,        // primary slot (lazy-loaded)
                    config.embed_family.clone(), // embed slot — drives pooling/instructions
                    config.code_family.clone(),  // code slot (lazy, hot-swaps with primary)
                )
                .map_err(|e| format!("Failed to load model: {e}"))?,
            );

            if config.primary_model_path.is_some() {
                // Configurable via `DesktopConfig.primary_idle_secs`
                // (default 300s). Raise toward `u64::MAX` to pin the
                // primary; lower for eager VRAM reclaim.
                loaded.start_idle_monitor(config.primary_idle_secs);
            }

            let raw: Arc<dyn InferenceProvider> = loaded;
            *inference_slot.write().await = Some(Arc::clone(&raw));
            raw
        }
    };

    // Wrap with mesh routing only in Local mode. Attach mode (`mesh ==
    // None`) hands the raw provider through — the CLI daemon already owns
    // peer routing, so wrapping against a None daemon would be a no-op.
    let inference: Arc<dyn InferenceProvider> = match mesh {
        Some(mesh) => Arc::new(sovereign_mesh::peer_inference::MeshInferenceProvider::new(
            Arc::clone(&raw_inference),
            Arc::clone(mesh),
        )),
        None => Arc::clone(&raw_inference),
    };
    Ok((raw_inference, inference))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::builders::test_support::StubInference;

    /// Pre-seeding the inference slot is the bootstrap injection seam: the
    /// in-process model load is skipped, so this exercises the reuse path +
    /// Attach-mode (no-mesh) passthrough with no GGUF and no Tauri handle.
    #[tokio::test]
    async fn reuses_pre_seeded_provider_and_skips_the_model_load() {
        let stub: Arc<dyn InferenceProvider> = Arc::new(StubInference);
        let slot: RwLock<Option<Arc<dyn InferenceProvider>>> =
            RwLock::new(Some(Arc::clone(&stub)));
        let config = DesktopConfig::default();

        let (raw, inference) = load_inference(&slot, None, &config, |_| {})
            .await
            .expect("reuse path must not load a model");

        assert!(
            Arc::ptr_eq(&raw, &stub),
            "raw should be the pre-seeded provider (no fresh load)"
        );
        assert!(
            Arc::ptr_eq(&inference, &raw),
            "Attach mode (mesh = None) hands the raw provider through unwrapped"
        );
    }
}
