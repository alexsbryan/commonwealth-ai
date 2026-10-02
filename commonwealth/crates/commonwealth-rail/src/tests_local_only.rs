// SPDX-License-Identifier: AGPL-3.0-or-later
//! A local-only namespace is journaled on this node and never offered to a
//! peer; a ring-carried one still is (five-programs-37).

use crate::*;

use commonwealth_rail_core::tests_support::*;

const LOCAL: &str = "notes-private";
const CARRIED: &str = "mesh-measurements";

/// Every local-only namespace is journalable: a directory name
/// `valid_namespace` accepts (fp-106; ARCH 10).
#[test]
fn every_local_only_namespace_is_a_valid_journal_name() {
    for ns in LOCAL_ONLY_NAMESPACES {
        assert!(valid_namespace(ns), "`{ns}` cannot name a journal");
    }
}

/// Watched red by deleting the local-only skip from `RingRail::namespaces`.
#[test]
fn a_local_only_journal_is_held_but_never_offered() {
    assert!(is_local_only(LOCAL) && !is_local_only(CARRIED));
    let dir = tempfile::tempdir().unwrap();
    let rail = RingRail::new(dir.path(), Arc::new(key(1)));
    let local = rail.journal(LOCAL).unwrap();
    let carried = rail.journal(CARRIED).unwrap();
    local
        .append(record("mine"), &key(1), &ring(), None, &Ed25519Verifier)
        .unwrap();
    carried
        .append(record("shared"), &key(1), &ring(), None, &Ed25519Verifier)
        .unwrap();

    assert_eq!(local.read().unwrap().0.len(), 1, "journaled on this node");
    assert_eq!(rail.namespaces().unwrap(), vec![CARRIED.to_string()]);

    let (for_peer, more) = local
        .ops_missing_from_within(&Ed25519Verifier, &Digest::new(), NO_BUDGET)
        .unwrap();
    assert!(for_peer.is_empty() && !more, "a peer is answered nothing");
    let (for_peer, _) = carried
        .ops_missing_from_within(&Ed25519Verifier, &Digest::new(), NO_BUDGET)
        .unwrap();
    assert_eq!(
        for_peer.len(),
        1,
        "control: the carried namespace is answered"
    );
}
