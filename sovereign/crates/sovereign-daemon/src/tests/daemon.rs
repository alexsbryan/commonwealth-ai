// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the embedded daemon — see `daemon.rs`.
//!
//! Their own file because keeping them inline put that file past its
//! arch-gate slack (ARCH §3.1). `#[path]`, so the names are unchanged.

use super::*;
use sovereign_core::setup_config::{DaemonSection, DataSection, ModelsSection, SetupConfig};
use std::path::PathBuf;

fn direct(ep: &str) -> Option<(String, String)> {
    Some((ep.to_string(), "direct-ip".to_string()))
}
fn bridge(ep: &str) -> Option<(String, String)> {
    Some((ep.to_string(), "iroh-bridge:x".to_string()))
}

#[test]
fn sticky_takes_fresh_direct_ip_immediately() {
    // First-ever sight of a verified direct-ip: no prior, take it, misses=0.
    let s = sticky_endpoint(None, direct("10.0.0.9:50052"), 3).unwrap();
    assert_eq!(s.endpoint, "10.0.0.9:50052");
    assert!(s.is_direct());
    assert_eq!(s.direct_misses, 0);
}

/// The regression this file's split-expansion comment describes: a slot
/// configured at shard 1 of a split GGUF must make ALL shards servable.
/// Advertising only shard 1 404s the rest, which strands any worker that
/// doesn't already hold the whole model — and because warm failure is
/// never-wedge safe, it surfaces as "the big model won't distribute"
/// rather than as an error.
#[test]
fn servable_files_expand_a_split_gguf_to_every_shard() {
    let dir = tempfile::tempdir().unwrap();
    let mk = |name: &str| {
        let p = dir.path().join(name);
        std::fs::write(&p, b"x").unwrap();
        p
    };
    let s1 = mk("big-00001-of-00003.gguf");
    let s2 = mk("big-00002-of-00003.gguf");
    let s3 = mk("big-00003-of-00003.gguf");
    let solo = mk("embed.gguf");

    // Config names shard 1 only; all three must become servable.
    let got = servable_model_files(&[s1.clone(), solo.clone()]);
    let canon = |p: &std::path::PathBuf| p.canonicalize().unwrap();
    assert_eq!(
        got,
        vec![canon(&s1), canon(&s2), canon(&s3), canon(&solo)],
        "split slot must advertise every shard, in order, then the solo slot"
    );

    // Dedup: primary_pool points several slots at the same GGUF.
    let got = servable_model_files(&[s1.clone(), s1.clone(), solo.clone()]);
    assert_eq!(
        got.len(),
        4,
        "same model twice must not be advertised twice"
    );
}

/// Never advertise what we cannot serve: with a sibling absent,
/// `shard_files` refuses to guess, so we fall back to the named file.
#[test]
fn servable_files_do_not_guess_missing_shards() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t-00001-of-00002.gguf");
    std::fs::write(&p, b"x").unwrap();
    let got = servable_model_files(&[p.clone()]);
    assert_eq!(got, vec![p.canonicalize().unwrap()]);
}

#[test]
fn sticky_holds_direct_ip_through_transient_misses_then_flips() {
    // The 2026-07-19 flap in miniature: a proven direct-ip must NOT flip to
    // the bridge on one miss — hold it until the threshold, THEN flip.
    let flip = 3;
    let s0 = sticky_endpoint(None, direct("10.0.0.5:50052"), flip).unwrap();
    // Miss 1: bridge offered, but hold direct-ip.
    let s1 = sticky_endpoint(Some(&s0), bridge("127.0.0.1:40001"), flip).unwrap();
    assert_eq!(s1.endpoint, "10.0.0.5:50052", "must not flip on one miss");
    assert!(s1.is_direct());
    assert_eq!(s1.direct_misses, 1);
    // Miss 2: still holding (2 < 3).
    let s2 = sticky_endpoint(Some(&s1), bridge("127.0.0.1:40001"), flip).unwrap();
    assert_eq!(s2.endpoint, "10.0.0.5:50052");
    assert_eq!(s2.direct_misses, 2);
    // Miss 3 reaches the threshold — NOW accept the bridge (durable change).
    let s3 = sticky_endpoint(Some(&s2), bridge("127.0.0.1:40001"), flip).unwrap();
    assert_eq!(s3.endpoint, "127.0.0.1:40001");
    assert_eq!(s3.via, "iroh-bridge:x");
    assert_eq!(s3.direct_misses, 0);
}

#[test]
fn sticky_direct_ip_recovery_resets_miss_count() {
    let flip = 3;
    let s0 = sticky_endpoint(None, direct("10.0.0.5:50052"), flip).unwrap();
    let s1 = sticky_endpoint(Some(&s0), None, flip).unwrap(); // total miss → hold
    assert_eq!(s1.direct_misses, 1);
    // Direct-ip answers again → back to a clean slate.
    let s2 = sticky_endpoint(Some(&s1), direct("10.0.0.5:50052"), flip).unwrap();
    assert!(s2.is_direct());
    assert_eq!(s2.direct_misses, 0);
}

#[test]
fn sticky_drops_a_non_direct_worker_when_unreachable() {
    // A bridge-only worker (no proven direct-ip to protect) is dropped the
    // moment it's unreachable — nothing to hold.
    let bridge_only = StickyEndpoint {
        endpoint: "127.0.0.1:1".to_string(),
        via: "iroh-bridge:x".to_string(),
        direct_misses: 0,
    };
    assert!(sticky_endpoint(Some(&bridge_only), None, 3).is_none());
}

fn held(via: &str) -> StickyEndpoint {
    StickyEndpoint {
        endpoint: "127.0.0.1:40021".to_string(),
        via: via.to_string(),
        direct_misses: 0,
    }
}

#[test]
fn reaffirm_probes_only_what_it_has_never_seen() {
    use RpcTunnelMode::*;
    // First sight of a peer: nothing held, so the full probe is the only way
    // to learn whether it serves an RPC worker at all.
    assert_eq!(reaffirm_plan(None, Auto), Reaffirm::FullProbe);
    // A proven direct-ip is re-affirmed from cache (2026-07-19 guard).
    assert_eq!(
        reaffirm_plan(Some(&held("direct-ip")), Auto),
        Reaffirm::Held
    );
    // A probe-host fallback is a last resort, not evidence of anything —
    // keep re-probing so it can be promoted to a real transport.
    assert_eq!(
        reaffirm_plan(Some(&held("probe-host")), Auto),
        Reaffirm::FullProbe
    );
}

#[test]
fn reaffirm_never_reprobes_a_known_bridged_worker_over_its_own_tunnel() {
    // THE 2026-07-26 REGRESSION. A bridged worker was re-probed via
    // `/status` every tick; that probe rides the same iroh path as the
    // tunnel, so under load it timed out, `fresh` went None, and
    // `sticky_endpoint` drops a non-direct endpoint on a miss (asserted in
    // `sticky_drops_a_non_direct_worker_when_unreachable`) — which the
    // eligibility tracker reads as a flap. Observed: endpoint pinned at
    // 127.0.0.1:40021 for six minutes while flaps climbed to 9 and the
    // cooldown compounded to 300s, excluding a peer that was serving.
    for via in ["iroh-bridge:x", "iroh-bridge:iroh:127.0.0.1:40021→86627fd5"] {
        assert_eq!(
            reaffirm_plan(Some(&held(via)), RpcTunnelMode::Auto),
            Reaffirm::Rebridge,
            "{via} must be re-minted from the local bridge cache, never re-probed"
        );
        assert_eq!(
            reaffirm_plan(Some(&held(via)), RpcTunnelMode::Always),
            Reaffirm::Rebridge
        );
    }
}

#[test]
fn reaffirm_respects_an_operator_opting_out_of_bridging() {
    // `SOVEREIGN_RPC_TUNNEL=never` withdraws permission to tunnel. Holding a
    // bridge endpoint would pin the worker to a transport we may no longer
    // use, so re-probe: it either surfaces at a direct address or drops out.
    assert_eq!(
        reaffirm_plan(Some(&held("iroh-bridge:x")), RpcTunnelMode::Never),
        Reaffirm::FullProbe
    );
    // The direct-ip hold is unaffected by the tunnel knob.
    assert_eq!(
        reaffirm_plan(Some(&held("direct-ip")), RpcTunnelMode::Never),
        Reaffirm::Held
    );
}

#[test]
fn sticky_flip_threshold_one_disables_the_hold() {
    // threshold 1 = flip on the first miss (the pre-guard behaviour), so the
    // env knob's floor is a conscious opt-out, not a silent no-op.
    let s0 = sticky_endpoint(None, direct("10.0.0.5:50052"), 1).unwrap();
    let s1 = sticky_endpoint(Some(&s0), bridge("127.0.0.1:2"), 1).unwrap();
    assert_eq!(s1.endpoint, "127.0.0.1:2", "threshold 1 flips immediately");
}

#[test]
fn rpc_tunnel_mode_parses_the_documented_values() {
    use RpcTunnelMode::*;
    assert_eq!(rpc_tunnel_mode_from(None), Auto);
    assert_eq!(rpc_tunnel_mode_from(Some("")), Auto);
    assert_eq!(rpc_tunnel_mode_from(Some("auto")), Auto);
    assert_eq!(rpc_tunnel_mode_from(Some("ALWAYS")), Always);
    assert_eq!(rpc_tunnel_mode_from(Some(" always ")), Always);
    assert_eq!(rpc_tunnel_mode_from(Some("never")), Never);
    assert_eq!(rpc_tunnel_mode_from(Some("off")), Never);
    assert_eq!(rpc_tunnel_mode_from(Some("0")), Never);
    // Unknown values degrade to the safe default, never panic.
    assert_eq!(rpc_tunnel_mode_from(Some("banana")), Auto);
}

#[test]
fn rpc_endpoint_directory_records_and_resolves() {
    // The warm orchestrator resolves worker identity through this
    // directory; an unknown endpoint (env-configured worker) is None so
    // callers fall back to raw-IP addressing.
    let daemon = EmbeddedDaemon::in_memory(
        SetupConfig::unconfigured(),
        crate::daemon_services::DaemonServices::mesh_admin(),
    );
    assert_eq!(daemon.rpc_endpoint_node("10.0.0.7:50052"), None);

    let node = NodeId::from_u128(42);
    daemon
        .rpc_endpoint_nodes
        .write()
        .unwrap()
        .insert("10.0.0.7:50052".to_string(), node);
    assert_eq!(daemon.rpc_endpoint_node("10.0.0.7:50052"), Some(node));
    // Re-discovery overwrites in place — same endpoint, later owner wins.
    let other = NodeId::from_u128(43);
    daemon
        .rpc_endpoint_nodes
        .write()
        .unwrap()
        .insert("10.0.0.7:50052".to_string(), other);
    assert_eq!(daemon.rpc_endpoint_node("10.0.0.7:50052"), Some(other));
}

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
#[test]
fn register_local_model_slots_writes_info_for_all_three_slots() {
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
    let node_id = commonwealth_core::ids::NodeId::generate();
    let mesh_store = Arc::new(commonwealth_state::MeshStore::in_memory().unwrap());
    let app_registry = Arc::new(sovereign_meshapp_registry::registry::AppRegistry::new());
    let app_state =
        AppState::new_with_platform_and_engine(node_id, mesh, mesh_store, app_registry, None);

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

    register_local_model_slots(&app_state, &cfg, node_id);

    let models = app_state.inner.store.inference_store.list_models();
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
    register_local_model_slots(&app_state, &cfg, node_id);
    let models2 = app_state.inner.store.inference_store.list_models();
    assert_eq!(
        models2.len(),
        3,
        "re-registering same config must upsert, not duplicate"
    );
}
