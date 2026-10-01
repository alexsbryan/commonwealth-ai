// SPDX-License-Identifier: AGPL-3.0-or-later
//! The keyed daemon — on-prem identity by API key (FIVE_PROGRAMS §2b step 3,
//! decision phase-b-86).
//!
//! # Why this exists
//!
//! Behind nginx on the same host every request arrives from loopback, and the
//! client surface admits a loopback caller as the owner
//! (`crate::client_auth`'s "Loopback caller → always admitted"). Worse, the
//! routers `EmbeddedDaemon::start_daemon` merges after `server::client_router`
//! — the turn, the documents, local-corpus ingest — sit outside
//! `client_auth_layer` altogether, because axum's `.layer` wraps only the
//! routes already present when it is applied. So a daemon fronted by a proxy
//! served every caller as its owner.
//!
//! # The decision
//!
//! A daemon whose key store holds at least one API key
//! ([`ClientTokenStore::is_keyed`](crate::client_tokens::ClientTokenStore::is_keyed))
//! is KEYED for its lifetime, and [`seal`] wraps the WHOLE merged client
//! router in [`keyed_auth_layer`]:
//!
//! - loopback grants nothing: the one edge resolver runs under
//!   [`ClientAuthPolicy::UNTRUSTED_LOOPBACK`], and only
//!   [`Principal::Asserted`] passes;
//! - no key, or a bearer that is not a key → 401 naming the key it needs;
//! - a key outside the `admin` group reaches only [`KEY_SCOPE`] — its own
//!   conversations and the document reads — and any other route is a 403
//!   naming the group. Default-deny is structural: a route a later row adds is
//!   admin-only until it is registered here (principle 10).
//!
//! A daemon with NO keys is unsealed: [`seal`] returns the router untouched,
//! so the desktop and every local user see nothing change (principle 6).
//!
//! # Ownership
//!
//! [`Caller`] replaces `LocalOnly` on the turn and document handlers. A keyed
//! caller's conversation ids are stored as `{sub}:{id}`, the scheme the
//! deleted server's `TenantRuntime` used, and [`KeyedOwners`] — the turn's
//! `PrincipalResolver` on a keyed daemon — reads the owner back from that
//! prefix and carries `[retrieval] corpora` as the corpus grant.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use sovereign_contracts::principal::{AttachedPrincipal, Principal};

use crate::client_auth::{ClientAuthPolicy, AUTH_EXEMPT_PATHS};
use crate::client_tokens::KEY_ADMIN_GROUP;
use crate::loopback_guard::{loopback_only, LocalOnly};
use crate::state::AppState;

/// What a key OUTSIDE the admin group may reach: `(method, path)`, where `*`
/// is any method and `{}` one path segment. Everything else is admin-only.
pub const KEY_SCOPE: &[(&str, &str)] = &[
    ("GET", "/v1/conversations"),
    ("POST", "/v1/conversations"),
    ("GET", "/v1/conversations/search"),
    ("*", "/v1/conversations/{}"),
    ("PUT", "/v1/conversations/{}/enabled-corpora"),
    ("POST", "/v1/conversations/{}/messages"),
    ("POST", "/v1/conversations/{}/messages/record"),
    ("GET", "/v1/conversations/{}/stream"),
    ("POST", "/v1/conversations/{}/end"),
    ("GET", "/v1/documents"),
    ("GET", "/v1/documents/{}"),
    ("GET", "/v1/documents/{}/progress"),
    ("POST", "/v1/documents/{}/ask"),
    ("GET", "/v1/documents/{}/ask/{}"),
];

/// Static segments a `{}` never matches, because the route they name is
/// admin-only: `/v1/documents/legacy` would otherwise read as a document id.
const RESERVED_SEGMENTS: &[&str] = &["legacy"];

/// Whether a key outside the admin group may make this request.
pub fn key_scope_permits(method: &Method, path: &str) -> bool {
    let segs: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    KEY_SCOPE.iter().any(|(m, pattern)| {
        let pat: Vec<&str> = pattern.split('/').collect();
        (*m == "*" || method.as_str() == *m)
            && pat.len() == segs.len()
            && pat.iter().zip(&segs).all(|(p, s)| match *p {
                "{}" => !s.is_empty() && !RESERVED_SEGMENTS.contains(s),
                lit => lit == *s,
            })
    })
}

/// Wrap the fully merged client router in [`keyed_auth_layer`] when this
/// daemon holds keys; return it untouched when it holds none.
pub fn seal(router: Router, state: &AppState) -> Router {
    if !state.inner.node.named_client_tokens.is_keyed() {
        tracing::debug!("api_keys: no API keys — the client surface is unkeyed");
        return router;
    }
    tracing::info!(
        "api_keys: this daemon holds API keys — every client route requires one, \
         and loopback grants nothing"
    );
    router.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        keyed_auth_layer,
    ))
}

/// The keyed daemon's one admission decision. See the module docs.
pub async fn keyed_auth_layer(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    if AUTH_EXEMPT_PATHS.contains(&path.as_str()) {
        return next.run(request).await;
    }
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let principal = state.resolve(
        request.headers(),
        peer,
        ClientAuthPolicy::UNTRUSTED_LOOPBACK,
    );
    let Principal::Asserted { sub, groups } = &principal else {
        let presented = request
            .headers()
            .contains_key(axum::http::header::AUTHORIZATION);
        tracing::info!(
            peer = ?peer,
            path = %path,
            resolved = %principal,
            presented_a_bearer = presented,
            "api_keys: refused — this daemon identifies every caller by API key"
        );
        return key_required(presented);
    };
    let admin = groups.iter().any(|g| g == KEY_ADMIN_GROUP);
    if !admin && !key_scope_permits(request.method(), &path) {
        tracing::info!(
            sub = %sub,
            method = %request.method(),
            path = %path,
            "api_keys: refused — the route is admin-only and this key is not in the admin group"
        );
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": format!(
                    "key '{sub}' is not in the '{KEY_ADMIN_GROUP}' group, and {} {path} needs it",
                    request.method()
                )
            })),
        )
            .into_response();
    }
    tracing::debug!(sub = %sub, admin, path = %path, "api_keys: key admitted");
    request
        .extensions_mut()
        .insert(AttachedPrincipal(principal.clone()));
    next.run(request).await
}

/// 401 for a caller that presented no key, or a bearer that is not one.
fn key_required(presented: bool) -> Response {
    let error = if presented {
        "the presented bearer is not an API key this daemon holds"
    } else {
        "this daemon identifies every caller by API key — send `Authorization: Bearer <key>`"
    };
    (
        StatusCode::UNAUTHORIZED,
        [("WWW-Authenticate", "Bearer")],
        Json(serde_json::json!({ "error": error })),
    )
        .into_response()
}

/// The router-level seal of the turn and document families: a keyed caller
/// the outer layer admitted, or a loopback caller exactly as
/// [`loopback_only`] decides it (same bytes when it refuses).
pub async fn local_or_keyed(request: Request, next: Next) -> Response {
    if asserted_sub(request.extensions()).is_some() {
        return next.run(request).await;
    }
    loopback_only(request, next).await
}

fn asserted_sub(ext: &axum::http::Extensions) -> Option<String> {
    match ext.get::<AttachedPrincipal>() {
        Some(AttachedPrincipal(Principal::Asserted { sub, .. })) => Some(sub.clone()),
        _ => None,
    }
}

/// Who owns what a turn or document handler touches. Replaces `LocalOnly`
/// on those handlers: a keyed caller is [`Caller::Keyed`]; anyone else must be
/// on loopback, with `LocalOnly`'s refusal bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// The local owner on an unkeyed daemon. Ids pass through unchanged.
    Local,
    /// A keyed caller. Its conversation ids are stored as `{sub}:{id}`.
    Keyed {
        /// The asserted subject.
        sub: String,
    },
}

impl<S> FromRequestParts<S> for Caller
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if let Some(sub) = asserted_sub(&parts.extensions) {
            return Ok(Self::Keyed { sub });
        }
        LocalOnly::from_request_parts(parts, state)
            .await
            .map(|LocalOnly| Self::Local)
    }
}

impl Caller {
    /// The stored id for the id this caller sent.
    pub fn scope(&self, id: &str) -> String {
        match self {
            Self::Local => id.to_string(),
            Self::Keyed { sub } => format!("{sub}:{id}"),
        }
    }

    /// The id this caller knows a stored id by, or `None` when the row is
    /// not this caller's.
    pub fn owned<'a>(&self, stored: &'a str) -> Option<&'a str> {
        match self {
            Self::Local => Some(stored),
            Self::Keyed { sub } => stored
                .strip_prefix(sub.as_str())
                .and_then(|r| r.strip_prefix(':')),
        }
    }
}

/// The turn's `PrincipalResolver` on a keyed daemon: the owner is the
/// `{sub}:` prefix [`Caller::scope`] wrote, and `[retrieval] corpora` is the
/// corpus grant every owner gets. An id with no prefix is unattributed, so its
/// turn sees no corpus (`PrincipalScope::Unresolved`).
pub struct KeyedOwners {
    /// `[retrieval] corpora`. Empty grants nothing.
    pub grant: Vec<String>,
}

impl sovereign_contracts::traits::PrincipalResolver for KeyedOwners {
    fn principal_for(&self, conversation_id: &str) -> Option<String> {
        conversation_id
            .split_once(':')
            .map(|(sub, _)| sub.to_string())
    }

    fn corpus_grant(&self) -> Option<Vec<String>> {
        Some(self.grant.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_scope_is_the_lawyer_surface_and_nothing_else() {
        let get = Method::GET;
        let post = Method::POST;
        assert!(key_scope_permits(&get, "/v1/conversations"));
        assert!(key_scope_permits(&Method::DELETE, "/v1/conversations/abc"));
        assert!(key_scope_permits(&get, "/v1/conversations/abc/stream"));
        assert!(key_scope_permits(&get, "/v1/documents/abc"));
        assert!(!key_scope_permits(&post, "/v1/documents"), "ingest by path");
        assert!(!key_scope_permits(&get, "/v1/documents/legacy"));
        assert!(!key_scope_permits(&post, "/v1/documents/legacy"));
        assert!(!key_scope_permits(&Method::DELETE, "/v1/documents/abc"));
        assert!(!key_scope_permits(&post, "/internal/corpus/local"));
        assert!(!key_scope_permits(&post, "/internal/corpus/watch/register"));
        assert!(!key_scope_permits(&post, "/v1/knowledge/search"));
        assert!(!key_scope_permits(&get, "/v1/conversations/abc/provenance"));
        assert!(!key_scope_permits(&get, "/v1/conversations//stream"));
    }

    #[test]
    fn a_keyed_caller_owns_only_its_own_prefix() {
        let alice = Caller::Keyed {
            sub: "alice".into(),
        };
        assert_eq!(alice.scope("c1"), "alice:c1");
        assert_eq!(alice.owned("alice:c1"), Some("c1"));
        assert_eq!(alice.owned("bob:c1"), None);
        assert_eq!(alice.owned("alicex:c1"), None);
        assert_eq!(alice.owned("c1"), None);
        assert_eq!(Caller::Local.owned("c1"), Some("c1"));
        use sovereign_contracts::traits::PrincipalResolver;
        let r = KeyedOwners { grant: vec![] };
        assert_eq!(r.principal_for("alice:c1").as_deref(), Some("alice"));
        assert_eq!(r.principal_for("c1"), None);
        assert_eq!(r.corpus_grant(), Some(vec![]), "empty grants nothing");
    }
}
