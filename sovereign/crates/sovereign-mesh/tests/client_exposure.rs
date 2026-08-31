// SPDX-License-Identifier: AGPL-3.0-or-later
//! Client-API exposure plumbing (2026-06 localhost-default + bearer).
//!
//! Pins the bind decision in `EmbeddedDaemon::start_daemon`:
//! - A daemon created WITHOUT `expose_client_api()` (the silent
//!   solo-mesh case) binds loopback-only and installs NO token —
//!   secure by default, single-user needs no auth.
//! - After `expose_client_api()` (explicit `mesh create`/`join`) the
//!   `client-exposed` marker persists, the next start binds `0.0.0.0`,
//!   and a bearer token is generated + installed for `client_auth`.
//!
//! `api_address()` reports the bind decision the daemon committed to;
//! `running_client_token()` reports the installed token. Custom ports
//! avoid colliding with a real daemon on 9741.
mod common;
use common::mesh_admin_services;

use std::collections::BTreeMap;
use std::path::PathBuf;

use sovereign_core::setup_config::{
    DaemonSection, DataSection, IrohSection, ModelsSection, SetupConfig,
};
use sovereign_mesh::daemon::EmbeddedDaemon;

fn cfg_with_ports(client_port: u16, internal_port: u16) -> SetupConfig {
    SetupConfig {
        engine: Default::default(),
        compute: Default::default(),
        search: Default::default(),
        models: ModelsSection {
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
        },
        daemon: DaemonSection {
            client_port,
            internal_port,
            ..Default::default()
        },
        data: DataSection::default(),
        watched_folders: Default::default(),
        memory: Default::default(),
        // Pinned off: the exposed-path test writes the client-exposed
        // marker, which auto-enables iroh (2026-07) — a real endpoint
        // bind + relay contact this hermetic test must not do.
        iroh: IrohSection {
            enabled: Some(false),
            ..Default::default()
        },
        shared_model: Default::default(),
        discovery: Default::default(),
        mcp_servers: Vec::new(),
    }
}

#[tokio::test]
async fn unexposed_solo_mesh_binds_loopback_with_no_token() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        dir.path().to_path_buf(),
        cfg_with_ports(38751, 38752),
        mesh_admin_services(),
    );
    // NO expose_client_api() — the silent solo-mesh path.
    daemon
        .create_mesh("solo", "node")
        .await
        .expect("create_mesh");

    let addr = daemon
        .api_address()
        .await
        .expect("api_address after create");
    assert!(
        addr.ip().is_loopback(),
        "unexposed daemon must bind loopback, got {addr}"
    );
    assert!(
        daemon.running_client_token().await.is_none(),
        "loopback-only daemon must NOT install a bearer token"
    );
    assert!(
        !sovereign_mesh::persist::client_exposed(dir.path()),
        "no marker should exist without expose_client_api()"
    );
    daemon.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn exposed_mesh_binds_wide_with_token_and_persists_marker() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        dir.path().to_path_buf(),
        cfg_with_ports(38851, 38852),
        mesh_admin_services(),
    );
    // Explicit share: expose BEFORE create so start_daemon binds wide
    // on first start (no restart).
    daemon.expose_client_api();
    let result = daemon
        .create_mesh("shared", "node")
        .await
        .expect("create_mesh");

    let addr = daemon
        .api_address()
        .await
        .expect("api_address after create");
    assert!(
        addr.ip().is_unspecified(),
        "exposed daemon must bind 0.0.0.0, got {addr}"
    );
    let token = daemon.running_client_token().await;
    assert!(
        token.is_some(),
        "exposed daemon must install a bearer token for client_auth"
    );
    assert_eq!(
        result.client_token, token,
        "CreateMeshResult.client_token must match the installed token (invite-screen value)"
    );
    assert_eq!(
        token.as_ref().map(|t| t.len()),
        Some(64),
        "256-bit hex token"
    );

    // Marker persists → a future restart re-binds wide without re-creating.
    assert!(
        sovereign_mesh::persist::client_exposed(dir.path()),
        "expose_client_api must persist the client-exposed marker"
    );
    daemon.shutdown().await.expect("shutdown");
}
