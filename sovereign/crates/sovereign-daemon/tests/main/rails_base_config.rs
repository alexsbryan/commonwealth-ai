//! `[daemon] rails_base` — where this daemon dials cw-rails (fp-112).
//!
//! Before the key, the base was the compiled `DEFAULT_RAILS_BASE`, so every
//! `EmbeddedDaemon` test dialed :9747, the port a developer's live cw-rails
//! listens on. These pin the one resolver's two answers on `NodeSeed`, and
//! watch a booted daemon's ring-rail read land on a stand-in door instead.

use crate::common::{mesh_admin_services, spawn_router};

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::RawQuery;
use axum::routing::get;
use axum::Router;
use sovereign_contracts::setup_config::{
    DaemonSection, DataSection, DiscoverySection, IrohSection, ModelsSection, SetupConfig,
};
use sovereign_daemon::daemon::EmbeddedDaemon;
use sovereign_daemon::rails_client::DEFAULT_RAILS_BASE;
use sovereign_daemon::state::node::NodeSeed;

/// A local-only config (no discovery, no relay) on ports nothing else in the
/// suite uses, dialing cw-rails at `rails_base` when given.
fn cfg(rails_base: Option<String>) -> SetupConfig {
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
            client_port: 29771,
            internal_port: 29772,
            local_only: true,
            rails_base,
            ..Default::default()
        },
        data: DataSection::default(),
        watched_folders: Default::default(),
        memory: Default::default(),
        iroh: IrohSection {
            enabled: Some(false),
            ..Default::default()
        },
        shared_model: Default::default(),
        discovery: DiscoverySection {
            mdns: false,
            ..Default::default()
        },
        mcp_servers: Vec::new(),
    }
}

async fn seed_base(rails_base: Option<String>) -> String {
    let dir = tempfile::tempdir().unwrap();
    let config = tokio::sync::RwLock::new(cfg(rails_base));
    NodeSeed::resolved(None, dir.path(), &config)
        .await
        .expect("the seed resolves")
        .rails_base
}

#[tokio::test]
async fn a_declared_rails_base_is_the_seeds_and_an_absent_one_is_the_default() {
    assert_eq!(
        seed_base(Some("http://127.0.0.1:39779".into())).await,
        "http://127.0.0.1:39779"
    );
    assert_eq!(seed_base(None).await, DEFAULT_RAILS_BASE);
}

/// THE boot-path half. `start_daemon` builds its `RailsRingRail` before the
/// node seed resolves, so it reads the key through the same resolver; a
/// `/v1/rail/log` read must dial the declared door's roster, not :9747.
#[tokio::test]
async fn a_booted_daemons_ring_rail_dials_the_declared_door() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let door = spawn_router(Router::new().route(
        "/v1/rail/roster",
        get({
            let seen = seen.clone();
            move |RawQuery(q): RawQuery| async move {
                seen.lock().unwrap().push(q.unwrap_or_default());
                axum::http::StatusCode::SERVICE_UNAVAILABLE
            }
        }),
    ))
    .await;

    let dir = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        dir.path().to_path_buf(),
        cfg(Some(format!("http://{door}"))),
        mesh_admin_services(),
    );
    daemon.start().await.expect("a local-only daemon boots");
    let addr = daemon
        .api_address()
        .await
        .expect("the daemon binds its client API");

    // The listener binds inside the spawned serve task: a bounded poll.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if reqwest::Client::new()
                .get(format!("http://{addr}/v1/rail/log?namespace=fp112-probe"))
                .send()
                .await
                .is_ok()
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the daemon serves its client API within 10s");

    assert_eq!(
        seen.lock().unwrap().as_slice(),
        ["namespace=fp112-probe"],
        "the ring rail's roster read reached the declared door"
    );
}
