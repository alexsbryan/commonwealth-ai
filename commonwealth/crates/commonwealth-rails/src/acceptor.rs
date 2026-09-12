// SPDX-License-Identifier: AGPL-3.0-or-later
//! The inbound half: who reaches what, by ALPN and by who dialed.
//!
//! An iroh endpoint accepts anyone — the dial string rides in every invite
//! and is gossiped as `node_pubkey`, so holding it is not a credential. What
//! the connection DOES carry is the dialer's key, verified by the QUIC
//! handshake. Two protocols, two different answers:
//!
//! - `cwth/http/0` is spliced to the internal gossip listener for ANY dialer.
//!   That listener authenticates for itself: `merge_from_authenticated`
//!   refuses a payload whose mesh or invite hash is not ours, so a stranger's
//!   round is a 401 rather than something the acceptor had to predict. It is
//!   also the only way a peer that has just been ADMITTED can reach us at
//!   all, before our roster knows its key — the same reasoning as
//!   `sovereign-mesh/src/iroh_access.rs`.
//! - `cwth/media/0` is the origin, and there the decision is
//!   `commonwealth_media::admit_media` — the one implementation of it, shared
//!   with the inference daemon. A non-member is closed (the origin
//!   authenticates nothing, so there is no safe downgrade), and a member
//!   outside a non-empty `media_allow` is closed and logged with the list.
//! - `cwth/app/0` is the same shape against a DIFFERENT list and a live
//!   registry: `commonwealth_media::admit_app`, again the one implementation.
//!   Which app is not decided here — that is per request, among the origins
//!   this node publishes at the moment the request arrives.
//!
//! **A rails node publishes apps through the loopback API, never through
//! `rails.toml`.** An `[apps]` table there would make an un-upgraded daemon
//! REFUSE TO BOOT on a config a newer one wrote, because `Config` and
//! `MediaSection` are `#[serde(deny_unknown_fields)]` — the same hazard
//! `commonwealth_media::declared` sidesteps by putting the value in a file
//! whose NAME is the key. The claim tier sidesteps it the same way, by
//! needing no config key at all: the registration lives in the process, which
//! is where the truth about a running app was anyway.
//!
//! Anything else is closed loudly. An unknown ALPN on a two-ALPN endpoint
//! means something dialed a protocol this build does not serve, and silence
//! there reads to the dialer exactly like a network fault.

use std::net::SocketAddr;
use std::sync::Arc;

use commonwealth_core::ids::NodePubkey;
use commonwealth_core::mesh::Mesh;
use commonwealth_media::{MemberCheck, MemberIdentity};
use commonwealth_transport::iroh::{Endpoint, Forward, IrohAcceptor, ALPN, APP_ALPN, MEDIA_ALPN};
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

/// Spawn the accept loop. Dropping the returned acceptor stops it.
pub fn spawn(
    endpoint: Endpoint,
    internal_addr: SocketAddr,
    mesh: Arc<RwLock<Mesh>>,
    media_origin: Option<SocketAddr>,
    media_allow: Vec<String>,
    media_declared: Vec<(String, String)>,
    apps: commonwealth_media::PublishedApps,
) -> IrohAcceptor {
    let check = member_check(mesh);
    // `cwth/app/0` is advertised only while something is published, and the
    // set is updated live because the claim tier changes it while the daemon
    // runs. Base list without it, so the hook below can add and remove it
    // without having to know what else the endpoint serves.
    {
        let endpoint = endpoint.clone();
        apps.on_serving_change(Arc::new(move |serving| {
            let mut set = vec![ALPN.to_vec(), MEDIA_ALPN.to_vec()];
            if serving {
                set.push(APP_ALPN.to_vec());
            }
            tracing::info!(
                target: "rails",
                serving_apps = serving,
                "acceptor: app publishing changed — the endpoint now {} cwth/app/0",
                if serving { "serves" } else { "does not serve" }
            );
            endpoint.set_alpns(set);
        }));
    }
    tracing::info!(
        target: "rails",
        internal = %internal_addr,
        media_origin = ?media_origin,
        media_allow = ?media_allow,
        // NAMES only. The values are this node's credentials for its own
        // origin; whether one is set is operational, what it is never is.
        media_declared = ?media_declared.iter().map(|(n, _)| n).collect::<Vec<_>>(),
        "acceptor: serving cwth/http/0 (gossip) and cwth/media/0 (origin)"
    );
    let media_declared = Arc::new(media_declared);
    IrohAcceptor::spawn_admitting_forward(endpoint, move |alpn, dialer| {
        let check = check.clone();
        let allow = media_allow.clone();
        let declared = media_declared.clone();
        let apps = apps.clone();
        async move {
            if alpn == ALPN {
                tracing::debug!(
                    target: "rails",
                    dialer = %hex::encode(dialer.0),
                    "acceptor: gossip dial spliced to the internal listener \
                     (the merge authorizes, not this)"
                );
                return Some(Forward::Splice(internal_addr));
            }
            if alpn == MEDIA_ALPN {
                let who = check(dialer).await;
                return commonwealth_media::admit_media(
                    who.as_ref(),
                    dialer,
                    media_origin,
                    &allow,
                    &declared,
                );
            }
            if alpn == APP_ALPN {
                let who = check(dialer).await;
                // Every member, because a rails node has no `app_allow` key
                // to narrow it with and inventing one is the config hazard
                // above. The narrower grant is the inference daemon's
                // `[iroh] app_allow`; here the honest statement is that
                // publishing an app on a rails node offers it to the mesh.
                return commonwealth_media::admit_app(who.as_ref(), dialer, &apps.snapshot(), &[]);
            }
            tracing::warn!(
                target: "rails",
                alpn = %String::from_utf8_lossy(&alpn),
                dialer = %hex::encode(dialer.0),
                "acceptor: unknown ALPN — closing. This build serves cwth/http/0, \
                 cwth/media/0 and cwth/app/0"
            );
            None
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
            capabilities: crate::gossip::minimal_capabilities(100, &[OriginKind::Media]),
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
