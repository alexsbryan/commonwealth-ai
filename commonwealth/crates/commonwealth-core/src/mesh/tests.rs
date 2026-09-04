// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unit tests for [`super`] — the mesh value, its merge algebra, and the
//! gossip authorization boundary.
//!
//! Split out of `mesh.rs` (ARCH §3.2) when that file crossed 2,000 lines and
//! two thirds of it was this module. A child `tests` module of `mesh` rather
//! than a `#[path]`-attached sibling: the conformance-tag scanner derives a
//! claim's JUnit key from the FILE path, so `mesh_tests.rs` would emit
//! `mesh_tests::…` for a test nextest reports as `mesh::tests::…` — a key that
//! matches nothing, which reads exactly like a test that never ran.

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

pub(crate) fn member(id: NodeId, name: &str, last_seen: u64) -> MemberRecord {
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

pub(crate) fn mesh_with(members: Vec<MemberRecord>, id: MeshId, hash: [u8; 32]) -> Mesh {
    let mut map = HashMap::new();
    for m in members {
        map.insert(m.node_id, m);
    }
    Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id,
        name: "test".into(),
        invite_key_hash: hash,
        invite_version: 0,
        require_encryption: false,
        members: map,
        peers: vec![],
    }
}

#[test]
fn is_dialable_accepts_ip_or_iroh_paths() {
    let node = NodeId::from_u128(7);

    // No IP, no iroh → not dialable (the record the filters drop).
    let bare = member(node, "bare", 1);
    assert!(!bare.is_dialable());

    // IP path.
    let mut ip = member(node, "ip", 1);
    ip.addresses = vec!["10.0.0.5:9742".parse().unwrap()];
    assert!(ip.is_dialable());

    // A pubkey ALONE is not a path — need a relay or a direct addr.
    let mut key_only = member(node, "key", 1);
    key_only.node_pubkey = Some(NodePubkey([9u8; 32]));
    assert!(!key_only.is_dialable());

    // pubkey + relay (the off-LAN no-VPN case).
    let mut relayed = key_only.clone();
    relayed.relay_url = Some("https://relay.example./".into());
    assert!(relayed.is_dialable());

    // pubkey + direct addr (the LAN-without-internet iroh case).
    let mut direct = key_only.clone();
    direct.iroh_direct_addrs = vec!["127.0.0.1:5000".parse().unwrap()];
    assert!(direct.is_dialable());

    // A relay/direct WITHOUT a pubkey is not dialable by key.
    let mut no_key = member(node, "nokey", 1);
    no_key.relay_url = Some("https://relay.example./".into());
    assert!(!no_key.is_dialable());
}

#[test]
fn iroh_dial_fields_serde_back_compat_and_mutable_lww() {
    // Back-compat: a record with no iroh dial info serializes
    // WITHOUT the keys (skip_serializing_if), so a pre-W2 node sees
    // identical bytes — and such a payload reads back as None/empty.
    let bare = member(NodeId::from_u128(1), "a", 1);
    let json = serde_json::to_value(&bare).unwrap();
    assert!(
        json.get("relay_url").is_none(),
        "relay_url omitted when None"
    );
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
    let rt: MemberRecord = serde_json::from_value(serde_json::to_value(&keyed).unwrap()).unwrap();
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
    let sig = key.sign(&crate::dial_sig::dial_info_message(
        &pk, version, relay, addrs,
    ));
    m.dial_info_sig = Some(hex::encode(sig.to_bytes()));
    m
}

/// covers: FE-23
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

// The two negative controls for the monotone encryption join
// (`merge_from_authenticated`, "stricter wins"). Until 2026-08-28 that
// rule was asserted only in prose beside the code — ARCH §7.2's smell,
// and §18.1's "a check with no failing input you can name": nothing
// anywhere asserted that a peer advertising `false` could not demote a
// local `true`, on a field that decides whether the whole ring speaks
// encrypted.
//
// They are TWO tests because they fail on different mutations, and
// either one alone is green against a broken join:
//   * `never_downgrades` is red if the join becomes an assignment or LWW
//     (`self.require_encryption = other.require_encryption`) — the
//     downgrade path. It is GREEN if the line is deleted outright.
//   * `turns_on_from_peer` is red if the line is deleted — the
//     never-propagates path. It is GREEN under LWW.
// Both were watched failing under their own mutation before landing.

/// covers: FE-24
#[test]
fn encryption_policy_never_downgrades() {
    let mesh_id = MeshId::from_u128(21);
    let hash = [9u8; 32];
    let a = NodeId::from_u128(1);

    let mut local = mesh_with(vec![member(a, "A", 100)], mesh_id, hash);
    local.require_encryption = true;

    // A peer past the auth boundary — stale or hostile — advertising the
    // weaker policy. Newer `last_seen`, so an LWW join would take it.
    let mut remote = mesh_with(vec![member(a, "A", 200)], mesh_id, hash);
    remote.require_encryption = false;

    local.merge_from(a, &remote);
    assert!(
        local.require_encryption,
        "a peer advertising require_encryption=false must never relax a \
         local true — once a node learns the mesh is encrypted, no gossip \
         round demotes it to plaintext"
    );
}

#[test]
fn encryption_policy_turns_on_from_peer() {
    let mesh_id = MeshId::from_u128(22);
    let hash = [9u8; 32];
    let a = NodeId::from_u128(1);

    let mut local = mesh_with(vec![member(a, "A", 100)], mesh_id, hash);
    local.require_encryption = false;

    let mut remote = mesh_with(vec![member(a, "A", 100)], mesh_id, hash);
    remote.require_encryption = true;

    local.merge_from(a, &remote);
    assert!(
        local.require_encryption,
        "the join is monotone, not inert: a peer advertising \
         require_encryption=true must turn a local false ON"
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
    assert_eq!(report.added(), 1);
    assert_eq!(report.updated(), 0);
    assert!(!report.rejected());
    assert_eq!(report.observed(), vec![b], "added member is observed");
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
    assert_eq!(report.added(), 0);
    assert_eq!(report.updated(), 1);
    assert_eq!(report.observed(), vec![b], "LWW-updated member is observed");
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
    assert_eq!(report.added(), 0);
    assert_eq!(report.updated(), 0);
    assert!(
        report.observed().is_empty(),
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
    assert_eq!(report.updated(), 0, "stale live record must not resurrect");
    assert!(
        report.observed().is_empty(),
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
    assert_eq!(
        report.updated(),
        1,
        "rejoin (last_seen 100 > removed_at 50) wins"
    );
    let merged = local.members.get(&b).unwrap();
    assert!(merged.is_active(), "rejoin clears the tombstone");
    assert_eq!(merged.last_seen, 100);
}

/// covers: FE-6
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
    assert_eq!(report.updated(), 1);
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
    assert_eq!(report.added(), 0);
    assert_eq!(report.updated(), 0);
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
    assert!(report.rejected());
    assert_eq!(report.added(), 0);
    assert_eq!(local.members.len(), 1, "no mutation on reject");
}

#[test]
fn merge_rejects_mismatched_invite_key_hash() {
    let me = NodeId::from_u128(1);
    let mesh_id = MeshId::from_u128(1);
    let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
    let remote = mesh_with(
        vec![member(NodeId::from_u128(2), "X", 100)],
        mesh_id,
        [9u8; 32], // different hash!
    );

    let report = local.merge_from(me, &remote);
    assert!(report.rejected());
    assert_eq!(local.members.len(), 1);
}

mod gossip_auth;

// ── The snapshot's wire shape ────────────────────────────────────────
//
// Both regressions this shape has suffered were FIELD DROPS inside a
// hand-written conversion, and both shipped green: `mesh_secret` zero-filled
// on 2026-08-26, and `invite_version` pinned at 0 so invite rotations stopped
// propagating. Nothing in the repo would have gone red for either. A shape
// test is the only instrument that catches that class, and there was none.

fn probe_mesh() -> Mesh {
    // Reuses the existing helper rather than minting a second constructor.
    let mut m = mesh_with(vec![], MeshId::generate(), [9u8; 32]);
    m.mesh_secret = [7u8; 32];
    m.invite_version = 42;
    m.invite_expires_at = Some(1234);
    m.require_encryption = true;
    m
}

/// The exact key set, pinned as a literal list. A test that derived the
/// expected keys from the struct would agree with any change to it.
#[test]
fn the_snapshot_carries_exactly_these_wire_keys() {
    let snap = MeshWire::for_peer(&probe_mesh(), SecretDisclosure::Disclose);
    let v: serde_json::Value = serde_json::to_value(&snap).unwrap();
    let obj = v.as_object().expect("snapshot serialises to an object");
    let mut got: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
    got.sort_unstable();
    assert_eq!(
        got,
        vec![
            "id",
            "invite_expires_at",
            "invite_version",
            "join_key_hash", // NOT `invite_key_hash` — dropping this rename made every round 422
            "members",
            "mesh_secret",
            "name",
            "peers",
            "require_encryption",
        ],
        "the snapshot's wire keys changed — this is a fleet-wide break"
    );
}

/// The `invite_version` regression, pinned. A conversion that hardcodes 0
/// here stops invite rotation propagating, silently, with gossip green.
#[test]
fn invite_version_survives_the_round_trip() {
    let snap = MeshWire::for_peer(&probe_mesh(), SecretDisclosure::Disclose);
    assert_eq!(snap.invite_version, 42);
    let back = snap.into_mesh();
    assert_eq!(
        back.invite_version, 42,
        "a rotation cannot propagate without it"
    );
    assert_eq!(back.invite_expires_at, Some(1234));
    assert!(back.require_encryption);
}

/// The `mesh_secret` regression, pinned from both directions — and the reason
/// `SecretDisclosure` has no `Default`.
#[test]
fn disclosure_decides_the_secret_and_nothing_else() {
    let m = probe_mesh();
    let disclosed = MeshWire::for_peer(&m, SecretDisclosure::Disclose);
    let redacted = MeshWire::for_peer(&m, SecretDisclosure::Redact);

    assert_eq!(disclosed.mesh_secret, [7u8; 32]);
    assert_eq!(
        redacted.mesh_secret, [0u8; 32],
        "redaction zeroes, never omits"
    );

    // Redaction must touch NOTHING else — the 2026-08-26 failure was a
    // conversion quietly zeroing more than it was asked to.
    assert_eq!(disclosed.invite_key_hash, redacted.invite_key_hash);
    assert_eq!(disclosed.invite_version, redacted.invite_version);
    assert_eq!(disclosed.invite_expires_at, redacted.invite_expires_at);
    assert_eq!(disclosed.require_encryption, redacted.require_encryption);
    assert_eq!(disclosed.id, redacted.id);

    // A redacted snapshot still deserialises on a pre-split peer as "not set",
    // which is the same value an absent field would have defaulted to.
    let json = serde_json::to_string(&redacted).unwrap();
    let back: MeshWire = serde_json::from_str(&json).unwrap();
    assert_eq!(back.mesh_secret, [0u8; 32]);
}

/// A pre-split peer omits the fields it does not know; every one of them must
/// default rather than fail the parse.
#[test]
fn a_pre_split_peers_payload_still_parses() {
    let json = serde_json::json!({
        "id": MeshId::generate(),
        "name": "old",
        "join_key_hash": vec![0u8; 32],
        "members": [],
        "peers": [],
    });
    let back: MeshWire =
        serde_json::from_value(json).expect("a pre-split payload must still parse");
    assert_eq!(back.mesh_secret, [0u8; 32]);
    assert_eq!(back.invite_version, 0);
    assert_eq!(back.invite_expires_at, None);
    assert!(!back.require_encryption);
}
