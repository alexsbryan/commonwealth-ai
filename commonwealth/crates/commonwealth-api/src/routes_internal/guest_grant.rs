// SPDX-License-Identifier: AGPL-3.0-or-later
//! Guest-grant lifecycle routes — mint, revoke, list.
//!
//! These are the operator's side of an ephemeral mesh link. `svrn mesh grant`
//! drives them; the guest never touches this module.
//!
//! # Why these live on `:9741` and not `:9742`
//!
//! `:9742` is perimeter-trusted: its routes carry no auth gate at all (see the
//! frontdoor comment at the bottom of `server::internal_router`). A mint route
//! there would let **any mesh peer forge guest credentials for outsiders** —
//! which is strictly worse than the membership it was supposed to be narrower
//! than.
//!
//! On `:9741` the `client_auth` layer already means loopback-or-full-token, and
//! a guest cannot reach these routes because **no [`Scope`] names them**. So
//! grants cannot mint grants, structurally, without a check anywhere.
//!
//! This is the same correction `/internal/inference/warmup` already got — it
//! sat on `internal_router` until 2026-07-27, "reachable by any mesh PEER, i.e.
//! an unauthenticated 'make that node load 18.5 GB off disk' lever". Same
//! mistake, so: same port, same reasoning, decided once.
//!
//! [`Scope`]: commonwealth_knowledge::Scope

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use commonwealth_knowledge::guest_grant::{Scope, DEFAULT_GUEST_TTL_SECS};

use crate::state::AppState;

use super::ErrorBody;

/// The scope set a mint request asks for.
///
/// One field per [`Scope`] variant, all optional — so adding a variant later is
/// an added key, not a wire break, and an old client's body still parses.
///
/// `deny_unknown_fields` is load-bearing (§4.3, §18.3): an operator who
/// misspells a scope key must get a 400, not a grant that silently permits less
/// than they asked for. A grant is a security object; quietly narrowing one is
/// the same class of failure as quietly widening it, because the operator walks
/// away believing something untrue about what they handed out.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeRequest {
    /// Exact model ids this grant may dispatch.
    #[serde(default)]
    pub models: Option<Vec<String>>,
}

impl ScopeRequest {
    fn into_scopes(self) -> Vec<Scope> {
        let mut out = Vec::new();
        if let Some(models) = self.models {
            out.push(Scope::Models(models));
        }
        out
    }
}

#[derive(Debug, Deserialize)]
pub struct GuestGrantRequest {
    pub scopes: ScopeRequest,
    /// Lifetime in seconds. Omitted → [`DEFAULT_GUEST_TTL_SECS`]; clamped to
    /// the store's max.
    #[serde(default)]
    pub ttl_secs: Option<u64>,
    /// Operator's own note, echoed back by `list`. Never consulted.
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GuestGrantResponse {
    pub token: String,
    pub expires_at_ms: u64,
    /// One-line rendering of what this grant buys, for the link's display
    /// string. Display only — the store is the authority.
    pub summary: String,
}

/// POST /internal/guest/grant — mint an ephemeral guest grant.
pub async fn guest_grant_issue(
    State(state): State<AppState>,
    Json(req): Json<GuestGrantRequest>,
) -> Result<Json<GuestGrantResponse>, (StatusCode, Json<ErrorBody>)> {
    let scopes = req.scopes.into_scopes();
    if scopes.is_empty() {
        // A grant that permits nothing is a legal state in the store, but
        // minting one is always a mistake — refuse rather than hand back a
        // link that cannot do anything (§18.3: absence reported, not defaulted).
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: "a grant must name at least one scope — pass `scopes.models`".into(),
            }),
        ));
    }

    // Validate every named model against what this node can ACTUALLY dispatch,
    // using the same set `/v1/models` reports. A grant minted for a name
    // nothing advertises is born broken: it looks fine to the operator, and
    // 403s on the guest's first request with a message about scope that sends
    // them hunting in the wrong place.
    let dispatchable = crate::routes_inference::dispatchable_ids(&state).await;
    for scope in &scopes {
        let Scope::Models(ids) = scope;
        for id in ids {
            if !dispatchable.iter().any(|d| d == id) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(ErrorBody {
                        error: format!(
                            "no model named '{id}' is dispatchable from this node — \
                             check `/v1/models` for the names it can serve"
                        ),
                    }),
                ));
            }
        }
    }

    let token = commonwealth_transport::identity::generate_bearer_token().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: format!("could not mint a token: {e}"),
            }),
        )
    })?;

    let ttl_secs = req.ttl_secs.unwrap_or(DEFAULT_GUEST_TTL_SECS);
    let now_ms = commonwealth_core::clock::unix_now_millis();
    let grant = state
        .inner
        .guest_grants
        .issue(token, scopes, req.label, ttl_secs, now_ms);

    tracing::info!(
        expires_at_ms = grant.expires_at_ms,
        grants = %grant.summary(),
        label = grant.label.as_deref().unwrap_or(""),
        "guest_grant: issued an ephemeral guest grant"
    );

    Ok(Json(GuestGrantResponse {
        token: grant.token.clone(),
        expires_at_ms: grant.expires_at_ms,
        summary: grant.summary(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct GuestGrantRevokeRequest {
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct GuestGrantRevokeResponse {
    /// True when a grant was found and revoked; false when there was nothing to
    /// revoke (idempotent — still 200).
    pub revoked: bool,
}

/// POST /internal/guest/grant/revoke — kill a link immediately.
///
/// The next request bearing that token fails closed: `revoke` flips the flag in
/// place and `live()` filters on it, so there is no window where a concurrent
/// request slips through behind the sweep.
pub async fn guest_grant_revoke(
    State(state): State<AppState>,
    Json(req): Json<GuestGrantRevokeRequest>,
) -> Json<GuestGrantRevokeResponse> {
    let revoked = state.inner.guest_grants.revoke(&req.token).is_some();
    if revoked {
        tracing::info!("guest_grant: revoked a guest grant");
    }
    Json(GuestGrantRevokeResponse { revoked })
}

#[derive(Debug, Serialize)]
pub struct GuestGrantRow {
    /// First 8 hex chars, enough to identify a row for `--revoke` without
    /// putting whole bearers in terminal scrollback and shell history.
    pub token_prefix: String,
    pub summary: String,
    pub label: Option<String>,
    pub expires_at_ms: u64,
    pub revoked: bool,
    pub live: bool,
}

/// GET /internal/guest/grant/list — what is outstanding.
///
/// Returns revoked and expired rows too, flagged: "I revoked that, right?" is
/// the question this surface exists to answer, and a list that silently omits
/// them cannot.
pub async fn guest_grant_list(State(state): State<AppState>) -> Json<Vec<GuestGrantRow>> {
    let now_ms = commonwealth_core::clock::unix_now_millis();
    Json(
        state
            .inner
            .guest_grants
            .all()
            .into_iter()
            .map(|g| GuestGrantRow {
                token_prefix: g.token.chars().take(8).collect(),
                summary: g.summary(),
                label: g.label.clone(),
                expires_at_ms: g.expires_at_ms,
                revoked: g.revoked,
                live: g.is_live(now_ms),
            })
            .collect(),
    )
}
