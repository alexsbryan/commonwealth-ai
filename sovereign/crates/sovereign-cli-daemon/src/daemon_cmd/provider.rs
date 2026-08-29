// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hot-reload inference provider factory — extracted from `daemon_cmd`
//! (§3.2). Rebuilds the embedded llama.cpp provider (wrapped in the
//! mesh-aware router) when the operator changes a model path at runtime.

use std::sync::Arc;

use async_trait::async_trait;
use sovereign_core::model_family::ModelFamily;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::embedded::EmbeddedLlamaCpp;
use sovereign_mesh::admin_http::ProviderFactory;

/// Rebuilds the embedded llama.cpp provider from a fresh `SetupConfig`,
/// wrapped in the same `MeshInferenceProvider` used at cold start so
/// hot-reloads preserve mesh-aware model routing.
///
/// Hot-swapped into `EmbeddedDaemon::inference_provider` by the admin
/// reload handler when the user changes a `models.*` path in
/// `~/.svrnmesh/config.toml` (e.g. via the desktop Settings
/// panel's model picker). Keeps the model-loading side of the daemon
/// out of `sovereign-mesh`, which has no business knowing about GGUF.
pub(super) struct LlamaCppFactory {
    /// Same `EmbeddedDaemon` the cold-start path wraps the raw
    /// llama.cpp provider against. Held here so a hot-reload
    /// (operator changing the primary GGUF path while the daemon is
    /// running) produces a `MeshInferenceProvider` view of the new
    /// raw provider — without this, reload would drop the wrapper
    /// and `/v1/chat/completions` would silently start substituting
    /// for peer-only model names again.
    pub(super) daemon: Arc<sovereign_mesh::DeferredDaemon>,
}

#[async_trait]
impl ProviderFactory for LlamaCppFactory {
    async fn build_provider(
        &self,
        cfg: &SetupConfig,
    ) -> Result<Arc<dyn InferenceProvider>, String> {
        // Mirror the load parameters used by `run_daemon` on cold
        // start — the reload must not silently downgrade context
        // size or auto-gpu-layer behaviour. Pulls context size and
        // idle timeout from `cfg` for the same reason: a hot-reload
        // shouldn't drop the operator's tuned values.
        let provider = EmbeddedLlamaCpp::load_full_with_families(
            cfg.models.fast_path(),
            Some(&cfg.models.primary),
            Some(&cfg.models.embed),
            cfg.models.code.as_deref(),
            // Per-slot windows (2026-08-25). `from_models` honours
            // `[models].fast_context_size` and falls back to the primary's
            // window when it is unset, so an existing config.toml is unchanged.
            sovereign_inference::embedded::SlotWindows::from_models(&cfg.models),
            None,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
            // code slot is Qwen3-Coder-30B-A3B-Instruct (the only code
            // GGUF we ship today). Pinning the family to Qwen3 picks up
            // Qwen's recommended sampling defaults — top_k=20 (vs the
            // Unknown fallback of 40), top_p=0.95, presence_penalty=1.5
            // — and the SystemPromptToken thinking control. Empirically
            // (2026-05-08 measurement) the Unknown defaults left the
            // sampler too permissive on long Rust emissions, contributing
            // to the character-drop pattern (`f3 2`, `Lat encyClass`).
            ModelFamily::Qwen3,
        )
        .map_err(|e| format!("reload: failed to load models: {e}"))?;
        // Keep a typed `Arc<EmbeddedLlamaCpp>` to fire
        // `start_idle_monitor` (inherent method), then upcast to
        // `Arc<dyn InferenceProvider>` so the wrapper can hold it.
        let raw_concrete = Arc::new(provider);
        raw_concrete.start_idle_monitor(cfg.daemon.primary_idle_secs);
        let raw: Arc<dyn InferenceProvider> = raw_concrete;

        // Wrap so a hot-reloaded daemon keeps its mesh-aware model
        // routing — same wrapper the cold-start path installs in
        // `run_daemon`. See the comment on the cold-start wiring
        // for why a bare `EmbeddedLlamaCpp` here would re-introduce
        // the silent-substitution bug.
        //
        // Hot-reload load-awareness invariant: the new MIP must
        // share the SAME `Arc<AtomicU32>` publisher as the old MIP
        // (held by AppState's OnceLock). Live `LocalTotalGuard`s
        // from the old MIP have already captured a clone of that
        // Arc and will continue to decrement it as their requests
        // drain. If we let the new MIP create a fresh publisher,
        // the old guards would write to an Arc nobody reads, and
        // gossip would see a counter that snaps to zero on reload
        // and stays there until new traffic flows. See
        // `sovereign/docs/MESH_LOAD_AWARENESS.md`.
        // The factory is only reachable through `POST /v1/admin/reload`, which
        // is served BY the daemon — so by the time this runs the handle is
        // always bound. `None` here would mean a reload that arrived before
        // the daemon existed, which the HTTP surface cannot produce.
        let daemon = self
            .daemon
            .get()
            .ok_or_else(|| "reload arrived before the daemon was commissioned".to_string())?;
        let peer_source: Arc<dyn sovereign_mesh::peer_inference::PeerEndpointSource> =
            Arc::clone(&self.daemon) as Arc<_>;
        let app_state_opt = daemon.app_state().await;
        let mesh_provider = if let Some(state) = app_state_opt.as_ref() {
            match state.in_flight_publisher() {
                Some(publisher) => Arc::new(
                    sovereign_mesh::peer_inference::MeshInferenceProvider::with_peer_source_and_publisher(
                        raw,
                        Arc::clone(&peer_source),
                        publisher,
                    ),
                ),
                // OnceLock not yet set means cold-start's spawned
                // task hasn't run; reload still installs the new
                // MIP, and the spawned task will install its
                // publisher when it next polls.
                None => Arc::new(
                    sovereign_mesh::peer_inference::MeshInferenceProvider::with_peer_source(
                        raw,
                        Arc::clone(&peer_source),
                    ),
                ),
            }
        } else {
            Arc::new(
                sovereign_mesh::peer_inference::MeshInferenceProvider::with_peer_source(
                    raw,
                    Arc::clone(&peer_source),
                ),
            )
        };
        // Push current slot aliases into the freshly-built mesh
        // provider so a reload preserves the deferred-resolution
        // wiring. Mirrors the cold-start spawned task in
        // `run_daemon`; here we run inline because the daemon is
        // already in the Running state at reload time.
        if let Some(state) = app_state_opt {
            let snapshot = state.inner.slot_aliases.load();
            let map: std::collections::HashMap<String, String> = snapshot
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if !map.is_empty() {
                mesh_provider.set_slot_aliases(map);
            }
        }
        // A guest link this node has accepted lets a granted model id resolve
        // to the LENDING node, while the turn itself stays here. Wired from
        // the data dir because that is where `svrn mesh use` writes
        // `guest.json`; a node that never ran it gets `NoGuestLenders` and
        // pays nothing. See `sovereign_mesh::guest_lender`.
        mesh_provider.set_guest_source(std::sync::Arc::new(
            sovereign_mesh::guest_lender::StoredGuestLink::new(),
        ));
        // Route this node's primary turns into the mesh-hosted shared model, if
        // one is configured (SOVEREIGN_SHARED_MODEL_ID, from [shared_model]
        // model_id). Survives reload — the env is set once at daemon entry.
        if let Some(id) = sovereign_contracts::launch::SharedModelFleet::from_env().model_id() {
            mesh_provider.set_shared_model_id(Some(id.to_string()));
        }
        let routed: Arc<dyn InferenceProvider> = mesh_provider;
        Ok(routed)
    }
}
