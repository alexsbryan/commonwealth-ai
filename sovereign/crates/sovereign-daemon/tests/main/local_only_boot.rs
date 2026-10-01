// SPDX-License-Identifier: AGPL-3.0-or-later
//! **The boot assertion for the local-only profile** — `cw-lift`'s
//! `cw-local-only-daemon` instrument, and the thing that replaced a manifest
//! count.
//!
//! # Why a boot, not a `cargo tree`
//!
//! The profile is a RUNTIME posture (see `sovereign_daemon::local_only`), so
//! its instrument is a runtime observation: the census of what a booted daemon
//! actually spawned (`RunningServices`), recorded at the spawn sites.
//!
//! Since pb-mesh-exit-transport the daemon binds no mesh endpoint, advertises
//! no mDNS and runs no gossip or ring round of its own — those are cw-rails',
//! and `svrn mesh up` hands this profile to it. What the census covers here is
//! the daemon's own mesh-facing loops: the peer-assisted ingest handoff, the
//! rail KV pump, and the origins it registers with cw-rails.
//!
//! # The control, and why it is load-bearing
//!
//! `the_control_a_networked_daemon_spawns_every_gated_loop` boots the SAME
//! code with the profile off and asserts the loops start. Without it the first
//! test passes vacuously the day someone deletes the spawns outright: "no
//! network service" is trivially true of a daemon that does nothing.
//!
//! # What the profile does NOT skip
//!
//! The model: `/v1/models` answers. A profile that made the daemon local-only
//! by making it useless would satisfy the census and fail the product.

use crate::common::mesh_admin_services;

use std::collections::BTreeMap;
use std::path::PathBuf;

use sovereign_contracts::setup_config::{DaemonSection, DataSection, ModelsSection, SetupConfig};
use sovereign_daemon::daemon::EmbeddedDaemon;
use sovereign_daemon::local_only::MeshService;

/// Every loop the profile gates.
const EVERY_NETWORK_SERVICE: &[MeshService] = MeshService::ALL;

/// A config on ports nothing else in the suite uses, below Linux's ephemeral
/// range (32768-60999): at 39751 another test's `bind(0)` held the port and
/// answered `/v1/models` with a 404 (pb-test-load).
fn cfg(client_port: u16, internal_port: u16, local_only: bool) -> SetupConfig {
    SetupConfig {
        engine: Default::default(),
        compute: Default::default(),
        search: Default::default(),
        models: Some(ModelsSection {
            primary: PathBuf::from("/models/primary.gguf"),
            fast: Some(PathBuf::from("/models/fast.gguf")),
            embed: PathBuf::from("/models/embed.gguf"),
            code: None,
            context_size: None,
            fast_context_size: None,
            max_extras_memory_gb: None,
            extra: BTreeMap::new(),
            primary_pool: None,
            edit: None,
            kinds: Default::default(),
        }),
        node: Default::default(),
        daemon: DaemonSection {
            client_port,
            internal_port,
            local_only,
            ..Default::default()
        },
        data: DataSection::default(),
        watched_folders: Default::default(),
        memory: Default::default(),
        iroh: Default::default(),
        shared_model: Default::default(),
        discovery: Default::default(),
        mcp_servers: Vec::new(),
    }
}

/// THE assertion. A local-only daemon boots, serves, and has started nothing
/// that talks to another machine.
#[tokio::test]
async fn a_local_only_daemon_spawns_no_network_service() {
    // The daemon's store ports dial cw-rails (five-programs fp-88), so the
    // model list is read from a stand-in door on `[daemon] rails_base`,
    // which records each read it serves. The door holds no models and no plan.
    // It stays a stand-in because no daemon boot brings cw-rails up
    // (pb-rails-untether); the real cw-rails is proven by sovereign-cli-mesh
    // tests/rails_up_e2e.rs.
    let reads: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>> = Default::default();
    let door = crate::common::spawn_router(
        axum::Router::new()
            .route(
                "/v1/ledger/inference/models",
                axum::routing::get({
                    let reads = reads.clone();
                    move || async move {
                        reads.lock().unwrap().push("models");
                        axum::Json(Vec::<()>::new())
                    }
                }),
            )
            .route(
                "/v1/ledger/inference/plan",
                axum::routing::get({
                    let reads = reads.clone();
                    move || async move {
                        reads.lock().unwrap().push("plan");
                        axum::Json(None::<()>)
                    }
                }),
            ),
    )
    .await;
    let mut config = cfg(29751, 29752, true);
    config.daemon.rails_base = Some(format!("http://{door}"));
    let dir = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(dir.path().to_path_buf(), config, mesh_admin_services());

    daemon.start().await.expect("a local-only daemon boots");

    let (profile, services) = daemon
        .running_services()
        .await
        .expect("a started daemon reports its services census");
    assert!(
        profile.is_local_only(),
        "the config asked for the local-only profile and the daemon resolved {}",
        profile.label()
    );

    // Every member of the closed set, named individually, so a failure says
    // WHICH loop came back rather than "the census was non-empty".
    for service in EVERY_NETWORK_SERVICE {
        assert!(
            !services.contains(*service),
            "a local-only daemon spawned `{}` — the census reads {:?}",
            service.as_str(),
            services.names()
        );
    }
    assert!(
        !services.any_network_service(),
        "local-only census must be empty, got {:?}",
        services.names()
    );

    // ...and it is still a daemon. The profile skips the NETWORK, not the
    // model: the client API answers.
    let addr = daemon
        .api_address()
        .await
        .expect("a local-only daemon still binds its client API");
    // `api_address` reports the bind the daemon COMMITTED to; the listener
    // itself binds inside the spawned serve task, so a bounded poll is the
    // honest wait. A hang here is a real failure mode (the daemon never
    // serving), so it is bounded rather than open-ended.
    let status = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Ok(resp) = reqwest::Client::new()
                .get(format!("http://{addr}/v1/models"))
                .send()
                .await
            {
                return resp.status();
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("a local-only daemon serves its client API within 10s");
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "GET /v1/models on a local-only daemon"
    );
    assert!(
        reads.lock().unwrap().contains(&"models"),
        "the model list was read from the declared door, got {:?}",
        reads.lock().unwrap()
    );

    daemon.shutdown().await.expect("shutdown");
}

/// THE CONTROL. The same code with the profile off starts every gated loop
/// that needs no corpus engine, so the assertion above cannot pass by the
/// loops having been deleted. (`WorkOrigin` also needs an engine, which this
/// mesh-admin daemon has none of.)
#[tokio::test]
async fn the_control_a_networked_daemon_spawns_every_gated_loop() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        dir.path().to_path_buf(),
        cfg(29851, 29852, false),
        mesh_admin_services(),
    );
    daemon.start().await.expect("start");

    let (profile, services) = daemon
        .running_services()
        .await
        .expect("a started daemon reports its services census");
    assert!(!profile.is_local_only(), "the control must be networked");

    for service in [
        MeshService::AutoIngestCollaborate,
        MeshService::RailKvPump,
        MeshService::PeerOrigin,
        MeshService::GuestOrigin,
    ] {
        assert!(
            services.contains(service),
            "a networked daemon must spawn `{}` — the census reads {:?}",
            service.as_str(),
            services.names()
        );
    }

    daemon.shutdown().await.expect("shutdown");
}
