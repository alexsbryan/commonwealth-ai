// SPDX-License-Identifier: AGPL-3.0-or-later
//! Guest-grant lifecycle routes — mint, revoke, list.
//!
//! These are the operator's side of an ephemeral mesh link. `svrn mesh grant`
//! drives them; the guest never touches this module.
//!
//! # Why these live on the OPERATOR surface, and nowhere else
//!
//! Mounted only on [`ClientSurface::Operator`] — `:9741`, the bind an operator
//! reaches by actually being on this machine. Three principals are ruled out,
//! each structurally:
//!
//! - **`:9742`** is perimeter-trusted: its routes carry no auth gate at all
//!   (see the frontdoor comment at the bottom of `server::internal_router`).
//!   A mint route there would let any mesh peer forge guest credentials for
//!   outsiders — strictly worse than the membership it was meant to narrow.
//! - **A guest** cannot reach them because no [`Scope`] names these paths. So
//!   grants cannot mint grants.
//! - **A mesh peer** cannot reach them because the peer bind of the client
//!   router does not SERVE them — it 404s.
//!
//! That last one was wrong until 2026-08-28, and the way it was wrong is worth
//! keeping. This doc used to argue that `:9741` was safe because "the
//! `client_auth` layer already means loopback-or-full-token". It does not, for
//! a caller the iroh acceptor forwards: the acceptor `TcpStream::connect`s
//! `127.0.0.1`, so a MEMBER dialling `CLIENT_ALPN` arrived wearing a loopback
//! address it did not earn and was admitted before any credential was read.
//! Note `3d2f1ae0`. A loopback guard on these handlers would have read as a fix
//! and gated nothing — which is why the fix is a third listener serving a
//! smaller router, not a predicate. See [`ClientSurface`].
//!
//! `/internal/inference/warmup` got the first half of this correction on
//! 2026-07-27 (off `:9742`, an unauthenticated "make that node load 18.5 GB off
//! disk" lever) and the second half here, in the same move: same surface, same
//! reasoning, decided once.
//!
//! [`Scope`]: sovereign_grants::Scope
//! [`ClientSurface`]: crate::server::ClientSurface
//! [`ClientSurface::Operator`]: crate::server::ClientSurface::Operator

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use sovereign_grants::guest_grant::{Scope, DEFAULT_GUEST_TTL_SECS};

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
    /// The single rail namespace this grant may read and write. One per
    /// grant — see [`Scope::Rails`] for why it is not a list.
    #[serde(default)]
    pub rail: Option<String>,
    /// The whole WALL: every namespace `[daemon.guest_pages]` declares. Names
    /// no namespace, because the registry is the one that does — see
    /// [`Scope::Wall`].
    #[serde(default)]
    pub wall: Option<bool>,
}

impl ScopeRequest {
    /// The scopes this request asks for, or the sentence refusing it.
    ///
    /// **The wall and one namespace are two different asks, and asking for
    /// both is a contradiction rather than a union.** `--wall --rail x` reads
    /// as "reach everything declared, and also only x"; minting the wider of
    /// the two would hand out reach the operator did not mean to give, and
    /// minting the narrower would quietly ignore half of what they typed.
    /// Refused, in the CLI and here, so neither surface is the only guard.
    fn into_scopes(self) -> Result<Vec<Scope>, String> {
        let mut out = Vec::new();
        if let Some(models) = self.models {
            out.push(Scope::Models(models));
        }
        match (self.wall.unwrap_or(false), self.rail) {
            (true, Some(rail)) => {
                return Err(format!(
                    "a grant is for the whole wall or for one app, not both — drop \
                     `scopes.wall` to keep '{rail}', or drop `scopes.rail` to reach \
                     every app this door registered for guests"
                ))
            }
            (true, None) => out.push(Scope::Wall),
            (false, Some(rail)) => out.push(Scope::Rails(rail)),
            (false, None) => {}
        }
        Ok(out)
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
    /// The base a phone opens (the room address, or the static origin). When
    /// present, the response carries the composed `link`, so a client that
    /// cannot compose it itself — the desktop, which is an HTTP client and
    /// does not link the mesh crates — displays exactly what the CLI would.
    #[serde(default)]
    pub url: Option<String>,
    /// THIS node's iroh dial string, when the caller has one to give
    /// (`/v1/mesh/status` → `self_reachability.dial`; the CLI reads the same
    /// field). Composed into the link as `iroh=` so a guest who shares no
    /// network with this machine can still reach it. Absent is the direct
    /// (plain-HTTP) form, and is never invented here: the daemon cannot read
    /// its own dial on this route — the extension that owns it is installed
    /// per-router and this surface does not carry it.
    #[serde(default)]
    pub dial: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GuestGrantResponse {
    pub token: String,
    pub expires_at_ms: u64,
    /// One-line rendering of what this grant buys, for the link's display
    /// string. Display only — the store is the authority.
    pub summary: String,
    /// The composed guest link when the request named a `url` — the same
    /// string the CLI writes into its QR. Absent otherwise (absence is
    /// reported, never a guessed base).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub link: Option<String>,
}

/// POST /internal/guest/grant — mint an ephemeral guest grant.
pub async fn guest_grant_issue(
    State(state): State<AppState>,
    Json(req): Json<GuestGrantRequest>,
) -> Result<Json<GuestGrantResponse>, (StatusCode, Json<ErrorBody>)> {
    // What composing the link needs, read BEFORE `scopes` is consumed.
    let url = req.url.clone();
    let dial = req.dial.clone();
    let rail = req.scopes.rail.clone();
    let wall = req.scopes.wall.unwrap_or(false);
    let scopes = req
        .scopes
        .into_scopes()
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(ErrorBody { error })))?;
    if scopes.is_empty() {
        // A grant that permits nothing is a legal state in the store, but
        // minting one is always a mistake — refuse rather than hand back a
        // link that cannot do anything (§18.3: absence reported, not defaulted).
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: "a grant must name at least one scope — pass `scopes.models`, \
                        `scopes.rail` or `scopes.wall`"
                    .into(),
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
        let ids = match scope {
            Scope::Models(ids) => ids,
            // A rail scope names a namespace, not a model, so there is
            // nothing to check against the dispatchable set. An unnamed
            // namespace is not an error either: a rail namespace is
            // created by its first write, so "not seen before" is the
            // normal case for the first app deployed to a ring.
            Scope::Rails(_) | Scope::Wall => continue,
        };
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
        .node
        .guest_grants
        .issue(token, scopes, req.label, ttl_secs, now_ms);

    tracing::info!(
        expires_at_ms = grant.expires_at_ms,
        grants = %grant.summary(),
        label = grant.label.as_deref().unwrap_or(""),
        "guest_grant: issued an ephemeral guest grant"
    );

    // The link a phone opens, when the caller named a base. ONE composer
    // (`sovereign_mesh::deep_link::wall_https_link`) — the same string the CLI
    // writes into its QR — so the desktop can display a link it cannot build
    // (it is an HTTP client; it does not link the mesh crates). The dial is
    // the caller's, read from this daemon's own status; see the field doc.
    let link = guest_link(
        url.as_deref(),
        rail.as_deref(),
        wall,
        dial.as_deref().filter(|d| !d.is_empty()),
        &grant.token,
        grant.expires_at_ms / 1_000,
        &grant.summary(),
    );

    Ok(Json(GuestGrantResponse {
        token: grant.token.clone(),
        expires_at_ms: grant.expires_at_ms,
        summary: grant.summary(),
        link,
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
    let revoked = state.inner.node.guest_grants.revoke(&req.token).is_some();
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
            .node
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

/// The link a guest opens, or `None` when the caller named no base.
///
/// ONE composer: [`sovereign_mesh::deep_link::wall_https_link`], the same one
/// the CLI's QR encodes — so a link the desktop displays from this response
/// and a link the CLI prints cannot disagree. `dial` is this node's own
/// reachability string; absent when iroh is not running, in which case the
/// link is the direct (plain-HTTP) form.
fn guest_link(
    url: Option<&str>,
    rail: Option<&str>,
    wall: bool,
    dial: Option<&str>,
    token: &str,
    expires_at_secs: u64,
    summary: &str,
) -> Option<String> {
    let base = url?;
    Some(sovereign_mesh::deep_link::wall_https_link(
        token,
        base,
        rail,
        wall,
        expires_at_secs,
        (!summary.is_empty()).then_some(summary),
        dial,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The response link is the CLI's link, built by the one composer; and
    /// absent when the caller named no base (absence reported, never a
    /// guessed address).
    #[test]
    fn the_response_link_is_the_cli_link_or_absent() {
        let link = guest_link(
            Some("https://svrnme.sh"),
            None,
            true,
            Some("5a46ef@https://relay.example/,10.0.0.1:1"),
            "tok",
            1_790_112_357,
            "primary; the wall",
        )
        .expect("a url was given");
        // The door's page path, the token, and the dial string, in the fragment.
        assert!(link.starts_with("https://svrnme.sh/ring/"), "{link}");
        assert!(link.contains("token=tok"), "{link}");
        assert!(link.contains("iroh="), "{link}");
        match sovereign_mesh::deep_link::parse_https_guest_link(&link) {
            Some(sovereign_mesh::deep_link::DeepLink::Guest { token, dial, .. }) => {
                assert_eq!(token, "tok");
                assert!(dial.is_some());
            }
            _ => panic!("the link did not parse: {link}"),
        }

        // No base → no link.
        assert!(guest_link(None, None, true, None, "tok", 1, "").is_none());

        // No dial → the direct form, with no `iroh=`.
        let direct =
            guest_link(Some("http://10.0.0.1:19947"), None, true, None, "t", 1, "").unwrap();
        assert!(!direct.contains("iroh="), "{direct}");

        // A rail grant names the app's page, not the door's index.
        let railed = guest_link(
            Some("http://10.0.0.1:19947"),
            Some("house-expenses"),
            false,
            None,
            "t",
            1,
            "",
        )
        .unwrap();
        assert!(
            railed.starts_with("http://10.0.0.1:19947/ring/house-expenses/"),
            "{railed}"
        );
    }
}
