// SPDX-License-Identifier: AGPL-3.0-or-later
//! `build_node_roster` names authors from the membership reader (cw-rails'
//! roster), never from svrn's frozen `mesh.json` (seat phase-b-84).

use corpus_index::types::NodeAttribution;
use kernel_types::NodeId;

use super::build_node_roster;
use crate::double::{member, roster};

/// A member the reader names resolves to the reader's name, self and peer
/// alike. Failing input, named: read names from anywhere but the reader
/// (svrn's `mesh.json`, which the test never writes) and both resolve as
/// unknown.
#[tokio::test]
async fn the_roster_names_authors_from_the_membership_reader() {
    let me = NodeId::from_u128(1);
    let peer = NodeId::from_u128(2);
    let reader = roster("m", vec![member(me, "workstation"), member(peer, "laptop")]);
    let built = build_node_roster(reader.as_ref(), me)
        .await
        .expect("a reader with members builds a roster");
    assert!(matches!(
        built.resolve(&me.to_hex()),
        NodeAttribution::SelfNode { name } if name == "workstation"
    ));
    assert!(matches!(
        built.resolve(&peer.to_hex()),
        NodeAttribution::Peer { name, .. } if name == "laptop"
    ));
}

/// A reader that names no one (cw-rails down, or no reader composed) builds
/// no roster: authors degrade to raw ids under the named warn.
#[tokio::test]
async fn a_reader_that_names_no_member_builds_no_roster() {
    let reader = roster("m", Vec::new());
    assert!(build_node_roster(reader.as_ref(), NodeId::from_u128(1))
        .await
        .is_none());
}
