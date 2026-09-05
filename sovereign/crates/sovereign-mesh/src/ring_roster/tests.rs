// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;

use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::ids::{MeshId, NodeId, NodePubkey};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use commonwealth_rail::{Person, RingSigner, SigningKey};

use super::MeshRoster;

pub(crate) fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

pub(crate) fn pubkey_of(k: &SigningKey) -> NodePubkey {
    commonwealth_transport::identity::node_pubkey(k)
}

pub(crate) fn member(node_id: NodeId, name: &str, pubkey: Option<NodePubkey>) -> MemberRecord {
    MemberRecord {
        node_id,
        name: name.to_string(),
        invited_by: node_id,
        joined_at: 100,
        last_seen: 100,
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
            reported_at: 100,
            inference_availability: 1.0,
            inference_capable: false,
            loaded_models: vec![],
            embed_model: None,
            benchmark: None,
            current_in_flight: None,
            anchor: None,
        },
        addresses: vec![],
        node_pubkey: pubkey,
        relay_url: None,
        iroh_direct_addrs: Vec::new(),
        dial_info_version: 0,
        dial_info_sig: None,
        removed_at: None,
    }
}

pub(crate) fn mesh_of(members: Vec<MemberRecord>) -> Mesh {
    Mesh {
        id: MeshId::generate(),
        name: "test".into(),
        mesh_secret: [0u8; 32],
        invite_key_hash: [0u8; 32],
        invite_expires_at: None,
        invite_version: 0,
        require_encryption: false,
        members: members
            .into_iter()
            .map(|m| (m.node_id, m))
            .collect::<HashMap<_, _>>(),
        peers: vec![],
    }
}

/// **The bridge, in one line.** The mesh advertises an identity as a
/// `NodePubkey`; the rail names a signer by `Op::actor`. This module maps one
/// to the other with no lookup table, and it may do that only because both
/// are `hex(verifying_key)` over the SAME key — the daemon derives its
/// `node_pubkey` and constructs `RingRail` from one
/// `load_or_generate_node_key`.
///
/// If either side ever changes its spelling this fails; if the two crates
/// ever reach different `ed25519-dalek` majors it stops compiling, which is
/// the better failure of the two.
#[test]
fn a_member_pubkey_and_a_rail_actor_are_the_same_spelling() {
    let k = key(7);
    assert_eq!(pubkey_of(&k).to_string(), RingSigner::actor(&k));
    assert_eq!(pubkey_of(&k).to_string().len(), 64);
}

/// **No placeholder, ever.** A member on a pre-identity build advertises no
/// key, and the tempting fix — a shared default so the row still lands — is
/// the one that must not be taken: it collides every unidentified node into a
/// single identity and admits their ops as one another's. Absent is absent
/// (ARCH §18.3); the report is the gap admission emits.
#[test]
fn a_member_with_no_identity_key_is_not_in_the_roster() {
    let known = key(1);
    let a = NodeId::from_u128(1);
    let b = NodeId::from_u128(2);
    let r = MeshRoster::derive(
        &mesh_of(vec![
            member(a, "halo", Some(pubkey_of(&known))),
            member(b, "beefy", None),
        ]),
        NodeId::from_u128(99),
        None,
    );
    assert_eq!(r.len(), 1, "only the identified member is claimed");
    assert!(r.roster().knows(&Person::from("halo")));
    assert!(
        !r.roster().knows(&Person::from("beefy")),
        "a member with no key must not be in the ring under any placeholder"
    );
    assert_eq!(r.node_id_of(&RingSigner::actor(&known)), Some(a));
}

/// **A tombstone retires a member, not their journal.** `removed_at` stops a
/// node being dialled and stops its gossip counting. Dropping it from the
/// roster too would turn every line it ever signed into an `UnknownSigner`
/// gap on the day it left — permanently, in an append-only log. The rail is
/// deliberately author-blind about replication (`ops_missing_from`); a roster
/// that forgot would undo that on the read side.
#[test]
fn a_departed_members_keys_stay_in_the_roster() {
    let gone = key(3);
    let id = NodeId::from_u128(5);
    let mut row = member(id, "cy", Some(pubkey_of(&gone)));
    row.removed_at = Some(1_700_000_000);
    row.status = NodeStatus::Offline;
    let r = MeshRoster::derive(&mesh_of(vec![row]), NodeId::from_u128(99), None);
    assert_eq!(
        r.roster().person_for(&RingSigner::actor(&gone)),
        Some(&Person::from("cy")),
        "a housemate who leaves does not take their half of the journal with them"
    );
}

/// **Our own key comes from the installed identity, not from the gossip
/// stamp.** The self row's `node_pubkey` is written by the gossip loop, so a
/// daemon that has just booted has a key and no stamp. Reading the row would
/// make this node unable to author on its own journal for the first round of
/// every boot — and the refusal it would get ("nobody in the roster claims
/// that key") describes something else entirely.
#[test]
fn our_own_key_comes_from_the_installed_identity_not_the_gossip_stamp() {
    let me = key(9);
    let id = NodeId::from_u128(42);
    let r = MeshRoster::derive(
        &mesh_of(vec![member(id, "halo", None)]),
        id,
        Some(pubkey_of(&me)),
    );
    assert_eq!(
        r.roster().person_for(&RingSigner::actor(&me)),
        Some(&Person::from("halo")),
        "a node must be able to sign on its own journal before its first gossip round"
    );
}

/// One person with two laptops is two keys in one roster row — the shape
/// `Roster` documents, arrived at here because the mesh names both machines
/// the same thing. Sorted, so the roster is a function of the membership set
/// rather than of hash-map iteration order.
#[test]
fn two_nodes_under_one_name_are_two_keys_in_one_row() {
    let one = key(11);
    let two = key(12);
    let r = MeshRoster::derive(
        &mesh_of(vec![
            member(NodeId::from_u128(1), "alex", Some(pubkey_of(&one))),
            member(NodeId::from_u128(2), "alex", Some(pubkey_of(&two))),
        ]),
        NodeId::from_u128(99),
        None,
    );
    let keys = r
        .roster()
        .members
        .get(&Person::from("alex"))
        .expect("one row");
    assert_eq!(keys.len(), 2);
    let mut expected = vec![RingSigner::actor(&one), RingSigner::actor(&two)];
    expected.sort();
    assert_eq!(keys, &expected);
    assert_eq!(r.len(), 2, "two keys, and each resolves to its own node");
}

/// A blank name is a cosmetic problem; a missing key is a permanent gap.
/// Trading the first for the second would be the wrong way round, so the
/// node id stands in as the display name and the key stays in the ring.
#[test]
fn a_blank_name_falls_back_to_the_node_id_rather_than_dropping_the_key() {
    let k = key(13);
    let id = NodeId::from_u128(0xabc);
    let r = MeshRoster::derive(
        &mesh_of(vec![member(id, "   ", Some(pubkey_of(&k)))]),
        NodeId::from_u128(99),
        None,
    );
    assert_eq!(
        r.roster().person_for(&RingSigner::actor(&k)),
        Some(&Person::from(id.to_string()))
    );
}
