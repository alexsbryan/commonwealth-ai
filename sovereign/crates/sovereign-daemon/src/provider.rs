// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hot-reload inference provider factory — extracted from `daemon_cmd`
//! (§3.2). Rebuilds the serving provider through the one serving assembly
//! (wrapped in the mesh-aware router) when the operator changes a model path
//! at runtime.

use std::sync::Arc;

use crate::admin_http::ProviderFactory;
use async_trait::async_trait;
use sovereign_compute::assembly::{AssemblyError, ReloadFactory, ServingParts};
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;

/// Rebuilds the serving provider from a fresh `SetupConfig`, wrapped in the
/// same `InferenceRouter` used at cold start so hot-reloads preserve
/// mesh-aware model routing.
///
/// Hot-swapped into `EmbeddedDaemon::inference_provider` by the admin
/// reload handler when the user changes a `models.*` path in
/// `~/.svrnmesh/config.toml` (e.g. via the desktop Settings
/// panel's model picker). Keeps the model-loading side of the daemon
/// out of `sovereign-mesh`, which has no business knowing about GGUF.
pub struct LlamaCppFactory {
    /// Same `EmbeddedDaemon` the cold-start path wraps the raw
    /// llama.cpp provider against. Held here so a hot-reload
    /// (operator changing the primary GGUF path while the daemon is
    /// running) produces a `InferenceRouter` view of the new
    /// raw provider — without this, reload would drop the wrapper
    /// and `/v1/chat/completions` would silently start substituting
    /// for peer-only model names again.
    pub daemon: Arc<crate::DeferredDaemon>,
    /// The factory cold start's assembly returned: the reload builds through
    /// the same assembly, against the compute children boot started.
    pub reload: Arc<ReloadFactory>,
}

impl LlamaCppFactory {
    /// The reload's engine: the one serving assembly, never a load of its own.
    pub(crate) fn rebuild_engine(&self, cfg: &SetupConfig) -> Result<ServingParts, AssemblyError> {
        self.reload.build(cfg)
    }
}

#[async_trait]
impl ProviderFactory for LlamaCppFactory {
    async fn build_provider(
        &self,
        cfg: &SetupConfig,
    ) -> Result<Arc<dyn InferenceProvider>, String> {
        // Only a holder has an engine to rebuild. A terminal's provider is a
        // forwarder built once against its entry node; the assembly refuses
        // its config by name (`SetupConfig::models`) rather than load empty
        // paths.
        let raw = self
            .rebuild_engine(cfg)
            .map_err(|e| format!("reload: {e}"))?
            .provider;

        // Wrap so a hot-reloaded daemon keeps its mesh-aware model
        // routing — same wrapper the cold-start path installs in
        // `run_daemon`. See the comment on the cold-start wiring
        // for why a bare `EmbeddedLlamaCpp` here would re-introduce
        // the silent-substitution bug.
        //
        // Hot-reload load-awareness invariant: the new router must
        // share the SAME `Arc<AtomicU32>` publisher as the old router
        // (held by AppState's OnceLock). Live `LocalTotalGuard`s
        // from the old router have already captured a clone of that
        // Arc and will continue to decrement it as their requests
        // drain. If we let the new router create a fresh publisher,
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
        let peer_source: Arc<dyn sovereign_contracts::venue::VenueSource> =
            Arc::clone(&self.daemon) as Arc<_>;
        let peer_host: Arc<dyn sovereign_serving_host::venue_host::VenueHost> =
            Arc::clone(&self.daemon) as Arc<_>;
        let app_state_opt = daemon.app_state().await;
        let mut builder = sovereign_serving_host::peer_inference::InferenceRouter::builder(raw)
            .candidates(Arc::clone(&peer_source))
            .host(Arc::clone(&peer_host))
            .manifest(Arc::new(crate::slot_manifest::CoreSlotManifest));
        // A reload must NOT mint a fresh publisher: live `LocalTotalGuard`s from
        // the old router hold a clone of the node's `Arc<AtomicU32>` and keep
        // decrementing it as their requests drain. The gauge exists from
        // construction (the bootstrap created it before the cold-start router),
        // so `AppState` already holds it and we hand the same `Arc` to the new
        // router. A node with no gauge (no router ever built) leaves the
        // builder to mint a private one.
        if let Some(publisher) = app_state_opt
            .as_ref()
            .and_then(|state| state.in_flight_publisher())
        {
            builder = builder.in_flight(publisher);
        }
        let mesh_provider = Arc::new(builder.build());
        // Push current slot aliases into the freshly-built mesh
        // provider so a reload preserves the deferred-resolution
        // wiring. Mirrors the cold-start spawned task in
        // `run_daemon`; here we run inline because the daemon is
        // already in the Running state at reload time.
        if let Some(state) = app_state_opt {
            let snapshot = state.inner.serving.slot_aliases.current();
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
        // pays nothing. See `sovereign_mesh::guest_source`.
        mesh_provider.set_guest_source(sovereign_mesh::guest_source::stored_guest_source());
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

/// A hot reload builds exactly what cold start builds (pb-serving-assembly).
///
/// Until 2026-09-26 the reload loaded GGUFs itself: it never read
/// `[engine] kind` or `[compute] distributed_primary`, so a remote-kind node
/// loaded weights on reload and a distributed-primary node loaded the
/// withheld primary in-process. The GGUFs here are files holding no model,
/// so any build that reaches llama.cpp fails on the header — after it has
/// planned. (A missing file would not do: the vendored loader
/// `debug_assert!`s that the path exists.) The plan is the slot set each path
/// asked for, compared whether or not it loaded.
#[cfg(test)]
mod reload_builds_what_cold_start_builds {
    use super::*;
    use sovereign_compute::assembly::{assemble_serving, PlannedSlot, ServingPlan};
    use sovereign_core::setup_config::{EngineKind, EngineSection, ModelsSection};
    use std::path::PathBuf;

    /// A directory of three files named like GGUFs, holding no model.
    fn models_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pb-serving-assembly-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp models dir");
        for name in [
            "big-primary.gguf",
            "small-fast.gguf",
            "Qwen3-Embedding-0.6B-Q8_0.gguf",
        ] {
            std::fs::write(dir.join(name), b"not a gguf").expect("write a non-model file");
        }
        dir
    }

    fn primary() -> PathBuf {
        models_dir().join("big-primary.gguf")
    }

    fn holder() -> SetupConfig {
        let dir = models_dir();
        let mut cfg = SetupConfig::unconfigured();
        cfg.data.dir = dir.join("data-never-written");
        cfg.models = Some(ModelsSection {
            primary: primary(),
            fast: Some(dir.join("small-fast.gguf")),
            embed: dir.join("Qwen3-Embedding-0.6B-Q8_0.gguf"),
            ..Default::default()
        });
        cfg
    }

    fn remote() -> SetupConfig {
        let mut cfg = holder();
        cfg.engine = EngineSection {
            kind: EngineKind::Remote,
            endpoint: Some("http://127.0.0.1:1/v1".to_string()),
            model_id: Some("some-remote-model".to_string()),
            ..Default::default()
        };
        cfg
    }

    fn distributed_primary() -> SetupConfig {
        let mut cfg = holder();
        cfg.compute.enabled = true;
        cfg.compute.distributed_primary = true;
        cfg
    }

    /// The daemon's reload path as boot installs it, on a node whose cold
    /// start started no compute children.
    fn reload_path() -> LlamaCppFactory {
        LlamaCppFactory {
            daemon: Arc::new(crate::DeferredDaemon::new()),
            reload: Arc::default(),
        }
    }

    fn slot_set(built: Result<ServingParts, AssemblyError>) -> ServingPlan {
        match built {
            Ok(parts) => parts.plan,
            Err(e) => e
                .plan
                .unwrap_or_else(|| panic!("the build refused before planning: {}", e.reason)),
        }
    }

    #[test]
    fn a_reload_builds_the_cold_start_slot_set() {
        for (label, cfg) in [
            ("llama", holder()),
            ("remote", remote()),
            ("distributed_primary", distributed_primary()),
        ] {
            let cold = slot_set(assemble_serving(&cfg));
            let reload = slot_set(reload_path().rebuild_engine(&cfg));
            assert_eq!(
                reload, cold,
                "{label}: the reload path must build the slot set cold start builds"
            );
        }
    }

    /// Equal is not enough: two paths can agree on the wrong set. Each
    /// config's set is the one it asks for.
    #[test]
    fn each_config_plans_the_slots_it_asks_for() {
        let llama = slot_set(reload_path().rebuild_engine(&holder()));
        assert_eq!(llama.engine, EngineKind::Llama);
        assert!(
            llama.in_process.contains(&PlannedSlot::Primary),
            "{llama:?}"
        );
        assert_eq!(
            llama.embed_family,
            sovereign_core::model_family::ModelFamily::Qwen3Embedding,
            "embed keeps its manifest family"
        );

        let remote = slot_set(reload_path().rebuild_engine(&remote()));
        assert_eq!(remote.engine, EngineKind::Remote);
        assert!(
            remote.in_process.is_empty(),
            "a remote-kind node holds no local slots: {remote:?}"
        );

        let dp = slot_set(reload_path().rebuild_engine(&distributed_primary()));
        assert!(
            !dp.in_process.contains(&PlannedSlot::Primary),
            "the primary is withheld from this process: {dp:?}"
        );
        assert!(
            dp.children
                .iter()
                .any(|(_, model)| model == &primary()),
            "a compute child owns the primary: {dp:?}"
        );
    }

    /// A remote-kind node reloads with no weights on disk, as it boots.
    #[test]
    fn a_remote_reload_needs_no_weights() {
        let cold = assemble_serving(&remote()).expect("cold start needs no weights");
        let reload = reload_path()
            .rebuild_engine(&remote())
            .expect("the reload needs no weights either");
        assert!(reload.llama.is_none() && cold.llama.is_none());
        assert_eq!(
            reload.provider.resident_slots().len(),
            cold.provider.resident_slots().len()
        );
    }
}
