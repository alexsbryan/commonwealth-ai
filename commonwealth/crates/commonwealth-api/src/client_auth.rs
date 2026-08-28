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
//! (localhost-default + bearer token); node-identity auth for mesh
//! peers is a later milestone.
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
//!   matching the daemon's configured token (constant-time compare).
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
use commonwealth_knowledge::GuestGrant;
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
/// before any handler work.
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

    // Remote, gated path. TWO credentials can admit here — the daemon-wide
    // token and an ephemeral guest grant — and they are INDEPENDENT. The
    // daemon token is checked first so a guest token can never widen into
    // full access by matching an earlier arm; that ordering is the whole
    // constraint, and it is the only one.
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

    if let (Some(expected), Some(p)) = (configured.as_ref(), presented) {
        if constant_time_eq(p.as_bytes(), expected.as_bytes()) {
            return next.run(request).await;
        }
    }

    // A guest grant — a bearer that is NOT membership and NOT the daemon
    // token.
    //
    // This arm never inspects a `Scope` variant: it asks the grant whether
    // it permits this path and inserts the grant for the handler. That is
    // what keeps a newly-added scope from having to touch this file at all
    // — see `commonwealth_knowledge::guest_grant`.
    if let Some(p) = presented {
        let now = commonwealth_core::clock::unix_now_millis();
        match state.inner.guest_grants.live(p, now) {
            Some(grant) if grant.permits_path(request.uri().path()) => {
                let mut request = request;
                request.extensions_mut().insert(Guest(Arc::new(grant)));
                return next.run(request).await;
            }
            Some(grant) => {
                // Out of scope, not unauthenticated. Say which — a bare 403
                // sends the operator hunting for a credential problem that
                // isn't there.
                tracing::info!(
                    peer = %peer,
                    path = %request.uri().path(),
                    scopes = %grant.summary(),
                    "client_auth: guest grant does not cover this path"
                );
                return guest_out_of_scope(&grant, request.uri().path());
            }
            // Not a live grant either. Fall through to the shared refusal
            // below rather than answering here, so that "this node has no
            // client token" wins over "your bearer did not match" — it is
            // the operative fact, and the more actionable one.
            None => {}
        }
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
pub struct Guest(pub Arc<GuestGrant>);

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
}
