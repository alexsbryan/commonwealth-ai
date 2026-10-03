//! The work atlas's two namespaces through this store's replication
//! chokepoints: node A's outbox, the KV payload, node B's projection
//! (pb-mesh-dissolve, phase-b-92).
//!
//! The atlas and this store meet at two app ids and nowhere else. No
//! production path composes `WorkAtlasStore` with `MeshStore` in-process:
//! every atlas outside tests dials cw-rails' KV. So the joint is pinned here
//! by the constants both sides name, `sovereign_contracts::peer::
//! WORK_ATLAS_APP_ID_*` (the atlas's `Privacy::app_id()` returns them), and
//! the atlas's record semantics are pinned over the port in
//! sovereign-work-atlas `tests/port_fake.rs`. The middle, signing onto a ring
//! journal, is the rail's and is not repeated here.

use super::*;
use crate::peer_preferences::{is_gossip_excluded, is_rail_carried};
use crate::rail_kv;
use commonwealth_core::ids::NodeId;
use sovereign_contracts::peer::{WORK_ATLAS_APP_ID_PRIVATE, WORK_ATLAS_APP_ID_PUBLIC};
use std::collections::BTreeMap;

fn node(n: u128) -> NodeId {
    NodeId::from_u128(n)
}

/// One round of the only replication path: everything `src` queued, through
/// the KV payload the pump signs, into `dst`'s projection. Returns the
/// namespaces it offered.
///
/// `outbox_take` leaves the rows (the pump acks separately), so a second call
/// replays the whole queue, which is the ring's anti-entropy. A local-only row
/// IS queued (fp-107) and is skipped the way the ring skips it:
/// `RingRail::namespaces` never offers one.
fn replicate(src: &MeshStore, dst: &MeshStore, src_node: NodeId, dst_node: NodeId) -> Vec<String> {
    let mut by_namespace: BTreeMap<String, Vec<rail_kv::Projected>> = BTreeMap::new();
    for row in src.outbox_take(4096).expect("drain the outbox") {
        assert!(
            !is_rail_carried(&row.app_id),
            "a rail-carried app_id '{}' was queued for the KV rail",
            row.app_id
        );
        if commonwealth_rail_core::is_local_only(&row.app_id) {
            continue;
        }
        // Through the wire vocabulary rather than around it: a value that
        // does not survive `to_payload`/`from_payload` reaches no peer.
        let payload = rail_kv::to_payload(&row.op.key, row.op.value.as_deref(), row.op.t)
            .expect("KV payload");
        let op = rail_kv::from_payload(&payload).expect("this build reads what it wrote");
        by_namespace
            .entry(row.app_id)
            .or_default()
            .push(rail_kv::Projected {
                key: op.key,
                value: op.value,
                t: op.t,
                actor: src_node.to_hex(),
            });
    }
    for (app_id, rows) in &by_namespace {
        let projection = rail_kv::Projection {
            rows: rows.clone(),
            ..Default::default()
        };
        dst.apply_projection(app_id, &projection, |_| Some(src_node), dst_node)
            .expect("project");
    }
    by_namespace.into_keys().collect()
}

/// A claim-shaped record, as the atlas stores it: JSON under `claim:<id>`.
const CLAIM: &[u8] = br#"{"claim_id":"c1","intent":"tuning fanout","received_at":null}"#;

#[test]
fn the_work_atlas_app_ids_are_classified_by_the_contract_constants() {
    assert!(is_gossip_excluded(WORK_ATLAS_APP_ID_PRIVATE));
    assert!(commonwealth_rail_core::is_local_only(
        WORK_ATLAS_APP_ID_PRIVATE
    ));
    assert!(!is_gossip_excluded(WORK_ATLAS_APP_ID_PUBLIC));
    assert!(!commonwealth_rail_core::is_local_only(
        WORK_ATLAS_APP_ID_PUBLIC
    ));
}

#[test]
fn a_public_work_atlas_row_crosses_byte_identical() {
    let (a, b) = (
        MeshStore::in_memory().unwrap(),
        MeshStore::in_memory().unwrap(),
    );
    a.set(
        WORK_ATLAS_APP_ID_PUBLIC,
        "claim:c1",
        Bytes::from_static(CLAIM),
        node(0xA),
    )
    .unwrap();
    assert!(b
        .get(WORK_ATLAS_APP_ID_PUBLIC, "claim:c1")
        .unwrap()
        .is_none());

    let offered = replicate(&a, &b, node(0xA), node(0xB));
    assert_eq!(offered, vec![WORK_ATLAS_APP_ID_PUBLIC.to_string()]);
    let got = b
        .get(WORK_ATLAS_APP_ID_PUBLIC, "claim:c1")
        .unwrap()
        .expect("the public row reached B");
    assert_eq!(got.value, Bytes::from_static(CLAIM));
    assert_eq!(got.origin, node(0xA), "B files the row under its author");
}

#[test]
fn a_private_work_atlas_row_is_never_offered_and_never_lands() {
    let (a, b) = (
        MeshStore::in_memory().unwrap(),
        MeshStore::in_memory().unwrap(),
    );
    a.set(
        WORK_ATLAS_APP_ID_PRIVATE,
        "claim:secret",
        Bytes::from_static(CLAIM),
        node(0xA),
    )
    .unwrap();
    // The control: the write happened, on A.
    assert!(a
        .get(WORK_ATLAS_APP_ID_PRIVATE, "claim:secret")
        .unwrap()
        .is_some());

    let offered = replicate(&a, &b, node(0xA), node(0xB));
    assert!(offered.is_empty(), "offered {offered:?}");
    assert!(b.scan(WORK_ATLAS_APP_ID_PRIVATE, "").unwrap().is_empty());

    // The receiver half: a peer that offers the namespace anyway is refused
    // by name, and B still holds nothing under it.
    let leaked = rail_kv::Projection {
        rows: vec![rail_kv::Projected {
            key: "claim:secret".into(),
            value: Some(Bytes::from_static(CLAIM)),
            t: now_secs(),
            actor: node(0xA).to_hex(),
        }],
        ..Default::default()
    };
    let err = b
        .apply_projection(
            WORK_ATLAS_APP_ID_PRIVATE,
            &leaked,
            |_| Some(node(0xA)),
            node(0xB),
        )
        .expect_err("a private namespace must be refused, not applied");
    assert!(err.to_string().contains(WORK_ATLAS_APP_ID_PRIVATE), "{err}");
    assert!(b.scan(WORK_ATLAS_APP_ID_PRIVATE, "").unwrap().is_empty());
}

#[test]
fn a_released_work_atlas_row_crosses_as_a_tombstone() {
    let (a, b) = (
        MeshStore::in_memory().unwrap(),
        MeshStore::in_memory().unwrap(),
    );
    a.set(
        WORK_ATLAS_APP_ID_PUBLIC,
        "claim:c1",
        Bytes::from_static(CLAIM),
        node(0xA),
    )
    .unwrap();
    replicate(&a, &b, node(0xA), node(0xB));

    assert!(a.delete(WORK_ATLAS_APP_ID_PUBLIC, "claim:c1").unwrap());
    // The control: B holds it until a round carries the tombstone.
    assert!(b
        .get(WORK_ATLAS_APP_ID_PUBLIC, "claim:c1")
        .unwrap()
        .is_some());

    replicate(&a, &b, node(0xA), node(0xB));
    assert!(
        b.get(WORK_ATLAS_APP_ID_PUBLIC, "claim:c1")
            .unwrap()
            .is_none(),
        "the release travelled as a tombstone"
    );
}
