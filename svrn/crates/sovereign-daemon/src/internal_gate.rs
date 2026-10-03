// SPDX-License-Identifier: AGPL-3.0-or-later
//! Whether this caller may reach the internal surface (`:9742`) AT ALL — the
//! refusal `internal_principal` deliberately does not make.
//!
//! ## Why a second layer and not a branch in the resolver
//!
//! [`crate::internal_principal`] answers "who is asking", and its whole
//! contract is that it refuses nothing: which routes an unverified caller may
//! reach is a decision, and a resolver that also decided would be two answers
//! in one function. This layer is the decider, mounted INSIDE the resolver's
//! (`server.rs` applies it first, so tower runs it second) — it reads what the
//! resolver attached and decides nothing the resolver already decided.
//!
//! ## What it admits, and why each arm
//!
//! * **[`Principal::Member`]** — cw-rails proved the key and the roster names
//!   it. The strongest thing this port can know.
//! * **A true loopback caller that is not [`Principal::Unverified`]** — the
//!   daemon's own desktop, CLI and in-process callers, which present no mesh
//!   identity and never had to. `Unverified` is excluded on purpose: a local
//!   process that TYPED an `x-mesh-*` it could not prove asked to be believed,
//!   and being local is not an answer to that.
//!
//! There is no door for a caller with no identity yet: joining and gossip are
//! cw-rails' (pb-mesh-exit-transport), so nothing on this port is how a
//! stranger becomes a member.
//!
//! Everything else is `401` with a sentence naming what was missing, and one
//! `warn!` carrying the route, the peer and the principal's key.
//!
//! ## The posture is a knob, and its default is the strict one
//!
//! [`InternalAuth::Member`] is the default. [`InternalAuth::Perimeter`] is the
//! behaviour every build before this one had — every caller that can route to
//! the port is served — and it is logged once at startup as the weaker
//! posture rather than silently. A closed set is an enum, and an unknown
//! spelling refuses to start rather than falling back (ARCH principle 6): an
//! operator who typed `internal_auth = "members"` asked for the strict posture
//! and would otherwise silently get the open one.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;

use sovereign_contracts::principal::{AttachedPrincipal, Principal};

use crate::state::AppState;

/// What the internal port requires of a caller. **CLOSED SET**, resolved once
/// from `[daemon] internal_auth` and carried on the node part — no request
/// path reads config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InternalAuth {
    /// **The default.** A caller must be a member of this mesh — verified by
    /// cw-rails' key — or a local process on this machine. Anything else gets
    /// a 401.
    #[default]
    Member,
    /// The posture every build before this one had: the port is trusted
    /// because of where it is, and any caller that can route to it is served.
    /// The listener binds loopback, so this serves every local process.
    Perimeter,
}

impl InternalAuth {
    /// Parse the configured value. Refuses an unknown one rather than falling
    /// back to a default (ARCH principle 6 — see the module docs).
    pub fn parse(raw: &str) -> Result<Self, UnknownInternalAuth> {
        match raw.trim() {
            "member" => Ok(Self::Member),
            "perimeter" => Ok(Self::Perimeter),
            other => Err(UnknownInternalAuth {
                value: other.to_string(),
            }),
        }
    }

    /// The configured spelling, for a trace that says which posture is live.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Perimeter => "perimeter",
        }
    }
}

/// `[daemon] internal_auth` named something that is not a posture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownInternalAuth {
    /// What was configured, so the refusal can show it back.
    pub value: String,
}

impl std::fmt::Display for UnknownInternalAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[daemon] internal_auth = '{}' is not an internal-port posture — \
             it is \"member\" (only a member of this mesh, or a local process, \
             is served) or \"perimeter\" (any caller that can route to the \
             port is served)",
            self.value
        )
    }
}

impl std::error::Error for UnknownInternalAuth {}

/// Why this caller may not reach the internal surface, or `None` to serve it.
///
/// ONE decider, taking what was presented rather than a request, so the
/// decision is a pure function testable without a listener. The sentence it
/// returns is the one the wire body carries, so the log and the body cannot
/// say different things.
fn refusal(who: &Principal, peer: Option<SocketAddr>) -> Option<&'static str> {
    if matches!(who, Principal::Member { .. }) {
        return None;
    }
    if matches!(who, Principal::Unverified) {
        // Being local is not an answer to a claim that failed.
        return Some("this caller presented a mesh identity that could not be verified");
    }
    // A missing `ConnectInfo` is NOT loopback — the same stricter reading
    // `internal_principal` and `client_auth` both fail closed on.
    (!peer.is_some_and(|p| p.ip().is_loopback()))
        .then_some("this caller presented no mesh identity")
}

/// `from_fn_with_state`-compatible gate for the internal router. Apply it
/// BEFORE `internal_principal_layer` in the builder chain so the resolver
/// stays outermost: this layer reads what the resolver attached.
pub async fn internal_gate_layer(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if state.inner.node.internal_auth == InternalAuth::Perimeter {
        // Announced once at startup, not per request: a line on every call
        // would bury the posture it is trying to disclose.
        return next.run(request).await;
    }
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let who = request
        .extensions()
        .get::<AttachedPrincipal>()
        .map(|a| a.0.clone())
        // An absent extension means no resolver ran in front, which on this
        // router is unreachable (`internal_principal_layer` is its outermost
        // layer). Read as `Anonymous` for the same reason
        // `admission::requester` reads it that way — named, not assumed.
        .unwrap_or(Principal::Anonymous);
    let path = request.uri().path().to_string();

    match refusal(&who, peer) {
        None => next.run(request).await,
        Some(sentence) => {
            tracing::warn!(
                target: "transport",
                route = %path,
                peer = ?peer,
                principal = %who.label(),
                "internal: refused — {sentence}"
            );
            (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": sentence })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_types::NodeId;

    fn loopback() -> Option<SocketAddr> {
        Some("127.0.0.1:41000".parse().unwrap())
    }

    fn lan() -> Option<SocketAddr> {
        Some("10.0.0.7:41000".parse().unwrap())
    }

    #[test]
    fn a_stranger_on_the_lan_is_refused_and_told_what_is_missing() {
        assert_eq!(
            refusal(&Principal::Anonymous, lan()),
            Some("this caller presented no mesh identity")
        );
    }

    #[test]
    fn a_lan_caller_that_typed_an_identity_is_told_the_claim_failed() {
        assert_eq!(
            refusal(&Principal::Unverified, lan()),
            Some("this caller presented a mesh identity that could not be verified")
        );
    }

    #[test]
    fn a_verified_member_is_admitted_from_anywhere() {
        let who = Principal::Member {
            node_id: NodeId::from_u128(9),
        };
        assert_eq!(refusal(&who, lan()), None);
    }

    /// The daemon's own callers — desktop, CLI, in-process — present nothing
    /// and must keep being served.
    #[test]
    fn a_local_caller_presenting_nothing_is_admitted() {
        assert_eq!(refusal(&Principal::Anonymous, loopback()), None);
    }

    /// Being local is not an answer to a claim that failed.
    #[test]
    fn a_local_caller_that_typed_an_unverifiable_identity_is_still_refused() {
        assert_eq!(
            refusal(&Principal::Unverified, loopback()),
            Some("this caller presented a mesh identity that could not be verified")
        );
    }

    /// No `ConnectInfo` fails closed, matching `internal_principal`.
    #[test]
    fn a_caller_with_no_peer_address_is_not_treated_as_local() {
        assert_eq!(
            refusal(&Principal::Anonymous, None),
            Some("this caller presented no mesh identity")
        );
    }

    #[test]
    fn an_unknown_posture_is_refused_rather_than_read_as_the_default() {
        assert_eq!(InternalAuth::parse("member"), Ok(InternalAuth::Member));
        assert_eq!(
            InternalAuth::parse("perimeter"),
            Ok(InternalAuth::Perimeter)
        );
        assert_eq!(InternalAuth::default(), InternalAuth::Member);
        let e = InternalAuth::parse("members").expect_err("not a posture");
        assert_eq!(e.value, "members");
        assert!(e.to_string().contains("perimeter"));
    }
}
