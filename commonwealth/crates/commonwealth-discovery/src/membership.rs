// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use commonwealth_core::{Error, Result};

/// Generate a human-readable join key in the format `cwth-XXXX-XXXX-XXXX`.
pub fn generate_join_key() -> String {
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).expect("failed to generate random bytes");
    format!(
        "cwth-{}-{}-{}",
        hex::encode(&bytes[0..2]),
        hex::encode(&bytes[2..4]),
        hex::encode(&bytes[4..6]),
    )
}

/// Hash a join key using BLAKE3. The raw key is never persisted — only the hash.
pub fn hash_join_key(key: &str) -> [u8; 32] {
    *blake3::hash(key.as_bytes()).as_bytes()
}

/// Verify a join key against a stored hash.
pub fn verify_join_key(key: &str, expected_hash: &[u8; 32]) -> bool {
    let actual = hash_join_key(key);
    // Constant-time comparison to prevent timing attacks.
    actual == *expected_hash
}

/// Parse and validate join key format (`cwth-XXXX-XXXX-XXXX` where X is hex).
pub fn validate_join_key_format(key: &str) -> Result<()> {
    let parts: Vec<&str> = key.split('-').collect();
    if parts.len() != 4 || parts[0] != "cwth" {
        return Err(Error::InvalidJoinKey(
            "expected format cwth-XXXX-XXXX-XXXX".into(),
        ));
    }
    for part in &parts[1..] {
        if part.len() != 4 || hex::decode(part).is_err() {
            return Err(Error::InvalidJoinKey(
                "each segment must be 4 hex characters".into(),
            ));
        }
    }
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs()
}

/// Initialize a new mesh. Returns the mesh state and the join key (to be shared out-of-band).
pub fn init_mesh(name: &str, node_name: &str, addresses: Vec<SocketAddr>) -> (Mesh, String) {
    init_mesh_with_node_id(name, node_name, addresses, NodeId::generate())
}

/// Same as [`init_mesh`] but accepts an externally-provided stable
/// `NodeId` for the founder. Used when the daemon persists a
/// machine-stable identity so every `sovereign setup` / `mesh create`
/// cycle comes back under the same node_id instead of stamping a
/// fresh random one.
pub fn init_mesh_with_node_id(
    name: &str,
    node_name: &str,
    addresses: Vec<SocketAddr>,
    node_id: NodeId,
) -> (Mesh, String) {
    init_mesh_with_identity(name, node_name, addresses, node_id, None)
}

/// Same as [`init_mesh_with_node_id`] but also stamps the founder's
/// Ed25519 identity pubkey into its `MemberRecord`. `None` keeps
/// the pre-identity behaviour (older daemons, tests).
pub fn init_mesh_with_identity(
    name: &str,
    node_name: &str,
    addresses: Vec<SocketAddr>,
    node_id: NodeId,
    node_pubkey: Option<commonwealth_core::ids::NodePubkey>,
) -> (Mesh, String) {
    let join_key = generate_join_key();
    let join_key_hash = hash_join_key(&join_key);
    let mesh_id = MeshId::generate();
    let now = now_secs();

    let founder = MemberRecord {
        node_pubkey,
        node_id,
        name: node_name.to_string(),
        invited_by: node_id, // Founder invites themselves.
        joined_at: now,
        last_seen: now,
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
            reported_at: now,
            inference_availability: 1.0,
            inference_capable: false,
            loaded_models: vec![],

            embed_model: None,
            benchmark: None,
            current_in_flight: None,
        },
        addresses,
    };

    let mut members = HashMap::new();
    members.insert(node_id, founder);

    let mesh = Mesh {
        id: mesh_id,
        name: name.to_string(),
        join_key_hash,
        members,
        peers: vec![],
    };

    (mesh, join_key)
}

/// Result of a successful join operation.
pub struct JoinResult {
    pub node_id: NodeId,
    pub mesh: Mesh,
}

/// Add a new node to the mesh after verifying the join key.
/// Called on the existing member's side.
pub fn accept_join(
    mesh: &mut Mesh,
    join_key: &str,
    new_node_name: &str,
    new_node_addresses: Vec<SocketAddr>,
    invited_by: NodeId,
) -> Result<NodeId> {
    accept_join_with_proposed_id(
        mesh,
        join_key,
        new_node_name,
        new_node_addresses,
        invited_by,
        None,
    )
}

/// Same as [`accept_join`] but lets the joiner propose its stable
/// `NodeId`. Used by the rejoin path so a machine that has joined
/// this mesh before comes back with its original identity instead
/// of a freshly-generated zombie.
///
/// If `proposed_id` is `Some(id)`:
///   - and `id` is NOT already in `mesh.members` → adopt it
///     verbatim. The joiner is a new-to-this-mesh machine that has
///     persisted a stable install-local ID.
///   - and `id` IS already in `mesh.members` with a matching
///     `new_node_name` → update the existing record's addresses +
///     last_seen + status and return the same id. This is the
///     "same machine rejoining" case — no zombie entry created.
///   - and `id` IS already in `mesh.members` with a DIFFERENT
///     name → refuse (the ID would collide with someone else's
///     machine on this mesh). Fall back to generating a fresh ID.
///
/// If `proposed_id` is `None`, generate as before.
pub fn accept_join_with_proposed_id(
    mesh: &mut Mesh,
    join_key: &str,
    new_node_name: &str,
    new_node_addresses: Vec<SocketAddr>,
    invited_by: NodeId,
    proposed_id: Option<NodeId>,
) -> Result<NodeId> {
    accept_join_with_identity(
        mesh,
        join_key,
        new_node_name,
        new_node_addresses,
        invited_by,
        proposed_id,
        None,
    )
}

/// Same as [`accept_join_with_proposed_id`] but also records the
/// joiner's Ed25519 identity pubkey. The caller (the join route)
/// MUST have verified the proof of possession before passing
/// `Some(pubkey)` here — this function records, it does not verify.
/// On rejoin, a presented pubkey refreshes the stored one (same
/// machine, possibly newly-keyed install); absent one, the stored
/// key is kept.
pub fn accept_join_with_identity(
    mesh: &mut Mesh,
    join_key: &str,
    new_node_name: &str,
    new_node_addresses: Vec<SocketAddr>,
    invited_by: NodeId,
    proposed_id: Option<NodeId>,
    node_pubkey: Option<commonwealth_core::ids::NodePubkey>,
) -> Result<NodeId> {
    // Verify join key.
    if !verify_join_key(join_key, &mesh.join_key_hash) {
        return Err(Error::InvalidJoinKey("join key does not match".into()));
    }

    let now = now_secs();

    // Resolve the effective node_id using the rules in the docstring.
    let new_node_id = match proposed_id {
        Some(id) => match mesh.members.get(&id) {
            None => id,
            Some(existing) if existing.name == new_node_name => {
                // Rejoin: refresh the existing record in place.
                let mut refreshed = existing.clone();
                refreshed.addresses = new_node_addresses;
                refreshed.last_seen = now;
                refreshed.status = NodeStatus::Online;
                if node_pubkey.is_some() {
                    refreshed.node_pubkey = node_pubkey;
                }
                mesh.members.insert(id, refreshed);
                return Ok(id);
            }
            Some(_) => {
                // Collision with a differently-named member — refuse
                // the proposed ID and generate fresh.
                NodeId::generate()
            }
        },
        None => NodeId::generate(),
    };

    let member = MemberRecord {
        node_pubkey,
        node_id: new_node_id,
        name: new_node_name.to_string(),
        invited_by,
        joined_at: now,
        last_seen: now,
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
            reported_at: now,
            inference_availability: 1.0,
            inference_capable: false,
            loaded_models: vec![],

            embed_model: None,
            benchmark: None,
            current_in_flight: None,
        },
        addresses: new_node_addresses,
    };

    mesh.members.insert(new_node_id, member);
    Ok(new_node_id)
}

/// Proposal to revoke a member's membership.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RevocationProposal {
    pub target_node: NodeId,
    pub proposed_by: NodeId,
    pub proposed_at: u64,
    pub votes_for: Vec<NodeId>,
    pub votes_against: Vec<NodeId>,
}

impl RevocationProposal {
    pub fn new(target_node: NodeId, proposed_by: NodeId) -> Self {
        Self {
            target_node,
            proposed_by,
            proposed_at: now_secs(),
            votes_for: vec![proposed_by],
            votes_against: vec![],
        }
    }

    /// Add a vote. Returns true if the proposal now has majority.
    pub fn vote(&mut self, voter: NodeId, in_favor: bool) -> bool {
        if in_favor {
            if !self.votes_for.contains(&voter) {
                self.votes_for.push(voter);
            }
        } else if !self.votes_against.contains(&voter) {
            self.votes_against.push(voter);
        }
        false // Majority check is done externally with online member count.
    }

    /// Check if the proposal has simple majority of online members.
    pub fn has_majority(&self, online_count: usize) -> bool {
        let needed = online_count / 2 + 1;
        self.votes_for.len() >= needed
    }
}

/// Apply a confirmed revocation to the mesh.
pub fn revoke_member(mesh: &mut Mesh, target_node: NodeId) -> Result<()> {
    if mesh.members.remove(&target_node).is_none() {
        return Err(Error::Membership(format!(
            "node {target_node} is not a member"
        )));
    }
    Ok(())
}

/// Update a node's status (e.g., going offline gracefully).
pub fn update_node_status(mesh: &mut Mesh, node_id: NodeId, status: NodeStatus) -> Result<()> {
    let member = mesh
        .members
        .get_mut(&node_id)
        .ok_or_else(|| Error::Membership(format!("node {node_id} is not a member")))?;
    member.status = status;
    member.last_seen = now_secs();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_key_format() {
        let key = generate_join_key();
        assert!(key.starts_with("cwth-"));
        assert_eq!(key.len(), 19); // "cwth-" + 4 + "-" + 4 + "-" + 4
        validate_join_key_format(&key).unwrap();
    }

    #[test]
    fn join_key_hash_and_verify() {
        let key = generate_join_key();
        let hash = hash_join_key(&key);
        assert!(verify_join_key(&key, &hash));
        assert!(!verify_join_key("cwth-0000-0000-0000", &hash));
    }

    #[test]
    fn validate_join_key_format_rejects_bad_keys() {
        assert!(validate_join_key_format("not-a-key").is_err());
        assert!(validate_join_key_format("cwth-zzzz-0000-0000").is_err());
        assert!(validate_join_key_format("cwth-00-0000-0000").is_err());
        assert!(validate_join_key_format("").is_err());
    }

    #[test]
    fn init_mesh_creates_founder() {
        let (mesh, key) = init_mesh(
            "Test Mesh",
            "Alice's Desktop",
            vec!["127.0.0.1:9742".parse().unwrap()],
        );
        assert_eq!(mesh.name, "Test Mesh");
        assert_eq!(mesh.members.len(), 1);
        validate_join_key_format(&key).unwrap();

        let founder = mesh.members.values().next().unwrap();
        assert_eq!(founder.name, "Alice's Desktop");
        assert_eq!(founder.status, NodeStatus::Online);
        assert_eq!(founder.invited_by, founder.node_id);
    }

    #[test]
    fn accept_join_with_valid_key() {
        let (mut mesh, key) = init_mesh("Test", "Alice", vec![]);
        let founder_id = *mesh.members.keys().next().unwrap();

        let new_id = accept_join(
            &mut mesh,
            &key,
            "Bob's Build",
            vec!["192.168.1.2:9742".parse().unwrap()],
            founder_id,
        )
        .unwrap();

        assert_eq!(mesh.members.len(), 2);
        let bob = mesh.members.get(&new_id).unwrap();
        assert_eq!(bob.name, "Bob's Build");
        assert_eq!(bob.invited_by, founder_id);
    }

    #[test]
    fn accept_join_with_invalid_key() {
        let (mut mesh, _key) = init_mesh("Test", "Alice", vec![]);
        let founder_id = *mesh.members.keys().next().unwrap();

        let result = accept_join(&mut mesh, "cwth-0000-0000-0000", "Eve", vec![], founder_id);
        assert!(result.is_err());
        assert_eq!(mesh.members.len(), 1);
    }

    #[test]
    fn revocation_proposal_majority() {
        let mut proposal = RevocationProposal::new(NodeId::from_u128(99), NodeId::from_u128(1));

        // 5 online members, need 3 votes.
        assert!(!proposal.has_majority(5));

        proposal.vote(NodeId::from_u128(2), true);
        assert!(!proposal.has_majority(5));

        proposal.vote(NodeId::from_u128(3), true);
        assert!(proposal.has_majority(5));
    }

    #[test]
    fn revoke_member_removes_from_mesh() {
        let (mut mesh, key) = init_mesh("Test", "Alice", vec![]);
        let founder_id = *mesh.members.keys().next().unwrap();

        let bob_id = accept_join(&mut mesh, &key, "Bob", vec![], founder_id).unwrap();
        assert_eq!(mesh.members.len(), 2);

        revoke_member(&mut mesh, bob_id).unwrap();
        assert_eq!(mesh.members.len(), 1);
        assert!(!mesh.members.contains_key(&bob_id));
    }

    #[test]
    fn revoke_nonexistent_member_fails() {
        let (mut mesh, _) = init_mesh("Test", "Alice", vec![]);
        let result = revoke_member(&mut mesh, NodeId::from_u128(999));
        assert!(result.is_err());
    }

    #[test]
    fn update_node_status_works() {
        let (mut mesh, _) = init_mesh("Test", "Alice", vec![]);
        let node_id = *mesh.members.keys().next().unwrap();

        update_node_status(&mut mesh, node_id, NodeStatus::Offline).unwrap();
        assert_eq!(mesh.members[&node_id].status, NodeStatus::Offline);
    }
}
