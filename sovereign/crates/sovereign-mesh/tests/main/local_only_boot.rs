// SPDX-License-Identifier: AGPL-3.0-or-later
//! **The boot assertion for the local-only profile** — `cw-lift`'s
//! `cw-local-only-daemon` instrument, and the thing that replaced a manifest
//! count.
//!
//! # Why a boot, not a `cargo tree`
//!
//! The bar's instrument counted BLOCKER CLASSES against the daemon's manifest.
//! Rungs 3a/3b took the direct `commonwealth-*` deps to zero and the claim
//! ("compiles and boots with zero commonwealth-\* and no iroh") still was not
//! met: ten commonwealth crates ride `sovereign-mesh` transitively, and the
//! 2026-09-08 fusion census measured that no seam removes them — 31 of 78
//! reached items are unmovable, 21 of those are the daemon's own lifecycle,
//! and the best cut moves 15,634 lines for zero deleted dependencies. K2
//! fired: the honest deliverable is a RUNTIME profile, so the honest
//! instrument is a RUNTIME observation.
//!
//! # The named failing inputs, watched red (ARCH §18.1, §5)
//!
//! Four sabotages were run before this file was committed; each turned
//! exactly the assertion that names it, and nothing else:
//!
//! | reverted | red | message |
//! |---|---|---|
//! | the four-loop gate in `start_daemon` | `..._spawns_no_network_service` | spawned `gossip` — census `["gossip", "auto_ingest_collaborate", "ring_sync", "rail_kv_pump"]` |
//! | `mdns_enabled_effective` stops reading the profile | same | spawned `mdns_advertise` — census `["mdns_advertise", "mdns_browse"]` |
//! | `iroh_access::resolve_enabled` stops reading the profile | same | spawned `iroh_endpoint` — census `["iroh_endpoint", "iroh_watchdog"]` |
//! | the local-only/`require_encryption` refusal | `..._refused_by_name` | the boot still fails, but on WS-B's "iroh endpoint failed to bind" — which names neither setting that disagrees |
//!
//! The fourth is the sharpest of them and the reason that test asserts on the
//! message rather than on `is_err()`: without the early refusal the daemon
//! still stops, so a weaker assertion would stay green while the operator is
//! told a socket failed to bind when what actually happened is that two
//! settings contradict each other.
//!
//! # The control, and why it is load-bearing
//!
//! `the_control_a_networked_daemon_spawns_every_gated_loop` boots the SAME
//! code with the profile off and asserts all four loops start. Without it the
//! first test passes vacuously the day someone deletes the spawns outright:
//! "no network service" is trivially true of a daemon that does nothing.
//!
//! # What the profile does NOT skip
//!
//! The mesh-of-one. `create_mesh` succeeds, the mesh persists with exactly
//! one member, and `/v1/models` answers — the solo case is the honest N=1,
//! never an `Option<Mesh>` (see `sovereign_mesh::local_only`). Both are
//! asserted here, because a profile that made the daemon local-only by making
//! it useless would satisfy the census and fail the product.
//!
//! Hermetic by construction: the local-only config asks for mDNS **on** and
//! `[iroh] enabled = true`, and the profile must still leave both off — so
//! this test never binds a multicast socket or contacts a relay, and the
//! assertion is a real differential rather than a restatement of the config.

use crate::common::mesh_admin_services;

use std::collections::BTreeMap;
use std::path::PathBuf;

use sovereign_core::setup_config::{
    DaemonSection, DataSection, DiscoverySection, IrohSection, ModelsSection, SetupConfig,
    WorkOfferSection,
};
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::local_only::MeshService;

/// The four loops the profile newly gates, plus the two that already had
/// runtime off-switches and now read the same decider.
const EVERY_NETWORK_SERVICE: &[MeshService] = MeshService::ALL;

/// A config on ports nothing else in the suite uses.
///
/// `mdns` and `[iroh] enabled` are deliberately set to the values that would
/// turn each on, so the local-only run proves the PROFILE is what keeps them
/// off. The control flips them to `false` — it must not open a multicast
/// socket or a relay connection just to observe four unrelated loops.
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
        iroh: IrohSection {
            enabled: Some(local_only),
            ..Default::default()
        },
        shared_model: Default::default(),
        discovery: DiscoverySection {
            mdns: local_only,
            ..Default::default()
        },
        mcp_servers: Vec::new(),
    }
}

/// The same config with an ACTIVE `[compute.work_offer]`.
///
/// `accept = "nobody"` on purpose: the question is whether the donor LOOP is
/// spawned, and a test that also opened this box to somebody else's argv
/// would be answering a second question with a live process group.
fn cfg_donating(client_port: u16, internal_port: u16, local_only: bool) -> SetupConfig {
    let mut c = cfg(client_port, internal_port, local_only);
    c.compute.work_offer = WorkOfferSection {
        kinds: vec!["process:v1".to_string()],
        max_concurrent: 1,
        ..Default::default()
    };
    c
}

/// **THE DONOR GATE (cw-lift 5d).** A local-only daemon does not donate.
///
/// Written as a DIFFERENTIAL in one test because the absence half is
/// worthless alone: `work_donor` is also absent from a daemon whose
/// `[compute.work_offer]` is inert, which is every daemon shipped today. So
/// the same config — same kinds, same concurrency, same ports-modulo-collision
/// — is booted twice and only the profile moves.
///
/// Watched red by moving `spawn_work_donor` out of `start_daemon`'s
/// local-only branch: the census then read `["work_donor"]` on the local-only
/// boot and this assertion named the loop that came back.
#[tokio::test]
async fn a_local_only_daemon_does_not_donate() {
    let quiet = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        quiet.path().to_path_buf(),
        cfg_donating(39651, 39652, true),
        mesh_admin_services(),
    );
    daemon
        .create_mesh("solo", "node")
        .await
        .expect("a local-only daemon with a work offer still boots");
    let (profile, services) = daemon
        .running_services()
        .await
        .expect("a started daemon reports its services census");
    assert!(profile.is_local_only(), "the config asked for local-only");
    assert!(
        !services.contains(MeshService::WorkDonor),
        "a local-only daemon donated — the census reads {:?}. Donating is a \
         conversation with a peer and the profile says there is no other side",
        services.names()
    );
    daemon.shutdown().await.expect("shutdown");

    // THE CONTROL, AND WHY IT CHANGED SHAPE ON 2026-09-10.
    //
    // It used to boot the SAME offer on a networked daemon and assert the
    // loop came back, which is what made the absence above mean "local-only
    // stopped it" rather than "nothing was offered". That control cannot run
    // through `process:v1` any more: the isolation floor refuses that kind at
    // boot on every build in this tree, so a networked daemon carrying this
    // config also spawns no donor, and asserting the absence on both sides
    // would be the exact vacuous pass this test's own doc warns about.
    //
    // So the control now proves the ALTERNATIVE explanation instead of the
    // positive one: the networked boot's silence is fully accounted for by
    // the floor, named, and therefore not evidence about local-only either
    // way. The assertion above keeps its meaning because this one pins the
    // only other reason the loop could be missing.
    //
    // RE-ARM THE POSITIVE CONTROL when a kind this build can isolate can be
    // offered from this harness — a container-backed executor, or `ingest:v1`
    // once these services carry a corpus engine. Tracked by the
    // `process:v1` donation row in `sovereign/DEFAULTS_LEDGER.md`; that row
    // graduating and this control staying in its negative form is the thing
    // to catch.
    let resolved = sovereign_mesh::work_donor::resolve_offer(
        &cfg_donating(39661, 39662, false).compute.work_offer,
        &sovereign_mesh::work_donor::donor_registry(
            None,
            commonwealth_work::sandbox::Sandbox::Direct,
        ),
        "linux",
        "x86_64",
        sovereign_mesh::work_donor::DONOR_ISOLATION,
    )
    .expect("the floor drops the kind rather than taking the daemon down");
    assert_eq!(
        resolved, None,
        "the networked boot's missing donor must be explained by the \
         isolation floor dropping `process:v1` — otherwise the absence \
         above proves nothing about local-only"
    );
}

/// THE assertion. A local-only daemon boots, serves, holds a one-member mesh,
/// and has started nothing that talks to another machine.
#[tokio::test]
async fn a_local_only_daemon_spawns_no_network_service() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        dir.path().to_path_buf(),
        cfg(39751, 39752, true),
        mesh_admin_services(),
    );

    // The mint that `daemon_cmd` performs at boot. Under the profile it must
    // SUCCEED without mDNS register/browse — the campaign's CLASS 3 blocker.
    daemon
        .create_mesh("solo", "node")
        .await
        .expect("a local-only daemon boots its solo mesh with no discovery");

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
    // model: the client API answers and the mesh-of-one exists.
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

    let state = daemon.app_state().await.expect("running daemon has state");
    assert_eq!(
        state.inner.mesh.read().await.members.len(),
        1,
        "the mesh-of-one is still minted — the profile skips the network, not the model"
    );

    daemon.shutdown().await.expect("shutdown");
}

/// THE CONTROL. The same code with the profile off starts all four gated
/// loops, so the assertion above cannot pass by the loops having been deleted.
///
/// mDNS and iroh are configured OFF here on purpose: their gates are covered
/// by `iroh_access::resolve_enabled_matrix` and `client_exposure`, and this
/// test must stay hermetic. What it exists to prove is that the profile — and
/// nothing else in this config — is what silenced the four.
#[tokio::test]
async fn the_control_a_networked_daemon_spawns_every_gated_loop() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        dir.path().to_path_buf(),
        cfg(39851, 39852, false),
        mesh_admin_services(),
    );
    daemon.create_mesh("solo", "node").await.expect("create");

    let (profile, services) = daemon
        .running_services()
        .await
        .expect("a started daemon reports its services census");
    assert!(!profile.is_local_only(), "the control must be networked");

    for service in [
        MeshService::Gossip,
        MeshService::AutoIngestCollaborate,
        MeshService::RingSync,
        MeshService::RailKvPump,
    ] {
        assert!(
            services.contains(service),
            "a networked daemon must spawn `{}` — the census reads {:?}",
            service.as_str(),
            services.names()
        );
    }
    // The two that were already gated stay off, from their OWN config, which
    // is what keeps this test hermetic.
    assert!(!services.contains(MeshService::MdnsAdvertise));
    assert!(!services.contains(MeshService::IrohEndpoint));

    daemon.shutdown().await.expect("shutdown");
}

/// The one contradiction the profile refuses rather than resolving: an
/// encrypted mesh must be dialable by key, and local-only binds no endpoint.
/// Refused loudly at boot (ARCH §18.3) instead of silently downgrading to
/// plaintext or silently binding the endpoint the profile promised not to.
#[tokio::test]
async fn local_only_plus_require_encryption_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        dir.path().to_path_buf(),
        cfg(39951, 39952, true),
        mesh_admin_services(),
    );
    let msg = match daemon.create_mesh_with("encrypted", "node", true).await {
        Err(e) => e.to_string(),
        Ok(_) => panic!("local-only + require_encryption must not boot"),
    };
    assert!(
        msg.contains("local-only") && msg.contains("require_encryption"),
        "the refusal must name BOTH settings that disagree, got: {msg}"
    );
    assert!(
        !daemon.is_running().await,
        "a refused boot must leave the daemon stopped, not half-started"
    );
}
