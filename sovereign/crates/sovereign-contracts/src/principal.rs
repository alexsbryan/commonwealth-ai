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
//! The five identities today's resolvers distinguish, each an arm here:
//!
//! - [`Principal::LocalOwner`] — a caller on this machine, carrying the
//!   declared sub-identity the client fairness gate buckets on;
//! - [`Principal::RemoteClient`] — a remote caller, by the credential it
//!   presented;
//! - [`Principal::Member`] — a verified mesh member, by node id;
//! - [`Principal::Guest`] — a caller holding a live guest grant;
//! - [`Principal::Anonymous`] — nothing was presented.
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

use kernel_types::NodeId;

/// Who is asking.
///
/// The key of `DAEMON_CORE.md` §1's one table `principal -> Scope` and of
/// admission's inflight guard and tally. `Eq + Hash` because a peer's row and
/// a local caller's row are the same shape: one key type, five arms, never a
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
            | Self::Anonymous => None,
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
        }
    }
}

impl std::fmt::Display for Principal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn node(n: u128) -> NodeId {
        NodeId::from_u128(n)
    }

    /// The five arms are five distinct keys — the union, never a collapse.
    /// A narrowing here is exactly the defect `DAEMON_CORE.md` §3.3 measured:
    /// ten callers treated as one.
    #[test]
    fn the_five_identities_are_five_distinct_keys() {
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
        ];
        let distinct: HashSet<&Principal> = all.iter().collect();
        assert_eq!(
            distinct.len(),
            all.len(),
            "each identity must be its own key: {all:?}"
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
        let member = Principal::Member { node_id: node(7) };
        assert_eq!(member.label(), format!("member:{}", node(7).to_hex()));
        assert_eq!(member.to_string(), member.label());
    }
}
