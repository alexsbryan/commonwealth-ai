use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use commonwealth_core::capabilities::NodeCapabilities;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::NodeStatus;
use commonwealth_core::Result;

/// A versioned piece of state that propagates via gossip.
/// Timestamp-based conflict resolution: highest timestamp wins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipEntry {
    pub key: GossipKey,
    pub value: GossipValue,
    /// Unix timestamp (seconds). Last-write-wins conflict resolution.
    pub timestamp: u64,
    /// Node that originated this entry.
    pub origin: NodeId,
}

/// Identifies what this gossip entry is about.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum GossipKey {
    /// A node's current status and capabilities.
    MemberState { node_id: NodeId },
    /// Mesh-wide configuration.
    MeshConfig,
    /// A key-value entry stored by a mesh app. Scoped to app_id + key.
    /// Inference plan, knowledge plan, and ledger entries all flow through
    /// this variant (app_id = "inference" or "knowledge").
    AppState { app_id: String, key: String },
    /// An app manifest entry in the mesh app registry.
    AppRegistry { app_id: String },
}

/// The payload of a gossip entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum GossipValue {
    MemberState {
        status: NodeStatus,
        capabilities: Box<NodeCapabilities>,
    },
    MeshConfig {
        config_json: String,
    },
    /// An app's KV store value (JSON-encoded bytes).
    AppState {
        value_json: String,
    },
    /// A serialized MeshAppManifest.
    AppRegistry {
        manifest_json: String,
    },
}

/// A digest entry sent during gossip to determine what needs syncing.
/// Includes only the key and timestamp, not the full value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestEntry {
    pub key: GossipKey,
    pub timestamp: u64,
}

/// Message exchanged between nodes during a gossip round.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum GossipMessage {
    /// Step 1: Initiator sends its digest (keys + timestamps).
    Digest { entries: Vec<DigestEntry> },
    /// Step 2: Responder sends back entries it has that are newer,
    /// plus a request for entries the initiator has that are newer.
    Delta {
        newer_entries: Vec<GossipEntry>,
        request_keys: Vec<GossipKey>,
    },
    /// Step 3: Initiator sends requested entries.
    Response { entries: Vec<GossipEntry> },
}

/// The local gossip state store. Each node maintains this.
#[derive(Debug, Clone)]
pub struct GossipState {
    entries: HashMap<GossipKey, GossipEntry>,
}

impl GossipState {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Insert or update an entry. Returns true if the entry was newer than existing.
    pub fn merge_entry(&mut self, entry: GossipEntry) -> bool {
        match self.entries.get(&entry.key) {
            Some(existing) if existing.timestamp >= entry.timestamp => false,
            _ => {
                self.entries.insert(entry.key.clone(), entry);
                true
            }
        }
    }

    /// Get an entry by key.
    pub fn get(&self, key: &GossipKey) -> Option<&GossipEntry> {
        self.entries.get(key)
    }

    /// Build a digest of all current entries (keys + timestamps only).
    pub fn digest(&self) -> Vec<DigestEntry> {
        self.entries
            .values()
            .map(|e| DigestEntry {
                key: e.key.clone(),
                timestamp: e.timestamp,
            })
            .collect()
    }

    /// Process an incoming digest from a peer. Returns:
    /// - `newer_entries`: entries we have that are newer than the peer's
    /// - `request_keys`: keys the peer has that are newer than ours
    pub fn process_digest(
        &self,
        peer_digest: &[DigestEntry],
    ) -> (Vec<GossipEntry>, Vec<GossipKey>) {
        let mut newer_entries = Vec::new();
        let mut request_keys = Vec::new();

        // Build a map of peer's timestamps.
        let peer_timestamps: HashMap<&GossipKey, u64> =
            peer_digest.iter().map(|d| (&d.key, d.timestamp)).collect();

        // Check our entries against peer's digest.
        for (key, entry) in &self.entries {
            match peer_timestamps.get(key) {
                Some(&peer_ts) if peer_ts < entry.timestamp => {
                    // We have a newer version.
                    newer_entries.push(entry.clone());
                }
                None => {
                    // Peer doesn't have this entry at all.
                    newer_entries.push(entry.clone());
                }
                _ => {} // Peer has same or newer.
            }
        }

        // Check peer's digest for entries we don't have or that are newer.
        for digest_entry in peer_digest {
            match self.entries.get(&digest_entry.key) {
                Some(our_entry) if our_entry.timestamp < digest_entry.timestamp => {
                    request_keys.push(digest_entry.key.clone());
                }
                None => {
                    request_keys.push(digest_entry.key.clone());
                }
                _ => {}
            }
        }

        (newer_entries, request_keys)
    }

    /// Merge multiple entries from a peer. Returns count of entries that were newer.
    pub fn merge_entries(&mut self, entries: Vec<GossipEntry>) -> usize {
        entries
            .into_iter()
            .filter(|e| self.merge_entry(e.clone()))
            .count()
    }

    /// Get all entries (for testing / inspection).
    pub fn all_entries(&self) -> impl Iterator<Item = &GossipEntry> {
        self.entries.values()
    }

    /// Number of entries in the store.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for GossipState {
    fn default() -> Self {
        Self::new()
    }
}

/// Select random peers for a gossip round.
/// Returns up to `count` random node IDs from `peers`, excluding `self_id`.
pub fn select_gossip_peers(self_id: NodeId, peers: &[NodeId], count: usize) -> Vec<NodeId> {
    use rand::seq::SliceRandom;

    let eligible: Vec<NodeId> = peers.iter().copied().filter(|&id| id != self_id).collect();
    if eligible.len() <= count {
        return eligible;
    }

    let mut rng = rand::rng();
    let mut selected = eligible;
    selected.shuffle(&mut rng);
    selected.truncate(count);
    selected
}

/// Run one complete gossip exchange between an initiator and responder.
/// This is the "push-pull" protocol.
///
/// Returns the number of entries updated on each side.
pub fn gossip_exchange(
    initiator: &mut GossipState,
    responder: &mut GossipState,
) -> Result<(usize, usize)> {
    // Step 1: Initiator sends digest.
    let initiator_digest = initiator.digest();

    // Step 2: Responder processes digest, sends delta.
    let (responder_newer, request_keys) = responder.process_digest(&initiator_digest);

    // Step 3: Initiator merges responder's newer entries.
    let initiator_updated = initiator.merge_entries(responder_newer);

    // Step 4: Initiator sends requested entries.
    let requested_entries: Vec<GossipEntry> = request_keys
        .iter()
        .filter_map(|key| initiator.get(key).cloned())
        .collect();

    // Step 5: Responder merges requested entries.
    let responder_updated = responder.merge_entries(requested_entries);

    Ok((initiator_updated, responder_updated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
    use commonwealth_core::ids::NodeId;

    fn make_member_state_entry(node_id: NodeId, timestamp: u64) -> GossipEntry {
        GossipEntry {
            key: GossipKey::MemberState { node_id },
            value: GossipValue::MemberState {
                status: NodeStatus::Online,
                capabilities: Box::new(NodeCapabilities {
                    hardware: HardwareProfile {
                        gpus: vec![],
                        system_ram_gb: 32,
                        cpu_cores: 8,
                        total_storage_gb: 500,
                        free_storage_gb: 200,
                        network_bandwidth_mbps: Some(1000),
                    },
                    available: AvailableResources::default(),
                    active_processes: vec![],
                    hosted_corpora: vec![],
                    reported_at: timestamp,
                    inference_availability: 1.0,
                    inference_capable: false,
                    loaded_models: vec![],

                    embed_model: None,
                    benchmark: None,
                }),
            },
            timestamp,
            origin: node_id,
        }
    }

    #[test]
    fn gossip_state_merge_newer_wins() {
        let mut state = GossipState::new();
        let node = NodeId::from_u128(1);

        let old = make_member_state_entry(node, 100);
        let new = make_member_state_entry(node, 200);

        assert!(state.merge_entry(old));
        assert_eq!(
            state
                .get(&GossipKey::MemberState { node_id: node })
                .unwrap()
                .timestamp,
            100
        );

        assert!(state.merge_entry(new));
        assert_eq!(
            state
                .get(&GossipKey::MemberState { node_id: node })
                .unwrap()
                .timestamp,
            200
        );
    }

    #[test]
    fn gossip_state_merge_older_rejected() {
        let mut state = GossipState::new();
        let node = NodeId::from_u128(1);

        let new = make_member_state_entry(node, 200);
        let old = make_member_state_entry(node, 100);

        assert!(state.merge_entry(new));
        assert!(!state.merge_entry(old));
        assert_eq!(
            state
                .get(&GossipKey::MemberState { node_id: node })
                .unwrap()
                .timestamp,
            200
        );
    }

    #[test]
    fn gossip_state_digest() {
        let mut state = GossipState::new();
        let a = NodeId::from_u128(1);
        let b = NodeId::from_u128(2);
        state.merge_entry(make_member_state_entry(a, 100));
        state.merge_entry(make_member_state_entry(b, 200));

        let digest = state.digest();
        assert_eq!(digest.len(), 2);
    }

    #[test]
    fn gossip_exchange_syncs_state() {
        let mut node_a = GossipState::new();
        let mut node_b = GossipState::new();

        let id1 = NodeId::from_u128(1);
        let id2 = NodeId::from_u128(2);
        let id3 = NodeId::from_u128(3);

        // Node A knows about 1 and 2.
        node_a.merge_entry(make_member_state_entry(id1, 100));
        node_a.merge_entry(make_member_state_entry(id2, 200));

        // Node B knows about 2 (newer) and 3.
        node_b.merge_entry(make_member_state_entry(id2, 300));
        node_b.merge_entry(make_member_state_entry(id3, 100));

        let (a_updated, b_updated) = gossip_exchange(&mut node_a, &mut node_b).unwrap();

        // A should have gotten: id2 updated to 300, id3 added.
        assert_eq!(a_updated, 2);
        assert_eq!(
            node_a
                .get(&GossipKey::MemberState { node_id: id2 })
                .unwrap()
                .timestamp,
            300
        );
        assert!(node_a
            .get(&GossipKey::MemberState { node_id: id3 })
            .is_some());

        // B should have gotten: id1 added.
        assert_eq!(b_updated, 1);
        assert!(node_b
            .get(&GossipKey::MemberState { node_id: id1 })
            .is_some());

        // Both now have 3 entries.
        assert_eq!(node_a.len(), 3);
        assert_eq!(node_b.len(), 3);
    }

    #[test]
    fn gossip_exchange_already_synced() {
        let mut node_a = GossipState::new();
        let mut node_b = GossipState::new();

        let id = NodeId::from_u128(1);
        let entry = make_member_state_entry(id, 100);
        node_a.merge_entry(entry.clone());
        node_b.merge_entry(entry);

        let (a_updated, b_updated) = gossip_exchange(&mut node_a, &mut node_b).unwrap();
        assert_eq!(a_updated, 0);
        assert_eq!(b_updated, 0);
    }

    #[test]
    fn select_gossip_peers_excludes_self() {
        let self_id = NodeId::from_u128(1);
        let peers = vec![
            NodeId::from_u128(1),
            NodeId::from_u128(2),
            NodeId::from_u128(3),
            NodeId::from_u128(4),
        ];
        let selected = select_gossip_peers(self_id, &peers, 2);
        assert_eq!(selected.len(), 2);
        assert!(!selected.contains(&self_id));
    }

    #[test]
    fn select_gossip_peers_returns_all_if_fewer_than_count() {
        let self_id = NodeId::from_u128(1);
        let peers = vec![NodeId::from_u128(2), NodeId::from_u128(3)];
        let selected = select_gossip_peers(self_id, &peers, 5);
        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn gossip_convergence_five_nodes() {
        // Simulate gossip rounds among 5 nodes until convergence.
        let node_ids: Vec<NodeId> = (1..=5).map(NodeId::from_u128).collect();
        let mut states: Vec<GossipState> = (0..5).map(|_| GossipState::new()).collect();

        // Each node knows only its own state initially.
        for (i, &id) in node_ids.iter().enumerate() {
            states[i].merge_entry(make_member_state_entry(id, 100 + i as u64));
        }

        // Run gossip rounds. Each round, each node talks to 2 random peers.
        for _round in 0..10 {
            for i in 0..5 {
                let peers = select_gossip_peers(node_ids[i], &node_ids, 2);
                for &peer in &peers {
                    let peer_idx = node_ids.iter().position(|&id| id == peer).unwrap();
                    // Clone one side to avoid double-mutable-borrow.
                    let mut initiator_clone = states[i].clone();
                    gossip_exchange(&mut initiator_clone, &mut states[peer_idx]).unwrap();
                    states[i] = initiator_clone;
                }
            }
        }

        // All nodes should have all 5 entries.
        for (i, state) in states.iter().enumerate() {
            assert_eq!(
                state.len(),
                5,
                "node {i} has {} entries, expected 5",
                state.len()
            );
        }
    }

    #[test]
    fn gossip_entry_serde_roundtrip() {
        let entry = make_member_state_entry(NodeId::from_u128(1), 100);
        let json = serde_json::to_string(&entry).unwrap();
        let back: GossipEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.timestamp, 100);
        assert_eq!(back.key, entry.key);
    }

    #[test]
    fn gossip_message_serde_roundtrip() {
        let msg = GossipMessage::Digest {
            entries: vec![DigestEntry {
                key: GossipKey::MemberState {
                    node_id: NodeId::from_u128(1),
                },
                timestamp: 100,
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: GossipMessage = serde_json::from_str(&json).unwrap();
        if let GossipMessage::Digest { entries } = back {
            assert_eq!(entries.len(), 1);
        } else {
            panic!("expected Digest");
        }
    }
}
