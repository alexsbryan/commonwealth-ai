// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end behaviour of the `:9741` client-auth layer
//! (`commonwealth_api::client_auth`), driven through the real
//! `client_router` via `tower::oneshot`.
//!
//! `ConnectInfo<SocketAddr>` is injected directly into request
//! extensions — the layer reads it from there, so we don't need a
//! live TCP listener (which would make the loopback-vs-remote split
//! flaky on CI boxes without a routable NIC). The auth decision is
//! made entirely from the injected peer addr + the `Authorization`
//! header + the daemon's installed token.
//!
//! Target path is `/v1/models` (a gated route with a trivial handler):
//! REJECT outcomes (401/403/500) come from the layer *before* the
//! handler, so we assert them exactly; ADMIT outcomes we assert as
//! "not an auth rejection" to stay decoupled from handler internals.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use commonwealth_api::server::client_router;
use commonwealth_api::state::AppState;
use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use tower::ServiceExt;

const LOOPBACK: &str = "127.0.0.1:55001";
const LAN_PEER: &str = "192.168.1.50:44444";
const TOKEN: &str = "deadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00d";

fn member(id: NodeId) -> MemberRecord {
    MemberRecord {
        removed_at: None,
        node_pubkey: None,
        relay_url: None,
        iroh_direct_addrs: Vec::new(),
        dial_info_version: 0,
        dial_info_sig: None,
        node_id: id,
        name: "A".into(),
        invited_by: id,
        joined_at: 0,
        last_seen: 0,
        status: NodeStatus::Online,
        capabilities: NodeCapabilities {
            hardware: HardwareProfile {
                gpus: vec![],
                system_ram_gb: 0,
                cpu_cores: 0,
                total_storage_gb: 0,
                free_storage_gb: 0,
                network_bandwidth_mbps: None,
            },
            available: AvailableResources::default(),
            active_processes: vec![],
            hosted_corpora: vec![],
            reported_at: 0,
            inference_availability: 1.0,
            inference_capable: false,
            loaded_models: vec![],
            embed_model: None,
            benchmark: None,
            current_in_flight: None,
            anchor: None,
        },
        addresses: vec!["192.168.1.1:9742".parse::<SocketAddr>().unwrap()],
    }
}

/// `AppState` with `token` installed (`Some` = token configured,
/// `None` = no token → remote callers fail closed).
fn state_with_token(token: Option<&str>) -> AppState {
    let node = NodeId::from_u128(1);
    let mut members = HashMap::new();
    members.insert(node, member(node));
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(7),
        name: "Test".into(),
        invite_key_hash: [3u8; 32],
        invite_version: 0,
        require_encryption: false,
        members,
        peers: vec![],
    };
    let state = AppState::new(node, mesh);
    state.install_client_token(token.map(Arc::<str>::from));
    state
}

/// Oneshot a GET through the real client_router. `peer` = injected
/// ConnectInfo (None ⇒ simulate a listener that forgot connect_info);
/// `bearer` = optional Authorization token.
async fn get_status(
    state: AppState,
    path: &str,
    peer: Option<&str>,
    bearer: Option<&str>,
) -> StatusCode {
    let mut builder = Request::get(path);
    if let Some(b) = bearer {
        builder = builder.header(axum::http::header::AUTHORIZATION, format!("Bearer {b}"));
    }
    let mut req = builder.body(Body::empty()).unwrap();
    if let Some(p) = peer {
        let addr: SocketAddr = p.parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(addr));
    }
    client_router(state).oneshot(req).await.unwrap().status()
}

fn is_auth_rejection(s: StatusCode) -> bool {
    s == StatusCode::UNAUTHORIZED
        || s == StatusCode::FORBIDDEN
        || s == StatusCode::INTERNAL_SERVER_ERROR
}

// ── loopback is always admitted ──────────────────────────────────

#[tokio::test]
async fn loopback_admitted_without_token_even_when_none_configured() {
    // No token configured (single-user/localhost deployment) and no
    // bearer: a loopback caller must pass straight through.
    let status = get_status(state_with_token(None), "/v1/models", Some(LOOPBACK), None).await;
    assert!(
        !is_auth_rejection(status),
        "loopback caller must be admitted (got {status})"
    );
}

// ── remote requires the token ────────────────────────────────────

#[tokio::test]
async fn remote_with_correct_bearer_is_admitted() {
    let status = get_status(
        state_with_token(Some(TOKEN)),
        "/v1/models",
        Some(LAN_PEER),
        Some(TOKEN),
    )
    .await;
    assert!(
        !is_auth_rejection(status),
        "remote caller with the right token must be admitted (got {status})"
    );
}

#[tokio::test]
async fn remote_with_wrong_bearer_is_401() {
    let status = get_status(
        state_with_token(Some(TOKEN)),
        "/v1/models",
        Some(LAN_PEER),
        Some("not-the-token"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn remote_without_bearer_is_401() {
    let status = get_status(
        state_with_token(Some(TOKEN)),
        "/v1/models",
        Some(LAN_PEER),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ── fail-closed cases ────────────────────────────────────────────

#[tokio::test]
async fn remote_with_no_token_configured_fails_closed_403() {
    // Bound somewhere a remote reached us, but no token was installed.
    // Must refuse — never admit an unauthenticated remote caller.
    let status = get_status(
        state_with_token(None),
        "/v1/models",
        Some(LAN_PEER),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn missing_connect_info_fails_closed_500() {
    // Listener forgot into_make_service_with_connect_info: can't
    // identify the caller, so refuse rather than admit.
    let status = get_status(state_with_token(Some(TOKEN)), "/v1/models", None, None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

// ── exempt paths stay open to remote callers ─────────────────────

#[tokio::test]
async fn exempt_path_admits_remote_without_token() {
    // /oicp/v1/capabilities is the federation handshake — a peer must
    // read it before it could hold a token.
    let status = get_status(
        state_with_token(Some(TOKEN)),
        "/oicp/v1/capabilities",
        Some(LAN_PEER),
        None,
    )
    .await;
    assert!(
        !is_auth_rejection(status),
        "exempt federation path must be reachable without a token (got {status})"
    );
}

#[tokio::test]
async fn gated_path_still_blocks_when_exempt_path_is_open() {
    // Sanity: the exemption is per-exact-path, not a global off switch.
    // Same remote caller, no token → /v1/models blocked, /status open.
    let blocked = get_status(
        state_with_token(Some(TOKEN)),
        "/v1/models",
        Some(LAN_PEER),
        None,
    )
    .await;
    assert_eq!(blocked, StatusCode::UNAUTHORIZED);

    let open = get_status(
        state_with_token(Some(TOKEN)),
        "/status",
        Some(LAN_PEER),
        None,
    )
    .await;
    assert!(
        !is_auth_rejection(open),
        "/status must stay open (got {open})"
    );
}

// ─────────────────────────── guest grants ────────────────────────────
//
// A guest is the third principal class on this port: not loopback, not the
// daemon token. The arm sits strictly AFTER the full-token arm, asks
// `permits_path`, and never inspects a `Scope` variant. These pin all three,
// plus the ratchet that makes a future scope safe to add.
//
// Every one of these was watched fail before it was kept — see
// `guest_falsification` at the bottom of this file for the probe.

use commonwealth_knowledge::{GuestGrant, Scope};

const GUEST_TOKEN: &str = "9c1f7b2ea4d68053aa11ff7c3e5b90d4c7a2f16b8e04d93b5c7a1e2f3b4d5c6a";
const GRANTED_MODEL: &str = "shared-primary";

/// Minutes, in the milliseconds the store speaks.
fn ms(mins: u64) -> u64 {
    mins * 60 * 1_000
}

/// `AppState` with the daemon token installed AND one guest grant live,
/// scoped to a single model.
fn state_with_guest(scopes: Vec<Scope>) -> AppState {
    let state = state_with_token(Some(TOKEN));
    let now = commonwealth_core::clock::unix_now_millis();
    state
        .inner
        .guest_grants
        .issue(GUEST_TOKEN, scopes, Some("test".into()), 3_600, now);
    state
}

/// Like `get_status`, but keeps the body so a refusal can be told apart from
/// its neighbours. Three different things return 403 on this port; a test that
/// asserted only the status could not say which one fired.
async fn get_with_body(
    state: AppState,
    path: &str,
    peer: Option<&str>,
    bearer: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::get(path);
    if let Some(b) = bearer {
        builder = builder.header(axum::http::header::AUTHORIZATION, format!("Bearer {b}"));
    }
    let mut req = builder.body(Body::empty()).unwrap();
    if let Some(p) = peer {
        req.extensions_mut()
            .insert(ConnectInfo(p.parse::<SocketAddr>().unwrap()));
    }
    let resp = client_router(state).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn a_live_guest_token_reaches_a_path_its_scope_names() {
    let state = state_with_guest(vec![Scope::Models(vec![GRANTED_MODEL.into()])]);
    let (status, _) = get_with_body(state, "/v1/models", Some(LAN_PEER), Some(GUEST_TOKEN)).await;
    assert!(
        !is_auth_rejection(status),
        "/v1/models is in Scope::Models::paths() — a live guest must reach it (got {status})"
    );
}

#[tokio::test]
async fn the_same_guest_token_is_403_on_a_path_no_scope_names() {
    let state = state_with_guest(vec![Scope::Models(vec![GRANTED_MODEL.into()])]);
    let (status, body) = get_with_body(
        state,
        "/v1/knowledge/search",
        Some(LAN_PEER),
        Some(GUEST_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The TYPED refusal, not the "remote access not configured" 403 and not a
    // bare one: a guest holding a valid credential must be told which boundary
    // they hit, or "the link is broken" gets filed against a working link.
    assert_eq!(body["error"]["type"], "guest_scope");
    assert_eq!(body["error"]["code"], "out_of_scope");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(GRANTED_MODEL),
        "the refusal names what the grant DOES cover: {body}"
    );
}

/// The ordering invariant, from the other side. A guest token must never
/// widen into the daemon token's reach — and the way you observe that is that
/// the guest arm answers (typed 403), not the full-token arm (admitted).
#[tokio::test]
async fn a_guest_token_never_satisfies_the_full_token_arm() {
    let state = state_with_guest(vec![Scope::Models(vec![GRANTED_MODEL.into()])]);
    let (guest_status, body) = get_with_body(
        state.clone(),
        "/v1/knowledge/search",
        Some(LAN_PEER),
        Some(GUEST_TOKEN),
    )
    .await;
    assert_eq!(guest_status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["type"], "guest_scope");

    // The daemon token on the same path IS admitted — so the 403 above is the
    // grant's narrowness, not the route being closed to everyone.
    let (full_status, _) =
        get_with_body(state, "/v1/knowledge/search", Some(LAN_PEER), Some(TOKEN)).await;
    assert!(
        !is_auth_rejection(full_status),
        "the daemon token reaches this route (got {full_status})"
    );
}

#[tokio::test]
async fn an_expired_guest_token_is_401_not_admitted() {
    let state = state_with_token(Some(TOKEN));
    let now = commonwealth_core::clock::unix_now_millis();
    // Issue against a clock two hours in the past with a 1s TTL: lapsed by the
    // time the layer reads it, without sleeping.
    state.inner.guest_grants.issue(
        GUEST_TOKEN,
        vec![Scope::Models(vec![GRANTED_MODEL.into()])],
        None,
        1,
        now - ms(120),
    );
    let (status, _) = get_with_body(state, "/v1/models", Some(LAN_PEER), Some(GUEST_TOKEN)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_revoked_guest_token_fails_closed_on_the_very_next_request() {
    let state = state_with_guest(vec![Scope::Models(vec![GRANTED_MODEL.into()])]);
    let (before, _) = get_with_body(
        state.clone(),
        "/v1/models",
        Some(LAN_PEER),
        Some(GUEST_TOKEN),
    )
    .await;
    assert!(!is_auth_rejection(before), "live before revoke");

    assert!(state.inner.guest_grants.revoke(GUEST_TOKEN).is_some());

    let (after, _) = get_with_body(state, "/v1/models", Some(LAN_PEER), Some(GUEST_TOKEN)).await;
    assert_eq!(
        after,
        StatusCode::UNAUTHORIZED,
        "revocation is immediate — there is no window behind the reaper"
    );
}

/// A grant that names no scope permits nothing. `any()` over an empty
/// iterator is false, and this pins that the direction never flips.
#[tokio::test]
async fn a_grant_with_no_scopes_permits_nothing() {
    let state = state_with_guest(vec![]);
    let (status, body) =
        get_with_body(state, "/v1/models", Some(LAN_PEER), Some(GUEST_TOKEN)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["type"], "guest_scope");
}

// ───────────────── the extension ratchet (plan §Verification) ─────────────────

/// Paths no guest scope may EVER unlock, matched by prefix.
///
/// The one hand-maintained list in this design, and deliberately deny-side:
/// forgetting an entry here fails closed for the operator (a dangerous path
/// simply isn't granted by any existing scope) rather than open. `/internal/*`
/// is where guest grants are MINTED — a scope reaching it would let grants
/// mint grants, which is the property the whole design rests on not being
/// expressible.
const NEVER_GUESTABLE: &[&str] = &["/internal/", "/v1/apps", "/v1/mesh/"];

/// One sample per `Scope` variant.
///
/// The `match` below is the ratchet: adding a variant makes it non-exhaustive,
/// so the build breaks HERE — in the test that would otherwise not know about
/// the new variant — rather than shipping an unexamined scope.
fn one_sample_per_scope_variant() -> Vec<Scope> {
    let all = vec![Scope::Models(vec![GRANTED_MODEL.into()])];
    for s in &all {
        match s {
            Scope::Models(_) => {}
        }
    }
    all
}

/// Every path any scope unlocks must (a) actually exist on the client router
/// and (b) not be privileged.
///
/// (a) catches a `paths()` arm naming a route that was renamed or never
/// mounted — which would ship a grant that 403s on a path the daemon does not
/// serve, and read as a scope bug. (b) catches the dangerous case: a variant
/// added later that grants `/internal/*` or the apps API.
#[tokio::test]
async fn every_scope_paths_are_mounted_and_never_privileged() {
    for scope in one_sample_per_scope_variant() {
        for path in scope.paths() {
            assert!(
                !NEVER_GUESTABLE.iter().any(|deny| path.starts_with(deny)),
                "scope {:?} unlocks the privileged path {path}",
                scope.label()
            );

            // Mounted? Drive it as a LOOPBACK caller so the auth layer admits
            // and routing gets to answer. 405 (wrong method for this probe's
            // GET) is a positive mount signal; 404 is the failure.
            let (status, _) =
                get_with_body(state_with_token(Some(TOKEN)), path, Some(LOOPBACK), None).await;
            assert_ne!(
                status,
                StatusCode::NOT_FOUND,
                "scope {:?} unlocks {path}, which is not mounted on client_router — \
                 a grant naming it would be born broken",
                scope.label()
            );
        }
    }
}

/// The deny-list is prefix-matched, and the sample paths prove it bites. If
/// this ever passes vacuously (empty list, or a `starts_with` that matches
/// nothing) the ratchet above is inert.
#[test]
fn the_never_guestable_list_actually_rejects_the_paths_it_names() {
    for privileged in [
        "/internal/guest/grant",
        "/internal/guest/grant/revoke",
        "/v1/apps",
        "/v1/mesh/join",
    ] {
        assert!(
            NEVER_GUESTABLE.iter().any(|d| privileged.starts_with(d)),
            "{privileged} must be denied by NEVER_GUESTABLE"
        );
    }
    for ordinary in ["/v1/models", "/v1/chat/completions"] {
        assert!(
            !NEVER_GUESTABLE.iter().any(|d| ordinary.starts_with(d)),
            "{ordinary} is guestable and must not be on the deny-list"
        );
    }
}

/// Not a scope check — a shape check on the grant the layer will consult.
/// `permits_path` is exact-match, the same discipline `AUTH_EXEMPT_PATHS`
/// keeps: no child path inherits access by prefix.
#[test]
fn permits_path_is_exact_and_grants_no_children() {
    let grant = GuestGrant {
        token: GUEST_TOKEN.into(),
        scopes: vec![Scope::Models(vec![GRANTED_MODEL.into()])],
        label: None,
        issued_at_ms: 0,
        expires_at_ms: u64::MAX,
        revoked: false,
    };
    assert!(grant.permits_path("/v1/models"));
    assert!(grant.permits_path("/v1/chat/completions"));
    assert!(!grant.permits_path("/v1/models/secret"));
    assert!(!grant.permits_path("/v1/chat/completions/../../internal/guest/grant"));
    assert!(!grant.permits_path("/v1/model"));
    assert!(!grant.permits_path(""));
}

// ── the GUEST listener: loopback is not a credential ───────────────
//
// The iroh acceptor forwards a tunnelled connection by
// `TcpStream::connect`ing `127.0.0.1`, so every request that arrives on the
// guest listener wears a loopback peer address it did not earn. On the
// default policy that address is admitted BEFORE any bearer is read, which
// would hand every holder of the node's public dial string the whole client
// API — and would make a guest grant's scope decorative, since the guest is
// admitted before its token is examined.
//
// `ClientAuthPolicy::UNTRUSTED_LOOPBACK` is the fix, and these are the tests
// that fail without it.

/// Oneshot through the GUEST bind of the router — the one the iroh acceptor
/// feeds. Same injection harness; the only difference is the policy.
async fn get_via_guest_listener(
    state: AppState,
    path: &str,
    peer: Option<&str>,
    bearer: Option<&str>,
) -> StatusCode {
    let mut builder = Request::get(path);
    if let Some(b) = bearer {
        builder = builder.header(axum::http::header::AUTHORIZATION, format!("Bearer {b}"));
    }
    let mut req = builder.body(Body::empty()).unwrap();
    if let Some(p) = peer {
        req.extensions_mut()
            .insert(ConnectInfo(p.parse::<SocketAddr>().unwrap()));
    }
    commonwealth_api::server::client_router_with(
        state,
        commonwealth_api::client_auth::ClientAuthPolicy::UNTRUSTED_LOOPBACK,
    )
    .oneshot(req)
    .await
    .unwrap()
    .status()
}

/// THE finding. A tunnelled caller presenting nothing looks exactly like the
/// local user to the default policy; on the guest listener it must not.
#[tokio::test]
async fn the_guest_listener_refuses_a_loopback_caller_with_no_credential() {
    let status =
        get_via_guest_listener(state_with_token(Some(TOKEN)), "/v1/models", Some(LOOPBACK), None)
            .await;
    assert!(
        is_auth_rejection(status),
        "a request forwarded by the iroh acceptor arrives from 127.0.0.1 and has \
         earned nothing by it (got {status})"
    );
}

/// And the same listener on the same request through the DEFAULT policy still
/// admits — otherwise this pair proves only that something is broken, not that
/// the policy is what separates the two listeners.
#[tokio::test]
async fn the_ordinary_listener_still_admits_that_same_loopback_caller() {
    let status = get_status(state_with_token(Some(TOKEN)), "/v1/models", Some(LOOPBACK), None).await;
    assert!(
        !is_auth_rejection(status),
        "the daemon's own listener must keep admitting the local user (got {status})"
    );
}

/// A guest reaching the guest listener over the tunnel is admitted on its
/// BEARER, from the same loopback address the test above refuses. This is the
/// arm that makes the feature work rather than merely fail closed.
#[tokio::test]
async fn a_guest_bearer_is_admitted_on_the_guest_listener_from_the_tunnel_hop() {
    let state = state_with_guest(vec![Scope::Models(vec![GRANTED_MODEL.into()])]);
    let status =
        get_via_guest_listener(state, "/v1/models", Some(LOOPBACK), Some(GUEST_TOKEN)).await;
    assert!(
        !is_auth_rejection(status),
        "the bearer is the credential, and the tunnel hop must not get in its way (got {status})"
    );
}

/// Scope still binds on this listener. Losing the loopback shortcut must not
/// quietly widen what a guest reaches — the ordering of the arms is unchanged,
/// so a guest token gets exactly the paths its grant names.
#[tokio::test]
async fn a_guest_bearer_is_still_scoped_on_the_guest_listener() {
    let state = state_with_guest(vec![Scope::Models(vec![GRANTED_MODEL.into()])]);
    let status = get_via_guest_listener(
        state,
        "/v1/knowledge/search",
        Some(LOOPBACK),
        Some(GUEST_TOKEN),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "no scope names /v1/knowledge/search"
    );
}

/// The health/federation surface stays open — it is what a guest's `mesh use`
/// probes, and it carries nothing.
#[tokio::test]
async fn the_exempt_paths_stay_open_on_the_guest_listener() {
    let status =
        get_via_guest_listener(state_with_token(Some(TOKEN)), "/status", Some(LOOPBACK), None).await;
    assert!(
        !is_auth_rejection(status),
        "/status is in AUTH_EXEMPT_PATHS on every listener (got {status})"
    );
}

/// A daemon token still works there. The guest listener is the client router
/// with one arm removed, not a different surface.
#[tokio::test]
async fn the_daemon_token_still_works_on_the_guest_listener() {
    let status = get_via_guest_listener(
        state_with_token(Some(TOKEN)),
        "/v1/models",
        Some(LAN_PEER),
        Some(TOKEN),
    )
    .await;
    assert!(
        !is_auth_rejection(status),
        "the full-token arm is untouched by the policy (got {status})"
    );
}
