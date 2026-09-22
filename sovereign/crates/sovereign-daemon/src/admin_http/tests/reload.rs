use super::*;

#[tokio::test]
async fn reload_is_noop_when_nothing_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_cfg(&tmp, "/m/primary.gguf");
    let initial = SetupConfig::load_from(&path).unwrap();

    let counter = Arc::new(AtomicUsize::new(0));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        initial,
        crate::daemon_services::fixtures::headless_with_factory(Arc::new(StubFactory {
            build_count: Arc::clone(&counter),
        })),
    );

    let base = spawn(Arc::clone(&daemon)).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/admin/reload"))
        .json(&serde_json::json!({ "config_path": path }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: ReloadResponse = resp.json().await.unwrap();
    assert!(body.reloaded_fields.is_empty());
    assert!(!body.restart_required);
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "factory must not be called"
    );
}

#[tokio::test]
async fn reload_swaps_inference_provider_when_models_change() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_cfg(&tmp, "/m/primary-v1.gguf");
    let initial = SetupConfig::load_from(&path).unwrap();

    let counter = Arc::new(AtomicUsize::new(0));
    // The headless profile carries the factory; the initial provider comes
    // in through the core ring, so there is no seeding step any more.
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        initial,
        crate::daemon_services::fixtures::headless_with_factory(Arc::new(StubFactory {
            build_count: Arc::clone(&counter),
        })),
    );

    // Change models.primary on disk, then POST reload.
    let _ = write_cfg(&tmp, "/m/primary-v2.gguf");

    let base = spawn(Arc::clone(&daemon)).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/admin/reload"))
        .json(&serde_json::json!({ "config_path": path }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: ReloadResponse = resp.json().await.unwrap();
    assert_eq!(body.reloaded_fields, vec!["models.primary".to_string()]);
    assert!(!body.restart_required);
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "factory must be invoked exactly once"
    );
}

/// RED before the `models.context_size` arm was added to `ConfigDiff`.
///
/// `svrn model context 65536` wrote the config, then reported "no config
/// changes detected — nothing to reload" while the daemon kept serving
/// 32,764 — a success message for work that did not happen (§18.3).
/// The window IS hot-reloadable: `build_provider` reads
/// `effective_context_size()` and rebuilds every slot. The diff simply
/// never looked at the field, so `is_noop()` short-circuited the rebuild.
#[tokio::test]
async fn reload_applies_a_context_size_change_without_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_cfg(&tmp, "/m/primary.gguf");
    let initial = SetupConfig::load_from(&path).unwrap();
    assert_eq!(
        initial.models().unwrap().context_size,
        None,
        "fixture starts at auto"
    );

    // Commissioned through the total constructor, like every other test in
    // this file. These two tests arrived on main written against the
    // `set_*` builders daemon-convergence Phase 2 deleted; the merge took
    // both sides' text and only the compiler noticed.
    let counter = Arc::new(AtomicUsize::new(0));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        initial.clone(),
        crate::daemon_services::fixtures::headless_with_factory(Arc::new(StubFactory {
            build_count: Arc::clone(&counter),
        })),
    );

    let mut modified = initial;
    modified.models.as_mut().unwrap().context_size = Some(65_536);
    modified.save_to(&path).unwrap();

    let base = spawn(Arc::clone(&daemon)).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/admin/reload"))
        .json(&serde_json::json!({ "config_path": path }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: ReloadResponse = resp.json().await.unwrap();
    assert_eq!(
        body.reloaded_fields,
        vec!["models.context_size".to_string()],
        "the window must be REPORTED as reloaded, not silently ignored"
    );
    assert!(
        !body.restart_required,
        "the factory rebuilds every slot from cfg — no restart is needed"
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "a context change must actually rebuild the provider"
    );
}

/// RED before the `models.extra` arm was added to `ConfigDiff`.
///
/// `svrn model set-extra Qwen3.8-27B <file>` wrote `[models.extra]`, the
/// CLI reported "no config changes detected — nothing to reload", and
/// `/v1/models` never advertised the name — so every call pinned to it
/// answered 503 "no node in this mesh advertises model" until a restart.
/// The failing input is the writer's own output: a config that differs
/// from the running one ONLY in the extras map.
#[tokio::test]
async fn reload_applies_an_extra_slot_without_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_cfg(&tmp, "/m/primary.gguf");
    let initial = SetupConfig::load_from(&path).unwrap();
    assert!(
        initial.models().unwrap().extra.is_empty(),
        "fixture starts with no extras"
    );

    let counter = Arc::new(AtomicUsize::new(0));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        initial.clone(),
        crate::daemon_services::fixtures::headless_with_factory(Arc::new(StubFactory {
            build_count: Arc::clone(&counter),
        })),
    );

    let mut modified = initial;
    modified
        .models
        .as_mut()
        .unwrap()
        .extra
        .insert("judge-27b".to_string(), PathBuf::from("/m/judge-27b.gguf"));
    modified.save_to(&path).unwrap();

    let base = spawn(Arc::clone(&daemon)).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/admin/reload"))
        .json(&serde_json::json!({ "config_path": path }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: ReloadResponse = resp.json().await.unwrap();
    assert_eq!(
        body.reloaded_fields,
        vec!["models.extra".to_string()],
        "an added extra slot must be REPORTED as reloaded, not silently ignored"
    );
    assert!(
        !body.restart_required,
        "extras are built by the same factory — no restart"
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "an extras change must actually rebuild the provider"
    );
}

/// The code slot had the same hole: `build_provider` passes
/// `cfg.models.code` to the loader, but the diff never compared it, so
/// `svrn model set code <file>` on a running daemon was a no-op that
/// reported success.
#[tokio::test]
async fn reload_applies_a_code_slot_change_without_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_cfg(&tmp, "/m/primary.gguf");
    let initial = SetupConfig::load_from(&path).unwrap();

    // Commissioned through the total constructor, like every other test in
    // this file. These two tests arrived on main written against the
    // `set_*` builders daemon-convergence Phase 2 deleted; the merge took
    // both sides' text and only the compiler noticed.
    let counter = Arc::new(AtomicUsize::new(0));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        initial.clone(),
        crate::daemon_services::fixtures::headless_with_factory(Arc::new(StubFactory {
            build_count: Arc::clone(&counter),
        })),
    );

    let mut modified = initial;
    modified.models.as_mut().unwrap().code = Some(PathBuf::from("/m/coder.gguf"));
    modified.save_to(&path).unwrap();

    let base = spawn(Arc::clone(&daemon)).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/admin/reload"))
        .json(&serde_json::json!({ "config_path": path }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: ReloadResponse = resp.json().await.unwrap();
    assert_eq!(body.reloaded_fields, vec!["models.code".to_string()]);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn reload_port_change_requires_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_cfg(&tmp, "/m/primary.gguf");
    let initial = SetupConfig::load_from(&path).unwrap();

    let counter = Arc::new(AtomicUsize::new(0));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        initial.clone(),
        crate::daemon_services::fixtures::headless_with_factory(Arc::new(StubFactory {
            build_count: Arc::clone(&counter),
        })),
    );

    // Rewrite config with a different client_port.
    let mut modified = initial;
    modified.daemon.client_port = 19741;
    modified.save_to(&path).unwrap();

    let base = spawn(Arc::clone(&daemon)).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/admin/reload"))
        .json(&serde_json::json!({ "config_path": path }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: ReloadResponse = resp.json().await.unwrap();
    assert!(body.reloaded_fields.is_empty());
    assert!(body.restart_required);
    assert_eq!(
        body.restart_required_fields,
        vec!["daemon.client_port".to_string()]
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "port-only change must not rebuild provider"
    );
}

/// `[iroh] media_origin` set and then `reload`ed: the acceptor built
/// BEFORE the reload forwards a member's media dial to the new origin, and
/// nothing asks for a restart. Red while the origin was read once at
/// acceptor construction (ring-room 99ca7e4cb leg 3: the restart that
/// carried it left peers dialing a stale endpoint for 120 s).
#[tokio::test]
async fn reload_moves_the_media_origin_without_a_restart() {
    use commonwealth_core::ids::NodePubkey;
    use sovereign_mesh::iroh_access::{AcceptorRoutes, AppRoutes, OfferRoutes};
    let tmp = tempfile::tempdir().unwrap();
    let path = write_cfg(&tmp, "/m/primary.gguf");
    let initial = SetupConfig::load_from(&path).unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        initial.clone(),
        crate::daemon_services::fixtures::headless_with_factory(Arc::new(StubFactory {
            build_count: Arc::new(AtomicUsize::new(0)),
        })),
    );
    let loopback: SocketAddr = ([127, 0, 0, 1], 9742).into();
    let routes = AcceptorRoutes {
        internal: loopback,
        peer: None,
        guest: None,
        rpc: None,
        media: daemon.media_route().clone(),
        apps: AppRoutes::default(),
        offer: OfferRoutes::default(),
    };
    let member = NodePubkey([7u8; 32]);
    let check: sovereign_mesh::iroh_access::MemberCheck = Arc::new(|_| {
        Box::pin(std::future::ready(Some(
            sovereign_mesh::iroh_access::MemberIdentity {
                name: "Bo".into(),
                node_id: commonwealth_core::ids::NodeId::from_u128(0xB0),
            },
        )))
    });
    let media = sovereign_contracts::transport::MEDIA_ALPN;
    assert_eq!(routes.forward_for(media, member, &check).await, None);

    let mut offered = initial;
    offered.iroh.media_origin = Some("127.0.0.1:8096".into());
    offered.save_to(&path).unwrap();
    let base = spawn(Arc::clone(&daemon)).await;
    let body: ReloadResponse = reqwest::Client::new()
        .post(format!("{base}/v1/admin/reload"))
        .json(&serde_json::json!({ "config_path": path }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body.reloaded_fields, vec!["iroh.media_origin".to_string()]);
    assert!(!body.restart_required, "{body:?}");
    let forward = routes.forward_for(media, member, &check).await;
    assert_eq!(
        forward.and_then(|f| f.target()),
        Some(([127, 0, 0, 1], 8096).into()),
        "the live acceptor answers on the reloaded origin"
    );
}

#[tokio::test]
async fn reload_without_factory_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_cfg(&tmp, "/m/primary-v1.gguf");
    let initial = SetupConfig::load_from(&path).unwrap();

    // The DESKTOP profile, which declares it carries no ProviderFactory.
    // The refusal must name the profile rather than report a missing
    // installation — nothing is missing, this shape has no factory.
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        initial,
        crate::daemon_services::fixtures::desktop(),
    );

    let _ = write_cfg(&tmp, "/m/primary-v2.gguf");

    let base = spawn(Arc::clone(&daemon)).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/admin/reload"))
        .json(&serde_json::json!({ "config_path": path }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 500);
}
