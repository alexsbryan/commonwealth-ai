// SPDX-License-Identifier: AGPL-3.0-or-later
//! The room: guest grants, their link, and their QR — the desktop's half.
//!
//! A "room" in this product is one member's machine serving guest pages to
//! people who are not members. The operator's side of that is an ephemeral
//! grant; `svrn mesh grant` in the CLI and this panel are two clients of the
//! same daemon routes (`/internal/guest/grant` and friends).
//!
//! # Why the link comes from the daemon and not from here
//!
//! Composing a guest link means the door's page path, the token, and — for a
//! guest who shares no network with this machine — the node's own iroh dial
//! string. That is one rule, and it lives in `sovereign_mesh::deep_link`
//! (`wall_page_base` / `wall_https_link`). This app is an HTTP client of the
//! daemon by design and does not link the mesh crates (see `src-tauri/Cargo.toml`),
//! so it asks the daemon: the mint request carries the base `url` and the
//! response carries the composed `link`. The desktop renders it with the same
//! `qrcode` dependency the mobile pairing card uses.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::state::AppState;

/// A freshly minted grant, with the link the daemon composed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestGrantDto {
    pub token: String,
    pub expires_at_ms: u64,
    /// One-line rendering of what this grant buys.
    pub summary: String,
    /// The composed guest link, present when a base `url` was given.
    #[serde(default)]
    pub link: Option<String>,
}

/// One outstanding grant, as the daemon lists it.
///
/// The full token is deliberately NOT here: the list identifies a row by an
/// 8-hex prefix, so a screen-shared settings panel never spills whole bearers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestGrantRowDto {
    pub token_prefix: String,
    pub summary: String,
    #[serde(default)]
    pub label: Option<String>,
    pub expires_at_ms: u64,
    pub revoked: bool,
    pub live: bool,
}

#[derive(Debug, Deserialize)]
struct RevokeResponseDto {
    revoked: bool,
}

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())
}

/// This node's own iroh dial string, from the daemon's status — the same field
/// the CLI reads (`/v1/mesh/status` → `self_reachability.dial`). Best-effort:
/// iroh off, no reachable address yet, or an unreachable status all mean "no
/// dial", and the composed link is then the direct (plain-HTTP) form. The
/// daemon composes; it cannot read its own dial on the grant route, so the
/// caller that just asked for the status hands it in.
async fn node_dial(state: &AppState) -> Option<String> {
    let url = format!("{}/v1/mesh/status", state.client_base_url());
    let resp = http().ok()?.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    body.get("self_reachability")?
        .get("dial")?
        .as_str()
        .map(str::to_string)
        .filter(|d| !d.is_empty())
}

/// Mint a guest grant for the room.
///
/// `scope` is `"wall"` (every app the door registers for guests) or a rail
/// namespace (one app). `models` empty means the daemon's primary slot.
/// `base_url` is what a phone opens (the room address, or the static origin).
#[tauri::command]
pub async fn guest_grant_create(
    state: State<'_, Arc<AppState>>,
    scope: String,
    models: Vec<String>,
    base_url: Option<String>,
    ttl_secs: Option<u64>,
    label: Option<String>,
) -> Result<GuestGrantDto, String> {
    let wall = {
        let s = scope.trim();
        s.is_empty() || s.eq_ignore_ascii_case("wall")
    };
    // The alias `primary`, not a copied id: the daemon resolves it, so it
    // follows the operator's next model change (same default the CLI applies).
    let models = if models.is_empty() {
        vec!["primary".to_string()]
    } else {
        models
    };

    let mut scopes = serde_json::json!({ "models": models });
    if wall {
        scopes["wall"] = serde_json::json!(true);
    } else {
        scopes["rail"] = serde_json::json!(scope.trim());
    }
    let mut body = serde_json::json!({ "scopes": scopes });
    if let Some(t) = ttl_secs {
        body["ttl_secs"] = serde_json::json!(t);
    }
    if let Some(l) = label.filter(|l| !l.trim().is_empty()) {
        body["label"] = serde_json::json!(l);
    }
    if let Some(u) = base_url.filter(|u| !u.trim().is_empty()) {
        body["url"] = serde_json::json!(u);
    }
    // The dial rides along so the daemon's composed link can reach a guest who
    // shares no network with this machine (iroh=). No dial is not an error.
    if let Some(d) = node_dial(&state).await {
        body["dial"] = serde_json::json!(d);
    }

    let url = format!("{}/internal/guest/grant", state.client_base_url());
    let resp = http()?
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("the daemon did not answer at {url}: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("minting the grant was refused ({status}): {text}"));
    }
    resp.json::<GuestGrantDto>()
        .await
        .map_err(|e| format!("unexpected grant response: {e}"))
}

/// What is outstanding: live, expired, and revoked grants alike.
#[tauri::command]
pub async fn guest_grant_list(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<GuestGrantRowDto>, String> {
    let url = format!("{}/internal/guest/grant/list", state.client_base_url());
    let resp = http()?
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("the daemon did not answer at {url}: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("listing grants was refused ({status}): {text}"));
    }
    resp.json::<Vec<GuestGrantRowDto>>()
        .await
        .map_err(|e| format!("unexpected grant list: {e}"))
}

/// Kill a link immediately. Idempotent at the daemon (revoking a token that
/// is already gone returns `revoked: false`, not an error).
#[tauri::command]
pub async fn guest_grant_revoke(
    state: State<'_, Arc<AppState>>,
    token: String,
) -> Result<bool, String> {
    let url = format!("{}/internal/guest/grant/revoke", state.client_base_url());
    let resp = http()?
        .post(&url)
        .json(&serde_json::json!({ "token": token }))
        .send()
        .await
        .map_err(|e| format!("the daemon did not answer at {url}: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("revoking was refused ({status}): {text}"));
    }
    let dto = resp
        .json::<RevokeResponseDto>()
        .await
        .map_err(|e| format!("unexpected revoke response: {e}"))?;
    Ok(dto.revoked)
}
