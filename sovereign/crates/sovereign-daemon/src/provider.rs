// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hot-reload inference provider factory — extracted from `daemon_cmd`
//! (§3.2). A reload swaps the provider cell under the router boot built and
//! hands that same router back (pb-serve-ranks: one router per node, cold
//! start and reload alike), when the operator changes a model path at
//! runtime.

use std::sync::Arc;

use crate::admin_http::ProviderFactory;
use async_trait::async_trait;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;

/// Reloads the serving provider from a fresh `SetupConfig` and hands back the
/// provider boot built over it, so a hot reload keeps mesh-aware model routing
/// without constructing a second router.
///
/// Hot-swapped into `EmbeddedDaemon::inference_provider` by the admin
/// reload handler when the user changes a `models.*` path in
/// `~/.svrnmesh/config.toml` (e.g. via the desktop Settings
/// panel's model picker). Keeps the model-loading side of the daemon
/// out of `sovereign-mesh`, which has no business knowing about GGUF.
pub struct LlamaCppFactory {
    /// The deferred handle boot bound; a reload reads the running daemon's
    /// state (its slot aliases) through it.
    pub daemon: Arc<crate::DeferredDaemon>,
    /// Where the reload's raw provider comes from.
    pub reload: ReloadSource,
    /// What boot built over the reload's cell: the router, where one ranks
    /// this node's turns. A reload swaps the cell under it and returns this,
    /// so the pinned pods, the venue composite, the guest source and the
    /// shared model the cold start wired survive every reload (pb-serving-
    /// proofs (a); the `set_shared_model_id` divergence).
    pub routed: Arc<dyn InferenceProvider>,
    /// Pushes the reloaded residency's slot aliases into that router; `None`
    /// where nothing ranks here.
    pub slot_aliases: Option<crate::serve_client::SlotAliasSink>,
}

/// The raw provider a reload wraps, from the path boot chose
/// (`serve_client::ServingPath`).
pub enum ReloadSource {
    /// A terminal: its provider is the entry-node forwarder boot built, and it
    /// holds no engine to rebuild, so a reload refuses by name
    /// (pb-serve-distributes; the in-process assembly arm is gone).
    Terminal,
    /// The dialing path: serve rebuilds through its own ReloadFactory, and the
    /// daemon rebuilds its loopback provider from serve's new self-report,
    /// into `cell`, the one boot wrapped. No engine in this process.
    Serve {
        base: crate::serve_client::ServeBase,
        config_context: u32,
        cell: Arc<sovereign_contracts::reloadable_provider::ReloadableProvider>,
        /// svrn alone's OpenAI relay, whose manifest re-reads serve's after
        /// the reload (pb-serve-ranks); `None` where a distribution ranks.
        relay: Option<Arc<oicp_client::openai_passthrough::OpenAiPassthrough>>,
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
    /// into the router below. Both serve paths publish through here.
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
            ReloadSource::Terminal => {
                // The terminal's own refusal when its config still holds no
                // models (what the assembly returned before); otherwise the
                // models are new since boot, and serving them is serve's.
                let why = match cfg.models() {
                    Err(why) => why,
                    Ok(_) => "this daemon booted as a terminal and holds no engine; \
                         restart it to serve the models now configured"
                        .to_string(),
                };
                tracing::warn!(target: "serving_path", reason = %why, "reload refused: a terminal holds no engine");
                Err(format!("reload: {why}"))
            }
            ReloadSource::Serve {
                base,
                config_context,
                cell,
                relay,
            } => {
                let served =
                    crate::serve_client::reload_through_serve(base, cell, *config_context).await?;
                if let Some(relay) = relay {
                    relay.read_manifest().await;
                }
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
        // Only serve has an engine to rebuild. A terminal's provider is a
        // forwarder built once against its entry node; its arm refuses by
        // name (`SetupConfig::models`) rather than load empty paths.
        self.raw_provider(cfg).await?;
        self.push_router_aliases().await;
        tracing::info!(
            target: "serving_path",
            ranks = self.slot_aliases.is_some(),
            "reload: the cell swapped under the provider boot built; no second router"
        );
        Ok(Arc::clone(&self.routed))
    }
}

impl LlamaCppFactory {
    /// The cell under the router now holds what serve holds. The alias map
    /// follows serve's residency into the same router; the in-flight gauge,
    /// whose live guards the old requests still hold, is the router's own and
    /// never re-minted.
    async fn push_router_aliases(&self) {
        if let Some(sink) = &self.slot_aliases {
            let state = match self.daemon.get() {
                Some(daemon) => daemon.app_state().await,
                None => None,
            };
            if let Some(state) = state {
                let snapshot = state.inner.serving.slot_aliases.current();
                let map: std::collections::HashMap<String, String> = snapshot
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if !map.is_empty() {
                    sink(map);
                }
            }
        }
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
        // What boot built over the cell (a router, where one ranks), stood in
        // for by one more wrapper; the reload must hand back this one.
        let routed: Arc<dyn InferenceProvider> = Arc::new(
            sovereign_contracts::reloadable_provider::ReloadableProvider::new(
                Arc::clone(&cell) as Arc<dyn InferenceProvider>,
                Default::default(),
            ),
        );
        let factory = LlamaCppFactory {
            daemon,
            reload: ReloadSource::Serve {
                base,
                config_context: 4096,
                cell: Arc::clone(&cell),
                relay: None,
            },
            routed: Arc::clone(&routed),
            slot_aliases: None,
        };
        let provider = factory
            .build_provider(&cfg)
            .await
            .expect("the reload is forwarded and the cell swapped");
        assert!(
            std::ptr::addr_eq(Arc::as_ptr(&provider), Arc::as_ptr(&routed)),
            "a reload must hand back the provider boot built, never a second router"
        );
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
        // The residency peers' manifests are built from (serve's
        // `build_self_manifest` reads `resident_slots`).
        let slots = provider.resident_slots();
        let ids: Vec<&str> = slots.iter().map(|s| s.model_id.as_str()).collect();
        assert!(
            ids.contains(&"after-reload"),
            "peers must see the reloaded model, saw {ids:?}"
        );
    }
}
