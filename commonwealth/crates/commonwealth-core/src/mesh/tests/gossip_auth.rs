// SPDX-License-Identifier: AGPL-3.0-or-later
//! The gossip authorization boundary: the proof of `mesh_secret` possession,
//! the four [`GossipAuthArm`]s, the split-generation verdict each arm implies,
//! and invite rotation — which is the thing the credential split exists to
//! make safe.
//!
//! Split from `mesh/tests.rs` (ARCH §3.2) when it crossed 1,200 lines. The
//! helpers stay in the parent so `mesh_identity`'s tests keep importing them
//! from one place.

use super::{member, mesh_with};
use crate::ids::{MeshId, NodeId};
use crate::mesh::*;

/// THE point of the credential split. Two members whose invite hashes have
/// diverged — one rotated, the other has not gossiped it yet — must still
/// authorize each other, because gossip auth reads `mesh_secret`.
///
/// Before the split this exact state was a symmetric partition: each side
/// rejected the other and both reported `[1/N online]`.
#[test]
fn a_rotated_invite_does_not_partition_the_mesh() {
    let me = NodeId::from_u128(1);
    let mesh_id = MeshId::from_u128(1);
    let secret = [3u8; 32];

    let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
    local.mesh_secret = secret;
    let mut remote = mesh_with(
        vec![member(NodeId::from_u128(2), "X", 100)],
        mesh_id,
        [9u8; 32], // rotated out from under us
    );
    remote.mesh_secret = secret;

    let report = local.merge_from(me, &remote);
    assert!(
        !report.rejected(),
        "same mesh_secret means same mesh, whatever the invite hash says"
    );
    assert_eq!(local.members.len(), 2);
}

/// A different mesh that happens to share an invite hash is still refused.
#[test]
fn a_shared_invite_hash_is_not_enough_once_secrets_are_set() {
    let me = NodeId::from_u128(1);
    let mesh_id = MeshId::from_u128(1);
    let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
    local.mesh_secret = [3u8; 32];
    let mut remote = mesh_with(
        vec![member(NodeId::from_u128(2), "X", 100)],
        mesh_id,
        [7u8; 32], // same invite hash...
    );
    remote.mesh_secret = [4u8; 32]; // ...different mesh

    assert!(local.merge_from(me, &remote).rejected());
    assert_eq!(local.members.len(), 1);
}

/// The compat arm: a pre-split peer sends a zeroed secret and must still be
/// admitted on the legacy predicate, or upgrading the fleet partitions it.
#[test]
fn a_pre_split_peer_authorizes_on_the_legacy_predicate() {
    let me = NodeId::from_u128(1);
    let mesh_id = MeshId::from_u128(1);
    let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
    local.mesh_secret = [3u8; 32];
    let remote = mesh_with(
        vec![member(NodeId::from_u128(2), "X", 100)],
        mesh_id,
        [7u8; 32],
    ); // mesh_secret defaults to zeroed

    let report = local.merge_from(me, &remote);
    assert!(!report.rejected(), "a peer mid-upgrade must not be dropped");
    assert_eq!(local.members.len(), 2);
    assert!(
        report.peer_pre_split(),
        "the merge must REPORT that this peer is pre-split — it is the \
         only moment that fact is visible, and rotate_invite depends on it"
    );
}

/// The signal `rotate_invite`'s guard stands on. Reported per merge,
/// because it describes the SENDER's build and nothing in any member
/// record carries it.
#[test]
fn a_post_split_peer_is_reported_as_post_split() {
    let me = NodeId::from_u128(1);
    let mesh_id = MeshId::from_u128(1);
    let secret = [3u8; 32];
    let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
    local.mesh_secret = secret;
    let mut remote = mesh_with(
        vec![member(NodeId::from_u128(2), "X", 100)],
        mesh_id,
        [7u8; 32],
    );
    remote.mesh_secret = secret;

    let report = local.merge_from(me, &remote);
    assert!(!report.rejected());
    assert!(
        !report.peer_pre_split(),
        "a peer that sent a mesh_secret is post-split; flagging it would \
         block invite rotation on a fully-upgraded fleet forever"
    );
}

/// A REFUSED merge says nothing about the sender's build, so the flag must
/// not be read as "post-split" on that path. Guards that treat an absent
/// answer as a positive one are the §18.3 substitution.
#[test]
fn a_rejected_merge_reports_no_split_generation() {
    let me = NodeId::from_u128(1);
    let mut local = mesh_with(vec![member(me, "M", 10)], MeshId::from_u128(1), [7u8; 32]);
    local.mesh_secret = [3u8; 32];
    let mut remote = mesh_with(vec![], MeshId::from_u128(2), [7u8; 32]);
    remote.mesh_secret = [4u8; 32];

    let report = local.merge_from(me, &remote);
    assert!(report.rejected());
    assert!(!report.peer_pre_split());
}

fn proof_mesh(secret: [u8; 32]) -> Mesh {
    proof_mesh_with_id(secret, MeshId::from_u128(77))
}

/// The proof binds to `mesh_id`, so a test that verifies across two Mesh
/// values must give them the SAME id or the proof legitimately fails.
fn proof_mesh_with_id(secret: [u8; 32], id: MeshId) -> Mesh {
    let mut m = mesh_with(vec![], id, [7u8; 32]);
    m.mesh_secret = secret;
    m
}

/// The happy path: a holder of the secret proves it without sending it.
#[test]
fn a_proof_from_the_same_secret_verifies() {
    let sender = NodeId::from_u128(5);
    let a = proof_mesh([9u8; 32]);
    let b = proof_mesh([9u8; 32]);
    let now = 1_000_000;
    assert!(b.verify_mesh_proof(&a.mesh_proof(sender, now).unwrap(), sender, now));
}

/// The point of the exercise: a different secret cannot forge one.
#[test]
fn a_proof_from_a_different_secret_is_refused() {
    let sender = NodeId::from_u128(5);
    let a = proof_mesh([9u8; 32]);
    let b = proof_mesh([8u8; 32]);
    let now = 1_000_000;
    assert!(!b.verify_mesh_proof(&a.mesh_proof(sender, now).unwrap(), sender, now));
}

/// covers: FE-13
///
/// Bound to the sender: an eavesdropper who captures a member's proof
/// cannot present it as themselves.
#[test]
fn a_proof_cannot_be_replayed_by_a_different_peer() {
    let real = NodeId::from_u128(5);
    let impostor = NodeId::from_u128(6);
    let a = proof_mesh([9u8; 32]);
    let b = proof_mesh([9u8; 32]);
    let now = 1_000_000;
    let stolen = a.mesh_proof(real, now).unwrap();
    assert!(b.verify_mesh_proof(&stolen, real, now));
    assert!(
        !b.verify_mesh_proof(&stolen, impostor, now),
        "a captured proof must not authorize a different node — otherwise it \
         is a bearer token for anyone who can sniff one packet"
    );
}

/// Bound to a time window: a captured proof goes stale rather than being
/// a credential forever. Two windows of slack, never more.
#[test]
fn a_proof_expires_after_two_windows() {
    let sender = NodeId::from_u128(5);
    let a = proof_mesh([9u8; 32]);
    let b = proof_mesh([9u8; 32]);
    let now = 1_000_000;
    let proof = a.mesh_proof(sender, now).unwrap();

    assert!(b.verify_mesh_proof(&proof, sender, now));
    assert!(
        b.verify_mesh_proof(&proof, sender, now + PROOF_WINDOW_SECS),
        "one window of skew must be tolerated, or a round that straddles a \
         boundary fails for no reason"
    );
    assert!(
        !b.verify_mesh_proof(&proof, sender, now + PROOF_WINDOW_SECS * 3),
        "the replay horizon must be bounded"
    );
}

/// A node with no secret must refuse rather than key every proof
/// identically across every un-migrated mesh.
#[test]
fn a_node_without_a_secret_verifies_nothing() {
    let sender = NodeId::from_u128(5);
    let holder = proof_mesh([9u8; 32]);
    let unset = proof_mesh([0u8; 32]);
    let now = 1_000_000;
    assert!(!unset.verify_mesh_proof(
        &holder.mesh_proof(sender, now).unwrap_or_default(),
        sender,
        now
    ));
}

/// The upgraded case: a peer proves possession and sends NO raw secret at
/// all. This is what takes the credential off the wire.
#[test]
fn a_valid_proof_authorizes_without_any_raw_secret_on_the_wire() {
    let me = NodeId::from_u128(1);
    let sender = NodeId::from_u128(2);
    let mesh_id = MeshId::from_u128(1);
    let now = 1_000_000;

    let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
    local.mesh_secret = [9u8; 32];

    // The peer's payload carries a ZEROED secret — it sent nothing.
    let mut remote = mesh_with(vec![member(sender, "P", 100)], mesh_id, [7u8; 32]);
    remote.mesh_secret = [0u8; 32];

    let holder = proof_mesh_with_id([9u8; 32], mesh_id);
    let auth = GossipAuth {
        sender: Some(sender),
        proof: holder.mesh_proof(sender, now),
        now_secs: now,
    };

    let report = local.merge_from_authenticated(me, &remote, &auth);
    assert!(
        !report.rejected(),
        "a proof of possession must authorize; otherwise the secret can \
         never leave the wire"
    );
    assert_eq!(local.members.len(), 2);
}

/// covers: FE-16
///
/// The mis-attribution this arm enum exists to prevent.
///
/// Once the outbound path stops sending the raw secret to a CONFIRMED
/// post-split peer, a zeroed `mesh_secret` on the wire stops meaning "old
/// build" and starts meaning "upgraded peer, deliberately withholding".
/// Reading the payload alone flips two upgraded nodes to pre-split, which
/// (a) blocks `rotate_invite` on both sides forever and (b) makes each
/// resume sending the credential it had just stopped sending. The proof
/// settles it: only a holder of the current secret can produce one.
#[test]
fn a_peer_that_proves_possession_is_never_reported_pre_split() {
    let me = NodeId::from_u128(1);
    let sender = NodeId::from_u128(2);
    let mesh_id = MeshId::from_u128(1);
    let now = 1_000_000;

    let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
    local.mesh_secret = [9u8; 32];

    // An UPGRADED peer that has confirmed us and now withholds the secret.
    let mut remote = mesh_with(vec![member(sender, "P", 100)], mesh_id, [7u8; 32]);
    remote.mesh_secret = [0u8; 32];

    let holder = proof_mesh_with_id([9u8; 32], mesh_id);
    let auth = GossipAuth {
        sender: Some(sender),
        proof: holder.mesh_proof(sender, now),
        now_secs: now,
    };

    let report = local.merge_from_authenticated(me, &remote, &auth);
    assert_eq!(report.auth_arm(), GossipAuthArm::Proof);
    assert!(
        !report.peer_pre_split(),
        "a peer that PROVED possession of the current secret is post-split \
         by definition; calling it pre-split blocks rotation on both sides \
         and puts the credential back on the wire"
    );
}

/// The compat half, still intact: no proof and no secret really is a
/// pre-split peer, and it must still be admitted.
#[test]
fn a_peer_with_neither_proof_nor_secret_is_still_pre_split() {
    let me = NodeId::from_u128(1);
    let sender = NodeId::from_u128(2);
    let mesh_id = MeshId::from_u128(1);

    let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
    local.mesh_secret = [9u8; 32];
    let mut remote = mesh_with(vec![member(sender, "P", 100)], mesh_id, [7u8; 32]);
    remote.mesh_secret = [0u8; 32];

    let report = local.merge_from_authenticated(me, &remote, &GossipAuth::none());
    assert!(!report.rejected(), "the compat arm must still admit");
    assert_eq!(report.auth_arm(), GossipAuthArm::Legacy);
    assert!(report.peer_pre_split());
}

/// Two post-split-but-pre-proof nodes: raw secrets match, and that arm is
/// the one the reply may still answer with the credential on.
#[test]
fn matching_raw_secrets_report_the_raw_secret_arm() {
    let me = NodeId::from_u128(1);
    let sender = NodeId::from_u128(2);
    let mesh_id = MeshId::from_u128(1);

    let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
    local.mesh_secret = [9u8; 32];
    let mut remote = mesh_with(vec![member(sender, "P", 100)], mesh_id, [7u8; 32]);
    remote.mesh_secret = [9u8; 32];

    let report = local.merge_from_authenticated(me, &remote, &GossipAuth::none());
    assert_eq!(report.auth_arm(), GossipAuthArm::RawSecret);
    assert!(!report.peer_pre_split());
}

/// Downgrade prevention. An OFFERED proof that does not verify is a
/// failure, not an invitation to try the weaker predicate — otherwise an
/// attacker sends junk and gets handed the legacy `invite_key_hash` arm.
#[test]
fn a_bad_proof_is_refused_and_does_not_fall_back_to_the_legacy_arm() {
    let me = NodeId::from_u128(1);
    let sender = NodeId::from_u128(2);
    let mesh_id = MeshId::from_u128(1);
    let now = 1_000_000;

    let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
    local.mesh_secret = [9u8; 32];

    // The attacker knows the invite hash — which rides every payload — and
    // would be admitted by the legacy arm if the bad proof fell through.
    let mut remote = mesh_with(vec![member(sender, "X", 100)], mesh_id, [7u8; 32]);
    remote.mesh_secret = [0u8; 32];

    let auth = GossipAuth {
        sender: Some(sender),
        proof: Some("not a real proof".into()),
        now_secs: now,
    };

    assert!(
        local
            .merge_from_authenticated(me, &remote, &auth)
            .rejected(),
        "a failed proof must REFUSE, not downgrade to invite_key_hash"
    );
    assert_eq!(local.members.len(), 1, "a refused merge must not mutate");
}
/// ME-03 — one payload, three verdicts.
///
/// The `mesh_secret` on the wire is byte-identical in all three rounds
/// below: zeroed. What differs is the evidence the round carried, and the
/// verdict must move with the evidence rather than with the field.
///
/// Reading the field alone is what shipped, and it was right only while
/// every upgraded build still sent the secret. Once an upgraded peer began
/// withholding it ON PURPOSE, a zeroed field inverted in meaning and the
/// three cases collapsed into one "legacy" answer. Two modern nodes then
/// reported each other pre-split, blocked `rotate_invite` on both sides
/// forever, and each resumed sending the credential it had just stopped
/// sending.
///
/// `a_peer_that_proves_possession_is_never_reported_pre_split` pins ONE of
/// these arms. This pins that the three stay APART — a refused round is
/// UNKNOWN, and folding it into the compat arm hands a stripped or forged
/// proof the weaker predicate.
#[test]
fn a_zeroed_secret_is_not_a_verdict_the_authorization_arm_is() {
    let me = NodeId::from_u128(1);
    let sender = NodeId::from_u128(2);
    let mesh_id = MeshId::from_u128(1);
    let secret = [9u8; 32];
    let now = 1_000_000;

    // ONE payload, handed verbatim to all three rounds.
    let mut wire = mesh_with(vec![member(sender, "P", 100)], mesh_id, [7u8; 32]);
    wire.mesh_secret = MESH_SECRET_UNSET;

    let fresh_local = || {
        let mut m = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
        m.mesh_secret = secret;
        m
    };
    let holder = proof_mesh_with_id(secret, mesh_id);

    // 1. Proved possession — post-split, whatever the payload carried.
    let proven = fresh_local().merge_from_authenticated(
        me,
        &wire,
        &GossipAuth {
            sender: Some(sender),
            proof: holder.mesh_proof(sender, now),
            now_secs: now,
        },
    );

    // 2. No evidence offered — the compat arm, and genuinely pre-split.
    let legacy = fresh_local().merge_from_authenticated(me, &wire, &GossipAuth::none());

    // 3. A proof that does not verify — refused. The round says nothing
    //    about the sender's build, so `peer_pre_split` is not an answer on
    //    this path and `rejected` is the only thing a caller may read.
    let mut refusing_local = fresh_local();
    let refused = refusing_local.merge_from_authenticated(
        me,
        &wire,
        &GossipAuth {
            sender: Some(sender),
            proof: Some("not a real proof".into()),
            now_secs: now,
        },
    );

    assert_eq!(
        [proven.auth_arm(), legacy.auth_arm(), refused.auth_arm()],
        [
            GossipAuthArm::Proof,
            GossipAuthArm::Legacy,
            GossipAuthArm::Refused,
        ],
        "one payload, three arms — the evidence decides, never the field"
    );
    assert_eq!(
        wire.mesh_secret, MESH_SECRET_UNSET,
        "the field the old predicate read is the same zeroes in all three"
    );
    assert!(
        !proven.peer_pre_split(),
        "a proof settles the generation: only a holder of the CURRENT \
         secret can produce one"
    );
    assert!(
        legacy.peer_pre_split(),
        "no proof and no secret really is a pre-split peer — the compat \
         half must survive, or the fleet cannot upgrade"
    );
    assert!(refused.rejected(), "a refused round is UNKNOWN, not legacy");
    assert_eq!(
        refusing_local.members.len(),
        1,
        "and UNKNOWN must not merge — the sender is still unattributed"
    );
}

/// A rotation must TRAVEL. Before `invite_version` existed, `merge_from`
/// merged only `require_encryption` and `members`, so a founder's rotate
/// was node-local: every other member kept admitting joiners on the
/// revoked key forever. This is the test that would have caught that.
#[test]
fn a_rotated_invite_propagates_to_a_peer() {
    let me = NodeId::from_u128(1);
    let mesh_id = MeshId::from_u128(1);
    let secret = [3u8; 32];

    let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
    local.mesh_secret = secret;

    // The founder rotates, then gossips at us.
    let mut founder = mesh_with(
        vec![member(NodeId::from_u128(2), "F", 100)],
        mesh_id,
        [7u8; 32],
    );
    founder.mesh_secret = secret;
    founder.rotate_invite_key([9u8; 32], Some(4242));

    assert_eq!(
        founder.invite_version, 1,
        "rotating must advance the version"
    );
    assert!(!local.merge_from(me, &founder).rejected());
    assert_eq!(
        local.invite_key_hash, [9u8; 32],
        "the peer must adopt the rotated invite, or it keeps admitting \
         joiners on the revoked key"
    );
    assert_eq!(
        local.invite_expires_at,
        Some(4242),
        "the TTL moves WITH the hash — a new key under a stale expiry is \
         worse than either endpoint"
    );
    assert_eq!(local.invite_version, 1);
}

/// Anti-rollback, same rule as `dial_info_version`: a replayed older
/// payload never wins. Without this a stale peer re-arms a revoked invite.
#[test]
fn an_older_invite_never_overwrites_a_newer_one() {
    let me = NodeId::from_u128(1);
    let mesh_id = MeshId::from_u128(1);
    let secret = [3u8; 32];

    let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
    local.mesh_secret = secret;
    local.rotate_invite_key([9u8; 32], None); // version 1

    let mut stale = mesh_with(
        vec![member(NodeId::from_u128(2), "S", 100)],
        mesh_id,
        [7u8; 32],
    );
    stale.mesh_secret = secret; // version 0, old hash

    assert!(!local.merge_from(me, &stale).rejected());
    assert_eq!(
        local.invite_key_hash, [9u8; 32],
        "a version-0 peer must not roll our rotation back"
    );
    assert_eq!(local.invite_version, 1);
}

/// Two nodes rotating in the same round land on the same version with
/// different hashes. "Keep ours" is not a decision — it is each node
/// keeping a different answer forever, i.e. two admission regimes in one
/// mesh. The hash comparison is a total order every node computes
/// identically, so they converge.
#[test]
fn a_simultaneous_rotation_converges_rather_than_splitting() {
    let mesh_id = MeshId::from_u128(1);
    let secret = [3u8; 32];
    let a_id = NodeId::from_u128(1);
    let b_id = NodeId::from_u128(2);

    let mut a = mesh_with(vec![member(a_id, "A", 10)], mesh_id, [7u8; 32]);
    a.mesh_secret = secret;
    a.rotate_invite_key([1u8; 32], None); // version 1, LOWER hash

    let mut b = mesh_with(vec![member(b_id, "B", 10)], mesh_id, [7u8; 32]);
    b.mesh_secret = secret;
    b.rotate_invite_key([2u8; 32], None); // version 1, HIGHER hash

    // Gossip both directions.
    let (a_snapshot, b_snapshot) = (a.clone(), b.clone());
    a.merge_from(a_id, &b_snapshot);
    b.merge_from(b_id, &a_snapshot);

    assert_eq!(
        a.invite_key_hash, b.invite_key_hash,
        "both sides must land on ONE invite; a split here means two \
         admission regimes inside one mesh"
    );
    assert_eq!(
        a.invite_key_hash, [2u8; 32],
        "the higher hash is the tie-break"
    );
}

/// covers: FE-11
///
/// ARCH §7.1: the invariant is structural, not remembered. `rotate_invite_key`
/// cannot name `mesh_secret`, and this pins that it stays that way.
#[test]
fn rotating_the_invite_never_touches_the_mesh_secret() {
    let mesh_id = MeshId::from_u128(1);
    let mut mesh = mesh_with(vec![], mesh_id, [7u8; 32]);
    mesh.mesh_secret = [3u8; 32];

    mesh.rotate_invite_key([8u8; 32], Some(1234));

    assert_eq!(mesh.invite_key_hash, [8u8; 32]);
    assert_eq!(mesh.invite_expires_at, Some(1234));
    assert_eq!(
        mesh.mesh_secret, [3u8; 32],
        "rotation must be structurally incapable of re-keying gossip"
    );
}

#[test]
fn an_invite_with_no_expiry_never_lapses() {
    let mesh_id = MeshId::from_u128(1);
    let mut mesh = mesh_with(vec![], mesh_id, [7u8; 32]);
    assert!(!mesh.invite_expired_at(u64::MAX));

    mesh.invite_expires_at = Some(100);
    assert!(!mesh.invite_expired_at(99));
    assert!(mesh.invite_expired_at(100), "expiry is inclusive");
    assert!(mesh.invite_expired_at(101));
}
