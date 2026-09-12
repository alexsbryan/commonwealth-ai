// SPDX-License-Identifier: AGPL-3.0-or-later
//! The desktop's inference provider: an HTTP client pointed at the daemon.
//!
//! # What this file stopped being (sv-surface svt-3)
//!
//! It used to hold two paths. Attach mode built the HTTP provider below;
//! Local mode ran a crash-isolated GPU smoke test, applied a CPU-compat
//! substitution, `mmap`ed the GGUFs through the embedded llama.cpp loader and
//! wrapped the result in a mesh-routing provider — this process holding the
//! weights and answering peers. That is the in-process hosting the campaign
//! removes: a desktop that loads a model is a daemon wearing a UI (ARCH
//! principle 12), and the crash-isolation subprocess only existed to guard a
//! load that no longer happens here.
//!
//! The loader's TYPE NAME is deliberately not spelled anywhere in this file.
//! `attach_construction_census::the_attach_provider_construction_is_pinned`
//! greps for it, so the absence is a gate rather than a promise — see that
//! test for what it refuses and why.
//!
//! ONE path now, which is why the `mesh` parameter and the `(raw, wrapped)`
//! distinction are gone: both halves of the returned pair are the same `Arc`.
//! The pair itself survives because the enrichment builders in `state.rs`
//! each take an owned handle.
//!
//! Reuses an already-loaded provider when `inference_slot` is populated (a
//! Runtime rebuild — or a test that pre-seeds a mock).

use std::sync::Arc;

use sovereign_core::traits::InferenceProvider;
use tokio::sync::RwLock;

use crate::state::{BootstrapPhase, ResolvedModelSlots};

/// Returns the provider twice — see the module note. Both are the same `Arc`.
///
/// `emit` still takes a [`BootstrapPhase`] sink so the splash keeps its
/// contract with `setup_flow`; this path emits none of them, because nothing
/// it does takes long enough for a user to see. It is a constructor over a
/// URL, not a model load.
pub(crate) async fn load_inference(
    inference_slot: &RwLock<Option<Arc<dyn InferenceProvider>>>,
    // Model-slot PATHS + context come from `SetupConfig` via
    // `ResolvedModelSlots` (single source of truth). The daemon loaded the
    // same files, so their stems are the ids it advertises on `/v1/models`.
    slots: &ResolvedModelSlots,
    _emit: impl Fn(BootstrapPhase),
) -> Result<(Arc<dyn InferenceProvider>, Arc<dyn InferenceProvider>), String> {
    if let Some(inf) = inference_slot.read().await.as_ref() {
        let raw = Arc::clone(inf);
        return Ok((Arc::clone(&raw), raw));
    }
    let raw: Arc<dyn InferenceProvider> = build_daemon_provider(slots)?;
    *inference_slot.write().await = Some(Arc::clone(&raw));
    Ok((Arc::clone(&raw), raw))
}

/// Build the daemon-routing provider: a
/// [`sovereign_inference::remote::SplitInferenceProvider`] that sends chat
/// completions + embeddings to the daemon's OpenAI-compatible `/v1` on the
/// configured client port, owning NO local weights. Model ids are the filename
/// stems of the configured primary (chat) + embed models — the same ids the
/// daemon advertises on `/v1/models` (it loaded the same `SetupConfig.models.*`
/// files), resolved the same way `sovereign chat`'s daemon bootstrap does. The
/// daemon's own engine still tier-routes Fast/Slow per request, so the chat id
/// is just the address — reasoning turns still reach the primary slot.
fn build_daemon_provider(slots: &ResolvedModelSlots) -> Result<Arc<dyn InferenceProvider>, String> {
    // `SetupConfig` is still loaded here for the daemon client port; the
    // model ids come from `slots` (already SetupConfig-derived, same source
    // the daemon advertises on `/v1/models`).
    let setup = sovereign_core::setup_config::SetupConfig::load()
        .map_err(|e| format!("load SetupConfig for daemon routing: {e}"))?;
    let v1 = format!("http://127.0.0.1:{}/v1", setup.daemon.client_port);
    let ctx = slots.windows.primary;

    let stem = |p: &std::path::Path| p.file_stem().and_then(|s| s.to_str()).map(str::to_string);
    let chat_id = slots
        .primary
        .as_deref()
        .and_then(stem)
        .or_else(|| stem(&slots.fast))
        .ok_or_else(|| "no chat model path to derive a daemon model id".to_string())?;
    let embed_id = slots
        .has_embed()
        .then(|| stem(&slots.embed))
        .flatten()
        .ok_or_else(|| "no embedding model configured (Settings → Embedding model)".to_string())?;

    tracing::info!(
        endpoint = %v1,
        chat_model = %chat_id,
        embed_model = %embed_id,
        context_size = ctx,
        "inference: routed to the daemon over HTTP — this process loads no weights"
    );
    // Behavior-preserving: the old `SplitInferenceProvider::new` derived the
    // embed slot's query-instruction prefix from `DEFAULT_MANIFEST` internally;
    // v0.4 threads it as an explicit constructor arg. Computed before the move
    // of `embed_id` into arg 3.
    let embed_query_instruction =
        sovereign_core::models_manifest::DEFAULT_MANIFEST.embed_query_instruction(&embed_id);
    Ok(Arc::new(
        sovereign_inference::remote::SplitInferenceProvider::new(
            &v1,
            chat_id,
            embed_id,
            ctx,
            embed_query_instruction,
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::builders::test_support::StubInference;

    /// Pre-seeding the inference slot is the bootstrap injection seam: the
    /// provider construction is skipped, so this exercises the reuse path with
    /// no `SetupConfig` on the host and no Tauri handle.
    #[tokio::test]
    async fn reuses_pre_seeded_provider_and_builds_nothing() {
        let stub: Arc<dyn InferenceProvider> = Arc::new(StubInference);
        let slot: RwLock<Option<Arc<dyn InferenceProvider>>> = RwLock::new(Some(Arc::clone(&stub)));
        // Explicit empty slots — NOT `load_or_default()`, which reads the
        // real ~/.svrnmesh/config.toml and makes the test host-dependent.
        // The reuse path must never touch these paths anyway.
        let slots = ResolvedModelSlots {
            fast: std::path::PathBuf::new(),
            primary: None,
            embed: std::path::PathBuf::new(),
            code: None,
            windows: sovereign_inference::embedded::SlotWindows::uniform(16_384),
        };

        let (raw, inference) = load_inference(&slot, &slots, |_| {})
            .await
            .expect("reuse path must not build a provider");

        assert!(
            Arc::ptr_eq(&raw, &stub),
            "raw should be the pre-seeded provider (no fresh build)"
        );
        assert!(
            Arc::ptr_eq(&inference, &raw),
            "both halves of the pair are the same Arc — there is no wrapper left"
        );
    }

    /// The one behaviour this file still decides: a missing embed model is
    /// REPORTED, not defaulted to some id the daemon never advertised
    /// (ARCH principle 6). Watched fail: drop the `ok_or_else` and this goes
    /// green on a provider pointed at an empty model id.
    #[test]
    fn a_missing_embed_model_is_refused_by_name() {
        let slots = ResolvedModelSlots {
            fast: std::path::PathBuf::from("/models/fast.gguf"),
            primary: None,
            embed: std::path::PathBuf::new(),
            code: None,
            windows: sovereign_inference::embedded::SlotWindows::uniform(16_384),
        };
        match build_daemon_provider(&slots) {
            Err(e) => assert!(
                e.contains("no embedding model configured"),
                "unexpected refusal: {e}"
            ),
            // `SetupConfig::load()` runs first and legitimately fails on a host
            // with no config; that is a different refusal and not this test's
            // subject. It must still be a refusal, never a provider.
            Ok(_) => panic!("an unset embed slot must not yield a provider"),
        }
    }
}
