// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the embedded daemon — see `daemon.rs`.
//!
//! Their own file because keeping them inline put that file past its
//! arch-gate slack (ARCH §3.1). `#[path]`, so the names are unchanged.

use super::*;
use sovereign_core::setup_config::{DaemonSection, DataSection, ModelsSection, SetupConfig};
use std::path::PathBuf;

/// The unauthenticated internal API binds loopback whatever `[daemon]
/// internal_bind` names: cw-rails, the node's one mesh ingress, forwards a
/// member's request over loopback (pb-mesh-exit-transport). Failing input:
/// honour a configured routable interface.
#[test]
fn internal_bind_is_loopback_always() {
    for configured in ["0.0.0.0", "10.0.1.4", "127.0.0.1"] {
        let addr = internal_bind_addr(configured, 9742);
        assert!(addr.ip().is_loopback(), "{configured} bound {addr}");
        assert_eq!(addr.port(), 9742);
    }
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

    // 1. THE DEFAULT: loopback, no auth layer, and — the part that is easy
    //    to lose — no credential minted at all. A daemon nothing can reach
    //    has no use for one.
    for bind in ["127.0.0.1", "::1", "localhost", "LOCALHOST"] {
        let p = resolve_client_bind_posture(bind, never);
        assert!(p.loopback, "{bind} is loopback");
        assert!(p.token.is_none());
    }

    // 2. THE DANGEROUS CASE. An explicit non-loopback bind where no token
    //    can be resolved — bad data-dir perms, no config, no env. The
    //    posture must be token-less, which is what makes `client_auth`
    //    refuse every remote caller. A posture that shipped `Some(..)` of
    //    anything here, or that quietly fell back to loopback, would each
    //    be a different kind of lie about what is listening.
    let p = resolve_client_bind_posture("0.0.0.0", no_token);
    assert_eq!(p.bind, "0.0.0.0");
    assert!(!p.loopback);
    assert!(
        p.token.is_none(),
        "no resolvable token on a non-loopback bind must install NONE — the auth \
             layer then refuses every remote caller (fail-closed)"
    );

    // 3. The same bind WITH a token: exposed, and guarded.
    let p = resolve_client_bind_posture("0.0.0.0", token);
    assert_eq!(p.bind, "0.0.0.0");
    assert!(!p.loopback);
    assert_eq!(p.token.as_deref(), Some("tok-abc"));

    // 4. An explicit routable bind is the operator's and is honoured
    //    verbatim, token required.
    let p = resolve_client_bind_posture("10.0.1.4", token);
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

    let node_id = kernel_types::NodeId::generate();
    let app_state = AppState::new(node_id);

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
