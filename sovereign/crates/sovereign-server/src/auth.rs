// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;

/// The tenant a request acts as. Defined in `oicp-types` — the serde-only
/// contract leaf both `commonwealth-core` and `sovereign-*` can see, which is
/// what `sovereign/deploy/mesh/GROUND_TRUTH.md` §"The layer contract that
/// decides where `TenantId` lives" settles. Re-exported here because this
/// module is where the type enters a request, and every consumer already
/// spells it `crate::auth::TenantId`. Reached through `sovereign-contracts`'
/// `oicp` re-export, so the move costs this crate no new Cargo edge (ARCH
/// §8.3).
pub use sovereign_contracts::oicp::TenantId;

pub use sovereign_contracts::principal::{AttachedPrincipal, Principal};
use sovereign_contracts::principal::{claimed_node_id, ClaimedNodeId};

/// Shared auth state extracted from config.
#[derive(Clone)]
pub struct AuthState {
    /// API key → tenant_id mapping. Empty = auth disabled.
    pub keys: Arc<HashMap<String, String>>,
}

impl AuthState {
    pub fn new(keys: HashMap<String, String>) -> Self {
        Self {
            keys: Arc::new(keys),
        }
    }

    pub fn disabled() -> Self {
        Self {
            keys: Arc::new(HashMap::new()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        !self.keys.is_empty()
    }
}

/// Extract the configured tenant string for a valid API key.
///
/// Returns the raw configured value — it is NOT validated here; the caller
/// parses it into a [`TenantId`] and refuses the request if it does not.
pub fn resolve_tenant(auth: &AuthState, api_key: &str) -> Option<String> {
    auth.keys.get(api_key).cloned()
}

/// This surface's ONE request-to-principal resolver.
///
/// `sovereign-server` has no iroh acceptor in front of it, so the strongest
/// thing it can say about a peer is what that peer TYPED. It says exactly
/// that: a readable claim is [`Principal::Member`], an unreadable one is
/// [`Principal::Unverified`] (never anonymous — the caller named itself and
/// failed), and no claim at all leaves the caller anonymous. The deciders
/// downstream — reciprocity's `UserKey` and the per-origin cap — read the
/// value, never the header (`sovereign_daemon::mesh_principal_gate` is the
/// ratchet, and it scans this crate too).
///
/// The wire form itself is read once, in `sovereign-contracts`, beside the key
/// it produces: two crates resolve principals and a private copy of the parser
/// in either is the FE-99 drift that clause forbids (ARCH principle 8).
fn resolve_principal(headers: &axum::http::HeaderMap) -> Principal {
    match claimed_node_id(headers) {
        ClaimedNodeId::Readable(node_id) => Principal::Member { node_id },
        ClaimedNodeId::Unreadable(raw) => {
            tracing::warn!(
                claimed = %raw,
                "a request claimed an origin node in a form that is not the \
                 canonical wire form — it is unverified, not anonymous, and \
                 it keys on no node"
            );
            Principal::Unverified
        }
        ClaimedNodeId::Absent => Principal::Anonymous,
    }
}

/// The principal a handler decides on: what [`auth_middleware`] attached, or
/// [`Principal::Anonymous`] where no auth layer ran.
///
/// Absent is anonymous and never a member: a route with no resolver in front
/// knows nothing about its caller, and "did not answer" must not become
/// "answered: a peer" (ARCH principle 6).
pub fn principal_of(attached: Option<axum::Extension<AttachedPrincipal>>) -> Principal {
    attached
        .map(|axum::Extension(a)| a.0)
        .unwrap_or(Principal::Anonymous)
}

/// Axum middleware that validates API keys.
/// Extracts the key from `Authorization: Bearer <key>` or `X-API-Key: <key>`.
/// Sets the tenant_id as a request extension.
pub async fn auth_middleware(request: Request, next: Next) -> Result<Response, StatusCode> {
    let auth = request
        .extensions()
        .get::<AuthState>()
        .cloned()
        .unwrap_or_else(AuthState::disabled);

    if !auth.is_enabled() {
        // Auth disabled — use default tenant. `default_tenant()` is
        // infallible by construction rather than a parsed literal, so this
        // path has no error arm to get wrong.
        let mut request = request;
        let principal = resolve_principal(request.headers());
        request.extensions_mut().insert(TenantId::default_tenant());
        request
            .extensions_mut()
            .insert(AttachedPrincipal(principal));
        return Ok(next.run(request).await);
    }

    // Try Authorization: Bearer <key>
    let api_key = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
        // Try X-API-Key: <key>
        .or_else(|| {
            request
                .headers()
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        });

    let api_key = match api_key {
        Some(k) => k,
        None => return Err(StatusCode::UNAUTHORIZED),
    };

    let tenant_id = match resolve_tenant(&auth, &api_key) {
        Some(t) => t,
        None => return Err(StatusCode::UNAUTHORIZED),
    };

    // The key matched, but the tenant it maps to is misconfigured. REFUSE —
    // do not fall back to the default tenant. Substituting here would silently
    // hand a misconfigured key another tenant's scope (ARCH §18.3).
    let tenant = match TenantId::parse(&tenant_id) {
        Ok(t) => t,
        Err(why) => {
            // Default (module-path) target, not a custom one: a custom
            // target is dark unless it is in the subscriber's filter, and a
            // refusal is exactly the event that must not be.
            tracing::warn!(
                tenant = %tenant_id,
                reason = %why,
                "refusing a request whose configured tenant id is invalid"
            );
            return Err(StatusCode::UNAUTHORIZED);
        }
    };

    let mut request = request;
    let principal = resolve_principal(request.headers());
    request.extensions_mut().insert(tenant);
    request
        .extensions_mut()
        .insert(AttachedPrincipal(principal));
    Ok(next.run(request).await)
}
