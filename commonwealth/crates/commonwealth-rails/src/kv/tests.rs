// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mesh store's tests: the doors over a real listener, a restart that
//! rebuilds the store from the journal, and the pump's append + seal.

use std::sync::Arc;

use commonwealth_core::capabilities::OriginKind;
use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use commonwealth_rail::{Ed25519Verifier, RingRail, SigningKey};
use commonwealth_state::rail_kv::{self, SEAL_AFTER_OWN_OPS};
use tokio::sync::RwLock;

use super::{KvHost, PumpOutcome};
use crate::rail::MembershipRosterSource;

const ME: u128 = 0xA11CE;
const NS: &str = "kv-test";

fn key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

fn pubkey() -> NodePubkey {
    commonwealth_transport::identity::node_pubkey(&key())
}

/// A mesh holding only this node, keyed — so membership, the default roster,
/// admits its own lines.
fn solo_mesh() -> Arc<RwLock<Mesh>> {
    let (mut mesh, _key) =
        commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
    mesh.members.clear();
    let me = NodeId::from_u128(ME);
    mesh.members.insert(
        me,
        MemberRecord {
            node_id: me,
            name: "me".into(),
            invited_by: me,
            joined_at: 100,
            last_seen: 100,
            status: NodeStatus::Online,
            capabilities: crate::gossip::minimal_capabilities(100, &[OriginKind::Media], None),
            addresses: Vec::new(),
            node_pubkey: Some(pubkey()),
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
            removed_at: None,
        },
    );
    Arc::new(RwLock::new(mesh))
}

/// A host over a rail rooted at `dir` — built the way `RailsDaemon::start`
/// builds it. Calling it twice on one dir is a restart.
fn host_at(dir: &std::path::Path, mesh: &Arc<RwLock<Mesh>>) -> Arc<KvHost> {
    let rail = Arc::new(RingRail::new(dir, Arc::new(key())));
    let me = NodeId::from_u128(ME);
    MembershipRosterSource::install(&rail, mesh, me, Some(pubkey()));
    Arc::new(KvHost::new(rail, mesh.clone(), me, Some(pubkey())).unwrap())
}

/// Serve the doors on an ephemeral loopback port.
async fn serve(host: Arc<KvHost>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, super::router(host)).await.unwrap();
    });
    format!("http://{addr}")
}

fn b64(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(s)
}

/// Drain until the outbox is empty, summing what each tick did.
async fn drain(host: &KvHost) -> PumpOutcome {
    let mut total = PumpOutcome::default();
    loop {
        let out = host.pump_once().await;
        if out == PumpOutcome::default() {
            return total;
        }
        total.appended += out.appended;
        total.deferred += out.deferred;
        total.refused += out.refused;
        total.sealed += out.sealed;
        total.snapshot_rows += out.snapshot_rows;
        assert_eq!(out.deferred + out.refused, 0, "{out:?}");
    }
}

#[tokio::test]
async fn set_then_scan_round_trips_over_the_doors() {
    let dir = tempfile::tempdir().unwrap();
    let mesh = solo_mesh();
    let base = serve(host_at(dir.path(), &mesh)).await;
    let http = reqwest::Client::new();
    let origin = NodeId::from_u128(ME);

    let changed: bool = http
        .post(format!("{base}/v1/mesh/kv/entry"))
        .json(&serde_json::json!({
            "app_id": NS, "key": "claims/a", "value": b64("hello"), "origin": origin,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(changed);

    let rows: Vec<serde_json::Value> = http
        .get(format!(
            "{base}/v1/mesh/kv/entries?app_id={NS}&prefix=claims/"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["key"], "claims/a");
    assert_eq!(rows[0]["value"], b64("hello"));
    assert_eq!(rows[0]["origin"], serde_json::json!(origin));

    let one: serde_json::Value = http
        .get(format!("{base}/v1/mesh/kv/entry?app_id={NS}&key=claims/a"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(one["value"], b64("hello"));

    let deleted: bool = http
        .delete(format!("{base}/v1/mesh/kv/entry?app_id={NS}&key=claims/a"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(deleted);
    let absent: serde_json::Value = http
        .get(format!("{base}/v1/mesh/kv/entry?app_id={NS}&key=claims/a"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(absent.is_null(), "an absent key is `null`, not an error");
}

#[tokio::test]
async fn a_restart_rehydrates_a_row_from_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let mesh = solo_mesh();
    let first = host_at(dir.path(), &mesh);
    first
        .store
        .set(NS, "k", "v".into(), NodeId::from_u128(ME))
        .unwrap();
    assert_eq!(drain(&first).await.appended, 1);
    drop(first);

    // A new process: an empty in-memory store over the same journals.
    let second = host_at(dir.path(), &mesh);
    assert!(second.store.get(NS, "k").unwrap().is_none());
    assert_eq!(second.project_all_on_disk().await, 1);
    let row = second.store.get(NS, "k").unwrap().expect("rehydrated");
    assert_eq!(&row.value[..], b"v");
    assert_eq!(row.origin, NodeId::from_u128(ME));
}

#[tokio::test]
async fn the_pump_appends_a_door_write_and_seals_past_the_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let mesh = solo_mesh();
    let host = host_at(dir.path(), &mesh);
    let base = serve(host.clone()).await;
    let me = NodeId::from_u128(ME);

    // A door write reaches the journal on the next tick.
    reqwest::Client::new()
        .post(format!("{base}/v1/mesh/kv/entry"))
        .json(&serde_json::json!({
            "app_id": NS, "key": "live/door", "value": b64("d"), "origin": me,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let first = drain(&host).await;
    assert_eq!((first.appended, first.sealed), (1, 0));

    // Two more live rows, then set+delete pairs until this node's own ops
    // cross the threshold: many lines, three live rows.
    for k in ["live/a", "live/b"] {
        host.store.set(NS, k, "x".into(), me).unwrap();
    }
    let mut i = 0u64;
    while 3 + 2 * i < SEAL_AFTER_OWN_OPS as u64 {
        let k = format!("pad/{i}");
        host.store.set(NS, &k, "p".into(), me).unwrap();
        assert!(host.store.delete(NS, &k).unwrap());
        i += 1;
    }
    let out = drain(&host).await;
    assert_eq!(out.sealed, 1, "{out:?}");
    assert_eq!(
        out.snapshot_rows, 3,
        "the live rows re-appended, no padding: {out:?}"
    );

    // Above the new floor: exactly the three live rows and the mark.
    let journal = host.rail.journal(NS).unwrap();
    let roster = host.rail.roster(&journal).await.unwrap();
    let admission = journal.admit(&roster, &Ed25519Verifier).unwrap();
    let projection = rail_kv::project(&admission);
    let mut live: Vec<&str> = projection
        .rows
        .iter()
        .filter(|r| r.value.is_some())
        .map(|r| r.key.as_str())
        .collect();
    live.sort();
    assert_eq!(live, ["live/a", "live/b", "live/door"]);

    // And a restart reads them back from what the seal left on disk.
    let again = host_at(dir.path(), &mesh);
    again.project_all_on_disk().await;
    assert_eq!(again.store.scan(NS, "live/").unwrap().len(), 3);
    assert!(again.store.scan(NS, "pad/").unwrap().is_empty());
}
