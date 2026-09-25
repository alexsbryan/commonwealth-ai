// SPDX-License-Identifier: AGPL-3.0-or-later
//! Who a verified dialer is, and how a `<peer>` argument resolves — the
//! member-view vocabulary both families speak.
//!
//! Moved here by five-programs fp-46 (§12 decision 3 — mesh identity
//! vocabulary crossing the package line). The row first named
//! `sovereign-contracts`, but the standing `[[forbid]]` rows keep
//! `commonwealth-core` and `commonwealth-media` free of every sovereign-*
//! crate (their package-closure-clean property is the point of those rows),
//! so the cluster lives at the NEUTRAL kernel instead — decision 3a's
//! brand-free-atoms home, and the ids home since fp-40. Both owners
//! re-import every item at its historical path (ARCH §10.6 — a re-export,
//! never a twin), so the acceptor, the media decisions and every `<peer>`
//! resolution keep their old paths while the one definition lives where both
//! families may name it.

use crate::ids::{NodeId, NodePubkey};

/// Resolve an operator's `<node>` argument against a member row: exact
/// name, or a node_id prefix of at least 4 hex characters (the `node-`
/// prefix is optional on either side).
///
/// The prefix is matched against the FULL 32-hex id (`to_hex`), not the
/// 16-hex `Display` form: status tables print 22 characters and the collision
/// warning prints all 32, and until 2026-09-22 neither resolved — only a
/// prefix of 16 or fewer did, so the id the repair hint told you to paste
/// answered "No member matching" (5302a6ed3).
///
/// A prefix shorter than 4 is refused rather than matched loosely — a
/// one-character prefix is very nearly "any member", and the callers act on the answer (`forget-member` writes a
/// tombstone; `mesh media <peer>` mints a bridge; `media_allow` admits a
/// dial). One implementation so every `<peer>` argument on every surface
/// resolves the same way (ARCH §10.6) — it lives at the shared seam rather
/// than in either runtime, because the package crates resolve the same
/// argument with no sovereign or commonwealth runtime under them.
pub fn member_matches(node_id: NodeId, name: &str, query: &str) -> bool {
    if name == query {
        return true;
    }
    let q = query.trim_start_matches("node-");
    q.len() >= 4 && node_id.to_hex().starts_with(q)
}

/// The one implementation of the `X-Mesh-*` scheme: what the acceptor tells an
/// origin about a dialer whose key the QUIC handshake verified.
///
/// The pubkey is always known — it is what the handshake proved — so it is
/// always named. The member name and node id are the ROSTER's word, and a
/// dialer the roster does not name gets neither rather than a placeholder: an
/// absent header is "the roster did not answer", and a `<none>` value would be
/// "the roster answered: nobody" (ARCH principle 6). A reader that needs a
/// member must refuse the key-only case, and can see that it must.
///
/// One function rather than one per ALPN because the names ARE the scheme: a
/// second spelling is how `cwth/http/0` learns to say `X-Mesh-NodeId` while
/// `cwth/media/0` says `X-Mesh-Node` (ARCH principle 8).
pub fn verified_headers(who: Option<&MemberIdentity>, dialer: NodePubkey) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(3);
    if let Some(who) = who {
        out.push(("X-Mesh-Member".to_string(), who.name.clone()));
        out.push(("X-Mesh-Node".to_string(), who.node_id.to_string()));
    }
    out.push(("X-Mesh-Pubkey".to_string(), hex::encode(dialer.0)));
    out
}

/// Who a verified dialer IS, as the roster names it. The fields are what an
/// origin behind `cwth/media/0` is handed on every request (`X-Mesh-Member`,
/// `X-Mesh-Node`), so a server that authenticates nothing can still tell
/// members apart — and so `media_allow` can be a list of names rather than
/// of keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberIdentity {
    pub name: String,
    pub node_id: NodeId,
}

impl MemberIdentity {
    /// The request headers the media origin receives. Values are visible
    /// ASCII by the time they reach the wire (`rewrite_head` filters), and
    /// any client-supplied header under `x-mesh-` is stripped before these
    /// are added, so the origin reads them as the acceptor's word.
    pub fn headers(&self, dialer: NodePubkey) -> Vec<(String, String)> {
        verified_headers(Some(self), dialer)
    }

    /// Whether a `media_allow` entry names this member: its exact name, or a
    /// node-id prefix of at least four characters — the same resolution every
    /// `<peer>` argument uses.
    pub fn named_by(&self, entry: &str) -> bool {
        member_matches(self.node_id, &self.name, entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `node-` plus four hex digits: the floor. Eight characters would
    /// leave three hex digits, which is BELOW the floor by design.
    #[test]
    fn a_prefix_matches_at_four_characters_and_refuses_shorter() {
        let id = NodeId::from_u128(0xb88252e400000000_0000000000000000);
        let m = |q: &str| member_matches(id, "BeefyMac", q);
        assert!(!m("b"), "1 char must not match");
        assert!(!m("b88"), "3 chars must not match");
        assert!(m("b882"), "4 chars is the floor");
        assert!(m("node-b882"), "the node- prefix is optional");
        assert!(m("BeefyMac"), "exact name matches");
        assert!(!m("Beefy"), "a partial NAME must not match");
        assert!(!m("b883"), "a wrong prefix must not match");
    }

    /// Every id form an operator is shown must resolve: the 16-hex `Display`
    /// (`node-…`), the 22-char column `mesh status` prints, and the full 32 the
    /// alias warning prints. Only the first did — `forget-member <id from the
    /// warning>` answered "No member matching" on a live roster.
    #[test]
    fn every_printed_node_id_form_resolves_its_member() {
        let id = NodeId::from_hex("188f04e2831741c77ccd5a142a314e07").unwrap();
        let m = |q: &str| member_matches(id, "LittleMac", q);
        assert!(m(&id.to_string()), "Display form: {id}");
        assert!(m("188f04e2831741c77ccd5a"), "status column (22)");
        assert!(m("188f04e2831741c77ccd5a142a314e07"), "full hex (32)");
        assert!(
            !m("188f04e2831741c77ccd5a142a314e08"),
            "a wrong full id must not match"
        );
    }

    /// An empty query must never match. It reaches here as `--force` with no
    /// member, and matching everything would retire whichever row the
    /// iteration happened to reach first.
    #[test]
    fn an_empty_query_matches_nothing() {
        let id = NodeId::from_u128(0xb88252e400000000_0000000000000000);
        assert!(!member_matches(id, "BeefyMac", ""));
    }

    /// The scheme's three headers, with the key always named and the roster's
    /// word present only when the roster answered.
    #[test]
    fn verified_headers_name_the_key_always_and_the_member_only_when_known() {
        let dialer = NodePubkey([7u8; 32]);
        let key_only = verified_headers(None, dialer);
        assert_eq!(
            key_only,
            vec![("X-Mesh-Pubkey".to_string(), hex::encode([7u8; 32]))]
        );
        let who = MemberIdentity {
            name: "LittleMac".into(),
            node_id: NodeId::from_u128(1),
        };
        let named = verified_headers(Some(&who), dialer);
        assert!(named.contains(&("X-Mesh-Member".to_string(), "LittleMac".to_string())));
        assert!(named.iter().any(|(k, _)| k == "X-Mesh-Node"));
    }
}
