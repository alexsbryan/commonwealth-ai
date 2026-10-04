use super::*;
use commonwealth_core::mesh::MemberRecord;
use std::collections::HashMap;

mod select_tests;

fn member(id: u128, name: &str, status: NodeStatus, keyed: bool) -> MemberRecord {
    MemberRecord {
        node_id: NodeId::from_u128(id),
        name: name.into(),
        invited_by: NodeId::from_u128(1),
        joined_at: 0,
        last_seen: 0,
        status,
        capabilities: minimal_capabilities(0, &[], None),
        addresses: Vec::new(),
        node_pubkey: keyed.then(|| commonwealth_core::ids::NodePubkey([id as u8; 32])),
        relay_url: None,
        iroh_direct_addrs: Vec::new(),
        dial_info_version: 0,
        dial_info_sig: None,
        removed_at: None,
    }
}

fn mesh_of(records: Vec<MemberRecord>) -> Mesh {
    let (mut mesh, _k) =
        commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
    mesh.members.clear();
    for r in records {
        mesh.members.insert(r.node_id, r);
    }
    mesh
}

const ME: u128 = 0xA11CE;

fn key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[3u8; 32])
}

/// **The failing input for the self-stamp.** A version that moves every
/// round is not an anti-rollback key, it is a clock — and a peer holding
/// our record would re-verify a signature on every heartbeat forever. The
/// bump happens on a CONTENT change and once to acquire a first
/// signature, and not otherwise.
#[test]
fn the_dial_version_bumps_on_a_change_and_stands_still_otherwise() {
    assert_eq!(
        next_dial_version(0, false, false),
        Some(1),
        "first signature"
    );
    assert_eq!(next_dial_version(4, true, false), None, "no change, signed");
    assert_eq!(next_dial_version(4, true, true), Some(5), "relay moved");
    assert_eq!(next_dial_version(0, false, true), Some(1));
}

/// The same property through the real stamp: two rounds with identical
/// reachability sign once.
#[test]
fn two_rounds_with_the_same_reachability_sign_once() {
    let mut mesh = mesh_of(vec![member(ME, "me", NodeStatus::Offline, false)]);
    let dial = DialInfo {
        relay_url: Some("https://relay.example/".into()),
        direct_addrs: vec!["192.168.1.8:41231".parse().unwrap()],
    };
    self_stamp(
        &mut mesh,
        NodeId::from_u128(ME),
        100,
        &dial,
        &[],
        &key(),
        None,
    );
    let after_first = mesh.members[&NodeId::from_u128(ME)].clone();
    assert_eq!(after_first.dial_info_version, 1);
    assert!(after_first.dial_info_sig.is_some());
    assert_eq!(after_first.status, NodeStatus::Online);

    self_stamp(
        &mut mesh,
        NodeId::from_u128(ME),
        110,
        &dial,
        &[],
        &key(),
        None,
    );
    let after_second = &mesh.members[&NodeId::from_u128(ME)];
    assert_eq!(after_second.dial_info_version, 1, "no content change");
    assert_eq!(after_second.dial_info_sig, after_first.dial_info_sig);
    assert_eq!(after_second.last_seen, 110, "the heartbeat still advances");

    let moved = DialInfo {
        relay_url: Some("https://other.example/".into()),
        ..dial.clone()
    };
    self_stamp(
        &mut mesh,
        NodeId::from_u128(ME),
        120,
        &moved,
        &[],
        &key(),
        None,
    );
    assert_eq!(mesh.members[&NodeId::from_u128(ME)].dial_info_version, 2);
}

/// **The failing input for the event time.** A departure leaves our row
/// a second ahead of the wall clock; the stamp that follows it in the same
/// second must land after it, never at `now` beneath it, or every peer
/// holding the departure keeps it and the next departure is lost.
#[test]
fn a_stamp_never_moves_our_row_back_in_time() {
    let mut ahead = member(ME, "me", NodeStatus::Offline, false);
    ahead.last_seen = 200;
    let mut mesh = mesh_of(vec![ahead]);
    let dial = DialInfo {
        relay_url: None,
        direct_addrs: vec!["127.0.0.1:41231".parse().unwrap()],
    };
    self_stamp(
        &mut mesh,
        NodeId::from_u128(ME),
        150,
        &dial,
        &[],
        &key(),
        None,
    );
    let me = &mesh.members[&NodeId::from_u128(ME)];
    assert_eq!(me.status, NodeStatus::Online);
    assert_eq!(
        me.event_time(),
        201,
        "strictly after the departure it follows"
    );
}

/// The catalogue's input. A configured origin is what puts `Media` on the
/// wire; without one the field is empty, which reads to every peer as
/// "advertises none" — absence reported, never defaulted to an offer.
#[test]
fn a_configured_origin_is_what_stamps_the_media_kind() {
    let mut mesh = mesh_of(vec![member(ME, "me", NodeStatus::Online, false)]);
    let dial = DialInfo {
        relay_url: None,
        direct_addrs: Vec::new(),
    };
    self_stamp(
        &mut mesh,
        NodeId::from_u128(ME),
        1,
        &dial,
        &[],
        &key(),
        None,
    );
    assert!(mesh.members[&NodeId::from_u128(ME)]
        .capabilities
        .origins
        .is_empty());
    self_stamp(
        &mut mesh,
        NodeId::from_u128(ME),
        2,
        &dial,
        &[OriginKind::Media],
        &key(),
        None,
    );
    assert_eq!(
        mesh.members[&NodeId::from_u128(ME)].capabilities.origins,
        vec![OriginKind::Media]
    );
}

/// **The failing input for decay.** Staleness is our clock against the
/// contact map. A peer we have never contacted has no entry, and decaying
/// it would mark a member Offline the round after we learned of it —
/// which is how a freshly-joined member disappears from `mesh media`
/// before it has ever been dialed.
#[test]
fn a_peer_never_contacted_is_not_decayed_and_a_stale_one_is() {
    let mut mesh = mesh_of(vec![
        member(ME, "me", NodeStatus::Online, false),
        member(0xB0B, "LittleMac", NodeStatus::Online, true),
        member(0xC0DE, "Quiet", NodeStatus::Online, true),
    ]);
    let mut contacts = HashMap::new();
    contacts.insert(NodeId::from_u128(0xB0B), 100u64);
    decay(&mut mesh, NodeId::from_u128(ME), 1000, 60, &contacts);
    assert_eq!(
        mesh.members[&NodeId::from_u128(0xB0B)].status,
        NodeStatus::Offline,
        "900s of silence against a 60s threshold"
    );
    assert_eq!(
        mesh.members[&NodeId::from_u128(0xC0DE)].status,
        NodeStatus::Online,
        "never contacted is not the same fact as gone"
    );
    assert_eq!(
        mesh.members[&NodeId::from_u128(ME)].status,
        NodeStatus::Online,
        "a node never decays itself"
    );
}

/// Online first, keyed only, self never — and the rotation reaches every
/// member of a mesh bigger than the fan.
#[test]
fn the_peer_pick_is_online_first_keyed_and_rotates() {
    let mesh = mesh_of(vec![
        member(ME, "me", NodeStatus::Online, true),
        member(0x01, "a", NodeStatus::Online, true),
        member(0x02, "b", NodeStatus::Offline, true),
        member(0x03, "c", NodeStatus::Online, true),
        member(0x04, "d", NodeStatus::Online, false),
        member(0x05, "e", NodeStatus::Online, true),
    ]);
    let me = NodeId::from_u128(ME);
    let first = select_peers(&mesh, me, 0);
    assert_eq!(first.len(), PEERS_PER_ROUND);
    let names: Vec<&str> = first.iter().map(|(_, n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["a", "c", "e"], "online first, keyed only");
    assert!(!first.iter().any(|(id, _, _)| *id == me));
    assert!(
        !names.contains(&"d"),
        "a member with no key has nothing to dial by"
    );

    // Over four rounds every candidate — the offline one included — is
    // dialed at least once, which is what keeps a bigger mesh converging.
    let mut seen: Vec<String> = Vec::new();
    for round in 0..4 {
        for (_, name, _) in select_peers(&mesh, me, round) {
            if !seen.contains(&name) {
                seen.push(name);
            }
        }
    }
    seen.sort();
    assert_eq!(seen, vec!["a", "b", "c", "e"]);
}

#[test]
fn a_mesh_of_one_has_nobody_to_dial() {
    let mesh = mesh_of(vec![member(ME, "me", NodeStatus::Online, true)]);
    assert!(select_peers(&mesh, NodeId::from_u128(ME), 0).is_empty());
}
