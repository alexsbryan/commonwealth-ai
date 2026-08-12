// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tiny shared parsers for cross-handler request headers.
//!
//! Right now this exists for the `X-Node-Id` header that
//! cross-mesh requests stamp on every inbound call so we can
//! attribute ledger events and apply per-peer affinity preferences.
//! Three handlers parse this — `routes_oicp::capabilities`,
//! `routes_inference::chat_completions`, and
//! `routes_internal::knowledge_search` — so the parser belongs
//! once, not three times.

use axum::http::HeaderMap;

use commonwealth_core::ids::NodeId;

/// Parse the optional `X-Node-Id` header into a [`NodeId`]. Returns
/// `None` for missing, malformed, or non-32-hex-char values; the
/// caller treats `None` as "local-origin / unknown peer", which is
/// the safe-default behaviour at every site.
///
/// ## The one canonical wire form (order commons-fluency fix 7)
///
/// The header value is [`NodeId::to_hex()`]: exactly 32 lowercase hex
/// chars, the encoding of the 16-byte id — nothing else is accepted
/// (no `node-` prefix, no truncated hex, no uppercase). Both header
/// spellings (`x-node-id` and `X-Node-Id`) are read. A value that
/// fails this parser is recorded by the admission layer and named on
/// `/status`'s zero-bucket tally row — see
/// [`crate::state::RejectedNodeIdHeader`].
pub fn parse_x_node_id(headers: &HeaderMap) -> Option<NodeId> {
    let raw = headers
        .get("x-node-id")
        .or_else(|| headers.get("X-Node-Id"))?;
    let s = raw.to_str().ok()?;
    if s.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        let pair = s.get(i * 2..i * 2 + 2)?;
        *b = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(NodeId::from_u128(u128::from_be_bytes(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nid(byte: u8) -> NodeId {
        NodeId::from_u128(byte as u128)
    }

    fn id_to_hex(id: &NodeId) -> String {
        id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn round_trips_for_lowercase_header() {
        let id = nid(0x42);
        let mut h = HeaderMap::new();
        h.insert("x-node-id", id_to_hex(&id).parse().unwrap());
        assert_eq!(parse_x_node_id(&h), Some(id));
    }

    #[test]
    fn round_trips_for_uppercase_header() {
        let id = nid(0x42);
        let mut h = HeaderMap::new();
        h.insert("X-Node-Id", id_to_hex(&id).parse().unwrap());
        assert_eq!(parse_x_node_id(&h), Some(id));
    }

    #[test]
    fn missing_header_returns_none() {
        assert_eq!(parse_x_node_id(&HeaderMap::new()), None);
    }

    #[test]
    fn wrong_length_returns_none() {
        let mut h = HeaderMap::new();
        h.insert("x-node-id", "abcd".parse().unwrap());
        assert_eq!(parse_x_node_id(&h), None);
    }

    #[test]
    fn non_hex_returns_none() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-node-id",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz".parse().unwrap(),
        );
        assert_eq!(parse_x_node_id(&h), None);
    }
}
