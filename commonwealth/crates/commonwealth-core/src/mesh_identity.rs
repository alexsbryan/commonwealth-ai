// SPDX-License-Identifier: AGPL-3.0-or-later
//! Endpoint-key identity: the rule that one dialable endpoint belongs to one
//! member, the predicate that decides it, and the guard that enforces it.
//!
//! Split out of `mesh.rs` because it is a different question from "how do two
//! views of a roster converge". Merge is about time; this is about identity.
//! Keeping them in one file also pushed `mesh.rs` further past ARCH §3.1's
//! size band, and this is the seam that was actually there.
//!
//! Everything that asks the question reaches [`aliased_endpoint_keys`] —
//! the DST invariant pack, the gossip admission guard, and the operator's
//! repair command. One rule, one implementation (§10.6).

use std::collections::BTreeMap;

use crate::ids::{NodeId, NodePubkey};
use crate::mesh::{MemberRecord, Mesh};

/// One endpoint key held by more than one LIVE member.
///
/// See [`Mesh::aliased_endpoint_keys`] for why this is a defect rather than a
/// curiosity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasedEndpointKey {
    pub node_pubkey: NodePubkey,
    /// Every live member carrying it, as `(node_id, name)`. Always length >= 2.
    pub members: Vec<(NodeId, String)>,
}

/// One member's claim on an endpoint key — the only input
/// [`aliased_endpoint_keys`] needs.
///
/// Exists so the rule can be asked of things that are not a [`Mesh`]: the DST
/// invariant pack holds per-node `MemberStat` snapshots, never a whole mesh,
/// and before this it re-implemented the grouping inline. Two implementations
/// of one predicate is §10.6, and the checker is the last place you want a
/// second opinion.
#[derive(Debug, Clone)]
pub struct EndpointClaim {
    pub node_id: NodeId,
    pub name: String,
    pub node_pubkey: Option<NodePubkey>,
    /// `removed_at.is_none()`. NOT liveness: a tombstoned row may legitimately
    /// share a key with a rejoined node.
    pub active: bool,
}

impl From<&MemberRecord> for EndpointClaim {
    fn from(m: &MemberRecord) -> Self {
        Self {
            node_id: m.node_id,
            name: m.name.clone(),
            node_pubkey: m.node_pubkey,
            active: m.is_active(),
        }
    }
}

/// THE rule: which endpoint keys are claimed by more than one ACTIVE member.
///
/// See [`Mesh::aliased_endpoint_keys`] for why an alias is a defect and what
/// it looked like in the wild.
///
/// # Scoped to active rows, deliberately
///
/// A tombstoned row may legitimately share a key with a rejoined node — a
/// genuine rejoin stamps activity newer than the removal and wins the LWW
/// ([`MemberRecord::effective_at`]). A naive "no two rows share a key" would
/// fire on every legitimate rejoin and be switched off within a week.
/// `active` on BOTH sides is what keeps this from crying wolf.
///
/// A `None` key is an absent claim, never a value: two keyless legacy members
/// do not alias each other.
pub fn aliased_endpoint_keys(
    claims: impl IntoIterator<Item = EndpointClaim>,
) -> Vec<AliasedEndpointKey> {
    let mut by_key: BTreeMap<[u8; 32], Vec<(NodeId, String)>> = BTreeMap::new();
    for c in claims {
        if !c.active {
            continue;
        }
        if let Some(pk) = c.node_pubkey {
            by_key
                .entry(*pk.as_bytes())
                .or_default()
                .push((c.node_id, c.name));
        }
    }
    by_key
        .into_iter()
        .filter(|(_, rows)| rows.len() > 1)
        .map(|(key, members)| AliasedEndpointKey {
            node_pubkey: NodePubkey(key),
            members,
        })
        .collect()
}

impl Mesh {
    /// LIVE members that share one endpoint key — the mesh's identity
    /// collision, as a value.
    ///
    /// # Why this exists, and why it is ONE rule
    ///
    /// `node_pubkey` is what a peer dials and what `daemon.rs`'s iroh acceptor
    /// admits on: "membership = dialability". Two LIVE rows carrying one key
    /// therefore split a single node's liveness across two member records that
    /// each look stale, while the endpoint behind both answers on demand.
    /// Neither is ever marked online, gossip burns its retry budget on the
    /// phantom, and the real node reads offline from every peer.
    ///
    /// Observed live 2026-08-28 on mesh `27ba8166…`, confirmed independently
    /// from both nodes' own `mesh.json`: `Alexs-MacBook-Pro-2` (`37f17554…`)
    /// and `BeefyMac` (`b88252e4…`) both carrying node_pubkey `86627fd5…`,
    /// neither tombstoned. Symptom on the aliased node was `iroh bridge: dial
    /// failed … Connecting to ourself is not supported`.
    ///
    /// The invariant pack, the admission guard and the operator's repair
    /// command all reach the free [`aliased_endpoint_keys`] through this or
    /// [`Mesh::endpoint_key_claimant`] rather than each comparing keys inline:
    /// a checker and an admitter with separate implementations of one
    /// predicate is the §10.6 duplicated decider, and the two would drift
    /// precisely where it costs most.
    pub fn aliased_endpoint_keys(&self) -> Vec<AliasedEndpointKey> {
        aliased_endpoint_keys(self.members.values().map(EndpointClaim::from))
    }

    /// The point-query form of [`Mesh::aliased_endpoint_keys`]: the ACTIVE
    /// member — other than `excluding` — that already claims `key`.
    ///
    /// This is what the admission guard asks before it writes a record:
    /// "would admitting this key under this node_id create an alias?" It does
    /// not compare keys itself. It builds the roster as it WOULD be and puts
    /// the question to the same predicate the checker uses, so the guard and
    /// the check cannot answer differently (§10.6).
    pub fn endpoint_key_claimant(
        &self,
        key: NodePubkey,
        excluding: NodeId,
    ) -> Option<(NodeId, String)> {
        let probe = EndpointClaim {
            node_id: excluding,
            name: String::new(),
            node_pubkey: Some(key),
            active: true,
        };
        let prospective = self
            .members
            .values()
            .filter(|m| m.node_id != excluding)
            .map(EndpointClaim::from)
            .chain(std::iter::once(probe));
        aliased_endpoint_keys(prospective)
            .into_iter()
            .find(|a| a.node_pubkey == key)
            .and_then(|a| a.members.into_iter().find(|(id, _)| *id != excluding))
    }

    /// The admission guard, as asked by both arms of
    /// [`Mesh::merge_from_authenticated`]: may this record be written?
    ///
    /// `Some(holder)` means writing it would leave two ACTIVE members
    /// claiming one endpoint key, and the caller must refuse it. Until
    /// 2026-08-28 nothing asked — the predicate existed and the merge path
    /// called it nowhere, so gossip could still CREATE the collision the
    /// checker had just learned to name.
    ///
    /// Two records are deliberately never refused:
    ///
    /// - A **tombstone** (`!active`). Refusing one would block removals from
    ///   converging, which is the opposite of a repair — and a retired row
    ///   sharing a key with a rejoined node is the legitimate case.
    /// - A record with **no key**. Absence is not a claim (§18.3): a keyless
    ///   legacy record aliases nothing.
    ///
    /// # What this deliberately does NOT do
    ///
    /// It never retires the incumbent in favour of the newcomer. Endpoint-key
    /// LWW would be self-healing and is the wrong trade: a peer past the auth
    /// boundary could then forge a newer record carrying a victim's key and
    /// tombstone that victim mesh-wide. Refusing is the conservative half —
    /// it cannot worsen a roster, and it cannot be turned into an eviction
    /// primitive. Repair stays an operator act (`svrn mesh forget-member`),
    /// which is a person deciding which of two rows is the ghost.
    pub(crate) fn alias_clash(
        &self,
        record: &MemberRecord,
        active: bool,
    ) -> Option<(NodeId, String)> {
        if !active {
            return None;
        }
        let key = record.node_pubkey?;
        self.endpoint_key_claimant(key, record.node_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MeshId;
    use crate::mesh::tests::{member, mesh_with};

    /// THE LIVE DEFECT, as a test. Two ACTIVE members carrying one endpoint
    /// key must be reported.
    ///
    /// Observed on mesh `27ba8166…` 2026-08-28 and confirmed from both
    /// nodes' own `mesh.json`: `Alexs-MacBook-Pro-2` and `BeefyMac` with
    /// distinct node_ids and identical node_pubkey `86627fd5…`, neither
    /// tombstoned. Both read offline from every peer while the endpoint
    /// behind them answered on demand.
    #[test]
    fn two_live_members_sharing_an_endpoint_key_are_reported() {
        let key = NodePubkey([0x86; 32]);
        let a = NodeId::from_u128(1);
        let b = NodeId::from_u128(2);
        let mut ma = member(a, "Alexs-MacBook-Pro-2", 100);
        ma.node_pubkey = Some(key);
        let mut mb = member(b, "BeefyMac", 100);
        mb.node_pubkey = Some(key);

        let mesh = mesh_with(vec![ma, mb], MeshId::from_u128(9), [0u8; 32]);
        let aliased = mesh.aliased_endpoint_keys();

        assert_eq!(aliased.len(), 1, "one aliased key, got {aliased:?}");
        assert_eq!(aliased[0].node_pubkey, key);
        let mut names: Vec<&str> = aliased[0].members.iter().map(|(_, n)| n.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["Alexs-MacBook-Pro-2", "BeefyMac"]);
    }

    /// THE CRY-WOLF CASE, and the reason the predicate is scoped to active
    /// rows. A node that left and rejoined legitimately leaves a TOMBSTONED
    /// row holding the same key as its new live row. A naive "no two rows
    /// share a key" fires on every honest rejoin, and an invariant that
    /// fires on healthy meshes gets switched off — taking the real defect
    /// with it.
    #[test]
    fn a_tombstoned_row_may_share_a_key_with_a_rejoined_node() {
        let key = NodePubkey([0x42; 32]);
        let old = NodeId::from_u128(1);
        let new = NodeId::from_u128(2);
        let mut gone = member(old, "laptop", 10);
        gone.node_pubkey = Some(key);
        gone.removed_at = Some(20);
        let mut back = member(new, "laptop", 30);
        back.node_pubkey = Some(key);

        let mesh = mesh_with(vec![gone, back], MeshId::from_u128(9), [0u8; 32]);
        assert!(
            mesh.aliased_endpoint_keys().is_empty(),
            "a tombstoned row sharing a key with a rejoined node is a LEGITIMATE \
             rejoin, not a collision"
        );
    }

    /// A healthy mesh reports nothing, including one where several members
    /// have no key at all (pre-identity builds). `None` is not a key and
    /// must never alias with another `None`.
    #[test]
    fn distinct_keys_and_absent_keys_are_not_collisions() {
        let a = NodeId::from_u128(1);
        let b = NodeId::from_u128(2);
        let c = NodeId::from_u128(3);
        let mut ma = member(a, "a", 1);
        ma.node_pubkey = Some(NodePubkey([1; 32]));
        let mut mb = member(b, "b", 1);
        mb.node_pubkey = Some(NodePubkey([2; 32]));
        // Two keyless members: legacy nodes, and emphatically not each other.
        let mc = member(c, "c", 1);

        let mesh = mesh_with(vec![ma, mb, mc], MeshId::from_u128(9), [0u8; 32]);
        assert!(mesh.aliased_endpoint_keys().is_empty());
    }

    /// THE GUARD, watched failing (§18.1). The predicate existed and the
    /// merge path called it nowhere, so gossip could still CREATE the exact
    /// collision the checker had just learned to name. This is the test that
    /// fails without `alias_clash` in `merge_from_authenticated`.
    ///
    /// Shape is the observed one: we hold a live `BeefyMac`; a peer gossips a
    /// NEW node_id under the SAME endpoint key. Admitting it would leave two
    /// active rows dialing one endpoint.
    #[test]
    fn gossip_may_not_admit_a_second_active_member_on_one_endpoint_key() {
        let key = NodePubkey([0x86; 32]);
        let me = NodeId::from_u128(0);
        let incumbent = NodeId::from_u128(1);
        let newcomer = NodeId::from_u128(2);

        let mut held = member(incumbent, "BeefyMac", 100);
        held.node_pubkey = Some(key);
        let mut local = mesh_with(
            vec![member(me, "self", 100), held],
            MeshId::from_u128(9),
            [0u8; 32],
        );

        let mut clone = member(newcomer, "Alexs-MacBook-Pro-2", 200);
        clone.node_pubkey = Some(key);
        let remote = mesh_with(vec![clone], MeshId::from_u128(9), [0u8; 32]);

        let report = local.merge_from(me, &remote);

        assert_eq!(report.added(), 0, "the aliasing record must not be written");
        assert_eq!(
            report.aliased_refused(),
            1,
            "and the refusal must be counted"
        );
        assert!(
            !local.members.contains_key(&newcomer),
            "roster must not carry the clone"
        );
        assert!(
            local.aliased_endpoint_keys().is_empty(),
            "the roster the guard protects must still satisfy the rule it enforces"
        );
    }

    /// The LWW arm is guarded too. A newer record for a member we already
    /// know can MOVE a key onto a second active row — same defect, different
    /// door, and a guard on only the first-sight arm would miss it.
    #[test]
    fn an_lww_update_may_not_move_a_key_onto_a_second_active_member() {
        let key = NodePubkey([0x86; 32]);
        let me = NodeId::from_u128(0);
        let holder = NodeId::from_u128(1);
        let other = NodeId::from_u128(2);

        let mut held = member(holder, "BeefyMac", 100);
        held.node_pubkey = Some(key);
        let mut mover = member(other, "laptop", 100);
        mover.node_pubkey = Some(NodePubkey([0x11; 32]));
        let mut local = mesh_with(
            vec![member(me, "self", 100), held, mover],
            MeshId::from_u128(9),
            [0u8; 32],
        );

        // Newer record for `other`, now claiming the incumbent's key.
        let mut moved = member(other, "laptop", 500);
        moved.node_pubkey = Some(key);
        let remote = mesh_with(vec![moved], MeshId::from_u128(9), [0u8; 32]);

        let report = local.merge_from(me, &remote);

        assert_eq!(report.updated(), 0, "the aliasing update must not land");
        assert_eq!(report.aliased_refused(), 1);
        assert_eq!(
            local.members[&other].node_pubkey,
            Some(NodePubkey([0x11; 32])),
            "the local record stands unchanged"
        );
    }

    /// A TOMBSTONE sharing a key is always admitted. Refusing removals would
    /// block them from converging — the opposite of a repair — and a retired
    /// row sharing a key with a rejoined node is the legitimate case the
    /// whole predicate is scoped around.
    #[test]
    fn a_tombstone_sharing_a_key_is_still_admitted() {
        let key = NodePubkey([0x42; 32]);
        let me = NodeId::from_u128(0);
        let live = NodeId::from_u128(1);
        let gone = NodeId::from_u128(2);

        let mut held = member(live, "laptop", 100);
        held.node_pubkey = Some(key);
        let mut local = mesh_with(
            vec![member(me, "self", 100), held],
            MeshId::from_u128(9),
            [0u8; 32],
        );

        let mut retired = member(gone, "laptop-old", 50);
        retired.node_pubkey = Some(key);
        retired.removed_at = Some(60);
        let remote = mesh_with(vec![retired], MeshId::from_u128(9), [0u8; 32]);

        let report = local.merge_from(me, &remote);

        assert_eq!(report.added(), 1, "a tombstone is not an alias");
        assert_eq!(report.aliased_refused(), 0);
        assert!(
            report.observed().is_empty(),
            "admitted, but emphatically not observed alive"
        );
    }

    /// A keyless record aliases nothing. Absence is not a claim (§18.3) —
    /// treating two `None`s as a match would refuse every legacy node.
    #[test]
    fn a_keyless_record_is_admitted_alongside_other_keyless_records() {
        let me = NodeId::from_u128(0);
        let a = NodeId::from_u128(1);
        let b = NodeId::from_u128(2);

        let mut local = mesh_with(
            vec![member(me, "self", 100), member(a, "legacy-a", 100)],
            MeshId::from_u128(9),
            [0u8; 32],
        );
        let remote = mesh_with(
            vec![member(b, "legacy-b", 100)],
            MeshId::from_u128(9),
            [0u8; 32],
        );

        let report = local.merge_from(me, &remote);
        assert_eq!(report.added(), 1);
        assert_eq!(report.aliased_refused(), 0);
    }

    /// The guard and the checker must never disagree — that is the whole
    /// point of routing both through one predicate (§10.6). Property-ish:
    /// after ANY merge, a roster that started clean is still clean.
    #[test]
    fn a_clean_roster_stays_clean_across_a_hostile_merge() {
        let key = NodePubkey([0x86; 32]);
        let me = NodeId::from_u128(0);
        let mut held = member(NodeId::from_u128(1), "real", 100);
        held.node_pubkey = Some(key);
        let mut local = mesh_with(
            vec![member(me, "self", 100), held],
            MeshId::from_u128(9),
            [0u8; 32],
        );
        assert!(local.aliased_endpoint_keys().is_empty(), "precondition");

        // Four different records all reaching for the same key.
        let mut attackers = Vec::new();
        for n in 10..14u128 {
            let mut m = member(NodeId::from_u128(n), &format!("clone-{n}"), 100 + n as u64);
            m.node_pubkey = Some(key);
            attackers.push(m);
        }
        let remote = mesh_with(attackers, MeshId::from_u128(9), [0u8; 32]);

        let report = local.merge_from(me, &remote);
        assert_eq!(report.aliased_refused(), 4);
        assert!(
            local.aliased_endpoint_keys().is_empty(),
            "the checker must find nothing the guard let through"
        );
    }
}
