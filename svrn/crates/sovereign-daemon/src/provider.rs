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
use sovereign_turn_client::serve_self::EngineStateRead;

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
                // The cell and the aliases now say what serve holds; the
                // reload succeeds only when that is what the config asked
                // for, and otherwise names each slot that did not load.
                let unmet = crate::serve_client::unmet_slots(cfg, &served);
                if !unmet.is_empty() {
                    self.push_router_aliases().await;
                    tracing::warn!(target: "serving_path", serve_base = %base.base, ?unmet, "reload: serve does not hold what the config asks for");
                    return Err(format!(
                        "reload: serve at {} reloaded but does not hold what the config asks for: {}",
                        base.base,
                        unmet.join("; ")
                    ));
                }
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

    /// Re-read serve's self-report every `every` and, when it differs from
    /// the one svrn last adopted, adopt it as a reload does (cell, relay
    /// manifest, aliases). svrn reads the self-report once at boot, so without
    /// this a serve restart left svrn answering from the boot snapshot. A read
    /// that fails keeps the last facts and is traced once per transition; "did
    /// not answer" is never adopted as "holds nothing" (principle 6).
    async fn follow(self: Arc<Self>, every: std::time::Duration, reach: &'static ServeReach) {
        let ReloadSource::Serve {
            base,
            config_context,
            cell,
            relay,
        } = &self.reload
        else {
            return;
        };
        reach.following();
        let mut held: Option<serde_json::Value> = None;
        let mut answering = true;
        loop {
            tokio::time::sleep(every).await;
            let read = sovereign_turn_client::serve_self::served_self_read(&base.base).await;
            reach.record(&read);
            let served = match read {
                EngineStateRead::Answered(served) => served,
                absent => {
                    if answering {
                        tracing::warn!(target: "serving_path", serve_base = %base.base, read = ?absent, "follow: serve's self-report did not answer; svrn keeps the last one it adopted");
                    }
                    answering = false;
                    continue;
                }
            };
            if !answering {
                tracing::info!(target: "serving_path", serve_base = %base.base, "follow: serve's self-report answers again");
            }
            answering = true;
            let now = serde_json::to_value(&served).ok();
            if now.is_some() && now == held {
                continue;
            }
            tracing::info!(target: "serving_path", serve_base = %base.base, primary = %served.primary_model, first = held.is_none(), "follow: serve's self-report changed; svrn adopts it");
            crate::serve_client::adopt_served(base, cell, &served, *config_context);
            if let Some(relay) = relay {
                relay.read_manifest().await;
            }
            self.publish_served_aliases(&served.resident_slots, "serve's self-report, followed")
                .await;
            self.push_router_aliases().await;
            held = now;
        }
    }
}

/// serve's reach as svrn's follower last read it: `/status`'s `serve_reach`
/// (pc-split-deploy-honesty-serve-reach). `/status` reports this reading and
/// its age and never dials serve itself (principle 1). Unset where no follower
/// runs (a hosted serve, a terminal), so the field is absent there.
pub struct ServeReach(std::sync::Mutex<Option<Option<(EngineStateRead<()>, std::time::Instant)>>>);

/// The one the boot's follower writes and `/status` reads.
static SERVE_REACH: ServeReach = ServeReach::new();

/// What `/status` says of serve's reach: `last_read` is `answered`,
/// `unreachable` (nothing answered, or it refused or was unreadable; `detail`
/// says which), `did_not_answer_in_time` or `not_read_yet`, `age_seconds`
/// how long ago that read was.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ServeReachStatus {
    /// What the last read found.
    pub last_read: &'static str,
    /// Why serve was unreachable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Seconds since that read; absent before the first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,
}

impl ServeReach {
    /// No follower yet: `/status` names no reach.
    pub const fn new() -> Self {
        Self(std::sync::Mutex::new(None))
    }

    fn following(&self) {
        let mut held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        held.get_or_insert(None);
    }

    fn record<T>(&self, read: &EngineStateRead<T>) {
        let kept = match read {
            EngineStateRead::Answered(_) => EngineStateRead::Answered(()),
            EngineStateRead::Unreachable(why) => EngineStateRead::Unreachable(why.clone()),
            EngineStateRead::DidNotAnswerInTime => EngineStateRead::DidNotAnswerInTime,
        };
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Some((kept, std::time::Instant::now())));
    }

    /// The last read and its age; `None` where no follower runs.
    pub fn status(&self) -> Option<ServeReachStatus> {
        let held = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        let Some((read, at)) = held else {
            return Some(ServeReachStatus {
                last_read: "not_read_yet",
                detail: None,
                age_seconds: None,
            });
        };
        let (last_read, detail) = match read {
            EngineStateRead::Answered(()) => ("answered", None),
            EngineStateRead::Unreachable(why) => ("unreachable", Some(why)),
            EngineStateRead::DidNotAnswerInTime => ("did_not_answer_in_time", None),
        };
        Some(ServeReachStatus {
            last_read,
            detail,
            age_seconds: Some(at.elapsed().as_secs()),
        })
    }
}

/// `/status`'s `serve_reach`: the boot follower's last read of serve.
pub fn serve_reach() -> Option<ServeReachStatus> {
    SERVE_REACH.status()
}

/// How often svrn re-reads serve's self-report on the dialing path.
pub const SERVE_FOLLOW_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// The factory boot registers, following serve's self-report every
/// [`SERVE_FOLLOW_INTERVAL`] where it dials serve (pc-split-deploy-honesty).
pub fn following(factory: LlamaCppFactory) -> Arc<LlamaCppFactory> {
    following_every(factory, SERVE_FOLLOW_INTERVAL, &SERVE_REACH)
}

fn following_every(
    factory: LlamaCppFactory,
    every: std::time::Duration,
    reach: &'static ServeReach,
) -> Arc<LlamaCppFactory> {
    let factory = Arc::new(factory);
    if matches!(factory.reload, ReloadSource::Serve { .. }) {
        tokio::spawn(Arc::clone(&factory).follow(every, reach));
    }
    factory
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

    fn slot(role: &str, model: &str) -> ResidentSlot {
        ResidentSlot {
            role: role.to_string(),
            model_id: model.to_string(),
            resident: true,
            size_bytes: None,
            transitioning: false,
            placement: None,
        }
    }

    fn served(model: &str) -> ServedSelf {
        ServedSelf {
            primary_model: model.to_string(),
            resident_slots: vec![slot("primary", model), slot("embed", "emb")],
            ..ServedSelf::default()
        }
    }

    /// A config whose `[models]` asks for `primary` and the stub's embed model.
    fn asking_for(primary: &str, dir: &std::path::Path) -> SetupConfig {
        let path = dir.join("config.toml");
        let text = format!("[models]\nprimary = \"/m/{primary}.gguf\"\nembed = \"/m/emb.gguf\"\n");
        std::fs::write(&path, text).expect("write config");
        SetupConfig::load_from(&path).expect("config parses")
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

    /// The dialing path's factory over a stub serve at `base`, and the cell and
    /// router boot would have built.
    type Wired = (
        LlamaCppFactory,
        Arc<sovereign_contracts::reloadable_provider::ReloadableProvider>,
        Arc<dyn InferenceProvider>,
    );

    fn factory_over(base: String, cfg: &SetupConfig, dir: &std::path::Path) -> Wired {
        let daemon = Arc::new(crate::DeferredDaemon::new());
        daemon.bind(crate::EmbeddedDaemon::new(
            dir.to_path_buf(),
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
        (factory, cell, routed)
    }

    #[tokio::test]
    async fn a_reload_reloads_serve_and_the_manifest_names_the_new_model() {
        let reloads = Arc::new(AtomicUsize::new(0));
        let base = stub_serve(Arc::clone(&reloads)).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = SetupConfig::unconfigured();
        let (factory, cell, routed) = factory_over(base, &cfg, dir.path());
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

    /// pc-split-deploy-honesty: a reload whose serve does not hold what the
    /// config asks for is refused, naming the slot, and svrn's model facts
    /// say what serve holds rather than what was asked for.
    #[tokio::test]
    async fn a_reload_serve_did_not_honour_is_refused_naming_the_slot() {
        let reloads = Arc::new(AtomicUsize::new(0));
        let base = stub_serve(Arc::clone(&reloads)).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = asking_for("wanted", dir.path());
        let (factory, cell, _) = factory_over(base, &cfg, dir.path());
        let err = match factory.build_provider(&cfg).await {
            Ok(_) => panic!("a reload serve did not honour reported success"),
            Err(e) => e,
        };
        assert!(
            err.contains("primary: asked for wanted, serve holds after-reload"),
            "{err}"
        );
        assert!(!err.contains("embed"), "embed was held, yet named: {err}");
        assert_eq!(cell.model_id_for(Speed::Slow), "after-reload");
    }

    /// pc-split-deploy-honesty: serve restarts holding another model, with no
    /// reload; svrn's model facts follow it within the follow interval.
    #[tokio::test]
    async fn svrn_follows_serve_across_a_restart() {
        let model = Arc::new(std::sync::Mutex::new("before-restart".to_string()));
        let answers = Arc::clone(&model);
        let app = axum::Router::new().route(
            SERVED_SELF_PATH,
            get(move || {
                let answers = Arc::clone(&answers);
                async move { axum::Json(served(&answers.lock().unwrap().clone())) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move { axum::serve(listener, app).await });
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = SetupConfig::unconfigured();
        let (factory, cell, _) = factory_over(base, &cfg, dir.path());
        let _factory = following_every(
            factory,
            std::time::Duration::from_millis(50),
            Box::leak(Box::new(ServeReach::new())),
        );

        *model.lock().unwrap() = "after-restart".to_string();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while cell.model_id_for(Speed::Slow) != "after-restart" {
            assert!(
                std::time::Instant::now() < deadline,
                "svrn still answers {} 5 s after serve restarted",
                cell.model_id_for(Speed::Slow)
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Follow `base` every 50 ms into a fresh reach until its last read is
    /// `want`, within 10 s.
    async fn reach_reads(base: String, want: &str) -> ServeReachStatus {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = SetupConfig::unconfigured();
        let (factory, _, _) = factory_over(base, &cfg, dir.path());
        let reach: &'static ServeReach = Box::leak(Box::new(ServeReach::new()));
        assert_eq!(reach.status(), None, "no follower, yet a reach was named");
        let _factory = following_every(factory, std::time::Duration::from_millis(50), reach);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let status = reach.status();
            if let Some(status) = status.clone().filter(|s| s.last_read == want) {
                return status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "serve's reach read {status:?} 10 s in, never {want}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// pc-split-deploy-honesty-serve-reach: serve stopped is named
    /// unreachable, never a timeout.
    #[tokio::test]
    async fn status_names_a_stopped_serve_unreachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        drop(listener);
        let status = reach_reads(base, "unreachable").await;
        assert!(
            status
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("not reachable"),
            "{status:?}"
        );
    }

    /// pc-split-deploy-honesty-serve-reach: serve accepting and not
    /// answering is named a timeout, never unreachable.
    #[tokio::test]
    async fn status_names_a_serve_that_accepts_and_does_not_answer_a_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let status = reach_reads(base, "did_not_answer_in_time").await;
        assert_eq!(status.detail, None);
    }

    /// pc-split-deploy-honesty-serve-reach: serve up names neither absence.
    #[tokio::test]
    async fn status_names_a_serve_that_answers_answered() {
        let base = stub_serve(Arc::new(AtomicUsize::new(0))).await;
        let status = reach_reads(base, "answered").await;
        assert_eq!(status.detail, None);
        assert!(status.age_seconds.is_some(), "{status:?}");
    }

    #[tokio::test]
    async fn a_reload_serve_honoured_succeeds() {
        let reloads = Arc::new(AtomicUsize::new(0));
        let base = stub_serve(Arc::clone(&reloads)).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = asking_for("after-reload", dir.path());
        let (factory, _, _) = factory_over(base, &cfg, dir.path());
        if let Err(e) = factory.build_provider(&cfg).await {
            panic!("serve holds what was asked for, yet the reload refused: {e}");
        }
    }
}
