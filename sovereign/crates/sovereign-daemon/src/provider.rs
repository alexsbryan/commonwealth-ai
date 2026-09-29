// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hot-reload inference provider factory — extracted from `daemon_cmd`
//! (§3.2). Rebuilds the serving provider through the one serving assembly
//! (wrapped in the mesh-aware router) when the operator changes a model path
//! at runtime.

use std::sync::Arc;

use crate::admin_http::ProviderFactory;
use async_trait::async_trait;
use sovereign_compute::assembly::ReloadFactory;
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
    /// The hosted path (pb-stock-binary): serve's reload route swaps `cell`
    /// itself, the one both programs hold, so the daemon forwards and never
    /// swaps a provider of its own into it.
    Hosted {
        cell: Arc<sovereign_contracts::reloadable_provider::ReloadableProvider>,
    },
}

impl LlamaCppFactory {
    /// The alias map follows what serve holds now; `build_provider` pushes it
    /// into the rebuilt router below. Both serve paths publish through here.
    async fn publish_served_aliases(
        &self,
        slots: &[sovereign_contracts::oicp::ResidentSlot],
        source: &'static str,
    ) {
        let state = match self.daemon.get() {
            Some(daemon) => daemon.app_state().await,
            None => None,
        };
        if let Some(state) = state {
            crate::daemon::publish_slot_aliases(
                &state,
                crate::serve_client::served_slot_aliases(slots),
                source,
            );
        }
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
                self.publish_served_aliases(
                    &served.resident_slots,
                    "serve's self-report after reload",
                )
                .await;
                Ok(Arc::clone(cell) as Arc<dyn InferenceProvider>)
            }
            ReloadSource::Hosted { cell } => {
                crate::serve_client::forward_reload(&crate::serve_client::default_serve_base())
                    .await?;
                self.publish_served_aliases(
                    &cell.resident_slots(),
                    "the hosted serve's residency after reload",
                )
                .await;
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
