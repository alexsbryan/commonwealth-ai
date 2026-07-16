// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri command surface for peer-assisted ingest ("Blanket").
//!
//! Thin HTTP proxies to the embedded/attached daemon's collaborate + grant
//! routes (`commonwealth-api::routes_internal::{corpus_collaborate, corpus_grant,
//! corpus_queue}`). The desktop:
//!   1. lists which mesh peers can help (`eligible_peers`),
//!   2. issues an ephemeral grant + kicks off collaborate scoped to the
//!      user-selected peers (`start`),
//!   3. polls per-peer progress + the verification result (`status`),
//!   4. can revoke mid-run (`revoke`).
//!
//! The corpus never leaves its standing local-only posture — the grant is the
//! out-of-band, revocable capability. Mirrors the `watched_folder_commands`
//! proxy pattern (same embedded-daemon HTTP path in Attach and Local modes).

use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

use crate::state::AppState;

const DEFAULT_CLIENT_PORT: u16 = 9741;

fn daemon_port(state: &AppState) -> u16 {
    use crate::bootstrap::BootstrapMode;
    match &state.bootstrap_mode {
        BootstrapMode::Attach { client_port, .. } => *client_port,
        BootstrapMode::Local { .. } => DEFAULT_CLIENT_PORT,
    }
}

fn base_url(state: &AppState) -> String {
    format!("http://127.0.0.1:{}", daemon_port(state))
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client builds")
}

/// Which mesh peers can help with a peer-assisted ingest of `corpus_id`.
/// Returns the raw daemon DTO `{ peers: [{node_id,name,online,eligible,reason}],
/// grantable }` — the picker renders eligible peers selectable and ineligible
/// peers dimmed with their reason.
#[tauri::command]
pub async fn mesh_assist_eligible_peers(
    state: State<'_, AppState>,
    corpus_id: String,
) -> Result<Value, String> {
    let url = format!(
        "{}/internal/corpus/collaborate/eligible_peers",
        base_url(&state)
    );
    let resp = client()
        .post(&url)
        .json(&json!({ "corpus_id": corpus_id }))
        .send()
        .await
        .map_err(|e| format!("eligible_peers request failed: {e}"))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("eligible_peers decode failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("eligible_peers {status}: {body}"));
    }
    Ok(body)
}

/// Result of kicking off a peer-assisted ingest. `handoff_id` is opaque — the
/// frontend round-trips it back to `mesh_assist_status` unchanged. The store
/// keys by `corpus_id` (one active grant per corpus).
#[derive(Debug, Serialize)]
pub struct AssistStartResult {
    pub corpus_id: String,
    /// Opaque handoff id (serialized form of the daemon's `HandoffId`),
    /// round-tripped to `mesh_assist_status`.
    pub handoff_id: Value,
    pub grant_expires_at_ms: u64,
    pub peer_count: usize,
}

/// Issue an ephemeral grant, then start collaborative ingest scoped to
/// `peer_node_ids`. If collaborate fails, the just-issued grant is revoked so
/// nothing lingers.
#[tauri::command]
pub async fn mesh_assist_start(
    state: State<'_, AppState>,
    corpus_id: String,
    peer_node_ids: Vec<String>,
    ttl_secs: Option<u64>,
) -> Result<AssistStartResult, String> {
    let base = base_url(&state);
    let http = client();

    // 1. Issue the ephemeral grant authorizing exactly the selected peers.
    let grant_resp = http
        .post(format!("{base}/internal/corpus/grant"))
        .json(&json!({
            "corpus_id": corpus_id,
            "allowed_peers": peer_node_ids,
            "ttl_secs": ttl_secs,
        }))
        .send()
        .await
        .map_err(|e| format!("grant request failed: {e}"))?;
    let grant_status = grant_resp.status();
    let grant_body: Value = grant_resp
        .json()
        .await
        .map_err(|e| format!("grant decode failed: {e}"))?;
    if !grant_status.is_success() {
        return Err(format!("grant {grant_status}: {grant_body}"));
    }
    let grant_expires_at_ms = grant_body
        .get("expires_at_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    // 2. Kick off collaborate scoped to the selected peers.
    let collab_resp = http
        .post(format!("{base}/internal/corpus/collaborate"))
        .json(&json!({
            "corpus_id": corpus_id,
            "allowed_peers": peer_node_ids,
        }))
        .send()
        .await
        .map_err(|e| format!("collaborate request failed: {e}"))?;
    let collab_status = collab_resp.status();
    let collab_body: Value = collab_resp
        .json()
        .await
        .map_err(|e| format!("collaborate decode failed: {e}"))?;
    if !collab_status.is_success() {
        // Best-effort: revoke the grant we just issued so it doesn't linger.
        let _ = http
            .post(format!("{base}/internal/corpus/grant/revoke"))
            .json(&json!({ "corpus_id": corpus_id }))
            .send()
            .await;
        return Err(format!("collaborate {collab_status}: {collab_body}"));
    }
    let handoff_id = collab_body
        .get("handoff_id")
        .cloned()
        .unwrap_or(Value::Null);

    Ok(AssistStartResult {
        corpus_id,
        handoff_id,
        grant_expires_at_ms,
        peer_count: peer_node_ids.len(),
    })
}

/// Poll glassbox progress for a running assist. `handoff_id` is the opaque
/// value returned by `mesh_assist_start`. Returns `null` once the queue is
/// gone (job complete / torn down).
#[tauri::command]
pub async fn mesh_assist_status(
    state: State<'_, AppState>,
    handoff_id: Value,
) -> Result<Value, String> {
    let url = format!("{}/internal/corpus/collaborate/status", base_url(&state));
    let resp = client()
        .post(&url)
        .json(&json!({ "handoff_id": handoff_id }))
        .send()
        .await
        .map_err(|e| format!("status request failed: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Value::Null);
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("status decode failed: {e}"))?;
    Ok(body)
}

/// Revoke the corpus's ephemeral grant and tear down the in-flight assist. The
/// local ingest continues; only the peer-assist layer is stopped. Idempotent.
#[tauri::command]
pub async fn mesh_assist_revoke(
    state: State<'_, AppState>,
    corpus_id: String,
) -> Result<(), String> {
    let url = format!("{}/internal/corpus/grant/revoke", base_url(&state));
    let resp = client()
        .post(&url)
        .json(&json!({ "corpus_id": corpus_id }))
        .send()
        .await
        .map_err(|e| format!("revoke request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("revoke returned {}", resp.status()));
    }
    Ok(())
}
