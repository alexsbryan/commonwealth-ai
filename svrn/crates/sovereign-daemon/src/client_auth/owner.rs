// SPDX-License-Identifier: AGPL-3.0-or-later
//! The owner, and the routes only the owner reaches (ADDRESSED_TEXT §5.5 rule
//! 3): minting a named credential and issuing a guest grant.
//!
//! Until 2026-10-08 these handlers carried no check of their own. The
//! operator listener was their whole guard, so any caller that passed
//! `client_auth` there could mint: a remote holder of the shared or a named
//! token on a non-loopback operator bind (appendix defect 5), and, once a
//! presented credential decides (rule 1), a named client on loopback, which
//! is a client and not the owner.
//!
//! The owner is one of two callers:
//!
//! - a local process presenting no credential, where loopback is the owner
//!   (`[daemon] loopback = "owner"`), decided by the one locality predicate
//!   (`host_kit::locality`), so a web page is never it;
//! - a named credential in the `admin` group, which is how the owner of a
//!   daemon under `loopback = "none"` is anyone at all.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use host_kit::locality::RequestLocality;
use sovereign_contracts::principal::{AttachedPrincipal, Principal};

use crate::client_tokens::KEY_ADMIN_GROUP;
use crate::state::AppState;

/// Whether `who`, arriving as `locality`, is this daemon's owner. THE one
/// decider: [`owner_only`] asks it and nothing else re-derives the rule.
pub fn is_owner(who: &Principal, locality: &RequestLocality, loopback_is_owner: bool) -> bool {
    match who {
        Principal::Asserted { groups, .. } => groups.iter().any(|g| g == KEY_ADMIN_GROUP),
        // What a local process presenting nothing resolves to: named by
        // `X-Principal` or not.
        Principal::LocalOwner { .. } | Principal::Anonymous => {
            loopback_is_owner && locality.is_local()
        }
        Principal::RemoteClient { .. }
        | Principal::Member { .. }
        | Principal::Guest { .. }
        | Principal::Unverified => false,
    }
}

/// `from_fn_with_state` middleware for the owner-only routes. Runs inside
/// `client_auth_layer`, whose principal it reads; a request with none
/// attached is refused rather than read as the owner (fail closed).
pub async fn owner_only(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let who = request
        .extensions()
        .get::<AttachedPrincipal>()
        .map(|p| p.0.clone());
    let locality = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| RequestLocality::of(&c.0, request.headers()));
    let owner = match (&who, &locality) {
        (Some(who), Some(at)) => is_owner(who, at, state.loopback_is_owner()),
        _ => false,
    };
    if owner {
        return next.run(request).await;
    }
    let label = who
        .as_ref()
        .map_or_else(|| "unresolved".to_string(), Principal::label);
    tracing::warn!(
        path = %request.uri().path(),
        principal = %label,
        locality = ?locality,
        "client_auth: refused an owner-only route to a caller that is not the owner"
    );
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "error": "owner-only",
            "detail": "minting credentials and guest grants is the owner's: a local process \
                       presenting nothing, or a credential in the admin group",
            "principal": label,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner is a local process presenting nothing, or an admin
    /// credential; a named client is not, wherever it calls from. Failing
    /// input: admit any `Asserted`, and a harness token mints tokens.
    #[test]
    fn a_named_client_is_not_the_owner_and_an_admin_credential_is() {
        let local = RequestLocality::Local;
        let named = Principal::Asserted {
            sub: "claude-code".into(),
            groups: vec![],
        };
        let admin = Principal::Asserted {
            sub: "it".into(),
            groups: vec![KEY_ADMIN_GROUP.into()],
        };
        assert!(!is_owner(&named, &local, true));
        assert!(is_owner(&admin, &RequestLocality::Remote, false));
        assert!(is_owner(&Principal::Anonymous, &local, true));
        assert!(is_owner(
            &Principal::LocalOwner {
                sub_identity: Some("desktop".into())
            },
            &local,
            true
        ));
        assert!(
            !is_owner(&Principal::Anonymous, &local, false),
            "loopback = none"
        );
        assert!(!is_owner(
            &Principal::Anonymous,
            &RequestLocality::CrossOrigin("https://evil.example".into()),
            true
        ));
        assert!(!is_owner(
            &Principal::RemoteClient {
                credential: "f".into()
            },
            &RequestLocality::Remote,
            true
        ));
    }
}
