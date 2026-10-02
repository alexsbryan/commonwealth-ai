// SPDX-License-Identifier: AGPL-3.0-or-later
//! The inbound half: who reaches what, by ALPN and by who dialed.
//!
//! An iroh endpoint accepts anyone — the dial string rides in every invite
//! and is gossiped as `node_pubkey`, so holding it is not a credential. What
//! the connection DOES carry is the dialer's key, verified by the QUIC
//! handshake. That key and the negotiated ALPN are all this consults.
//!
//! **The table is the origin registry** ([`OriginRegistry::forward_for`];
//! phase-b pb-rails-origins, FIVE_PROGRAMS §4 rule 8). There is no arm per
//! protocol here: every ALPN this endpoint serves, who may reach it and where
//! it goes is a registration — this endpoint's own gossip and join routes and
//! its `rails.toml` media origin (`crate::origins::stand_own`), the app entry,
//! and each program's registered origin. The endpoint advertises exactly the
//! registered ALPNs and re-advertises when they change, because a negotiated
//! protocol with nothing behind it turns a clean refusal into a hang.
//!
//! - `cwth/http/0` forwards by registered path prefix to ANY dialer: a joiner
//!   is not a member yet, and the gossip merge authorizes for itself. A
//!   members-only prefix is refused to a stranger by name; an unregistered
//!   prefix is refused by name, so nothing unregistered is forwarded.
//! - `cwth/media/0` and `cwth/offer/0` go through the one members-and-allow
//!   decider (`commonwealth_media::admit_spliced_origin`), shared with the
//!   inference daemon. `cwth/app/0` is `admit_app` against the live app
//!   registry; which app is decided per request.
//!
//! **A rails node publishes through the loopback API, never through
//! `rails.toml`.** An `[apps]` table there would make an un-upgraded daemon
//! REFUSE TO BOOT on a config a newer one wrote, because `Config` and
//! `MediaSection` are `#[serde(deny_unknown_fields)]` — the same hazard
//! `commonwealth_media::declared` sidesteps by putting the value in a file
//! whose NAME is the key. The claim tier sidesteps it the same way, by
//! needing no config key at all: the registration lives in the process, which
//! is where the truth about a running origin was anyway.

use std::sync::Arc;

use commonwealth_core::ids::NodePubkey;
use commonwealth_core::mesh::Mesh;
use commonwealth_media::origins::OriginRegistry;
use commonwealth_media::{MemberCheck, MemberIdentity};
use commonwealth_transport::iroh::{Endpoint, IrohAcceptor};
use tokio::sync::RwLock;

/// The roster consult the media arm makes on EVERY dial. Not cached: a member
/// can leave between two dials, and a cached set would keep admitting a
/// departed node for as long as it was stale. Tombstoned rows are excluded,
/// so leaving the mesh takes reachability with it.
pub fn member_check(mesh: Arc<RwLock<Mesh>>) -> MemberCheck {
    Arc::new(move |dialer: NodePubkey| {
        let mesh = mesh.clone();
        Box::pin(async move {
            let mesh = mesh.read().await;
            mesh.members
                .values()
                .find(|m| m.node_pubkey == Some(dialer) && m.removed_at.is_none())
                .map(|m| MemberIdentity {
                    name: m.name.clone(),
                    node_id: m.node_id,
                })
        })
    })
}

/// Spawn the accept loop over `origins`. Dropping the returned acceptor
/// stops it.
pub fn spawn(endpoint: Endpoint, mesh: Arc<RwLock<Mesh>>, origins: OriginRegistry) -> IrohAcceptor {
    let check = member_check(mesh);
    {
        let endpoint = endpoint.clone();
        origins.on_alpns_change(Arc::new(move |set| {
            tracing::info!(
                target: "rails",
                alpns = ?set.iter().map(|a| String::from_utf8_lossy(a).into_owned()).collect::<Vec<_>>(),
                "acceptor: the endpoint now serves exactly the registered ALPNs"
            );
            endpoint.set_alpns(set);
        }));
    }
    IrohAcceptor::spawn_admitting_forward(endpoint, move |alpn, dialer| {
        let check = check.clone();
        let origins = origins.clone();
        async move {
            let who = check(dialer).await;
            tracing::debug!(
                target: "rails",
                alpn = %String::from_utf8_lossy(&alpn),
                dialer = %hex::encode(dialer.0),
                member = who.as_ref().map(|w| w.name.as_str()),
                "acceptor: dial — the registry decides"
            );
            origins.forward_for(&alpn, who.as_ref(), dialer)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::capabilities::OriginKind;
    use commonwealth_core::ids::NodeId;
    use commonwealth_core::mesh::{MemberRecord, NodeStatus};

    fn member(id: u128, name: &str, key: Option<NodePubkey>) -> MemberRecord {
        MemberRecord {
            node_id: NodeId::from_u128(id),
            name: name.into(),
            invited_by: NodeId::from_u128(1),
            joined_at: 100,
            last_seen: 100,
            status: NodeStatus::Online,
            capabilities: crate::gossip::minimal_capabilities(100, &[OriginKind::Media], None),
            addresses: Vec::new(),
            node_pubkey: key,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
            removed_at: None,
        }
    }

    fn mesh_with(records: Vec<MemberRecord>) -> Arc<RwLock<Mesh>> {
        let (mut mesh, _key) =
            commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
        mesh.members.clear();
        for r in records {
            mesh.members.insert(r.node_id, r);
        }
        Arc::new(RwLock::new(mesh))
    }

    /// The happy path, and what makes the two refusals below mean something.
    #[tokio::test]
    async fn a_member_on_the_roster_is_named_by_its_key() {
        let key = NodePubkey([7u8; 32]);
        let check = member_check(mesh_with(vec![member(0xB0B, "LittleMac", Some(key))]));
        let who = check(key).await.expect("a member");
        assert_eq!(who.name, "LittleMac");
        assert_eq!(who.node_id, NodeId::from_u128(0xB0B));
    }

    /// **The failing input.** A key nobody on the roster holds must resolve to
    /// `None`, because `admit_media(None, ..)` is what closes the dial. A
    /// check that returned any identity here would hand a stranger the origin.
    #[tokio::test]
    async fn a_key_that_is_not_on_the_roster_is_nobody() {
        let check = member_check(mesh_with(vec![member(
            0xB0B,
            "LittleMac",
            Some(NodePubkey([7u8; 32])),
        )]));
        assert!(check(NodePubkey([9u8; 32])).await.is_none());
    }

    /// **The second failing input.** A member who LEFT keeps its row while the
    /// tombstone circulates, and a check reading `members` without the
    /// `removed_at` filter would keep admitting it — reachability outliving
    /// membership, which is the failure the tombstone exists to prevent.
    #[tokio::test]
    async fn a_tombstoned_member_is_nobody_even_though_its_row_is_still_there() {
        let key = NodePubkey([7u8; 32]);
        let mut gone = member(0xB0B, "LittleMac", Some(key));
        gone.removed_at = Some(200);
        let check = member_check(mesh_with(vec![gone]));
        assert!(
            check(key).await.is_none(),
            "a departed member must not still reach the origin"
        );
    }

    /// A member with no key at all is never matched by `Some(dialer)` — the
    /// `Option == Option` comparison would be a true-for-None bug if it were
    /// written as `m.node_pubkey.is_none() || ...`.
    #[tokio::test]
    async fn a_pre_identity_member_matches_nobody() {
        let check = member_check(mesh_with(vec![member(0xB0B, "LittleMac", None)]));
        assert!(check(NodePubkey([0u8; 32])).await.is_none());
    }
}
