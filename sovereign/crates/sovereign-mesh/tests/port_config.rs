// SPDX-License-Identifier: AGPL-3.0-or-later
//! Port-config plumbing test.
//!
//! Pins that the `SetupConfig.daemon.{client_port, internal_port}`
//! fields the operator can already set in `~/.sovereign/config.toml`
//! actually drive the listener bind decision inside
//! `EmbeddedDaemon::start_daemon`. Pre-fix, those fields were
//! defined but ignored: the daemon bound `0.0.0.0:9741` /
//! `0.0.0.0:9742` regardless.
//!
//! Why this matters beyond operator ergonomics: integration tests
//! that need to spin up multiple `EmbeddedDaemon`s in one process
//! couldn't, because every `create_mesh` call raced the same
//! hardcoded ports. Tier-1+ integration coverage of the daemon's
//! real lifecycle is what this PR unblocks.
//!
//! The bind itself runs inside a `tokio::spawn` that doesn't surface
//! errors, so a port collision in the test environment won't fail
//! the test — but the `api_address()` value is the *decision* the
//! daemon committed to before spawning the listener, which is the
//! assertion that matters: did the SetupConfig flow through?
use std::collections::BTreeMap;
use std::path::PathBuf;

use sovereign_core::setup_config::{DaemonSection, DataSection, ModelsSection, SetupConfig};
use sovereign_mesh::daemon::EmbeddedDaemon;

fn cfg_with_ports(client_port: u16, internal_port: u16) -> SetupConfig {
    SetupConfig {
        models: ModelsSection {
            primary: PathBuf::from("/models/primary.gguf"),
            fast: Some(PathBuf::from("/models/fast.gguf")),
            embed: PathBuf::from("/models/embed.gguf"),
            code: None,
            context_size: None,
            max_extras_memory_gb: None,
            extra: BTreeMap::new(),
            primary_pool: None,
        },
        daemon: DaemonSection {
            client_port,
            internal_port,
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

#[tokio::test]
async fn default_ports_used_when_setup_config_absent() {
    // No `set_setup_config` call → `resolved_ports()` returns the
    // historic (9741, 9742) defaults. After `create_mesh`,
    // `api_address()` exposes the chosen client bind decision.
    let daemon = EmbeddedDaemon::new_in_memory();
    daemon
        .create_mesh("default-port test", "node")
        .await
        .expect("create_mesh succeeds against an empty in-memory daemon");

    let addr = daemon
        .api_address()
        .await
        .expect("daemon must report an api_address after create_mesh");
    assert_eq!(
        addr.port(),
        9741,
        "no setup_config installed → daemon binds the historic 9741 default"
    );

    daemon.shutdown().await.expect("graceful shutdown");
}

#[tokio::test]
async fn custom_client_port_from_setup_config_flows_to_api_address() {
    // Operator sets a non-default `client_port` in their config.
    // After `set_setup_config` + `create_mesh`, the daemon's
    // bind decision must reflect it. Pre-fix this was a silent
    // no-op (operator changed the TOML, daemon still bound 9741).
    let daemon = EmbeddedDaemon::new_in_memory();
    daemon.set_setup_config(cfg_with_ports(39741, 39742)).await;
    daemon
        .create_mesh("custom-port test", "node")
        .await
        .expect("create_mesh succeeds with custom-port config");

    let addr = daemon
        .api_address()
        .await
        .expect("daemon must report an api_address after create_mesh");
    assert_eq!(
        addr.port(),
        39741,
        "setup_config.daemon.client_port must drive the client bind decision; \
         api_address still reports 9741 means the wiring is broken"
    );

    daemon.shutdown().await.expect("graceful shutdown");
}

// Note: a direct assertion that the configured `internal_port` flows
// to `MemberRecord.addresses` would duplicate what test 2 already
// proves — both paths route through the same `resolved_ports()`
// helper, so if `client_port` flows correctly, `internal_port`
// flows correctly too. `MeshState::MeshMember` deliberately doesn't
// surface raw addresses (privacy concern in the UI surface), so
// asserting through the public API would require either a new
// accessor or reaching into `AppState.inner` from tests. Both add
// surface for a duplicated assertion — not worth it.
