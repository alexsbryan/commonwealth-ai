// SPDX-License-Identifier: AGPL-3.0-or-later
//! Who is asking — the identity a request resolves to before admission, and
//! the key of the daemon's one `principal -> Scope` table.
//!
//! `quality/DAEMON_CORE.md` §3.3 measured five resolvers answering "who is
//! asking" over four identities, no two sharing a type, and the corpus ceiling
//! not wired at all. This is the one type they resolve to. It is **published
//! language**: it lives here, beside the rest of the daemon↔package contract,
//! because Serving's package and Answering both key on it and neither may name
//! the daemon.
//!
//! # The type is here; the resolver is the daemon's
//!
//! Resolving a request to a [`Principal`] needs the client token, the guest
//! grant store and the member roster, so the resolution lives in the daemon's
//! `edge` and `node` — the only place that holds all three. This module carries
//! the *value* those resolvers produce, never the resolution itself.
//!
//! # The shape is the union, never a narrowing
//!
//! The six identities today's resolvers distinguish, each an arm here:
//!
//! - [`Principal::LocalOwner`] — a caller on this machine, carrying the
//!   declared sub-identity the client fairness gate buckets on;
//! - [`Principal::RemoteClient`] — a remote caller, by the credential it
//!   presented;
//! - [`Principal::Member`] — a verified mesh member, by node id;
//! - [`Principal::Guest`] — a caller holding a live guest grant;
//! - [`Principal::Anonymous`] — nothing was presented;
//! - [`Principal::Unverified`] — an identity was presented and could not be
//!   verified, which is not the same absence.
//!
//! `SERVING_BOUNDARY.md`'s earlier `Local | Member` sketch is withdrawn: it
//! folded the fairness buckets into one arm and had no guest.
//!
//! # The principal is the key
//!
//! [`Principal`] is `Eq + Hash`, so it is its own fairness key and its own
//! peer key: admission's inflight guard and tally key on it directly, and a
//! peer's row and a local caller's row have the same shape (`DAEMON_CORE.md`
//! §1). The one derived key is the peer key, [`Principal::node_id`], which is
//! present only for a member.
//!
//! # Non-secret by construction
//!
//! A principal becomes a map key and a log field, so the two arms that name a
//! secret — [`Principal::RemoteClient::credential`] and
//! [`Principal::Guest::grant`] — carry a **fingerprint, never the secret
//! itself**. The resolver is what fingerprints; the field contract is stated
//! here so the next resolver cannot get it wrong silently.

/// Re-exported: [`NodeId`] is in this module's public signature
/// ([`Principal::Member`], [`ClaimedNodeId::Readable`]), so a crate that names
/// a principal can name its key without a second Cargo edge to `kernel-types`.
pub use kernel_types::NodeId;

/// Who is asking.
///
/// The key of `DAEMON_CORE.md` §1's one table `principal -> Scope` and of
/// admission's inflight guard and tally. `Eq + Hash` because a peer's row and
/// a local caller's row are the same shape: one key type, six arms, never a
/// parallel identity scheme (ARCH principle 8).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Principal {
    /// A caller on this machine, reached over loopback — the owner, the
    /// desktop, `svrn chat`, MCP, another local process.
    ///
    /// The owner is a distinct identity from an anonymous caller: the two were
    /// one bucket before, and telling them apart is the point of the arm.
    /// Admission reads this arm as "the owner's own chat, always admitted".
    LocalOwner {
        /// The self-declared `X-Principal` the client fairness gate buckets
        /// on, or `None` when the owner named no sub-identity.
        ///
        /// `None` is a reported absence — the owner without a name — and never
        /// a shared placeholder: it is a different key from a named owner.
        /// Only a loopback caller can set this, so a remote caller cannot mint
        /// a fresh bucket by rotating the header.
        sub_identity: Option<String>,
    },
    /// A remote caller, keyed by the credential it presented.
    RemoteClient {
        /// A **non-secret fingerprint** of the presented credential, never the
        /// credential itself. The daemon-wide client token, a per-caller
        /// bearer and an owner-signed `WorkerToken` all ride this arm — a
        /// worker token is a plain bearer, so it needs no arm of its own.
        credential: String,
    },
    /// A verified mesh member, by node id.
    ///
    /// This is the arm the peer gate's `X-Node-Id` resolves to; converging it
    /// onto [`Principal`] is what makes the node id a branch of the one key
    /// rather than a parallel identity scheme.
    Member {
        /// The member's verified [`NodeId`]. Identity comes from what the
        /// member presented, never from where it connected (ARCH principle 8:
        /// no key from an address or a counter).
        node_id: NodeId,
    },
    /// A caller holding a live guest grant.
    ///
    /// The grant is what bounds this caller's routes; this arm carries only
    /// enough to key admission on the guest.
    Guest {
        /// A **non-secret identity** for the grant — a fingerprint of its
        /// token, never the token itself. The grant's `Scope`s are read from
        /// the grant, not from here.
        grant: String,
    },
    /// Nothing was presented. Every such caller shares this one bucket, which
    /// is what they are today: the no-change arm, not a new grouping.
    Anonymous,
    /// An identity was presented and this daemon could not verify it.
    ///
    /// The internal surface's arm. A request that crossed the iroh acceptor
    /// carries a key the QUIC handshake proved; a request that reached the
    /// internal port by some other route carries whatever its sender typed,
    /// and the two are indistinguishable once the headers are read. This arm
    /// is the second case, named.
    ///
    /// It is deliberately NOT [`Anonymous`](Self::Anonymous): anonymous is
    /// "nothing was presented", this is "I was asked to believe something and
    /// I cannot" — "did not answer" is not "answered: no" (ARCH principle 6).
    /// Folding them would make a caller that claimed membership on an
    /// unverifiable connection read exactly like a caller that claimed
    /// nothing, which is the one distinction a decider needs.
    ///
    /// Nor is it a local caller: an untied connection may be a local process,
    /// but it may equally be a LAN caller on a plaintext mesh, and the daemon
    /// cannot tell. A decider that needs an identity refuses this arm.
    Unverified,
}

impl Principal {
    /// The **peer key**: the node id a [`Member`](Self::Member) is verified by.
    ///
    /// `None` for every other identity — a local owner, a remote client and a
    /// guest are not members, and `None` here is a reported absence, never a
    /// placeholder id (ARCH principle 6). A caller that keys peer accounting on
    /// this must treat `None` as "not a peer", not as node zero.
    pub fn node_id(&self) -> Option<NodeId> {
        match self {
            Self::Member { node_id } => Some(*node_id),
            Self::LocalOwner { .. }
            | Self::RemoteClient { .. }
            | Self::Guest { .. }
            | Self::Anonymous
            | Self::Unverified => None,
        }
    }

    /// Stable, non-secret rendering for logs and `/status`.
    ///
    /// A credential and a grant render as their fingerprint, so the secret
    /// never reaches a log line; a member renders its full node id, which is
    /// not secret and must not collide with another member's label.
    pub fn label(&self) -> String {
        match self {
            Self::LocalOwner {
                sub_identity: Some(name),
            } => format!("owner:{name}"),
            Self::LocalOwner { sub_identity: None } => "owner".to_string(),
            Self::RemoteClient { credential } => format!("cred:{credential}"),
            Self::Member { node_id } => format!("member:{}", node_id.to_hex()),
            Self::Guest { grant } => format!("guest:{grant}"),
            Self::Anonymous => "anon".to_string(),
            Self::Unverified => "unverified".to_string(),
        }
    }
}

impl std::fmt::Display for Principal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label())
    }
}

/// The [`Principal`] a request resolved to, attached to the request by the one
/// resolver that surface has.
///
/// Every decider downstream reads THIS, never the headers the resolver read.
/// It lives beside [`Principal`] rather than with either HTTP surface because
/// two crates attach it — the daemon's `client_auth`/`internal_principal`
/// layers and `sovereign-server`'s `auth` layer — and a second type of the
/// same shape would be a second identity scheme (ARCH principle 8).
#[derive(Clone, Debug)]
pub struct AttachedPrincipal(pub Principal);

/// What a request CLAIMS about its origin node, read from the wire.
///
/// A closed set of three, because the caller's next decision has three cases
/// and folding any two loses the one a decider needs: a claim that was never
/// made is not a claim that could not be read (ARCH principle 6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimedNodeId {
    /// No peer identity was claimed. The caller is whatever the rest of the
    /// resolution says — a local owner, a bearer, or anonymous.
    Absent,
    /// A claim in the canonical wire form. Whether it is BELIEVED is the
    /// resolver's question, not this parser's.
    Readable(NodeId),
    /// Present and not the canonical wire form, carrying the raw value so
    /// `/status` can NAME it on its zero-bucket row rather than render an
    /// opaque placeholder. Resolves to [`Principal::Unverified`].
    Unreadable(String),
}

/// **The one production read of the `x-node-id` header in `sovereign/crates`.**
///
/// The header is what a peer TYPES about itself, so it is evidence of a claim
/// and never of an identity: on a surface the iroh acceptor fronts, the
/// acceptor's verified key outranks it; on a surface nothing fronts, a claim
/// that does not resolve is [`Principal::Unverified`], not a member.
///
/// It lives here, with the key it produces, so that a grep for the literal
/// header finds exactly one file (`sovereign-daemon/src/mesh_principal_gate.rs`
/// is the test that enforces it). Two crates resolve requests to a principal —
/// `sovereign-daemon` and `sovereign-server` — and neither may hold the wire
/// form privately without the two drifting.
///
/// ## The one canonical wire form
///
/// Exactly 32 lowercase hex chars, the encoding of the 16-byte id — nothing
/// else is accepted (no `node-` prefix, no truncated hex, no uppercase value
/// re-read as hex). Both header spellings (`x-node-id` and `X-Node-Id`) are
/// read, because a header NAME is case-insensitive on the wire and a header
/// VALUE is not.
pub fn claimed_node_id(headers: &http::HeaderMap) -> ClaimedNodeId {
    let Some(raw) = headers
        .get("x-node-id")
        .or_else(|| headers.get("X-Node-Id"))
    else {
        return ClaimedNodeId::Absent;
    };
    let Ok(s) = raw.to_str() else {
        return ClaimedNodeId::Unreadable("<non-ascii header value>".to_string());
    };
    match parse_canonical_node_id(s) {
        Some(id) => ClaimedNodeId::Readable(id),
        None => ClaimedNodeId::Unreadable(s.to_string()),
    }
}

/// The canonical wire form, parsed. `None` on anything else — a longer value
/// is REFUSED, never truncated into a valid id.
fn parse_canonical_node_id(s: &str) -> Option<NodeId> {
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
    use std::collections::HashSet;

    fn node(n: u128) -> NodeId {
        NodeId::from_u128(n)
    }

    fn headers(pairs: &[(&str, &str)]) -> http::HeaderMap {
        let mut h = http::HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    /// covers: FE-99
    ///
    /// Both header spellings, one wire form. Moved here from the daemon's
    /// `headers.rs` when the parser converged onto the key it produces.
    #[test]
    fn either_header_spelling_reads_the_one_wire_form() {
        let id = node(0x42);
        for name in ["x-node-id", "X-Node-Id"] {
            let h = headers(&[(name, &id.to_hex())]);
            assert_eq!(claimed_node_id(&h), ClaimedNodeId::Readable(id), "{name}");
        }
    }

    /// An absent claim is its own answer — the branch that lets a local
    /// caller stay a local caller rather than becoming an unverified peer.
    #[test]
    fn no_header_is_an_absent_claim_not_an_unreadable_one() {
        assert_eq!(
            claimed_node_id(&http::HeaderMap::new()),
            ClaimedNodeId::Absent
        );
    }

    /// covers: FE-99
    ///
    /// "Exactly one canonical wire form", and the half that had no witness: a
    /// value LONGER than the wire form must be refused, not truncated to it.
    /// The short case is caught by the slice bound rather than by the length
    /// check, so deleting `if s.len() != 32` left the old file green — a
    /// 34-hex-char value parsed its first 32 chars and silently became a valid
    /// node id. That is a display form accepted as a wire form.
    #[test]
    fn a_wrong_length_claim_is_unreadable_and_keeps_its_raw_value() {
        for bad in ["abcd", "0000000000000000000000000000002aff"] {
            assert_eq!(
                claimed_node_id(&headers(&[("x-node-id", bad)])),
                ClaimedNodeId::Unreadable(bad.to_string()),
                "{bad} must be REFUSED, never truncated into a valid id — and \
                 its raw value kept so /status can name it"
            );
        }
    }

    /// covers: FE-99
    ///
    /// Right length, wrong alphabet — the other half of the parser contract.
    #[test]
    fn a_non_hex_claim_is_unreadable() {
        let bad = "z".repeat(32);
        assert_eq!(
            claimed_node_id(&headers(&[("x-node-id", &bad)])),
            ClaimedNodeId::Unreadable(bad)
        );
    }

    /// The six arms are six distinct keys — the union, never a collapse.
    /// A narrowing here is exactly the defect `DAEMON_CORE.md` §3.3 measured:
    /// ten callers treated as one.
    #[test]
    fn the_six_identities_are_six_distinct_keys() {
        let all = [
            Principal::LocalOwner {
                sub_identity: Some("desktop".into()),
            },
            Principal::RemoteClient {
                credential: "deadbeefdeadbeef".into(),
            },
            Principal::Member { node_id: node(7) },
            Principal::Guest {
                grant: "cafef00dcafef00d".into(),
            },
            Principal::Anonymous,
            Principal::Unverified,
        ];
        let distinct: HashSet<&Principal> = all.iter().collect();
        assert_eq!(
            distinct.len(),
            all.len(),
            "each identity must be its own key: {all:?}"
        );
    }

    /// An unverifiable claim is its own answer, never the absence of one.
    /// ARCH principle 6: "did not answer" is not "answered: no". A caller
    /// that claimed membership on a connection the daemon could not tie to
    /// its own acceptor must not read like a caller that claimed nothing.
    #[test]
    fn an_unverified_claim_is_not_an_absent_one() {
        assert_ne!(Principal::Unverified, Principal::Anonymous);
        assert_eq!(Principal::Unverified.node_id(), None, "not a peer key");
        assert_ne!(
            Principal::Unverified,
            Principal::LocalOwner { sub_identity: None },
            "an untied connection may be a LAN caller, not only a local one"
        );
    }

    /// The local owner carries the declared sub-identity the fairness gate
    /// buckets on, and an owner with no declared name is still not anonymous.
    #[test]
    fn a_local_owner_carries_its_declared_sub_identity() {
        let desktop = Principal::LocalOwner {
            sub_identity: Some("desktop".into()),
        };
        let cli = Principal::LocalOwner {
            sub_identity: Some("cli".into()),
        };
        let unnamed = Principal::LocalOwner { sub_identity: None };
        assert_ne!(desktop, cli, "two declared sub-identities are two buckets");
        assert_ne!(
            desktop, unnamed,
            "the owner named desktop is not the owner with no name"
        );
        assert_ne!(unnamed, Principal::Anonymous, "the owner is not anonymous");
    }

    /// A remote client is keyed by its credential, not dropped into one shared
    /// bucket — the property the fairness gate exists for.
    #[test]
    fn remote_clients_are_distinct_by_credential() {
        let a = Principal::RemoteClient {
            credential: "cred-a".into(),
        };
        let b = Principal::RemoteClient {
            credential: "cred-b".into(),
        };
        assert_ne!(a, b, "different credentials are different callers");
    }

    /// The peer key is present only for a member, and it is the verified node
    /// id — never a placeholder for the other four identities.
    #[test]
    fn only_a_member_has_a_peer_key() {
        let member = Principal::Member {
            node_id: node(0xBEEF),
        };
        assert_eq!(member.node_id(), Some(node(0xBEEF)));
        assert_ne!(
            member,
            Principal::Member {
                node_id: node(0xD00D)
            },
            "two members are two peer keys"
        );
        for non_member in [
            Principal::LocalOwner {
                sub_identity: Some("desktop".into()),
            },
            Principal::LocalOwner { sub_identity: None },
            Principal::RemoteClient {
                credential: "x".into(),
            },
            Principal::Guest { grant: "x".into() },
            Principal::Anonymous,
        ] {
            assert_eq!(
                non_member.node_id(),
                None,
                "{non_member:?} is not a peer — absence, not node zero"
            );
        }
    }

    /// The label is the loggable rendering: a fingerprint for the two arms
    /// that name a secret, a full node id for a member, and a distinct
    /// spelling per arm.
    #[test]
    fn a_label_is_stable_and_spells_each_arm_apart() {
        assert_eq!(
            Principal::LocalOwner {
                sub_identity: Some("desktop".into())
            }
            .label(),
            "owner:desktop"
        );
        assert_eq!(
            Principal::LocalOwner { sub_identity: None }.label(),
            "owner"
        );
        assert_eq!(
            Principal::RemoteClient {
                credential: "deadbeef".into()
            }
            .label(),
            "cred:deadbeef"
        );
        assert_eq!(
            Principal::Guest {
                grant: "cafe".into()
            }
            .label(),
            "guest:cafe"
        );
        assert_eq!(Principal::Anonymous.label(), "anon");
        assert_eq!(Principal::Unverified.label(), "unverified");
        let member = Principal::Member { node_id: node(7) };
        assert_eq!(member.label(), format!("member:{}", node(7).to_hex()));
        assert_eq!(member.to_string(), member.label());
    }
}
