// SPDX-License-Identifier: AGPL-3.0-or-later
//! Who a verified dialer is, and whether a media origin is handed its dial.

use std::net::SocketAddr;

use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_core::mesh::member_matches;
use commonwealth_transport::iroh::Forward;

/// The roster consult behind every admission decision at the acceptor:
/// `Some` is membership, named. Consulted PER DIAL and never cached — a
/// member can leave between two dials, and a cached set would admit a
/// departed node (or refuse a fresh one) for as long as it was stale. A
/// daemon supplies one reading its live mesh; tombstoned rows are excluded,
/// so leaving the mesh takes reachability with it.
pub type MemberCheck = std::sync::Arc<
    dyn Fn(
            NodePubkey,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<MemberIdentity>> + Send>>
        + Send
        + Sync,
>;

/// A check that admits nobody. For hosts with no mesh to consult. Fail-CLOSED
/// by construction: forgetting to wire the real check cannot widen access,
/// only narrow it.
pub fn admits_no_one() -> MemberCheck {
    std::sync::Arc::new(|_| Box::pin(std::future::ready(None)))
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
        vec![
            ("X-Mesh-Member".to_string(), self.name.clone()),
            ("X-Mesh-Node".to_string(), self.node_id.to_string()),
            ("X-Mesh-Pubkey".to_string(), hex::encode(dialer.0)),
        ]
    }

    /// Whether a `media_allow` entry names this member: its exact name, or a
    /// node-id prefix of at least four characters — the same resolution every
    /// `<peer>` argument uses.
    pub fn named_by(&self, entry: &str) -> bool {
        member_matches(self.node_id, &self.name, entry)
    }
}

/// The holder's decision for a `cwth/media/0` dial — the ONE place that turns
/// (verified dialer, declared origin, allow-list) into a forward or a refusal.
///
/// - No origin declared: closed. The protocol is not advertised, so a dial is
///   closed rather than left hanging on a route to nowhere.
/// - A non-member: closed. The origin authenticates nothing, so there is no
///   safe downgrade (a stranger on the client ALPN meets a bearer gate; here
///   there is none to meet).
/// - A member outside a non-empty `allow`: closed, logged with the list.
/// - Otherwise `Forward::Http` — the origin is told who is asking.
pub fn admit_media(
    who: Option<&MemberIdentity>,
    dialer: NodePubkey,
    origin: Option<SocketAddr>,
    allow: &[String],
    declared: &[(String, String)],
) -> Option<Forward> {
    let Some(who) = who else {
        tracing::warn!(
            target: "transport",
            dialer = %hex::encode(dialer.0),
            "media: REFUSED a MEDIA_ALPN dial from a non-member — the media origin \
             authenticates nothing, so there is no safe downgrade"
        );
        return None;
    };
    let origin = origin?;
    if !allow.is_empty() && !allow.iter().any(|entry| who.named_by(entry)) {
        tracing::warn!(
            target: "transport",
            member = %who.name,
            node_id = %who.node_id,
            allow = ?allow,
            "media: REFUSED a MEDIA_ALPN dial from a member outside media_allow"
        );
        return None;
    }
    tracing::info!(
        target: "transport",
        member = %who.name,
        node_id = %who.node_id,
        "media: dial admitted — the origin is told who is asking"
    );
    // The verified identity FIRST, then this node's own credentials for its
    // own origin. Both go through the one `headers` vec because
    // `rewrite_head` gives every name in it the same guarantee: a client's
    // copy of that name is stripped before ours is appended, so exactly one
    // reaches the origin and it is the one this node chose. Declared values
    // are secrets read from 0600 files (`crate::declared`) and are never
    // logged -- that one is SET is logged where it is read.
    let mut headers = who.headers(dialer);
    headers.extend(declared.iter().cloned());
    Some(Forward::Http { origin, headers })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn who() -> MemberIdentity {
        MemberIdentity {
            name: "LittleMac".into(),
            // A realistic id: `to_string()` renders the TOP 64 bits, so a
            // small literal would print as sixteen zeros and make the
            // prefix case below vacuous.
            node_id: NodeId::from_u128(0xb0b252e400000000_0000000000000000),
        }
    }

    fn dialer() -> NodePubkey {
        NodePubkey([7u8; 32])
    }

    fn origin() -> Option<SocketAddr> {
        Some("127.0.0.1:8096".parse().unwrap())
    }

    /// The failing input is the membership check removed: a `None` identity
    /// reaching the origin.
    #[test]
    fn a_non_member_is_closed_and_a_member_is_forwarded_with_its_name() {
        assert!(admit_media(None, dialer(), origin(), &[], &[]).is_none());
        match admit_media(Some(&who()), dialer(), origin(), &[], &[]) {
            Some(Forward::Http { origin, headers }) => {
                assert_eq!(origin, "127.0.0.1:8096".parse::<SocketAddr>().unwrap());
                assert!(headers.contains(&("X-Mesh-Member".to_string(), "LittleMac".to_string())));
                assert!(headers
                    .iter()
                    .any(|(k, v)| k == "X-Mesh-Pubkey" && v == &hex::encode([7u8; 32])));
            }
            other => panic!("expected an HTTP forward, got {other:?}"),
        }
    }

    /// `media_allow` narrows WHICH members reach the origin, by name or
    /// by a ≥4-char id prefix; everyone else on the roster is closed. The
    /// failing input is the check disabled — `Quiet` forwarded.
    /// The declared credential rides in the SAME `headers` vec as the verified
    /// identity, because `rewrite_head` gives every name in that vec the same
    /// guarantee: a client's copy is stripped before ours is appended. Two vecs
    /// would be two rules, and only one of them would have been the strict one.
    #[test]
    fn a_declared_header_rides_beside_the_verified_identity() {
        let declared = vec![("x-emby-token".to_string(), "the-holders-key".to_string())];
        match admit_media(Some(&who()), dialer(), origin(), &[], &declared) {
            Some(Forward::Http { headers, .. }) => {
                assert!(
                    headers.contains(&("X-Mesh-Member".to_string(), "LittleMac".to_string())),
                    "the verified identity still reaches the origin"
                );
                assert!(
                    headers.contains(&("x-emby-token".to_string(), "the-holders-key".to_string())),
                    "and so does this node's own credential for its own origin"
                );
            }
            other => panic!("expected an Http forward, got {other:?}"),
        }
    }

    /// A declaration is not an admission. It is added AFTER the roster and the
    /// allow-list have both said yes, so it can never widen who gets in — a
    /// refused dial carries no credential anywhere.
    #[test]
    fn a_declaration_does_not_admit_anyone_who_was_refused() {
        let declared = vec![("x-emby-token".to_string(), "the-holders-key".to_string())];
        assert!(
            admit_media(None, dialer(), origin(), &[], &declared).is_none(),
            "a non-member stays closed"
        );
        assert!(
            admit_media(
                Some(&who()),
                dialer(),
                origin(),
                &["somebody-else".to_string()],
                &declared
            )
            .is_none(),
            "a member outside the allow-list stays closed"
        );
        assert!(
            admit_media(Some(&who()), dialer(), None, &[], &declared).is_none(),
            "a node with no origin stays closed"
        );
    }

    #[test]
    fn the_allow_list_admits_by_name_or_id_prefix_and_refuses_the_rest() {
        let by_name = vec!["LittleMac".to_string()];
        assert!(admit_media(Some(&who()), dialer(), origin(), &by_name, &[]).is_some());
        // `node-` plus four hex digits: the floor. Eight characters would
        // leave three hex digits, which is BELOW the floor by design.
        let prefix = who().node_id.to_string()[..9].to_string();
        assert!(admit_media(Some(&who()), dialer(), origin(), &[prefix], &[]).is_some());
        let other = MemberIdentity {
            name: "Quiet".into(),
            node_id: NodeId::from_u128(0xC0DE),
        };
        assert!(admit_media(Some(&other), dialer(), origin(), &by_name, &[]).is_none());
        // A three-character prefix is not a name for anyone.
        assert!(admit_media(Some(&who()), dialer(), origin(), &["nod".to_string()], &[]).is_none());
    }

    /// No declared origin means the protocol is not advertised; a member's
    /// dial is closed, not forwarded to a port nothing listens on.
    #[test]
    fn no_declared_origin_closes_even_a_member() {
        assert!(admit_media(Some(&who()), dialer(), None, &[], &[]).is_none());
    }
}
