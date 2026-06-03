//! Internal mesh routes for app state and registry gossip.
//!
//! POST /internal/app/state    — receive gossiped AppState entries
//! POST /internal/app/registry — receive gossiped AppRegistry manifests

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use bytes::Bytes;
use serde::Deserialize;

use commonwealth_app::manifest::MeshAppManifest;
use commonwealth_state::StoreEntry;

use crate::state::AppState;

/// A batch of store entries received from a peer via gossip.
#[derive(Deserialize)]
pub struct AppStateGossipBody {
    pub entries: Vec<GossipStoreEntry>,
}

#[derive(Deserialize)]
pub struct GossipStoreEntry {
    pub app_id: String,
    pub key: String,
    pub value_b64: String, // base64-encoded bytes
    pub timestamp: u64,
    pub origin_hex: String, // hex NodeId (16 bytes = 32 hex chars)
}

/// `POST /internal/app/state` — merge gossiped store entries.
pub async fn recv_app_state(
    State(state): State<AppState>,
    Json(body): Json<AppStateGossipBody>,
) -> impl IntoResponse {
    let mut merged = 0usize;
    for raw in body.entries {
        let value_bytes = match base64_decode(&raw.value_b64) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let origin = match hex_to_node_id(&raw.origin_hex) {
            Some(id) => id,
            None => continue,
        };
        let entry = StoreEntry {
            app_id: raw.app_id,
            key: raw.key,
            value: Bytes::from(value_bytes),
            timestamp: raw.timestamp,
            origin,
        };
        if state.inner.mesh_store.merge_entry(entry).unwrap_or(false) {
            merged += 1;
        }
    }
    (StatusCode::OK, Json(serde_json::json!({"merged": merged})))
}

/// A batch of app manifests received via gossip.
#[derive(Deserialize)]
pub struct AppRegistryGossipBody {
    pub manifests: Vec<MeshAppManifest>,
}

/// `POST /internal/app/registry` — merge gossiped app manifests.
pub async fn recv_app_registry(
    State(state): State<AppState>,
    Json(body): Json<AppRegistryGossipBody>,
) -> impl IntoResponse {
    let mut merged = 0usize;
    for manifest in body.manifests {
        if state.inner.app_registry.merge(manifest).await {
            merged += 1;
        }
    }
    (StatusCode::OK, Json(serde_json::json!({"merged": merged})))
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn base64_decode(s: &str) -> Result<Vec<u8>, ()> {
    // For now treat value_b64 as raw UTF-8 bytes.
    // TODO: replace with proper base64 decode once dep is added.
    Ok(s.as_bytes().to_vec())
}

fn hex_to_node_id(hex: &str) -> Option<commonwealth_core::ids::NodeId> {
    let bytes = hex::decode(hex).ok()?;
    if bytes.len() != 16 {
        return None;
    }
    let mut arr = [0u8; 16];
    arr.copy_from_slice(&bytes);
    Some(commonwealth_core::ids::NodeId::from_u128(
        u128::from_le_bytes(arr),
    ))
}
