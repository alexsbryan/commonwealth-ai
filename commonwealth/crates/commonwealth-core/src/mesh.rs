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
    /// Mesh-wide encryption policy, set by the founder at creation and
    /// inherited by every joiner (it rides the same live gossip + join
    /// snapshot as [`Self::join_key_hash`]). When `true`, every member
    /// enforces dial-by-key iroh transport for all traffic classes with
    /// no plaintext fallback, closes its plaintext ingress, and requires
    /// an encrypted join. Founder-set and **monotonic**: [`Self::merge_from`]
    /// only ever turns this ON (stricter-wins), so no peer — stale or
    /// hostile — can demote an encrypted mesh to plaintext. `#[serde(default)]`
    /// keeps wire/persist bytes identical for pre-policy nodes (they
    /// read and write `false`).
    #[serde(default)]
    pub require_encryption: bool,
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
    /// Monotonic counter for this node's SIGNED dial info (relay_url +
    /// iroh_direct_addrs). Bumped by the OWNER each time its reachability
    /// changes; the anti-rollback key in [`Mesh::merge_from`] — a merge
    /// adopts dial info only if its version is `>=` the version we hold,
    /// so a replayed older signed record can't roll a node back. `0` =
    /// legacy / unsigned. See [`crate::dial_sig`].
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub dial_info_version: u64,
    /// Hex-encoded Ed25519 signature by this node's `node_pubkey` over the
    /// canonical dial-info message (`crate::dial_sig::dial_info_message`).
    /// When present and valid, the dial info is tamper-evident: only the
    /// key-holder can change its own reachability, so a gossip-strip
    /// attacker past the join-key gate cannot force a peer unreachable or
    /// (on a non-required class) downgrade it. `None` = legacy / unsigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dial_info_sig: Option<String>,
    /// Tombstone: unix-seconds at which this member was removed (graceful
    /// `leave` or `revoke_member`). `None` = active member. Gossiped
    /// (wire-compatible like `node_pubkey`): a tombstone propagates mesh-wide
    /// and, via the event-time LWW in [`Mesh::merge_from`], out-competes any
    /// stale *live* record so a departed node can't be resurrected by a peer
    /// still holding its old record — while a genuine rejoin (activity newer
    /// than the removal) still wins. Read paths filter `removed_at.is_some()`
    /// out of active / online views.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_at: Option<u64>,
}

/// `skip_serializing_if` predicate for the dial-info version, so an
/// unsigned/legacy record (version 0) stays byte-identical on the wire.
fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

impl MemberRecord {
    /// Whether this member is active (not tombstoned). Read paths
    /// (online counts, gossip targets, knowledge fan-out roster) filter on
    /// this so a departed node is invisible to scheduling while its tombstone
    /// still circulates for convergence.
    pub fn is_active(&self) -> bool {
        self.removed_at.is_none()
    }

    /// The timestamp of this record's latest lifecycle event — the later of
    /// its self-stamped `last_seen` and its `removed_at` tombstone. This is the
    /// LWW key in [`Mesh::merge_from`]: a removal stamped after the node's last
    /// heartbeat out-competes stale live copies, but a rejoin whose `last_seen`
    /// post-dates the removal out-competes the tombstone.
    fn event_time(&self) -> u64 {
        self.last_seen.max(self.removed_at.unwrap_or(0))
    }
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// Node IDs whose records we just observed advance (added or
    /// LWW-updated) in this merge. The caller stamps these in its local
    /// liveness map (`AppState::observe_peer_contact`) so offline-decay
    /// measures *local observation staleness*, not the peer's own
    /// (possibly clock-skewed) `last_seen`. Empty on a rejected merge.
    pub observed: Vec<NodeId>,
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
                observed: Vec::new(),
            };
        }

        // Mesh-wide encryption policy is monotonic: stricter wins. A peer
        // (stale or hostile, but past the join_key_hash auth boundary
        // above) advertising `require_encryption = false` can never relax
        // a local `true`. Once a node learns the mesh is encrypted, no
        // gossip round demotes it to plaintext — this only ever turns ON.
        if other.require_encryption {
            self.require_encryption = true;
        }
        // Whether to REJECT unsigned dial info outright (WS-D). An
        // encrypted mesh trusts only signed reachability; a plaintext
        // mesh accepts unsigned dial info where there is no already-trusted
        // signed path to protect (legacy LWW). Captured after the
        // monotonic policy OR-in above.
        let enforce_signed = self.require_encryption;

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
                    // First sight of this member: trust its dial info only
                    // if it is validly signed, else clear it (a new member
                    // can't be poisoned with attacker-supplied reachability
                    // on first contact). The member is still added.
                    let mut record = incoming.clone();
                    Self::reconcile_dial_info(&mut record, None, enforce_signed);
                    let active = record.is_active();
                    self.members.insert(*id, record);
                    report.added += 1;
                    // A tombstone we've never seen is still added (so it
                    // converges mesh-wide), but it is not "observed alive".
                    if active {
                        report.observed.push(*id);
                    }
                }
                Some(existing) if incoming.event_time() > existing.event_time() => {
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
                    // The non-security fields (last_seen, status, …) take
                    // the LWW win, but dial info travels only if signed +
                    // fresh; otherwise it is pinned to the value we already
                    // trust. So a forged-newer record advances liveness but
                    // cannot move a peer's reachability (WS-D).
                    Self::reconcile_dial_info(&mut record, Some(existing), enforce_signed);
                    let active = record.is_active();
                    self.members.insert(*id, record);
                    report.updated += 1;
                    if active {
                        report.observed.push(*id);
                    }
                }
                Some(_) => {
                    // Existing is equal or newer — keep ours.
                }
            }
        }
        report
    }

    /// WS-D dial-info reconciliation. `record` already has its
    /// `node_pubkey` preserved by the caller. Trust `record`'s dial info
    /// (relay + addrs) only if it is signed, verifies under that pubkey,
    /// and — versus an `existing` record — is not a version rollback.
    /// Otherwise pin the dial info to `existing`'s already-trusted value,
    /// or clear it on first sight, so a gossip-strip attacker past the
    /// join-key gate cannot move a peer's reachability.
    ///
    /// `enforce_signed` (the mesh's `require_encryption`) rejects ALL
    /// unsigned dial info. A plaintext mesh accepts unsigned dial info
    /// only when there is no already-trusted signed path to protect —
    /// preserving legacy last-writer-wins for an all-unsigned fleet while
    /// a signed path, once learned, can never be downgraded by an
    /// unsigned record.
    fn reconcile_dial_info(
        record: &mut MemberRecord,
        existing: Option<&MemberRecord>,
        enforce_signed: bool,
    ) {
        let signed_ok = match (record.node_pubkey, record.dial_info_sig.as_deref()) {
            (Some(pk), Some(sig)) => {
                let version_ok = match existing {
                    Some(e) => record.dial_info_version >= e.dial_info_version,
                    None => true,
                };
                version_ok
                    && crate::dial_sig::verify_dial_info_hex(
                        &pk,
                        record.dial_info_version,
                        record.relay_url.as_deref(),
                        &record.iroh_direct_addrs,
                        sig,
                    )
            }
            _ => false,
        };
        if signed_ok {
            return;
        }
        let existing_unsigned = match existing {
            Some(e) => e.dial_info_sig.is_none(),
            None => true,
        };
        let accept_unsigned =
            !enforce_signed && record.dial_info_sig.is_none() && existing_unsigned;
        if accept_unsigned {
            return;
        }
        // Reject: pin to the dial info we already trust, or clear on first
        // sight. The member is still added/updated — it just isn't
        // dialable-by-key until a verified record arrives.
        match existing {
            Some(e) => {
                record.relay_url = e.relay_url.clone();
                record.iroh_direct_addrs = e.iroh_direct_addrs.clone();
                record.dial_info_version = e.dial_info_version;
                record.dial_info_sig = e.dial_info_sig.clone();
            }
            None => {
                record.relay_url = None;
                record.iroh_direct_addrs = Vec::new();
                record.dial_info_version = 0;
                record.dial_info_sig = None;
            }
        }
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
            removed_at: None,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
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
                anchor: None,
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
            require_encryption: false,
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

    // ── WS-D: signed dial-info anti-downgrade ─────────────────

    fn signed_member(
        id: NodeId,
        last_seen: u64,
        key: &ed25519_dalek::SigningKey,
        version: u64,
        relay: Option<&str>,
        addrs: &[std::net::SocketAddr],
    ) -> MemberRecord {
        use ed25519_dalek::Signer;
        let pk = NodePubkey(key.verifying_key().to_bytes());
        let mut m = member(id, "signed", last_seen);
        m.node_pubkey = Some(pk);
        m.relay_url = relay.map(|s| s.to_string());
        m.iroh_direct_addrs = addrs.to_vec();
        m.dial_info_version = version;
        let sig = key.sign(&crate::dial_sig::dial_info_message(&pk, version, relay, addrs));
        m.dial_info_sig = Some(hex::encode(sig.to_bytes()));
        m
    }

    #[test]
    fn dial_info_strip_attack_is_rejected_and_pinned() {
        let mesh_id = MeshId::from_u128(11);
        let hash = [4u8; 32];
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let b = NodeId::from_u128(7);
        let addrs: Vec<std::net::SocketAddr> = vec!["10.0.0.5:9742".parse().unwrap()];

        // Local holds B's SIGNED dial info at version 3.
        let trusted = signed_member(b, 100, &key, 3, Some("https://relay.example./"), &addrs);
        let mut local = mesh_with(
            vec![member(NodeId::from_u128(1), "self", 100), trusted],
            mesh_id,
            hash,
        );

        // Attacker (past the join-key gate) publishes a forged-NEWER record
        // (higher last_seen) with the dial info STRIPPED and unsigned.
        let mut stripped = member(b, "signed", 200);
        stripped.node_pubkey = Some(NodePubkey(key.verifying_key().to_bytes()));
        let incoming = mesh_with(vec![stripped], mesh_id, hash);

        local.merge_from(NodeId::from_u128(1), &incoming);
        let merged = local.members.get(&b).unwrap();
        assert_eq!(
            merged.relay_url.as_deref(),
            Some("https://relay.example./"),
            "stripped dial info rejected — pinned to the signed value"
        );
        assert_eq!(merged.iroh_direct_addrs, addrs);
        assert_eq!(merged.dial_info_version, 3);
        assert_eq!(
            merged.last_seen, 200,
            "non-security fields (last_seen) still take the LWW win"
        );
    }

    #[test]
    fn dial_info_substitution_with_foreign_sig_is_rejected() {
        let mesh_id = MeshId::from_u128(12);
        let hash = [4u8; 32];
        let owner = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let attacker = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let b = NodeId::from_u128(7);
        let real: Vec<std::net::SocketAddr> = vec!["10.0.0.5:9742".parse().unwrap()];

        let trusted = signed_member(b, 100, &owner, 3, Some("https://relay.example./"), &real);
        let mut local = mesh_with(
            vec![member(NodeId::from_u128(1), "self", 100), trusted],
            mesh_id,
            hash,
        );

        // Attacker substitutes their OWN addrs, signed with the ATTACKER's
        // key (version bumped) — but B's preserved pubkey won't verify it.
        let evil: Vec<std::net::SocketAddr> = vec!["10.0.0.99:9742".parse().unwrap()];
        let mut sub = signed_member(b, 200, &attacker, 9, Some("https://evil./"), &evil);
        // Carry B's real pubkey so preserved_pubkey resolves to B (the sig
        // is the attacker's, so verification under B's key must fail).
        sub.node_pubkey = Some(NodePubkey(owner.verifying_key().to_bytes()));
        let incoming = mesh_with(vec![sub], mesh_id, hash);

        local.merge_from(NodeId::from_u128(1), &incoming);
        let merged = local.members.get(&b).unwrap();
        assert_eq!(
            merged.iroh_direct_addrs, real,
            "attacker-signed substitution rejected — pinned to B's real addrs"
        );
        assert_eq!(merged.dial_info_version, 3);
    }

    #[test]
    fn replayed_older_signed_dial_info_loses_version_check() {
        let mesh_id = MeshId::from_u128(13);
        let hash = [4u8; 32];
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let b = NodeId::from_u128(7);
        let a5: Vec<std::net::SocketAddr> = vec!["10.0.0.5:9742".parse().unwrap()];

        let v5 = signed_member(b, 100, &key, 5, Some("relay-v5"), &a5);
        let mut local = mesh_with(
            vec![member(NodeId::from_u128(1), "self", 100), v5],
            mesh_id,
            hash,
        );

        // A genuine OLDER signed record (version 2, valid sig) replayed with
        // a forged-newer last_seen must not roll the dial info back.
        let a2: Vec<std::net::SocketAddr> = vec!["10.0.0.2:9742".parse().unwrap()];
        let older = signed_member(b, 999, &key, 2, Some("relay-v2"), &a2);
        let incoming = mesh_with(vec![older], mesh_id, hash);

        local.merge_from(NodeId::from_u128(1), &incoming);
        let merged = local.members.get(&b).unwrap();
        assert_eq!(
            merged.dial_info_version, 5,
            "version rollback rejected — kept version 5"
        );
        assert_eq!(merged.relay_url.as_deref(), Some("relay-v5"));
        assert_eq!(merged.last_seen, 999, "liveness still advances");
    }

    #[test]
    fn encrypted_mesh_rejects_unsigned_dial_info() {
        let mesh_id = MeshId::from_u128(14);
        let hash = [4u8; 32];
        let b = NodeId::from_u128(7);

        let mut local = mesh_with(
            vec![
                member(NodeId::from_u128(1), "self", 100),
                member(b, "b", 100),
            ],
            mesh_id,
            hash,
        );
        local.require_encryption = true; // encrypted mesh enforces signed dial info

        // Newer record with UNSIGNED attacker dial info.
        let mut unsigned = member(b, "b", 200);
        unsigned.relay_url = Some("https://attacker./".into());
        let incoming = mesh_with(vec![unsigned], mesh_id, hash);

        local.merge_from(NodeId::from_u128(1), &incoming);
        let merged = local.members.get(&b).unwrap();
        assert!(
            merged.relay_url.is_none(),
            "encrypted mesh must reject unsigned dial info (cleared), got {:?}",
            merged.relay_url
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
        assert_eq!(report.observed, vec![b], "added member is observed");
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
        assert_eq!(report.observed, vec![b], "LWW-updated member is observed");
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
        assert!(
            report.observed.is_empty(),
            "no advance => nothing observed"
        );
        assert_eq!(local.members.get(&b).unwrap().name, "B-fresh");
    }

    #[test]
    fn tombstone_is_not_resurrected_by_stale_live_record() {
        // The immortal-ghost fix: a tombstoned member must out-compete a stale
        // live copy that a lagging peer still gossips. B was removed at t=50;
        // a peer relays B's old live record (last_seen=20 < 50) → must NOT win.
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let b_tombstone = {
            let mut m = member(b, "B", 10);
            m.removed_at = Some(50);
            m
        };
        let mut local = mesh_with(vec![member(a, "A", 10), b_tombstone], mesh_id, hash);
        let remote = mesh_with(vec![member(b, "B-live-stale", 20)], mesh_id, hash);

        let report = local.merge_from(a, &remote);
        assert_eq!(report.updated, 0, "stale live record must not resurrect");
        assert!(
            report.observed.is_empty(),
            "a non-event must not stamp liveness"
        );
        let merged = local.members.get(&b).unwrap();
        assert_eq!(merged.removed_at, Some(50), "B stays tombstoned");
        assert!(!merged.is_active());
    }

    #[test]
    fn genuine_rejoin_resurrects_a_tombstone() {
        // A live record whose last_seen post-dates the removal IS a real
        // rejoin — event-time LWW lets it win and clear the tombstone.
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let b_tombstone = {
            let mut m = member(b, "B", 10);
            m.removed_at = Some(50);
            m
        };
        let mut local = mesh_with(vec![member(a, "A", 10), b_tombstone], mesh_id, hash);
        let remote = mesh_with(vec![member(b, "B-rejoined", 100)], mesh_id, hash);

        let report = local.merge_from(a, &remote);
        assert_eq!(report.updated, 1, "rejoin (last_seen 100 > removed_at 50) wins");
        let merged = local.members.get(&b).unwrap();
        assert!(merged.is_active(), "rejoin clears the tombstone");
        assert_eq!(merged.last_seen, 100);
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
