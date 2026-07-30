// SPDX-License-Identifier: AGPL-3.0-or-later
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
use commonwealth_state::{is_gossip_excluded, StoreEntry};

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
        // Receiver-side privacy guard (defense in depth). The gossip
        // SENDER already filters local-only namespaces via
        // `all_entries_for_gossip`, but a buggy or hostile peer can
        // POST anything to this route — mTLS proves the caller is in
        // the mesh, not that it runs honest code. A local-only app_id
        // arriving from the wire can only be a bug or an attack, so we
        // refuse to merge it: the privacy guarantee must hold on BOTH
        // ends, never trusting the sender to have filtered. See
        // `GOSSIP_EXCLUDED_APP_IDS`.
        if is_gossip_excluded(&raw.app_id) {
            tracing::warn!(
                app_id = %raw.app_id,
                "rejected gossiped entry in a local-only namespace — a peer \
                 sent a private app_id it should never have shipped (bug or \
                 attack); not merging"
            );
            continue;
        }
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
    // Senders hex-encode `origin.as_bytes()` verbatim (sovereign-mesh's
    // `gossip.rs`), and NodeId stores its u128 big-endian (`from_u128` calls
    // `to_be_bytes`), so decoding must invert with `from_be_bytes`. Using
    // `from_le_bytes` here reversed all 16 bytes of every remote node id, so
    // `origin` never matched a member and peer measurements rendered as
    // "Measured by node-<reversed-hex>" instead of the node's name. Same bug,
    // same fix as `commonwealth_state::store::node_id_from_bytes`.
    Some(commonwealth_core::ids::NodeId::from_u128(
        u128::from_be_bytes(arr),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::ids::NodeId;

    /// Regression pin for the gossip-side twin of the `store.rs` origin
    /// reversal. `hex_to_node_id` decoded the sender's verbatim big-endian
    /// bytes with `u128::from_le_bytes`, so every remote `StoreEntry.origin`
    /// arrived with all 16 bytes reversed. Nothing failed loudly — the id was
    /// still well-formed, it just matched no member, so peer measurements
    /// rendered as "Measured by node-<reversed-hex>" instead of the node's
    /// name.
    ///
    /// The fixtures are the two real node ids from the live Meshsonics mesh
    /// that exposed this on 2026-07-30. Low-int fixtures like `from_u128(1)`
    /// CANNOT catch a reversal: `Display` prints only the first 8 bytes, and
    /// the halves are indistinguishable reversed.
    #[test]
    fn origin_hex_round_trips_without_byte_reversal() {
        for hex in [
            "b88252e4325bc377465f51a0c0b6830d", // BeefyMac
            "44ae76142b0c3c723051ff98f043104a", // RuggedFox
        ] {
            let decoded = hex_to_node_id(hex).expect("well-formed 16-byte hex");
            assert_eq!(
                decoded.to_hex(),
                hex,
                "decoding must be the exact inverse of the sender's \
                 hex::encode(origin.as_bytes())"
            );

            // And explicitly pin the failure mode, so a future endianness
            // regression names itself rather than surfacing as a silent
            // attribution miss on a peer's dashboard.
            let reversed: String = {
                let mut b = *decoded.as_bytes();
                b.reverse();
                hex::encode(b)
            };
            assert_ne!(
                decoded.to_hex(),
                reversed,
                "fixture must have distinguishable halves to be able to \
                 detect a reversal at all"
            );
            assert!(
                hex_to_node_id(&reversed).expect("valid hex") != decoded,
                "byte-reversed input must not decode to the same NodeId"
            );
        }
    }

    #[test]
    fn origin_hex_rejects_malformed_input() {
        assert!(hex_to_node_id("not-hex").is_none());
        assert!(hex_to_node_id("b88252e4").is_none(), "too short");
        assert!(
            hex_to_node_id("b88252e4325bc377465f51a0c0b6830d00").is_none(),
            "too long"
        );
    }

    /// The sender side is `hex::encode(origin.as_bytes())`; this pins that
    /// `NodeId::to_hex` is that same encoding, so the two halves of the wire
    /// contract cannot drift apart independently.
    #[test]
    fn to_hex_matches_the_senders_encoding() {
        let id = NodeId::from_u128(0x1122_3344_5566_7788_AABB_CCDD_EEFF_0011);
        assert_eq!(id.to_hex(), hex::encode(id.as_bytes()));
        assert_eq!(hex_to_node_id(&id.to_hex()), Some(id));
    }
}
