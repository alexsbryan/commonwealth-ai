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
    /// Where the reload's raw provider comes from.
    pub reload: ReloadSource,
}

/// The raw provider a reload wraps, from the path boot chose
/// (`serve_client::ServingPath`).
pub enum ReloadSource {
    /// The in-process path: the factory cold start's assembly returned, so the
    /// reload builds through the same assembly, against the compute children
    /// boot started.
    Assembly(Arc<ReloadFactory>),
    /// The dialing path: serve rebuilds through its own ReloadFactory, and the
    /// daemon rebuilds its loopback provider from serve's new self-report,
    /// into `cell`, the one boot wrapped. No engine in this process.
    Serve {
        base: crate::serve_client::ServeBase,
        config_context: u32,
        cell: Arc<sovereign_contracts::reloadable_provider::ReloadableProvider>,
    },
}

impl LlamaCppFactory {
    /// The reload's engine: the one serving assembly, never a load of its own.
    #[cfg(test)]
    pub(crate) fn rebuild_engine(&self, cfg: &SetupConfig) -> Result<ServingParts, AssemblyError> {
        let ReloadSource::Assembly(reload) = &self.reload else {
            panic!("rebuild_engine is the in-process path's")
        };
        reload.build(cfg)
    }

    async fn raw_provider(&self, cfg: &SetupConfig) -> Result<Arc<dyn InferenceProvider>, String> {
        match &self.reload {
            ReloadSource::Assembly(reload) => Ok(reload
                .build(cfg)
                .map_err(|e| format!("reload: {e}"))?
                .provider),
            ReloadSource::Serve {
                base,
                config_context,
                cell,
            } => {
                let served =
                    crate::serve_client::reload_through_serve(base, cell, *config_context).await?;
                // The alias map follows what serve holds now; `build_provider`
                // pushes it into the rebuilt router below.
                let state = match self.daemon.get() {
                    Some(daemon) => daemon.app_state().await,
                    None => None,
                };
                if let Some(state) = state {
                    crate::daemon::publish_slot_aliases(
                        &state,
                        crate::serve_client::served_slot_aliases(&served.resident_slots),
                        "serve's self-report after reload",
                    );
                }
                Ok(Arc::clone(cell) as Arc<dyn InferenceProvider>)
            }
        }
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
        let raw = self.raw_provider(cfg).await?;

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
            reload: ReloadSource::Assembly(Arc::default()),
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
            dp.children.iter().any(|(_, model)| model == &primary()),
            "a compute child owns the primary: {dp:?}"
        );
    }

    /// A running compute generation with no process in it: a remote engine
    /// holds no weights, and the distributed primary's child stays unspawned
    /// until a warmed worker set exists.
    fn remote_distributed_primary() -> SetupConfig {
        let mut cfg = remote();
        cfg.compute.enabled = true;
        cfg.compute.distributed_primary = true;
        cfg
    }

    /// The reload the admin route calls, bound to a commissioned daemon and to
    /// the factory cold start returned.
    fn bound_reload(cold: &ServingParts, cfg: &SetupConfig) -> LlamaCppFactory {
        let daemon = Arc::new(crate::DeferredDaemon::new());
        daemon.bind(crate::EmbeddedDaemon::new(
            models_dir().join("daemon-data"),
            cfg.clone(),
            crate::daemon_services::fixtures::headless(),
        ));
        LlamaCppFactory {
            daemon,
            reload: ReloadSource::Assembly(Arc::clone(&cold.reload_factory)),
        }
    }

    /// `build_provider` — what `POST /v1/admin/reload` runs — against the
    /// compute generation cold start started. The same children are re-wrapped
    /// and the provider it installs still holds the child's primary; different
    /// children are refused by name. Asserted on what installed, not the plan.
    #[tokio::test]
    async fn build_provider_rewraps_the_running_children_and_refuses_new_ones() {
        let cfg = remote_distributed_primary();
        let cold = assemble_serving(&cfg).expect("a remote engine needs no weights");
        let child = cold
            .distributed_primary
            .clone()
            .expect("cold start registers the distributed-primary child");
        let reload = bound_reload(&cold, &cfg);

        let installed = match reload.build_provider(&cfg).await {
            Ok(provider) => provider,
            Err(e) => panic!("the same children must reload: {e}"),
        };
        let primary = installed
            .resident_slots()
            .into_iter()
            .find(|s| s.role == "primary")
            .expect("the reloaded provider must still route the primary to its child");
        assert_eq!(primary.model_id, "big-primary");
        let rebuilt = reload.rebuild_engine(&cfg).expect("same children");
        assert!(
            rebuilt
                .distributed_primary
                .is_some_and(|slot| Arc::ptr_eq(&slot, &child)),
            "a reload wraps the running child; it never registers a second one"
        );

        let mut changed = cfg.clone();
        changed
            .compute
            .slot
            .push(sovereign_core::setup_config::ComputeSlotConfig {
                name: "another".into(),
                role: "generate".into(),
                model: models_dir().join("small-fast.gguf"),
                context_size: None,
                n_gpu_layers: None,
                warm: false,
                capture_embed: false,
            });
        match reload.build_provider(&changed).await {
            Ok(_) => panic!("a reload that asks for different children must refuse"),
            Err(e) => assert!(e.contains("restart the daemon"), "got: {e}"),
        }
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

/// On the dialing path a reload is serve's (pb-svrn-dials-serve): the daemon
/// forwards it, then rebuilds its loopback provider from serve's new
/// self-report, so this node's model facts, and the manifest peers read, name
/// the reloaded model.
#[cfg(test)]
mod reload_through_serve {
    use super::*;
    use axum::routing::{get, post};
    use sovereign_contracts::engine_state::{
        EngineReloaded, ServedSelf, RELOAD_PATH, SERVED_SELF_PATH,
    };
    use sovereign_contracts::oicp::ResidentSlot;
    use sovereign_core::types::Speed;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn served(model: &str) -> ServedSelf {
        ServedSelf {
            primary_model: model.to_string(),
            resident_slots: vec![ResidentSlot {
                role: "primary".to_string(),
                model_id: model.to_string(),
                resident: true,
                size_bytes: None,
                transitioning: false,
                placement: None,
            }],
            ..ServedSelf::default()
        }
    }

    /// serve's two reload routes: the reload counts, and the self-report names
    /// the model the last reload left resident.
    async fn stub_serve(reloads: Arc<AtomicUsize>) -> String {
        let counted = Arc::clone(&reloads);
        let app = axum::Router::new()
            .route(
                RELOAD_PATH,
                post(move || {
                    let counted = Arc::clone(&counted);
                    async move {
                        counted.fetch_add(1, Ordering::SeqCst);
                        axum::Json(EngineReloaded {
                            resident_models: vec!["after-reload".to_string()],
                        })
                    }
                }),
            )
            .route(
                SERVED_SELF_PATH,
                get(move || {
                    let reloads = Arc::clone(&reloads);
                    async move {
                        let model = if reloads.load(Ordering::SeqCst) > 0 {
                            "after-reload"
                        } else {
                            "before-reload"
                        };
                        axum::Json(served(model))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_reload_reloads_serve_and_the_manifest_names_the_new_model() {
        let reloads = Arc::new(AtomicUsize::new(0));
        let base = stub_serve(Arc::clone(&reloads)).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = SetupConfig::unconfigured();
        let daemon = Arc::new(crate::DeferredDaemon::new());
        daemon.bind(crate::EmbeddedDaemon::new(
            dir.path().to_path_buf(),
            cfg.clone(),
            crate::daemon_services::fixtures::headless(),
        ));
        let base = crate::serve_client::ServeBase {
            base,
            source: crate::serve_client::ServeBaseSource::Default,
        };
        let cell = Arc::new(
            sovereign_contracts::reloadable_provider::ReloadableProvider::new(
                Arc::new(crate::serve_client::loopback_provider(
                    &base,
                    served("before-reload"),
                    4096,
                )),
                Default::default(),
            ),
        );
        let factory = LlamaCppFactory {
            daemon,
            reload: ReloadSource::Serve {
                base,
                config_context: 4096,
                cell: Arc::clone(&cell),
            },
        };
        let provider = factory
            .build_provider(&cfg)
            .await
            .expect("the reload is forwarded and the provider rebuilt");
        assert_eq!(
            reloads.load(Ordering::SeqCst),
            1,
            "the reload must reach serve"
        );
        assert_eq!(provider.model_id_for(Speed::Slow), "after-reload");
        assert_eq!(
            cell.model_id_for(Speed::Slow),
            "after-reload",
            "the reload must land in the cell boot wrapped"
        );
        let manifest = sovereign_serving_host::oicp_synthesis::build_self_manifest(
            provider.as_ref(),
            &crate::slot_manifest::CoreSlotManifest,
        );
        let ids: Vec<&str> = manifest.models.iter().map(|m| m.id.as_str()).collect();
        assert!(
            ids.contains(&"after-reload"),
            "peers must see the reloaded model, saw {ids:?}"
        );
    }
}
