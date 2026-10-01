// SPDX-License-Identifier: AGPL-3.0-or-later
//! The gossip selection properties cw-rails' `select_peers` holds, as
//! successors of sovereign-mesh gossip.rs' selection tests, which retired
//! with that round (pb-mesh-exit-transport; phase-b-81 (4), phase-b-82).
//! The properties specific to the deleted algorithm are ledgered D. With
//! them, the decay property the daemon's gossip_integration tests carried.

use super::*;

/// Decay reads this node's contact clock, never the peer's record: a peer
/// that answers stays Online whether its gossiped `last_seen` is frozen far
/// in the past or skewed into the future. Successor of the daemon's
/// `answering_peer_whose_record_is_frozen_must_not_decay` and
/// `gossip_skewed_last_seen_does_not_false_decay`. Failing input: decay on
/// `now - last_seen`.
#[test]
fn an_answering_peer_does_not_decay_whatever_its_record_says() {
    let mut frozen = member(0x01, "frozen", NodeStatus::Online, true);
    frozen.last_seen = 1;
    let mut skewed = member(0x02, "skewed", NodeStatus::Online, true);
    skewed.last_seen = 1_000_000;
    let mut mesh = mesh_of(vec![
        member(ME, "me", NodeStatus::Online, true),
        frozen,
        skewed,
    ]);
    let now = 10_000;
    let contacts: HashMap<NodeId, u64> = [
        (NodeId::from_u128(0x01), now - 5),
        (NodeId::from_u128(0x02), now - 5),
    ]
    .into();
    decay(&mut mesh, NodeId::from_u128(ME), now, 60, &contacts);
    for id in [0x01, 0x02] {
        assert_eq!(
            mesh.members[&NodeId::from_u128(id)].status,
            NodeStatus::Online
        );
    }
}

/// A tombstoned row is never a candidate, even while its stale record still
/// reads Online, and a retired twin sharing a live member's key leaves the
/// live row in. Successor of `a_tombstoned_member_is_not_dialed`,
/// `a_tombstone_is_excluded_even_while_it_still_reads_online` and
/// `a_retired_twin_sharing_an_endpoint_key_is_dropped_and_the_live_row_kept`.
/// Failing input: the `is_active` filter dropped.
#[test]
fn a_tombstoned_member_is_never_dialed_even_while_it_reads_online() {
    let mut twin = member(0x02, "twin", NodeStatus::Online, true);
    twin.node_pubkey = member(0x03, "live", NodeStatus::Online, true).node_pubkey;
    twin.removed_at = Some(1);
    let mesh = mesh_of(vec![
        member(ME, "me", NodeStatus::Online, true),
        twin,
        member(0x03, "live", NodeStatus::Online, true),
    ]);
    for round in 0..4 {
        let names: Vec<String> = select_peers(&mesh, NodeId::from_u128(ME), round)
            .into_iter()
            .map(|(_, n, _)| n)
            .collect();
        assert_eq!(names, vec!["live".to_string()], "round {round}");
    }
}

/// Selection reads reachability (status, key, tombstone) and never
/// capability: two members that differ only in what they advertise are
/// picked alike. Successor of `selection_reads_reachability_only_never_capability`.
#[test]
fn selection_reads_reachability_only_never_capability() {
    let mut rich = member(0x01, "rich", NodeStatus::Online, true);
    rich.capabilities.inference_capable = true;
    rich.capabilities.available.available_for_mesh = true;
    rich.capabilities.hardware.system_ram_gb = 512;
    let mesh = mesh_of(vec![
        member(ME, "me", NodeStatus::Online, true),
        rich,
        member(0x02, "bare", NodeStatus::Online, true),
    ]);
    let names: Vec<String> = select_peers(&mesh, NodeId::from_u128(ME), 0)
        .into_iter()
        .map(|(_, n, _)| n)
        .collect();
    assert_eq!(names, vec!["rich".to_string(), "bare".to_string()]);
}

/// A candidate list shorter than the fan is dialed whole, each once, every
/// distinct key kept; an empty one (`a_mesh_of_one_has_nobody_to_dial`)
/// dials nobody. Successor of
/// `short_candidate_lists_and_an_empty_mesh_are_handled` and
/// `distinct_endpoint_keys_are_all_kept`.
#[test]
fn a_short_candidate_list_is_dialed_whole_and_once() {
    let mesh = mesh_of(vec![
        member(ME, "me", NodeStatus::Online, true),
        member(0x01, "a", NodeStatus::Online, true),
        member(0x02, "b", NodeStatus::Offline, true),
    ]);
    for round in 0..3 {
        let mut names: Vec<String> = select_peers(&mesh, NodeId::from_u128(ME), round)
            .into_iter()
            .map(|(_, n, _)| n)
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["a".to_string(), "b".to_string()],
            "round {round}"
        );
    }
}
