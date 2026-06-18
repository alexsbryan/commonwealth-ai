// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::capabilities::NodeCapabilities;
use crate::ids::{MeshId, NodeId, NodePubkey};

/// A Commonwealth mesh — a closed group of trusted nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mesh {
    pub id: MeshId,
    pub name: String,
    /// BLAKE3 hash of the join key; raw key is never persisted.
    pub join_key_hash: [u8; 32],
    pub members: HashMap<NodeId, MemberRecord>,
    pub peers: Vec<MeshPeering>,
}

/// Record of a member node in the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberRecord {
    pub node_id: NodeId,
    pub name: String,
    pub invited_by: NodeId,
    pub joined_at: u64,
    pub last_seen: u64,
    pub status: NodeStatus,
    pub capabilities: NodeCapabilities,
    pub addresses: Vec<SocketAddr>,
    /// Ed25519 identity key (see [`NodePubkey`]). `None` for nodes
    /// running pre-identity builds. Serde-defaulted both directions:
    /// old nodes ignore the field on receive and new nodes read old
    /// payloads as `None`; `skip_serializing_if` keeps new→old wire
    /// bytes identical when no key exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_pubkey: Option<NodePubkey>,
    /// iroh relay URL for dial-by-key reachability (Track W2 of
    /// TRANSPORT_MIGRATION.md). `None` when this node isn't reachable
    /// over iroh (iroh disabled, or no relay connected yet). Together
    /// with [`Self::node_pubkey`] and [`Self::iroh_direct_addrs`] this
    /// is everything a peer needs to dial this node by key — the
    /// "membership = dialability" collapse. Unlike `node_pubkey` (an
    /// immutable identity, anti-downgrade-protected in `merge_from`),
    /// this is MUTABLE reachability: it rides normal last-seen LWW, so
    /// a node that gains/loses a relay updates peers within one round.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    /// iroh direct (hole-punched / LAN) socket hints for dial-by-key.
    /// Empty when unknown. Mutable reachability — rides normal LWW like
    /// [`Self::relay_url`]. Lets a LAN peer dial without a relay round
    /// trip; iroh still verifies the key on connect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iroh_direct_addrs: Vec<SocketAddr>,
}

/// This node's current iroh dial info, pulled fresh each gossip round
/// from the live endpoint and stamped into its own [`MemberRecord`].
/// A plain struct (no iroh types) so it crosses the
/// `commonwealth-core` boundary — the `commonwealth-api` `AppState`
/// stores a type-erased provider yielding this, installed by the
/// daemon (which owns the iroh endpoint). Empty/`None` fields mean
/// "not reachable that way yet"; the values change over a node's
/// lifetime as iroh discovers a relay and hole-punches direct paths.
#[derive(Debug, Clone, Default)]
pub struct IrohDialInfo {
    pub relay_url: Option<String>,
    pub direct_addrs: Vec<SocketAddr>,
}

/// Current status of a node as observed by the mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Online,
    /// Under heavy local load.
    Busy,
    /// Not responding but not formally departed.
    Away,
    /// Gracefully disconnected.
    Offline,
}

/// Trust relationship with a peer mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshPeering {
    pub peer_mesh_id: MeshId,
    pub peer_mesh_name: String,
    pub trust_level: PeerTrustLevel,
    pub established_at: u64,
    pub contact_nodes: Vec<SocketAddr>,
}

/// Level of trust between peered meshes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerTrustLevel {
    /// Share model files and corpus indexes only.
    ModelAndKnowledgeSharing,
    /// Share everything plus allow overflow inference routing.
    Full,
}

/// Summary of what a `Mesh::merge_from` call did. Used for tracing
/// ("we learned about 1 new member") and test assertions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Members that were absent locally and got added from `other`.
    pub added: usize,
    /// Members that existed locally but were replaced by a newer
    /// record from `other` (higher `last_seen`).
    pub updated: usize,
    /// True when the merge was refused outright because `other`
    /// described a different mesh (mismatching `id` or
    /// `join_key_hash`). When set, nothing was mutated.
    pub rejected: bool,
}

impl Mesh {
    /// Merge another view of this mesh into `self`. Per-member
    /// `last_seen` acts as the Lamport-ish clock: the record with
    /// the higher timestamp wins. Own record (the caller's
    /// `self_node_id`) is *never* overwritten via gossip — we are
    /// always authoritative for our own liveness, capabilities,
    /// addresses, etc. Peers can learn about us from others, but
    /// they can't replace what we know about ourselves.
    ///
    /// Returns a [`MergeReport`] so callers can surface "added 1,
    /// updated 0" in tracing logs — useful for noticing when gossip
    /// is actually converging vs. spinning.
    ///
    /// Rejects outright when `other.id` or `other.join_key_hash`
    /// doesn't match ours — that's the auth boundary. Anyone who
    /// knows our mesh_id (public via mDNS) but not the join_key
    /// shouldn't be able to inject members into our view.
    pub fn merge_from(&mut self, self_node_id: NodeId, other: &Mesh) -> MergeReport {
        if self.id != other.id || self.join_key_hash != other.join_key_hash {
            return MergeReport {
                added: 0,
                updated: 0,
                rejected: true,
            };
        }

        let mut report = MergeReport::default();
        for (id, incoming) in &other.members {
            if *id == self_node_id {
                // Authoritative-for-self: never accept an incoming
                // record about us, regardless of its `last_seen`.
                // If a buggy peer has a stale view of us, we'll
                // correct them on our next gossip round when we
                // ship our current record in the push-pull reply.
                continue;
            }
            match self.members.get(id) {
                None => {
                    self.members.insert(*id, incoming.clone());
                    report.added += 1;
                }
                Some(existing) if incoming.last_seen > existing.last_seen => {
                    // Anti-downgrade: a newer record relayed by a
                    // pre-identity build carries `node_pubkey: None`.
                    // Without this preservation, ONE old peer in the
                    // gossip path strips every node's pubkey on each
                    // LWW win. An identity key never changes within
                    // a membership, so keeping the locally-known key
                    // while taking the rest of the newer record is
                    // always correct.
                    let preserved_pubkey = match incoming.node_pubkey {
                        Some(pk) => Some(pk),
                        None => existing.node_pubkey,
                    };
                    let mut record = incoming.clone();
                    record.node_pubkey = preserved_pubkey;
                    self.members.insert(*id, record);
                    report.updated += 1;
                }
                Some(_) => {
                    // Existing is equal or newer — keep ours.
                }
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_status_serde_roundtrip() {
        for status in [
            NodeStatus::Online,
            NodeStatus::Busy,
            NodeStatus::Away,
            NodeStatus::Offline,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let back: NodeStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }
    }

    #[test]
    fn peer_trust_level_serde_roundtrip() {
        for level in [
            PeerTrustLevel::ModelAndKnowledgeSharing,
            PeerTrustLevel::Full,
        ] {
            let json = serde_json::to_string(&level).unwrap();
            let back: PeerTrustLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(level, back);
        }
    }

    #[test]
    fn mesh_peering_serializes_to_json() {
        let peering = MeshPeering {
            peer_mesh_id: MeshId::from_u128(42),
            peer_mesh_name: "Mission District Co-op".into(),
            trust_level: PeerTrustLevel::Full,
            established_at: 1700000000,
            contact_nodes: vec!["10.0.1.50:9742".parse().unwrap()],
        };
        let json = serde_json::to_string(&peering).unwrap();
        let back: MeshPeering = serde_json::from_str(&json).unwrap();
        assert_eq!(back.peer_mesh_name, "Mission District Co-op");
        assert_eq!(back.trust_level, PeerTrustLevel::Full);
    }

    // ── Mesh::merge_from ──────────────────────────────────────

    use crate::capabilities::{AvailableResources, HardwareProfile};

    fn member(id: NodeId, name: &str, last_seen: u64) -> MemberRecord {
        MemberRecord {
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            node_id: id,
            name: name.into(),
            invited_by: id,
            joined_at: 0,
            last_seen,
            status: NodeStatus::Online,
            capabilities: NodeCapabilities {
                hardware: HardwareProfile {
                    gpus: vec![],
                    system_ram_gb: 0,
                    cpu_cores: 0,
                    total_storage_gb: 0,
                    free_storage_gb: 0,
                    network_bandwidth_mbps: None,
                },
                available: AvailableResources::default(),
                active_processes: vec![],
                hosted_corpora: vec![],
                reported_at: last_seen,
                inference_availability: 1.0,
                inference_capable: false,
                loaded_models: vec![],

                embed_model: None,
                benchmark: None,
                current_in_flight: None,
            },
            addresses: vec![],
        }
    }

    fn mesh_with(members: Vec<MemberRecord>, id: MeshId, hash: [u8; 32]) -> Mesh {
        let mut map = HashMap::new();
        for m in members {
            map.insert(m.node_id, m);
        }
        Mesh {
            id,
            name: "test".into(),
            join_key_hash: hash,
            members: map,
            peers: vec![],
        }
    }

    #[test]
    fn iroh_dial_fields_serde_back_compat_and_mutable_lww() {
        // Back-compat: a record with no iroh dial info serializes
        // WITHOUT the keys (skip_serializing_if), so a pre-W2 node sees
        // identical bytes — and such a payload reads back as None/empty.
        let bare = member(NodeId::from_u128(1), "a", 1);
        let json = serde_json::to_value(&bare).unwrap();
        assert!(json.get("relay_url").is_none(), "relay_url omitted when None");
        assert!(
            json.get("iroh_direct_addrs").is_none(),
            "iroh_direct_addrs omitted when empty"
        );
        let back: MemberRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back.relay_url, None);
        assert!(back.iroh_direct_addrs.is_empty());

        // Round-trips with values.
        let mut keyed = member(NodeId::from_u128(3), "c", 5);
        keyed.relay_url = Some("https://relay.example./".into());
        keyed.iroh_direct_addrs = vec!["127.0.0.1:5000".parse().unwrap()];
        let rt: MemberRecord =
            serde_json::from_value(serde_json::to_value(&keyed).unwrap()).unwrap();
        assert_eq!(rt.relay_url.as_deref(), Some("https://relay.example./"));
        assert_eq!(rt.iroh_direct_addrs, keyed.iroh_direct_addrs);

        // The load-bearing distinction: relay_url/iroh_direct_addrs are
        // MUTABLE reachability and ride normal last-seen LWW — a newer
        // record replaces them (even to None when a node turns iroh
        // off). node_pubkey is IMMUTABLE identity and is
        // anti-downgrade-preserved when a relayer drops it.
        let mesh_id = MeshId::from_u128(9);
        let hash = [3u8; 32];
        let mut have = member(NodeId::from_u128(7), "p", 1);
        have.relay_url = Some("https://old.relay./".into());
        have.node_pubkey = Some(NodePubkey([0xAB; 32]));
        let mut local = mesh_with(
            vec![member(NodeId::from_u128(1), "self", 100), have],
            mesh_id,
            hash,
        );

        let mut newer = member(NodeId::from_u128(7), "p", 2); // higher last_seen
        newer.relay_url = Some("https://new.relay./".into());
        newer.node_pubkey = None; // relayed by a peer that didn't carry the key
        let incoming = mesh_with(vec![newer], mesh_id, hash);

        local.merge_from(NodeId::from_u128(1), &incoming);
        let merged = local.members.get(&NodeId::from_u128(7)).unwrap();
        assert_eq!(
            merged.relay_url.as_deref(),
            Some("https://new.relay./"),
            "relay_url is mutable LWW — the newer record wins"
        );
        assert_eq!(
            merged.node_pubkey,
            Some(NodePubkey([0xAB; 32])),
            "node_pubkey anti-downgrade still preserves the known identity key"
        );
    }

    #[test]
    fn merge_adds_missing_members() {
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let mut local = mesh_with(vec![member(a, "A", 10)], mesh_id, hash);
        let remote = mesh_with(vec![member(a, "A", 10), member(b, "B", 20)], mesh_id, hash);

        let report = local.merge_from(a, &remote);
        assert_eq!(report.added, 1);
        assert_eq!(report.updated, 0);
        assert!(!report.rejected);
        assert_eq!(local.members.len(), 2);
        assert!(local.members.contains_key(&b));
    }

    #[test]
    fn merge_updates_stale_records_via_last_seen() {
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let mut local = mesh_with(
            vec![member(a, "A", 10), member(b, "B-stale", 5)],
            mesh_id,
            hash,
        );
        let remote = mesh_with(vec![member(b, "B-fresh", 50)], mesh_id, hash);

        let report = local.merge_from(a, &remote);
        assert_eq!(report.added, 0);
        assert_eq!(report.updated, 1);
        assert_eq!(local.members.get(&b).unwrap().name, "B-fresh");
        assert_eq!(local.members.get(&b).unwrap().last_seen, 50);
    }

    #[test]
    fn merge_keeps_newer_local_over_older_incoming() {
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let mut local = mesh_with(
            vec![member(a, "A", 10), member(b, "B-fresh", 100)],
            mesh_id,
            hash,
        );
        let remote = mesh_with(vec![member(b, "B-stale", 20)], mesh_id, hash);

        let report = local.merge_from(a, &remote);
        assert_eq!(report.added, 0);
        assert_eq!(report.updated, 0);
        assert_eq!(local.members.get(&b).unwrap().name, "B-fresh");
    }

    #[test]
    fn merge_preserves_pubkey_when_old_peer_relays_record_without_it() {
        // The mixed-version mesh scenario: B has an identity key we
        // already know. An OLD-build peer relays B's record with a
        // newer last_seen but no node_pubkey field (its build
        // predates the field, so it gossips None). The LWW win must
        // NOT strip the key we know.
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let mut local = mesh_with(
            vec![member(a, "A", 10), {
                let mut m = member(b, "B", 5);
                m.node_pubkey = Some(NodePubkey([0xAB; 32]));
                m
            }],
            mesh_id,
            hash,
        );
        let remote = mesh_with(vec![member(b, "B", 50)], mesh_id, hash);

        let report = local.merge_from(a, &remote);
        assert_eq!(report.updated, 1);
        let merged = local.members.get(&b).unwrap();
        assert_eq!(merged.last_seen, 50, "rest of the newer record adopted");
        assert_eq!(
            merged.node_pubkey,
            Some(NodePubkey([0xAB; 32])),
            "locally-known pubkey survives a None-bearing LWW win"
        );
    }

    #[test]
    fn merge_adopts_pubkey_from_newer_record_that_carries_one() {
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let mut local = mesh_with(vec![member(a, "A", 10), member(b, "B", 5)], mesh_id, hash);
        let remote = mesh_with(
            vec![{
                let mut m = member(b, "B", 50);
                m.node_pubkey = Some(NodePubkey([0xCD; 32]));
                m
            }],
            mesh_id,
            hash,
        );

        local.merge_from(a, &remote);
        assert_eq!(
            local.members.get(&b).unwrap().node_pubkey,
            Some(NodePubkey([0xCD; 32]))
        );
    }

    #[test]
    fn member_record_wire_compat_with_pre_identity_builds() {
        // New → old: a record without a key serializes WITHOUT the
        // node_pubkey field, byte-identical to the pre-identity wire.
        let m = member(NodeId::from_u128(1), "A", 10);
        let json = serde_json::to_value(&m).unwrap();
        assert!(
            json.get("node_pubkey").is_none(),
            "None must not appear on the wire"
        );

        // Old → new: pre-identity JSON (no node_pubkey key) parses
        // with node_pubkey = None.
        let back: MemberRecord = serde_json::from_value(json).unwrap();
        assert!(back.node_pubkey.is_none());

        // Round-trip with a key present.
        let mut keyed = member(NodeId::from_u128(2), "B", 10);
        keyed.node_pubkey = Some(NodePubkey([9u8; 32]));
        let json = serde_json::to_string(&keyed).unwrap();
        let back: MemberRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.node_pubkey, Some(NodePubkey([9u8; 32])));
    }

    #[test]
    fn merge_never_overwrites_self_even_if_incoming_is_newer() {
        // A peer could ship us an old view of ourselves that has a
        // stale address list, wrong name, or Offline status. We're
        // authoritative for self — ignore it.
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let me = NodeId::from_u128(100);

        let mut local = mesh_with(vec![member(me, "Me-Real", 10)], mesh_id, hash);
        let remote = mesh_with(
            vec![{
                let mut m = member(me, "Me-Imposter", 9999);
                m.status = NodeStatus::Offline;
                m
            }],
            mesh_id,
            hash,
        );

        let report = local.merge_from(me, &remote);
        assert_eq!(report.added, 0);
        assert_eq!(report.updated, 0);
        assert_eq!(local.members.get(&me).unwrap().name, "Me-Real");
        assert_eq!(local.members.get(&me).unwrap().last_seen, 10);
        assert_eq!(local.members.get(&me).unwrap().status, NodeStatus::Online);
    }

    #[test]
    fn merge_rejects_different_mesh_id() {
        let me = NodeId::from_u128(1);
        let hash = [7u8; 32];
        let mut local = mesh_with(vec![member(me, "M", 10)], MeshId::from_u128(1), hash);
        let remote = mesh_with(
            vec![member(NodeId::from_u128(2), "X", 100)],
            MeshId::from_u128(2), // different!
            hash,
        );

        let report = local.merge_from(me, &remote);
        assert!(report.rejected);
        assert_eq!(report.added, 0);
        assert_eq!(local.members.len(), 1, "no mutation on reject");
    }

    #[test]
    fn merge_rejects_mismatched_join_key_hash() {
        let me = NodeId::from_u128(1);
        let mesh_id = MeshId::from_u128(1);
        let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
        let remote = mesh_with(
            vec![member(NodeId::from_u128(2), "X", 100)],
            mesh_id,
            [9u8; 32], // different hash!
        );

        let report = local.merge_from(me, &remote);
        assert!(report.rejected);
        assert_eq!(local.members.len(), 1);
    }
}
