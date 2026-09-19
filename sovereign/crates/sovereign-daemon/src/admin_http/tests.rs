use super::*;
use crate::loopback_guard::enforce_localhost;
use crate::EmbeddedDaemon;
use async_trait::async_trait;
use sovereign_core::setup_config::{DaemonSection, DataSection, ModelsSection};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;

/// Stub provider that records a version so tests can assert the
/// swap actually happened. Only `capabilities()` is exercised; the
/// other trait methods would panic if called, but the admin
/// handler never calls them during a reload test.
struct StubProvider {
    #[allow(dead_code)]
    version: usize,
}

#[async_trait]
impl InferenceProvider for StubProvider {
    async fn complete(
        &self,
        _request: &sovereign_core::types::CompletionRequest,
    ) -> sovereign_core::error::Result<sovereign_core::types::CompletionResponse> {
        unimplemented!("stub")
    }

    async fn complete_stream(
        &self,
        _request: &sovereign_core::types::CompletionRequest,
    ) -> sovereign_core::error::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = sovereign_core::error::Result<String>> + Send>,
        >,
    > {
        unimplemented!("stub")
    }

    async fn embed(&self, _text: &str) -> sovereign_core::error::Result<Vec<f32>> {
        unimplemented!("stub")
    }

    fn capabilities(&self) -> sovereign_core::types::ProviderCapabilities {
        sovereign_core::types::ProviderCapabilities {
            max_context_tokens: 0,
            supports_structured_output: false,
            relative_speed: sovereign_core::types::Speed::Fast,
            relative_reasoning: sovereign_core::types::Depth::Shallow,
        }
    }
}

struct StubFactory {
    build_count: Arc<AtomicUsize>,
}

#[async_trait]
impl ProviderFactory for StubFactory {
    async fn build_provider(
        &self,
        _cfg: &SetupConfig,
    ) -> Result<Arc<dyn InferenceProvider>, String> {
        let v = self.build_count.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Arc::new(StubProvider { version: v }))
    }
}

fn write_cfg(dir: &TempDir, primary: &str) -> PathBuf {
    let path = dir.path().join("config.toml");
    let cfg = SetupConfig {
        engine: Default::default(),
        compute: Default::default(),
        search: Default::default(),
        models: Some(ModelsSection {
            primary: PathBuf::from(primary),
            fast: Some(PathBuf::from("/m/fast.gguf")),
            embed: PathBuf::from("/m/embed.gguf"),
            code: None,
            context_size: None,
            fast_context_size: None,
            max_extras_memory_gb: None,
            extra: std::collections::BTreeMap::new(),
            primary_pool: None,
            edit: None,
        }),
        node: Default::default(),
        daemon: DaemonSection::default(),
        data: DataSection::default(),
        watched_folders: Default::default(),
        memory: Default::default(),
        iroh: Default::default(),
        shared_model: Default::default(),
        discovery: Default::default(),
        mcp_servers: Vec::new(),
    };
    cfg.save_to(&path).unwrap();
    path
}

#[test]
fn config_diff_flags_iroh_changes_as_restart_required() {
    let base = SetupConfig {
        engine: Default::default(),
        compute: Default::default(),
        search: Default::default(),
        models: Some(ModelsSection {
            primary: PathBuf::from("/m/primary.gguf"),
            fast: None,
            embed: PathBuf::from("/m/embed.gguf"),
            code: None,
            context_size: None,
            fast_context_size: None,
            max_extras_memory_gb: None,
            extra: std::collections::BTreeMap::new(),
            primary_pool: None,
            edit: None,
        }),
        node: Default::default(),
        daemon: DaemonSection::default(),
        data: DataSection::default(),
        watched_folders: Default::default(),
        memory: Default::default(),
        iroh: Default::default(),
        shared_model: Default::default(),
        discovery: Default::default(),
        mcp_servers: Vec::new(),
    };

    let mut enabled_flipped = base.clone();
    enabled_flipped.iroh.enabled = Some(true);
    let d = ConfigDiff::diff(&base, &enabled_flipped);
    assert_eq!(d.restart_required, vec!["iroh.enabled"]);
    assert!(d.models_changed.is_empty());

    let mut class_pinned = base.clone();
    class_pinned.iroh.transport.inference = Some("ip".into());
    let d = ConfigDiff::diff(&base, &class_pinned);
    assert_eq!(d.restart_required, vec!["iroh.transport"]);

    let d = ConfigDiff::diff(&base, &base.clone());
    assert!(d.restart_required.is_empty());
}

/// The failing input this exists for, and it was a live one: adding
/// `[iroh] media_origin` and running `svrn daemon reload` printed
/// "✓ no config changes detected — nothing to reload" while the daemon
/// went on serving the old value, because none of these three fields was
/// compared. A house is converted along exactly this path — "one config
/// line and a join" — so the reload saying nothing happened is the
/// difference between a working demo and a silent one.
///
/// Each is asserted ALONE. A single config differing in all three would
/// pass even if only one comparison existed.
#[test]
fn an_origin_config_change_is_never_reported_as_no_change() {
    let base = SetupConfig {
        engine: Default::default(),
        compute: Default::default(),
        search: Default::default(),
        models: None,
        node: Default::default(),
        daemon: DaemonSection::default(),
        data: DataSection::default(),
        watched_folders: Default::default(),
        memory: Default::default(),
        iroh: Default::default(),
        shared_model: Default::default(),
        discovery: Default::default(),
        mcp_servers: Vec::new(),
    };

    let mut origin_set = base.clone();
    origin_set.iroh.media_origin = Some("127.0.0.1:8096".into());
    let d = ConfigDiff::diff(&base, &origin_set);
    assert_eq!(d.media_changed, vec!["iroh.media_origin"]);
    assert!(
        d.restart_required.is_empty(),
        "the media origin reloads live"
    );
    assert!(!d.is_noop(), "a changed config must never read as a no-op");

    let mut allow_set = base.clone();
    allow_set.iroh.media_allow = vec!["LittleMac".into()];
    let d = ConfigDiff::diff(&base, &allow_set);
    assert_eq!(d.media_changed, vec!["iroh.media_allow"]);
    assert!(
        d.restart_required.is_empty(),
        "the media allow list reloads live"
    );
    assert!(!d.is_noop());

    let mut app_published = base.clone();
    app_published
        .iroh
        .apps
        .insert("chores".into(), "127.0.0.1:5000".into());
    let d = ConfigDiff::diff(&base, &app_published);
    assert_eq!(d.restart_required, vec!["iroh.apps"]);
    assert!(!d.is_noop());

    let mut offer_set = base.clone();
    offer_set.iroh.offer_origin = Some("127.0.0.1:8710".into());
    let d = ConfigDiff::diff(&base, &offer_set);
    assert_eq!(d.restart_required, vec!["iroh.offer_origin"]);
    assert!(!d.is_noop());

    let mut offer_allow_set = base.clone();
    offer_allow_set.iroh.offer_allow = vec!["LittleMac".into()];
    let d = ConfigDiff::diff(&base, &offer_allow_set);
    assert_eq!(d.restart_required, vec!["iroh.offer_allow"]);
    assert!(!d.is_noop());

    // The other half of the claim: an unchanged config still reads as one.
    assert!(ConfigDiff::diff(&base, &base.clone()).is_noop());
}

async fn spawn(daemon: Arc<EmbeddedDaemon>) -> String {
    let app = admin_router(Arc::clone(&daemon));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    format!("http://{addr}")
}

/// Direct unit test for the guard itself — independent of axum
/// extraction. Guards are small; bugs here are quiet, so pin both
/// directions (loopback passes, everything else rejected).
#[test]
fn enforce_localhost_rejects_non_loopback() {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    let allowed = [
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9741),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 1, 2, 3)), 9741),
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9741),
    ];
    for addr in allowed {
        assert!(
            enforce_localhost(&addr).is_ok(),
            "loopback {addr} must pass"
        );
    }

    // Covers the attack scenarios: LAN peer, Tailscale peer, and a
    // public IP — none should reach the admin handler.
    let denied = [
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7)), 9741),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2)), 9741),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 9741),
        SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0x2606, 0, 0, 0, 0, 0, 0, 1)), 9741),
    ];
    for addr in denied {
        let Err(resp) = enforce_localhost(&addr) else {
            panic!("non-loopback {addr} must be rejected");
        };
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }
}

/// A provider that OWNS a local slot, so the two `Option` fields can
/// be something other than the trait's `None` default. Without this
/// the no-slot case and the has-slot case are indistinguishable and
/// the test below would pass against a handler that always answered
/// `None`.
struct SlotProvider;

#[async_trait]
impl InferenceProvider for SlotProvider {
    async fn complete(
        &self,
        _request: &sovereign_core::types::CompletionRequest,
    ) -> sovereign_core::error::Result<sovereign_core::types::CompletionResponse> {
        unimplemented!("stub")
    }

    async fn complete_stream(
        &self,
        _request: &sovereign_core::types::CompletionRequest,
    ) -> sovereign_core::error::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = sovereign_core::error::Result<String>> + Send>,
        >,
    > {
        unimplemented!("stub")
    }

    async fn embed(&self, _text: &str) -> sovereign_core::error::Result<Vec<f32>> {
        unimplemented!("stub")
    }

    fn capabilities(&self) -> sovereign_core::types::ProviderCapabilities {
        sovereign_core::types::ProviderCapabilities {
            max_context_tokens: 32_768,
            supports_structured_output: false,
            relative_speed: sovereign_core::types::Speed::Fast,
            relative_reasoning: sovereign_core::types::Depth::Shallow,
        }
    }

    fn effective_context_size(&self) -> Option<u32> {
        Some(32_768)
    }

    fn n_ctx_train_for_primary(&self) -> Option<u32> {
        Some(131_072)
    }
}

/// The window comes from the slot the DAEMON is serving on, and
/// `configured` from its config — three numbers that are allowed to
/// disagree, which is why the route reports all three.
#[tokio::test]
async fn context_window_reports_the_daemons_slot() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_cfg(&tmp, "/m/primary.gguf");
    let initial = SetupConfig::load_from(&path).unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        initial,
        crate::daemon_services::fixtures::headless_with_provider(Arc::new(SlotProvider)),
    );

    let base = spawn(Arc::clone(&daemon)).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/admin/context-window"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: ContextWindow = resp.json().await.unwrap();
    assert_eq!(body.effective, Some(32_768));
    assert_eq!(body.n_ctx_train, Some(131_072));
}

/// A provider with NO local slot — the remote-only case — answers
/// `None` for both, and neither is quietly filled in from
/// `configured`. "There is no slot to ask" and "the slot agrees with
/// the config" are different answers, and a Settings panel renders
/// them differently (ARCH principle 6).
///
/// The fixture is deliberately `NullProvider`, which does not
/// override either method and so answers the trait's `None`. That
/// makes this test's pass condition weak ON ITS OWN — a handler
/// hard-coding `None` would satisfy it — which is exactly why it is
/// paired with the test above, where a provider that DOES own a slot
/// must come back with that slot's numbers. Neither test is a gate
/// without the other.
#[tokio::test]
async fn context_window_reports_absence_rather_than_echoing_configured() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_cfg(&tmp, "/m/primary.gguf");
    let initial = SetupConfig::load_from(&path).unwrap();
    let initial_ctx = initial.effective_context_size();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        initial,
        crate::daemon_services::fixtures::headless(),
    );

    let base = spawn(Arc::clone(&daemon)).await;
    let body: ContextWindow = reqwest::Client::new()
        .get(format!("{base}/v1/admin/context-window"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body.effective, None, "no slot is not a number");
    assert_eq!(body.n_ctx_train, None, "no model is not a ceiling");
    assert_ne!(
        body.effective,
        Some(body.configured),
        "absence must not be reported as agreement with the config"
    );
    // And `configured` is the DAEMON's own, from the config it was
    // commissioned with — not a re-read of the serving process's
    // `~/.svrnmesh/config.toml`.
    assert_eq!(body.configured, initial_ctx);
}

/// Seed one assistant message whose metadata carries the provenance the
/// rollup reads, plus one that carries none, and assert the route serves
/// the DAEMON's store's numbers.
///
/// The positive control is the point. Its partner below can only reach
/// the trait's `Err(NotImplemented)` default, which a handler
/// hard-coding a 501 would satisfy; this one cannot pass unless the
/// route actually asked a store that counted something (ARCH principle
/// 5, and the exact pairing `context_window`'s two tests carry).
///
/// WATCHED TO FAIL: with the handler's
/// `store.summarize_chat_activity(window_secs)` replaced by an
/// all-zero `ChatActivitySummary`, this test fails on `turns`
/// (`left: 0, right: 1`) while
/// `chat_activity_reports_a_store_that_declines_the_rollup` still
/// passes. Restored after.
#[tokio::test]
async fn chat_activity_reports_the_daemons_own_store() {
    use sovereign_core::traits::ConversationStore;

    let tmp = tempfile::tempdir().unwrap();
    let path = write_cfg(&tmp, "/m/primary.gguf");
    let initial = SetupConfig::load_from(&path).unwrap();

    let store = Arc::new(
        sovereign_store::sqlite::SqliteStateStore::open(&tmp.path().join("sovereign.db"))
            .expect("open sqlite state store"),
    );
    let now = sovereign_time::unix_now();
    // The shape the runtime persists: provenance nested under
    // `metadata["provenance"]`, `completion_tokens` preferred over
    // `tokens_used`, one retrieved source per corpus.
    //
    // Built from the TYPE, not from hand-written JSON, and that is not
    // style. My first draft spelled the object by hand with four of the
    // eleven fields; `ResponseProvenance` requires `intent`,
    // `search_method`, `oicp_match` and `total_latency_ms` with no serde
    // default, so `from_value` failed, the rollup SKIPPED the message
    // (it is best-effort by contract), and the route answered
    // `turns: 0`. The test caught it — that is the positive control
    // doing its job — but a fixture that cannot drift is better than one
    // that is checked once (ARCH principle 10, and §18.4: validate the
    // instrument before the result).
    let provenance = sovereign_contracts::types::ResponseProvenance {
        intent: "DeepQuery".into(),
        search_method: Some("CorpusEngine".into()),
        sources: vec![sovereign_contracts::types::SourceSummary {
            origin: "wikipedia".into(),
            count: 3,
            from_peer: None,
            display_name: None,
        }],
        inference_backend: "primary-122b".into(),
        oicp_match: None,
        total_latency_ms: 42,
        tokens_used: 999,
        coarse_intent: None,
        router: None,
        self_assessment: None,
        routing_trigger: None,
        coverage: None,
        finish_reason: None,
        max_tokens_budget: None,
        completion_tokens: Some(64),
        context_window: None,
    };
    store
        .save_message(&sovereign_contracts::types::Message {
            id: "m-1".into(),
            conversation_id: "c-1".into(),
            role: sovereign_contracts::types::Role::Assistant,
            content: "an answer".into(),
            created_at: now,
            metadata: Some(serde_json::json!({
                "provenance": serde_json::to_value(&provenance).unwrap(),
            })),
            version: now,
        })
        .await
        .unwrap();
    // A message with no provenance is SKIPPED, not counted as a turn —
    // so a route that merely counted rows would over-report and this
    // asserts the fold, not the SELECT.
    store
        .save_message(&sovereign_contracts::types::Message {
            id: "m-2".into(),
            conversation_id: "c-1".into(),
            role: sovereign_contracts::types::Role::Assistant,
            content: "a pre-provenance answer".into(),
            created_at: now,
            metadata: None,
            version: now,
        })
        .await
        .unwrap();

    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        initial,
        crate::daemon_services::fixtures::headless_with_store(store),
    );
    let base = spawn(Arc::clone(&daemon)).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/admin/chat-activity?window_secs=86400"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: ChatActivitySummary = resp.json().await.unwrap();
    assert_eq!(body.turns, 1, "one message carried provenance, not two");
    assert_eq!(
        body.tokens_generated, 64,
        "completion_tokens wins over tokens_used"
    );
    assert_eq!(body.chunks_retrieved, 3);
    assert_eq!(body.window_days, 1, "the window the caller asked for");
    assert_eq!(body.by_model.len(), 1);
    assert_eq!(body.by_model[0].model, "primary-122b");
    assert_eq!(body.by_corpus.len(), 1);
    assert_eq!(body.by_corpus[0].origin, "wikipedia");
    assert!(!body.by_corpus[0].from_peer);
}

/// A store that keeps no message metadata REFUSES, by name — it does
/// not answer an all-zero summary. "There is nothing to ask" and "you
/// ran no turns this week" render differently in a usage pane
/// (principle 6), and the in-memory store is exactly the case: it
/// inherits `ConversationStore::summarize_chat_activity`'s
/// `Err(NotImplemented)` default.
#[tokio::test]
async fn chat_activity_reports_a_store_that_declines_the_rollup() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_cfg(&tmp, "/m/primary.gguf");
    let initial = SetupConfig::load_from(&path).unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        initial,
        crate::daemon_services::fixtures::headless(),
    );

    let base = spawn(Arc::clone(&daemon)).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/admin/chat-activity"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        501,
        "a declined rollup is Not Implemented, not 200 with zeros"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    let err = body["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("summarize_chat_activity"),
        "the refusal must name the method that declined; got {err}"
    );
}

/// The window is the HOST's to clamp: a caller asking for zero (or a
/// negative) window would summarise nothing and the pane would render
/// "no activity", which is a claim about the user rather than about the
/// request.
#[tokio::test]
async fn chat_activity_clamps_the_window_to_at_least_a_day() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_cfg(&tmp, "/m/primary.gguf");
    let initial = SetupConfig::load_from(&path).unwrap();
    let store = Arc::new(
        sovereign_store::sqlite::SqliteStateStore::open(&tmp.path().join("sovereign.db"))
            .expect("open sqlite state store"),
    );
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        initial,
        crate::daemon_services::fixtures::headless_with_store(store),
    );
    let base = spawn(Arc::clone(&daemon)).await;
    let body: ChatActivitySummary = reqwest::Client::new()
        .get(format!("{base}/v1/admin/chat-activity?window_secs=0"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body.window_days, 1, "a zero window is clamped to one day");
}

/// Regression test for the production listener shape.
///
/// The daemon's real client listener uses
/// `router.into_make_service_with_connect_info::<SocketAddr>()`
/// so that `ConnectInfo<SocketAddr>` extractors in mesh_http,
/// admin_http, and mcp_router can read the peer address and
/// enforce the loopback-only guard. An earlier version of
/// `daemon.rs` used bare `axum::serve(listener, router)` which
/// made ConnectInfo extraction fail with 500 "Missing request
/// extension" — breaking the guards for legitimate localhost
/// callers (and, more subtly, defeating them for remote callers).
///
/// This test pins the correct shape so a future refactor can't
/// silently revert to the bare-serve pattern.
#[tokio::test]
async fn loopback_guard_works_under_production_listener_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_cfg(&tmp, "/m/primary.gguf");
    let initial = SetupConfig::load_from(&path).unwrap();

    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        initial,
        crate::daemon_services::fixtures::headless(),
    );

    let app = admin_router(Arc::clone(&daemon));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // Exact shape `daemon::start_daemon` uses — if this line
        // ever drifts from the production call site, the admin
        // surface breaks for localhost and this test must fail.
        let service = app.into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, service).await.ok();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/admin/reload"))
        .json(&serde_json::json!({ "config_path": path }))
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "loopback must pass the guard; got body: {}",
        resp.text().await.unwrap_or_default()
    );
}

mod reload;
