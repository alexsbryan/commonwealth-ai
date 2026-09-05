// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /internal/app/state` — the ONE receiver for gossiped mesh_store
//! entries.
//!
//! Its sibling `POST /internal/app/registry` was deleted by cw-lift rung
//! 2c. It had **no sender**: nothing in the workspace ever POSTed an
//! `AppRegistry` manifest to a peer, so `AppRegistry::merge`'s version
//! comparison ran on exactly zero production inputs and the route was a
//! standing invitation for any peer that could route here to install an
//! app manifest. `GROUND_TRUTH.md` had recorded it as senderless since
//! before this campaign started.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use commonwealth_state::{is_gossip_excluded, StoreEntry};

use crate::state::AppState;

/// A batch of store entries on the wire — the ONE declaration of this
/// shape (ARCH §10.6).
///
/// `Serialize` as well as `Deserialize` because the SENDERS build it too.
/// Until cw-lift rung 2c there were three hand-written `serde_json::json!`
/// literals of these five fields against this one `Deserialize` struct, and
/// nothing made them agree: a renamed field would have 422'd the round at
/// runtime, silently, on the branch that logs at debug. The same hazard the
/// gossip round's own `MeshWire` comment records having been paid twice.
#[derive(Serialize, Deserialize)]
pub struct AppStateGossipBody {
    pub entries: Vec<GossipStoreEntry>,
}

#[derive(Serialize, Deserialize)]
pub struct GossipStoreEntry {
    pub app_id: String,
    pub key: String,
    pub value_b64: String, // see `encode_value` / `base64_decode`
    pub timestamp: u64,
    pub origin_hex: String, // hex NodeId (16 bytes = 32 hex chars)
}

impl From<&StoreEntry> for GossipStoreEntry {
    /// The one projection from a stored row to its wire form. Its inverse
    /// is [`recv_app_state`]'s body, and the `value_b64` stub is now a
    /// single pair of functions rather than an encode spelled at each
    /// sender and a decode spelled here.
    fn from(e: &StoreEntry) -> Self {
        Self {
            app_id: e.app_id.clone(),
            key: e.key.clone(),
            value_b64: encode_value(&e.value),
            timestamp: e.timestamp,
            origin_hex: hex::encode(e.origin.as_bytes()),
        }
    }
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

// ── helpers ──────────────────────────────────────────────────────────────────

/// `value_b64` is not base64 yet — the field name is the wire contract and
/// the encoding is a stub on BOTH ends. Every current namespace stores JSON
/// blobs, which round-trip through UTF-8 cleanly; a value that does not
/// would be lossy here, which is why the replacement is a pair and not two
/// independent edits.
///
/// TODO: real base64 once the dep is added — change these two together.
pub fn encode_value(value: &[u8]) -> String {
    String::from_utf8_lossy(value).into_owned()
}

fn base64_decode(s: &str) -> Result<Vec<u8>, ()> {
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

    /// RED-FIRST (cw-lift 2c, ARCH §18.1). The two halves of this route's
    /// contract used to be a `Deserialize` struct here and three
    /// hand-written `serde_json::json!` literals in two other crates, with
    /// nothing holding them to the same field names. A rename on either
    /// side 422s the round at runtime, on a branch that logs at `debug` —
    /// the same failure the gossip round's `MeshWire` comment records
    /// having been paid twice.
    ///
    /// Now there is one struct and one `From<&StoreEntry>`, and this drives
    /// a row all the way out and back: project, serialise, parse, decode.
    /// Watched red by making the projection write `e.key` into `app_id`;
    /// it fails on the first field assertion rather than anywhere near the
    /// serde layer, which is the point.
    #[test]
    fn a_store_entry_round_trips_through_the_one_wire_shape() {
        let origin = NodeId::from_u128(0xb882_52e4_325b_c377_465f_51a0_c0b6_830d);
        let entry = StoreEntry {
            app_id: "corpus-engine".into(),
            key: "handoff:7".into(),
            value: Bytes::from_static(b"{\"units\":3}"),
            timestamp: 1_788_000_000,
            origin,
        };

        // The sender's half, verbatim: the projection, then one body.
        let body = AppStateGossipBody {
            entries: vec![GossipStoreEntry::from(&entry)],
        };
        let on_the_wire = serde_json::to_vec(&body).expect("serialise");

        // The receiver's half, verbatim: parse, then decode each field the
        // way `recv_app_state` decodes it.
        let parsed: AppStateGossipBody = serde_json::from_slice(&on_the_wire).expect(
            "the sender's body must parse as the receiver's type — this is              the assertion three hand-written json! literals could not make",
        );
        assert_eq!(parsed.entries.len(), 1);
        let raw = &parsed.entries[0];
        assert_eq!(raw.app_id, entry.app_id);
        assert_eq!(raw.key, entry.key);
        assert_eq!(raw.timestamp, entry.timestamp);
        assert_eq!(
            Bytes::from(base64_decode(&raw.value_b64).expect("decode value")),
            entry.value,
            "encode_value and base64_decode are one pair; a value that              survives one and not the other is a silent truncation"
        );
        assert_eq!(
            hex_to_node_id(&raw.origin_hex),
            Some(entry.origin),
            "origin must survive the round trip — the reversal this file's              other test pins is what happens when it does not"
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
