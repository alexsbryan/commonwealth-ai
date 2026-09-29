// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the embedded daemon — see `daemon.rs`.
//!
//! Their own file because keeping them inline put that file past its
//! arch-gate slack (ARCH §3.1). `#[path]`, so the names are unchanged.

use super::*;
use sovereign_core::setup_config::{DaemonSection, DataSection, ModelsSection, SetupConfig};
use std::path::PathBuf;

#[test]
fn internal_bind_is_loopback_only_under_encryption() {
    // WS-C receiver lockout: an encrypted mesh binds the internal
    // router loopback-only (iroh acceptor is the sole network path);
    // a plaintext mesh keeps the historical wildcard bind.
    let net = crate::local_only::LocalOnlyProfile::default();
    let encrypted = internal_bind_addr(net, true, "0.0.0.0", 9742);
    assert!(
        encrypted.ip().is_loopback(),
        "encrypted mesh must bind internal router loopback-only, got {encrypted}"
    );
    assert_eq!(encrypted.port(), 9742);

    let plaintext = internal_bind_addr(net, false, "0.0.0.0", 9742);
    assert!(
        plaintext.ip().is_unspecified(),
        "plaintext mesh keeps the 0.0.0.0 internal bind, got {plaintext}"
    );

    // A configured private bind is honoured on a plaintext mesh...
    let pinned = internal_bind_addr(net, false, "10.0.1.4", 9742);
    assert_eq!(pinned.to_string(), "10.0.1.4:9742");
    // ...but encryption still forces loopback, ignoring the config.
    let pinned_encrypted = internal_bind_addr(net, true, "10.0.1.4", 9742);
    assert!(pinned_encrypted.ip().is_loopback());

    // ...and so does the local-only profile, on a plaintext mesh with an
    // explicitly pinned routable interface: the unauthenticated internal
    // API is not offered to a LAN this daemon will never talk to.
    let local = crate::local_only::LocalOnlyProfile::decide(None, true);
    assert!(internal_bind_addr(local, false, "10.0.1.4", 9742)
        .ip()
        .is_loopback());
    assert!(internal_bind_addr(local, false, "0.0.0.0", 9742)
        .ip()
        .is_loopback());
}

/// covers: UI-22
///
/// Secure by default, as one decision. Both halves are asserted because
/// each is separately capable of shipping an open door: bind loopback
/// unless something explicit says otherwise, and a non-loopback bind
/// either carries a bearer token or serves nobody.
///
/// Until this was extracted the whole posture lived inline in
/// `start_daemon` — a ~4700-line async fn — so the only way to reach the
/// branch that decides whether an unauthenticated listener goes on the
/// network was to start a daemon and try it from another machine.
#[test]
fn the_client_api_binds_loopback_by_default_and_never_exposes_an_unauthenticated_listener() {
    let never = || panic!("a loopback daemon must not mint or persist a client token");
    let token = || Some("tok-abc".to_string());
    let no_token = || None;

    // 1. THE DEFAULT. No marker, no encryption: loopback, no auth layer,
    //    and — the part that is easy to lose — no credential minted at
    //    all. A daemon nothing can reach has no use for one.
    for bind in ["127.0.0.1", "::1", "localhost", "LOCALHOST"] {
        let p = resolve_client_bind_posture(bind, false, false, never);
        assert!(p.loopback, "{bind} is loopback");
        assert!(p.token.is_none());
    }

    // 2. THE DANGEROUS CASE. An explicit non-loopback bind where no token
    //    can be resolved — bad data-dir perms, no config, no env. The
    //    posture must be token-less, which is what makes `client_auth`
    //    refuse every remote caller. A posture that shipped `Some(..)` of
    //    anything here, or that quietly fell back to loopback, would each
    //    be a different kind of lie about what is listening.
    let p = resolve_client_bind_posture("0.0.0.0", false, false, no_token);
    assert_eq!(p.bind, "0.0.0.0");
    assert!(!p.loopback);
    assert!(
        p.token.is_none(),
        "no resolvable token on a non-loopback bind must install NONE — the auth \
             layer then refuses every remote caller (fail-closed)"
    );

    // 3. The same bind WITH a token: exposed, and guarded.
    let p = resolve_client_bind_posture("0.0.0.0", false, false, token);
    assert_eq!(p.bind, "0.0.0.0");
    assert!(!p.loopback);
    assert_eq!(p.token.as_deref(), Some("tok-abc"));

    // 4. THE OPT-OUT, and its exact scope. The `client-exposed` marker
    //    promotes a loopback DEFAULT to 0.0.0.0 — and, because that is
    //    now a non-loopback bind, it goes through the token requirement
    //    like any other. The marker cannot open an unauthenticated port.
    let p = resolve_client_bind_posture("127.0.0.1", true, false, token);
    assert_eq!(p.bind, "0.0.0.0");
    assert!(!p.loopback);
    assert_eq!(p.token.as_deref(), Some("tok-abc"));

    let p = resolve_client_bind_posture("127.0.0.1", true, false, no_token);
    assert!(!p.loopback);
    assert!(
        p.token.is_none(),
        "the marker must not be a route around the token requirement"
    );

    // 5. An ENCRYPTED mesh overrides everything back to loopback (WS-C
    //    receiver lockout) — the marker above, and an explicit config
    //    bind too. Remote peers arrive via the key-authenticated iroh
    //    acceptor instead, so no plaintext token is minted.
    let p = resolve_client_bind_posture("127.0.0.1", true, true, never);
    assert_eq!(p.bind, "127.0.0.1");
    assert!(p.loopback && p.token.is_none());

    let p = resolve_client_bind_posture("10.0.1.4", false, true, never);
    assert_eq!(p.bind, "127.0.0.1");
    assert!(p.loopback && p.token.is_none());

    // 6. And on a PLAINTEXT mesh an explicit routable bind is honoured
    //    verbatim — the control for [5], so "forced loopback" is known to
    //    be the encryption doing it rather than the function refusing
    //    every non-loopback address.
    let p = resolve_client_bind_posture("10.0.1.4", false, false, token);
    assert_eq!(p.bind, "10.0.1.4");
    assert!(!p.loopback);
    assert_eq!(p.token.as_deref(), Some("tok-abc"));
}

/// Regression for: after `sovereign setup`, `GET /v1/models`
/// returned `{"data":[]}`. Root cause was that the daemon never
/// registered its loaded model slots into `inference_store`, so
/// Commonwealth's handler had nothing to list.
#[tokio::test]
async fn register_local_model_slots_writes_info_for_all_three_slots() {
    use crate::state::AppState;
    use commonwealth_core::mesh::Mesh;

    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: commonwealth_core::ids::MeshId::generate(),
        name: "test".into(),
        invite_key_hash: [0u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: Default::default(),
        peers: vec![],
    };
    let node_id = kernel_types::NodeId::generate();
    let app_state = AppState::new(node_id, mesh);

    let cfg = SetupConfig {
        engine: Default::default(),
        compute: Default::default(),
        search: Default::default(),
        models: Some(ModelsSection {
            primary: PathBuf::from("/m/qwen3-coder-30b.gguf"),
            fast: Some(PathBuf::from("/m/qwen3-1.7b.gguf")),
            embed: PathBuf::from("/m/qwen3-embedding-0.6b.gguf"),
            code: None,
            context_size: None,
            fast_context_size: None,
            max_extras_memory_gb: None,
            extra: std::collections::BTreeMap::new(),
            primary_pool: None,
            edit: None,
            kinds: Default::default(),
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

    register_local_model_slots(&app_state, &cfg, node_id).await;

    let models = app_state.list_models().await.unwrap();
    assert_eq!(
        models.len(),
        3,
        "primary/fast/embed must each produce one ModelInfo"
    );
    let names: std::collections::HashSet<String> =
        models.values().map(|m| m.name.clone()).collect();
    assert!(names.contains("qwen3-coder-30b"));
    assert!(names.contains("qwen3-1.7b"));
    assert!(names.contains("qwen3-embedding-0.6b"));

    // Second call with the same config must not duplicate entries
    // (deterministic ModelId per slot + path).
    register_local_model_slots(&app_state, &cfg, node_id).await;
    let models2 = app_state.list_models().await.unwrap();
    assert_eq!(
        models2.len(),
        3,
        "re-registering same config must upsert, not duplicate"
    );
}
