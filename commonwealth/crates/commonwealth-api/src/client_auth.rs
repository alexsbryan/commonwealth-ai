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

use std::net::SocketAddr;

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

/// Exact request paths that remain reachable without a token, even
/// from a non-loopback caller. Both are read-only and advertise-by-
/// design: `/oicp/v1/capabilities` is the federation handshake a peer
/// reads to decide whether to peer, and `/status` is the liveness /
/// pairing surface. Matched by EXACT equality (not prefix), so no
/// child path inherits the exemption.
pub const AUTH_EXEMPT_PATHS: &[&str] = &["/status", "/oicp/v1/capabilities"];

/// Constant-time byte comparison. Unequal lengths short-circuit
/// (length is not the secret — the token width is fixed and public);
/// equal lengths fold an XOR accumulator over every byte so the
/// running time doesn't depend on the position of the first
/// mismatch. Dependency-free; deliberately NOT `==` (which
/// short-circuits at the first differing byte and leaks a timing
/// oracle on the secret).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

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
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
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

    // Loopback is always local — admit without a token.
    if peer.ip().is_loopback() {
        return next.run(request).await;
    }

    // Federation/health surface stays open to remote callers.
    if AUTH_EXEMPT_PATHS.contains(&request.uri().path()) {
        return next.run(request).await;
    }

    // Remote, gated path → require the configured token.
    let Some(expected) = state.client_token() else {
        // Bound somewhere a remote could reach us, but no token was
        // ever configured. Refuse — never admit an unauthenticated
        // remote caller just because the operator forgot a secret.
        tracing::warn!(
            peer = %peer,
            path = %request.uri().path(),
            "client_auth: remote caller but no client token configured — \
             refusing (bind 127.0.0.1, or set a token to serve remotely)"
        );
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "remote access not configured" })),
        )
            .into_response();
    };

    match bearer_token(&request) {
        Some(presented) if constant_time_eq(presented.as_bytes(), expected.as_bytes()) => {
            next.run(request).await
        }
        Some(_) => unauthorized("bearer mismatch"),
        None => unauthorized("missing bearer"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_only_identical_bytes() {
        assert!(constant_time_eq(b"abc123", b"abc123"));
        assert!(!constant_time_eq(b"abc123", b"abc124"));
        assert!(!constant_time_eq(b"abc", b"abcd")); // length differs
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }

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
