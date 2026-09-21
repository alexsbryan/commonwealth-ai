// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bearer-token authentication for the client API (`:9741`).
//!
//! The client surface — inference, knowledge search, the Ollama shim,
//! the apps API — was historically unauthenticated and bound
//! `0.0.0.0`, on the assumption that "the network is the perimeter"
//! (a closed mesh on a trusted LAN/tailnet). That holds for a private
//! alpha but breaks the moment the daemon runs on a routable address
//! someone untrusted can reach (a VPS, shared wifi, a tailnet with
//! guests). This layer is the **B** tier of the 2026-06 auth plan
//! (localhost-default + bearer token).
//!
//! **Node-identity auth for mesh peers is no longer "a later milestone" on
//! every surface, and this one is not the surface that has it.** On the
//! INTERNAL router (`:9742`) a peer's identity is the Ed25519 key the iroh
//! handshake proved, forwarded by this daemon's own acceptor and resolved by
//! [`crate::internal_principal`] — a header claiming otherwise is stripped
//! before a handler sees it. HERE, on the client surface, the acceptor does
//! not yet append a verified identity to `CLIENT_ALPN`, so a peer is still
//! identified by the `x-node-id` it TYPES. That claim is read exactly once —
//! by [`crate::client_principal`], through the one canonical wire parser —
//! and every decider downstream reads the [`Principal`] it produces rather
//! than the header (`crate::mesh_principal_gate` is the ratchet). A claim that
//! does not parse is [`Principal::Unverified`], and the peer gate refuses it.
//!
//! ## Decision (per connection, not per header)
//!
//! - **Loopback caller** → always admitted. The local user, the
//!   desktop app (attach-mode probes `127.0.0.1`), and in-process
//!   callers never need a token. This is decided from the real
//!   `ConnectInfo<SocketAddr>` peer address — NOT a request header.
//!   (The old local-vs-peer split keyed off the *presence* of the
//!   spoofable `X-Node-Id` header, which meant "omit the header" was a
//!   full-trust bypass. That footgun dies here.)
//! - **Remote caller** → must present `Authorization: Bearer <token>`
//!   matching a NAMED token ([`crate::client_tokens`], one per device and
//!   revocable alone) or the daemon's configured token (constant-time compare
//!   either way). `[daemon] client_tokens = "named-only"` refuses the second
//!   with a sentence.
//!   - No token configured (`AppState::client_token` is `None`) →
//!     **fail closed** (403): a remote request reached a daemon that
//!     never set up a secret; refuse rather than admit.
//!   - Wrong / missing bearer → 401.
//! - **`ConnectInfo` absent** (listener forgot
//!   `into_make_service_with_connect_info`) → **fail closed** (500),
//!   mirroring [`crate::loopback`-style] guards: better broken than
//!   bypassed.
//!
//! ## Open routes
//!
//! [`AUTH_EXEMPT_PATHS`] stay reachable without a token even from
//! remote callers: the federation/health surface a peer must read
//! *before* it could possibly hold a token. Everything that does work
//! or returns user data is gated.
//!
//! ## One resolution, attached for the inner layers
//!
//! The edge resolver runs ONCE here, before any admission branch, and the value
//! is attached to the request as [`crate::admission::AttachedPrincipal`] so the
//! fairness and peer-admission middlewares read it rather than resolving a
//! second time (`DAEMON_CORE.md` §3.3, "authenticates a request once and
//! attaches a `Principal`"). The internal router carries no `client_auth_layer`
//! — it runs [`crate::internal_principal::internal_principal_layer`], which
//! attaches the same extension from the acceptor's verified key.
//!
//! ## Loopback is a property of the LISTENER, not of the layer
//!
//! "Admit loopback" is right for the daemon's own client listener and
//! wrong for the one the iroh acceptor forwards GUEST traffic to: that
//! acceptor `TcpStream::connect`s `127.0.0.1`, so every tunnelled request
//! arrives wearing a loopback peer address it did not earn. A guest whose
//! entire credential is a bearer would be admitted before the bearer was
//! read.
//!
//! [`ClientAuthPolicy`] is therefore per-listener state, not a global. The
//! default (`trust_loopback: true`) is the listener an operator's own tools
//! talk to; the daemon binds the router a SECOND time with
//! `trust_loopback: false` and routes
//! [`commonwealth_transport::iroh::GUEST_ALPN`] there. Mesh peers keep
//! `CLIENT_ALPN` → the trusting listener, which is what lets their federated
//! inference (which carries no `Authorization` at all) keep working.

use commonwealth_core::ct::constant_time_eq;
use sovereign_grants::{GuestGrant, GuestSession};
use sovereign_serving_host::admission::Principal;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::state::AppState;

/// Re-exported so daemon entries can source the client token without
/// taking a direct `commonwealth-transport` dependency — the token's
/// load/persist lives next to `node_key` in that crate.
pub use commonwealth_transport::identity::load_or_create_client_token;

/// Per-listener auth posture. See the module docs: the daemon binds the
/// client router more than once, and the binds differ only in whether a
/// loopback peer address is evidence of a local caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientAuthPolicy {
    /// Admit a loopback peer without reading a credential.
    ///
    /// True for a listener a caller reaches by actually being on this
    /// machine. FALSE for one fed by the iroh acceptor, where the loopback
    /// address is the acceptor's own forward hop and says nothing about who
    /// dialled.
    pub trust_loopback: bool,
}

impl Default for ClientAuthPolicy {
    /// The historical posture, and the right one for the daemon's own
    /// listener: the local user, the desktop app and in-process callers
    /// never present a token.
    fn default() -> Self {
        Self {
            trust_loopback: true,
        }
    }
}

impl ClientAuthPolicy {
    /// The posture for a listener whose callers all arrive through a tunnel:
    /// nothing is local, so nothing is free.
    pub const UNTRUSTED_LOOPBACK: Self = Self {
        trust_loopback: false,
    };
}

/// Middleware state for [`client_auth_layer`]: the daemon's state plus the
/// posture of the listener this copy of the layer guards.
#[derive(Clone)]
pub struct ClientAuthState {
    pub state: AppState,
    pub policy: ClientAuthPolicy,
}

impl ClientAuthState {
    pub fn new(state: AppState, policy: ClientAuthPolicy) -> Self {
        Self { state, policy }
    }
}

/// Exact request paths that remain reachable without a token, even
/// from a non-loopback caller. Both are read-only and advertise-by-
/// design: `/oicp/v1/capabilities` is the federation handshake a peer
/// reads to decide whether to peer, and `/status` is the liveness /
/// pairing surface. Matched by EXACT equality (not prefix), so no
/// child path inherits the exemption.
pub const AUTH_EXEMPT_PATHS: &[&str] = &["/status", "/oicp/v1/capabilities"];

/// Extract the bearer token from an `Authorization` header value, if
/// present and well-formed (`Bearer <token>`, case-insensitive scheme).
fn bearer_token(req: &Request) -> Option<&str> {
    let header = req.headers().get(axum::http::header::AUTHORIZATION)?;
    let value = header.to_str().ok()?;
    let rest = value.strip_prefix("Bearer ").or_else(|| {
        // Tolerate lowercase / mixed-case scheme without allocating.
        let (scheme, rest) = value.split_once(' ')?;
        scheme.eq_ignore_ascii_case("bearer").then_some(rest)
    })?;
    let token = rest.trim();
    (!token.is_empty()).then_some(token)
}

fn unauthorized(reason: &'static str) -> Response {
    // Don't leak which check failed in a way useful to a prober; the
    // reason rides the log, the body is generic.
    tracing::warn!(reason, "client_auth: rejected remote caller");
    (
        StatusCode::UNAUTHORIZED,
        [("WWW-Authenticate", "Bearer")],
        Json(serde_json::json!({ "error": "authentication required" })),
    )
        .into_response()
}

/// `from_fn_with_state`-compatible client-API auth layer. See module
/// docs for the full decision table. Apply as the OUTERMOST layer on
/// the client router so it runs before load-shedding admission and
/// before any handler work. It resolves the caller once and attaches the
/// [`Principal`](crate::admission::AttachedPrincipal) for the admission
/// middlewares.
pub async fn client_auth_layer(
    State(auth): State<ClientAuthState>,
    request: Request,
    next: Next,
) -> Response {
    let ClientAuthState { state, policy } = auth;
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);

    let peer = match peer {
        Some(p) => p,
        None => {
            // Listener didn't wire ConnectInfo — can't identify the
            // caller, so we cannot safely admit. Fail closed.
            tracing::error!(
                path = %request.uri().path(),
                "client_auth: no ConnectInfo on request — check listener wiring \
                 (into_make_service_with_connect_info)"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "listener misconfigured: missing connect_info"
                })),
            )
                .into_response();
        }
    };

    // Resolve ONCE, at the edge, and attach the value for every downstream
    // reader (`DAEMON_CORE.md` §3.3, "authenticates a request once and attaches
    // a `Principal`"). The admission middlewares read this rather than
    // resolving a second time; the internal router, which carries no
    // `client_auth_layer`, keeps its `AdmissionHost::resolve` fallback.
    let principal = state.resolve(request.headers(), Some(peer), policy);
    let mut request = request;
    request
        .extensions_mut()
        .insert(crate::admission::AttachedPrincipal(principal.clone()));

    // Loopback is local — admit without a token, on a listener where that
    // inference holds. It does not hold on the guest listener: see the module
    // docs, and `ClientAuthPolicy`.
    if peer.ip().is_loopback() && policy.trust_loopback {
        return next.run(request).await;
    }

    // Federation/health surface stays open to remote callers.
    if AUTH_EXEMPT_PATHS.contains(&request.uri().path()) {
        return next.run(request).await;
    }

    // Remote, gated path. The caller's identity is resolved ONCE, through the
    // one edge resolver, and this layer's decision reads the arm it returns.
    // Two credentials can admit here — the daemon-wide token and an ephemeral
    // guest grant — and they are INDEPENDENT. The resolver reads the grant
    // store, so a live grant is the `Guest` arm and any other bearer the
    // `RemoteClient` arm; the token is checked only on that arm.
    //
    // Until 2026-08-28 this read "no daemon token configured → 403" BEFORE
    // ever looking at a grant, which made a live guest grant unusable on any
    // daemon that had no client token — including the daemon that minted it.
    // Observed on the wire: FOX minted a link, MAC presented it through the
    // guest tunnel, and FOX answered `remote access not configured` (live
    // bar 3.2, 2026-08-28). A valid credential refused because an unrelated
    // one is absent is the substitution this codebase refuses (§18.3).
    let configured = state.client_token();
    let presented = bearer_token(&request);

    match principal {
        // A bearer that is a live guest grant. The grant bounds the routes:
        // it must cover this path, and it is attached for the handlers that
        // refine a scope per-request. It is re-read here because the resolver
        // returns only the non-secret fingerprint, not the grant itself.
        Principal::Guest { .. } => {
            if let Some(p) = presented {
                let now = commonwealth_core::clock::unix_now_millis();
                match state.inner.node.guest_grants.live(p, now) {
                    Some(grant) if grant.permits_path(request.uri().path()) => {
                        // Debug, not info: the ring page drains its live lane
                        // on a timer, so this fires several times a second.
                        tracing::debug!(
                            peer = %peer,
                            path = %request.uri().path(),
                            label = ?grant.label,
                            scopes = %grant.summary(),
                            "client_auth: guest grant admitted"
                        );
                        // WHO, beside WHAT. A handle the store does not know
                        // under this grant is REFUSED rather than dropped to
                        // `None`: "this phone's session lapsed" and "this phone
                        // never claimed a name" are the two answers a guest
                        // most needs to tell apart, and defaulting the first to
                        // the second would silently un-name them mid-room
                        // (ARCH 6).
                        let session = match request
                            .headers()
                            .get(crate::routes_guest_session::RING_SESSION_HEADER)
                        {
                            None => None,
                            Some(raw) => {
                                let handle = raw.to_str().unwrap_or("").trim();
                                match state.inner.node.guest_sessions.live(handle, &grant, now) {
                                    Some(s) => Some(s),
                                    None => {
                                        tracing::info!(
                                            peer = %peer,
                                            path = %request.uri().path(),
                                            "client_auth: guest session handle is not live \
                                             under this grant"
                                        );
                                        return stale_session();
                                    }
                                }
                            }
                        };
                        request.extensions_mut().insert(Guest {
                            grant: Arc::new(grant),
                            session,
                        });
                        return next.run(request).await;
                    }
                    Some(grant) => {
                        // Out of scope, not unauthenticated. Say which — a bare
                        // 403 sends the operator hunting for a credential
                        // problem that isn't there.
                        tracing::info!(
                            peer = %peer,
                            path = %request.uri().path(),
                            scopes = %grant.summary(),
                            "client_auth: guest grant does not cover this path"
                        );
                        return guest_out_of_scope(&grant, request.uri().path());
                    }
                    // Not a live grant either — a raced revocation. Fall
                    // through to the shared refusal below.
                    None => {}
                }
            }
        }
        // A bearer that is not a grant. TWO credentials admit here and they
        // are independent: a NAMED token (one device, revocable alone) and
        // the daemon-wide one. The named set is read first because it is the
        // one that can be withdrawn without disturbing anything else, and
        // because its admit line can name WHO — see `crate::client_tokens`.
        Principal::RemoteClient { .. } => {
            if let Some(p) = presented {
                if let Some(label) = state.inner.node.named_client_tokens.label_for(p) {
                    // The LABEL, never the token: a credential in a log is a
                    // credential in every scrollback, bug report and log
                    // shipper downstream of it.
                    tracing::debug!(
                        peer = %peer,
                        path = %request.uri().path(),
                        label = %label,
                        "client_auth: named client token admitted"
                    );
                    return next.run(request).await;
                }
                if let Some(expected) = configured.as_ref() {
                    if constant_time_eq(p.as_bytes(), expected.as_bytes()) {
                        if state.inner.node.client_tokens.admits_shared_token() {
                            return next.run(request).await;
                        }
                        return shared_token_refused(&peer, request.uri().path());
                    }
                }
            }
        }
        // A member, a local owner or an anonymous caller is not admitted on a
        // remote gated path by this layer.
        _ => {}
    }

    // Nothing admitted. A daemon that never configured a client token is
    // reachable from somewhere remote and can serve nobody but a live guest;
    // saying THAT is more useful than a generic 401, which sends the operator
    // hunting for a credential problem when the node was simply never set up
    // to serve remotely. Same reasoning as `guest_out_of_scope`: name the
    // boundary when naming it leaks nothing the caller could not already
    // infer from being refused.
    if configured.is_none() {
        tracing::warn!(
            peer = %peer,
            path = %request.uri().path(),
            presented_a_bearer = presented.is_some(),
            "client_auth: remote caller refused and no client token configured — \
             (bind 127.0.0.1, or set a token to serve remotely)"
        );
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "remote access not configured" })),
        )
            .into_response();
    }
    unauthorized(if presented.is_some() {
        "bearer mismatch"
    } else {
        "missing bearer"
    })
}

/// The authenticated guest behind a request, attached by [`client_auth_layer`]
/// and read by the handlers that refine a scope per-request (today:
/// `routes_inference`). Absent on every other request — a loopback caller, a
/// full-token caller, and an unauthenticated one all have no `Guest`.
///
/// `Arc` because the grant is cloned out of the store once per request and read
/// by more than one place in a handler.
#[derive(Clone)]
pub struct Guest {
    /// What this caller may reach. THE decider — see
    /// [`GuestGrant::permits_path`].
    pub grant: Arc<GuestGrant>,
    /// WHO is holding the phone, when they have claimed a name
    /// ([`routes_guest_session`](crate::routes_guest_session)). `None` is a
    /// guest who has not claimed one yet — one QR serves a room, so the grant
    /// cannot answer this and the absence is never read as a name. A session
    /// carries no scope of its own: it says who, never what.
    pub session: Option<GuestSession>,
}

/// 409 for a phone presenting a session handle this grant does not know — a
/// grant that lapsed and was re-issued, a handle from another room, or a
/// session swept after its grant's expiry.
///
/// Its audience is a page that can fix it: the handle is not a credential, so
/// naming the state leaks nothing, and the shim's answer is to ask for the name
/// again. A 401 would be wrong — the BEARER authenticated fine.
fn stale_session() -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": "this ring session is no longer live under this link — claim a name again",
            "code": "stale_session",
        })),
    )
        .into_response()
}

/// 401 for a VALID daemon-wide token on a node that has stopped accepting it
/// (`[daemon] client_tokens = "named-only"`).
///
/// Named rather than folded into [`unauthorized`], for the same reason
/// [`guest_out_of_scope`] is: the audience is not a prober guessing
/// credentials — it is somebody holding a credential this node used to honour,
/// and "authentication required" would send them checking the token they are
/// already sending correctly. Naming the posture leaks nothing they could not
/// infer from being refused while the desktop on the same machine still works.
fn shared_token_refused(peer: &SocketAddr, path: &str) -> Response {
    tracing::warn!(
        peer = %peer,
        path = %path,
        "client_auth: the shared client token is refused under \
         [daemon] client_tokens = \"named-only\""
    );
    (
        StatusCode::UNAUTHORIZED,
        [("WWW-Authenticate", "Bearer")],
        Json(serde_json::json!({
            "error": "this node no longer admits the shared client token \
                      ([daemon] client_tokens = \"named-only\") — ask its operator \
                      for a token of your own (`svrn mesh token --new <label>`)",
            "code": "named_token_required",
        })),
    )
        .into_response()
}

/// 403 for a live grant that simply doesn't cover this route.
///
/// Distinct from [`unauthorized`] on purpose. That one deliberately withholds
/// which check failed, because its audience is a prober guessing credentials.
/// This one's audience is a guest holding a *valid* credential who asked for
/// something outside it — nothing is leaked by naming the boundary they already
/// hold, and refusing without saying why is how "the link is broken" tickets get
/// filed against a link that is working exactly as issued.
fn guest_out_of_scope(grant: &GuestGrant, path: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "error": {
                "message": format!(
                    "this guest link does not cover {path} — it grants: {}",
                    grant.summary()
                ),
                "type": "guest_scope",
                "code": "out_of_scope",
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_parsing_is_scheme_insensitive_and_trims() {
        fn req_with(auth: &str) -> Request {
            Request::builder()
                .header(axum::http::header::AUTHORIZATION, auth)
                .body(axum::body::Body::empty())
                .unwrap()
        }
        assert_eq!(bearer_token(&req_with("Bearer tok123")), Some("tok123"));
        assert_eq!(bearer_token(&req_with("bearer tok123")), Some("tok123"));
        assert_eq!(bearer_token(&req_with("BEARER  tok123 ")), Some("tok123"));
        assert_eq!(bearer_token(&req_with("Basic tok123")), None);
        assert_eq!(bearer_token(&req_with("Bearer ")), None);
        // No header at all.
        let bare = Request::builder().body(axum::body::Body::empty()).unwrap();
        assert_eq!(bearer_token(&bare), None);
    }

    #[test]
    fn exempt_paths_are_exact_match_only() {
        assert!(AUTH_EXEMPT_PATHS.contains(&"/status"));
        assert!(AUTH_EXEMPT_PATHS.contains(&"/oicp/v1/capabilities"));
        // A child path must NOT be exempt by prefix.
        assert!(!AUTH_EXEMPT_PATHS.contains(&"/status/../v1/chat/completions"));
        assert!(!AUTH_EXEMPT_PATHS.contains(&"/oicp/v1/capabilities/secret"));
    }

    /// The edge resolves the caller once and attaches the published
    /// `Principal` for the admission middlewares (`DAEMON_CORE.md` §3.3, "one
    /// resolution at the edge, one value"). A loopback caller is admitted
    /// without a credential, so this also proves the attachment happens on the
    /// early-admit branch, not only on the gated one.
    #[tokio::test]
    async fn the_edge_attaches_the_resolved_principal() {
        use axum::body::Body;
        use axum::routing::get;
        use axum::Router;
        use commonwealth_core::ids::{MeshId, NodeId};
        use commonwealth_core::mesh::Mesh;
        use std::collections::HashMap;
        use tower::ServiceExt;

        let mesh = Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id: MeshId::from_u128(1),
            name: "Attach Test".into(),
            invite_key_hash: [0u8; 32],
            invite_version: 0,
            require_encryption: false,
            members: HashMap::new(),
            peers: vec![],
        };
        let state = AppState::new(NodeId::from_u128(1), mesh);

        let app = Router::new()
            .route(
                "/probe",
                get(|req: Request| async move {
                    match req
                        .extensions()
                        .get::<crate::admission::AttachedPrincipal>()
                    {
                        Some(p) => p.0.label(),
                        None => "none".to_string(),
                    }
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                ClientAuthState::new(state, ClientAuthPolicy::default()),
                client_auth_layer,
            ));

        let mut req = Request::builder()
            .uri("/probe")
            .header("x-principal", "desktop")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:51000".parse::<SocketAddr>().unwrap(),
        ));

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            &body[..],
            b"owner:desktop",
            "the edge must attach the resolved principal for the inner layers"
        );
    }
}
