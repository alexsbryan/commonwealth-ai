// SPDX-License-Identifier: AGPL-3.0-or-later
//! Named-credential lifecycle routes — mint, revoke, list.
//!
//! `svrn daemon key` drives them when a daemon is running; with none
//! running it edits the store directly. Nothing else does.
//!
//! # Why these live on the OPERATOR surface, and nowhere else
//!
//! Mounted only on [`ClientSurface::Operator`] — the same listener, for the
//! same three reasons, as [`super::guest_grant`]: the internal port would let
//! any mesh peer mint a client credential for an outsider, no [`Scope`] names
//! these paths so a guest cannot reach them, and the peer bind of the client
//! router does not serve them at all. Read that module's header before
//! changing which surface carries these; the reasoning is decided once, there.
//!
//! The listener is not the whole guard: `server.rs` mounts these behind
//! `crate::client_auth::owner_only`, because a named client on loopback is a
//! client, not the owner (ADDRESSED_TEXT §5.5 rule 3).
//!
//! [`Scope`]: sovereign_grants::Scope
//! [`ClientSurface::Operator`]: crate::server::ClientSurface::Operator

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

use super::ErrorBody;

/// The three routes, mounted as one. A `Router` rather than three exported
/// handlers because `server.rs` is over its size ceiling and new code goes
/// beside it, not in it.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/internal/client/token", post(client_token_issue))
        .route("/internal/client/token/revoke", post(client_token_revoke))
        .route("/internal/client/token/list", get(client_token_list))
}

/// Mint a credential named `name` asserting `groups`.
#[derive(Debug, Deserialize)]
pub struct ClientTokenRequest {
    /// What calls made with it are logged by, and what revokes it. Letters,
    /// digits, `-` and `_`.
    pub name: String,
    /// The groups it asserts. `admin` reaches ingest, the admin routes and
    /// minting.
    #[serde(default)]
    pub groups: Vec<String>,
}

/// The one moment the token itself crosses a wire.
#[derive(Debug, Serialize)]
pub struct ClientTokenResponse {
    /// The bearer to hand to the client. Not recoverable from `list`; it is
    /// recoverable from `<data_dir>/client-tokens/<name>.key` by whoever can
    /// read the daemon's data directory, which is the operator.
    pub token: String,
    /// The bucket key a log line carries — see [`crate::client_tokens`].
    pub fingerprint: String,
}

/// POST /internal/client/token — mint a named credential.
pub async fn client_token_issue(
    State(state): State<AppState>,
    Json(req): Json<ClientTokenRequest>,
) -> Result<Json<ClientTokenResponse>, (StatusCode, Json<ErrorBody>)> {
    let token = crate::client_auth::generate_bearer_token().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: format!("could not mint a token: {e}"),
            }),
        )
    })?;
    let row = state
        .inner
        .node
        .named_client_tokens
        .mint(&req.name, &req.groups, token.clone())
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorBody {
                    error: e.to_string(),
                }),
            )
        })?;
    Ok(Json(ClientTokenResponse {
        token,
        fingerprint: row.fingerprint,
    }))
}

/// Revoke the credential named `name`.
#[derive(Debug, Deserialize)]
pub struct ClientTokenRevokeRequest {
    /// The name, not the token: revoking by a secret means having the secret
    /// to hand, which is exactly what the operator does not keep.
    pub name: String,
}

/// Whether there was anything to revoke.
#[derive(Debug, Serialize)]
pub struct ClientTokenRevokeResponse {
    /// True when a credential was found and revoked; false when no such name
    /// existed (idempotent — still 200, and the CLI says so).
    pub revoked: bool,
}

/// POST /internal/client/token/revoke — stop admitting one client.
///
/// Takes effect on the next request, in this daemon's lifetime: the store
/// mutates its in-memory set and deletes the file. No restart.
pub async fn client_token_revoke(
    State(state): State<AppState>,
    Json(req): Json<ClientTokenRevokeRequest>,
) -> Json<ClientTokenRevokeResponse> {
    let revoked = state.inner.node.named_client_tokens.revoke(&req.name);
    Json(ClientTokenRevokeResponse { revoked })
}

/// One named credential, as `list` reports it.
#[derive(Debug, Serialize)]
pub struct ClientTokenRow {
    /// The name it was minted under.
    pub name: String,
    /// The groups it asserts.
    pub groups: Vec<String>,
    /// The bucket key, so a row here can be matched against a log line
    /// without either of them holding a credential.
    pub fingerprint: String,
}

/// GET /internal/client/token/list — which clients this node admits.
///
/// Never the tokens. A list surface that printed them would put every
/// credential the node holds into one response, one scrollback and one
/// screenshot — and the question this answers is "who can reach me", which
/// names answer completely.
pub async fn client_token_list(State(state): State<AppState>) -> Json<Vec<ClientTokenRow>> {
    Json(
        state
            .inner
            .node
            .named_client_tokens
            .list()
            .into_iter()
            .map(|r| ClientTokenRow {
                name: r.name,
                groups: r.groups,
                fingerprint: r.fingerprint,
            })
            .collect(),
    )
}
