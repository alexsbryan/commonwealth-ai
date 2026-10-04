// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mesh store's tests: the doors over a real listener, a restart that
//! rebuilds the store from the journal, and the pump's append + seal.

use std::sync::Arc;

use commonwealth_core::capabilities::OriginKind;
use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use commonwealth_rail::{Ed25519Verifier, RailAct, RingRail, SigningKey};
use commonwealth_state::rail_kv::{self, SEAL_AFTER_OWN_OPS};
use tokio::sync::RwLock;

use super::{KvHost, PumpOutcome};
use crate::rail::MembershipRosterSource;

pub(super) const ME: u128 = 0xA11CE;
const NS: &str = "kv-test";

fn key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

fn pubkey() -> NodePubkey {
    commonwealth_transport::identity::node_pubkey(&key())
}

/// A mesh holding only this node, keyed — so membership, the default roster,
/// admits its own lines.
pub(super) fn solo_mesh() -> Arc<RwLock<Mesh>> {
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
pub(super) fn host_at(dir: &std::path::Path, mesh: &Arc<RwLock<Mesh>>) -> Arc<KvHost> {
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
        let forever = std::future::pending::<()>();
        host_kit::shell::serve([listener], vec![super::router(host)], forever)
            .await
            .unwrap();
    });
    format!("http://{addr}")
}

fn b64(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(s)
}

/// Drain until the outbox is empty, summing what each tick did.
pub(super) async fn drain(host: &KvHost) -> PumpOutcome {
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

/// A peer's op admitted through the ingest door reaches the running store
/// on the next tick's fold, not only after a restart (fp-109) — and not
/// before that fold: the door itself only journals and marks.
#[tokio::test]
async fn a_peer_op_ingested_through_the_door_is_served_after_one_fold() {
    let dir = tempfile::tempdir().unwrap();
    let peer_dir = tempfile::tempdir().unwrap();
    let mesh = solo_mesh();
    let peer_key = SigningKey::from_bytes(&[9; 32]);
    let peer_pubkey = commonwealth_transport::identity::node_pubkey(&peer_key);
    let peer = NodeId::from_u128(0xBEEF);
    {
        let mut m = mesh.write().await;
        let mut record = m.members[&NodeId::from_u128(ME)].clone();
        record.node_id = peer;
        record.name = "peer".into();
        record.node_pubkey = Some(peer_pubkey);
        m.members.insert(peer, record);
    }
    // The peer's own rail, where its op is signed and journaled.
    let peer_rail = Arc::new(RingRail::new(peer_dir.path(), Arc::new(peer_key.clone())));
    MembershipRosterSource::install(&peer_rail, &mesh, peer, Some(peer_pubkey));
    let peer_journal = peer_rail.journal(NS).unwrap();
    let peer_roster = peer_rail.roster(&peer_journal).await.unwrap();
    let payload = rail_kv::to_payload("theirs", Some(&b"peer-value"[..]), 100).unwrap();
    peer_journal
        .append(
            commonwealth_rail::RailAct::Record { payload },
            &peer_key,
            &peer_roster,
            None,
            &commonwealth_rail::Ed25519Verifier,
        )
        .unwrap();
    let (ops, _) = peer_journal.read().unwrap();
    assert_eq!(ops.len(), 1);

    let host = host_at(dir.path(), &mesh);
    let journal = host.rail.journal(NS).unwrap();
    let answer = crate::rail::ingest_answer(&journal, &host, crate::rail::IngestBody { ops });
    assert!(answer.status().is_success(), "{:?}", answer.status());
    assert!(
        host.store.get(NS, "theirs").unwrap().is_none(),
        "the door folded on its own; the fold belongs to the tick"
    );

    assert_eq!(host.project_dirty().await, 1);
    let row = host.store.get(NS, "theirs").unwrap().expect("folded");
    assert_eq!(&row.value[..], b"peer-value");
    assert_eq!(row.origin, peer);
    assert_eq!(host.project_dirty().await, 0, "the dirty set drained");
}

/// A local-only row journaled by this node survives a restart: the fresh
/// store rehydrates it through the own-journal door (fp-108), though
/// `RingRail::namespaces` never lists the namespace.
#[tokio::test]
async fn a_restart_rehydrates_a_local_only_row_from_its_own_journal() {
    const PRIVATE: &str = "portfolio-private";
    assert!(commonwealth_rail::is_local_only(PRIVATE));
    let dir = tempfile::tempdir().unwrap();
    let mesh = solo_mesh();
    let first = host_at(dir.path(), &mesh);
    let base = serve(first.clone()).await;
    reqwest::Client::new()
        .post(format!("{base}/v1/mesh/kv/entry"))
        .json(&serde_json::json!({
            "app_id": PRIVATE, "key": "holding", "value": b64("mine"), "origin": NodeId::from_u128(ME),
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        drain(&first).await.appended,
        0,
        "the door journaled the write before it answered"
    );

    let second = host_at(dir.path(), &mesh);
    assert!(second.store.get(PRIVATE, "holding").unwrap().is_none());
    assert_eq!(second.project_all_on_disk().await, 1);
    let row = second
        .store
        .get(PRIVATE, "holding")
        .unwrap()
        .expect("rehydrated");
    assert_eq!(&row.value[..], b"mine");
    assert_eq!(row.origin, NodeId::from_u128(ME));
}

/// A peer-signed op in a local-only journal is NOT merged by the restart:
/// the own-journal door takes this node's actor only. The peer is a keyed
/// member, so the roster ADMITS its line — only the actor filter stops it.
#[tokio::test]
async fn a_peer_op_in_a_local_only_journal_is_not_rehydrated() {
    const PRIVATE: &str = "portfolio-private";
    let dir = tempfile::tempdir().unwrap();
    let mesh = solo_mesh();
    let peer_key = SigningKey::from_bytes(&[9; 32]);
    let peer = NodeId::from_u128(0xBEEF);
    {
        let mut m = mesh.write().await;
        let mut record = m.members[&NodeId::from_u128(ME)].clone();
        record.node_id = peer;
        record.name = "peer".into();
        record.node_pubkey = Some(commonwealth_transport::identity::node_pubkey(&peer_key));
        m.members.insert(peer, record);
    }
    let first = host_at(dir.path(), &mesh);
    let journal = first.rail.journal(PRIVATE).unwrap();
    let roster = first.rail.roster(&journal).await.unwrap();
    let payload = rail_kv::to_payload("planted", Some(&b"theirs"[..]), 100).unwrap();
    journal
        .append(
            commonwealth_rail::RailAct::Record { payload },
            &peer_key,
            &roster,
            None,
            &commonwealth_rail::Ed25519Verifier,
        )
        .expect("the peer is in the roster, so its line is admitted");
    drop(first);

    let second = host_at(dir.path(), &mesh);
    second.project_all_on_disk().await;
    assert!(
        second.store.get(PRIVATE, "planted").unwrap().is_none(),
        "a peer's row in a local-only journal was rehydrated as ours"
    );
}

/// A local-only write through the door is journaled on this node and never
/// offered to a peer (fp-107: the outbox guard skips only rail-carried
/// namespaces; privacy lives at the wire). Watched red by restoring
/// `is_gossip_excluded` in `backend::memory`'s `enqueue`: the journal was
/// empty.
#[tokio::test]
async fn a_local_only_write_is_journaled_and_never_offered() {
    const PRIVATE: &str = "notes-private";
    assert!(commonwealth_rail::is_local_only(PRIVATE));
    let dir = tempfile::tempdir().unwrap();
    let mesh = solo_mesh();
    let host = host_at(dir.path(), &mesh);
    let base = serve(host.clone()).await;
    let me = NodeId::from_u128(ME);

    reqwest::Client::new()
        .post(format!("{base}/v1/mesh/kv/entry"))
        .json(&serde_json::json!({
            "app_id": PRIVATE, "key": "secret", "value": b64("mine"), "origin": me,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let (ops, _) = host.rail.journal(PRIVATE).unwrap().read().unwrap();
    assert_eq!(
        ops.len(),
        1,
        "the door journaled the write before it answered"
    );

    let namespaces = host.rail.namespaces().unwrap();
    assert!(
        !namespaces.iter().any(|n| n == PRIVATE),
        "never offered: {namespaces:?}"
    );
    let (for_peer, more) = host
        .rail
        .journal(PRIVATE)
        .unwrap()
        .ops_missing_from_within(
            &commonwealth_rail::Ed25519Verifier,
            &commonwealth_rail::Digest::new(),
            commonwealth_rail::NO_BUDGET,
        )
        .unwrap();
    assert!(for_peer.is_empty() && !more, "a peer is answered nothing");
}

#[tokio::test]
async fn a_door_write_is_journaled_before_its_answer_and_the_pump_seals_past_the_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let mesh = solo_mesh();
    let host = host_at(dir.path(), &mesh);
    let base = serve(host.clone()).await;
    let me = NodeId::from_u128(ME);

    // A door write is on the journal before the door answers, so the tick
    // finds nothing left to append (pc-solo-durable).
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
    let (ops, _) = host.rail.journal(NS).unwrap().read().unwrap();
    assert_eq!(ops.len(), 1, "journaled before the answer");
    let first = drain(&host).await;
    assert_eq!((first.appended, first.sealed), (0, 0));

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

/// The reading pc-rails-journal-linear's proof names, not a gate: the drain
/// of 10 and 256 rows and the ingest of as many peer ops, each on a journal
/// of ~5,000 lines. Run it with
/// `cargo test -p commonwealth-rails --lib journal_cost_reading -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "a timing reading, not a gate"]
async fn journal_cost_reading() {
    let dir = tempfile::tempdir().unwrap();
    let mesh = solo_mesh();
    let host = host_at(dir.path(), &mesh);
    let me = NodeId::from_u128(ME);
    let journal = host.rail.journal(NS).unwrap();
    let roster = host.rail.roster(&journal).await.unwrap();
    let base: Vec<RailAct> = (0..5_000)
        .map(|i| RailAct::Record {
            payload: rail_kv::to_payload(&format!("base/{i}"), Some(b"x".as_slice()), 1).unwrap(),
        })
        .collect();
    journal
        .append_all(
            base,
            host.rail.signer(),
            &roster,
            None,
            &commonwealth_rail::Ed25519Verifier,
        )
        .unwrap();
    let peer = SigningKey::from_bytes(&[9; 32]);
    let mut peer_seq = 0u64;
    let mut peer_ops = |tag: &str, n: usize| -> Vec<_> {
        (0..n)
            .map(|i| {
                peer_seq += 1;
                super::projection_tests::kv_op(
                    NS,
                    &peer,
                    peer_seq,
                    &format!("{tag}/{i}"),
                    Some(b"z".as_slice()),
                    1,
                )
            })
            .collect()
    };
    for n in [10usize, 256] {
        for i in 0..n {
            host.store
                .set(NS, &format!("w{n}/{i}"), "y".into(), me)
                .unwrap();
        }
        let t = std::time::Instant::now();
        let out = host.journal_outbox().await;
        let drain = t.elapsed();
        assert_eq!(out.appended, n, "{out:?}");

        let batch = peer_ops(&format!("b{n}"), n);
        let t = std::time::Instant::now();
        assert_eq!(journal.ingest_all(&batch).unwrap(), n);
        let ingest_all = t.elapsed();

        let singles = peer_ops(&format!("s{n}"), n);
        let t = std::time::Instant::now();
        for op in &singles {
            assert!(journal.ingest(op).unwrap());
        }
        let ingest_each = t.elapsed();
        let lines = journal.read().unwrap().0.len();
        eprintln!(
            "journal_cost_reading rows={n} log_lines={lines} drain_ms={} ingest_all_ms={} \
             ingest_each_ms={}",
            drain.as_millis(),
            ingest_all.as_millis(),
            ingest_each.as_millis()
        );
    }
}

/// **A live set over the bar seals once, not on every write after.** The
/// snapshot re-appends the whole live set above the new floor; counted
/// toward the bar, a live set of `SEAL_AFTER_OWN_OPS` or more cleared it by
/// itself and every tick that appended anything re-sealed (F13). The bar
/// counts this node's writes since its snapshot, so the next seal waits for
/// that many new writes, from the cache and after a restart alike.
#[tokio::test]
async fn a_live_set_over_the_bar_seals_once_not_on_every_write() {
    let dir = tempfile::tempdir().unwrap();
    let mesh = solo_mesh();
    let host = host_at(dir.path(), &mesh);
    let me = NodeId::from_u128(ME);
    let live = SEAL_AFTER_OWN_OPS + 5;
    for i in 0..live {
        host.store
            .set(NS, &format!("live/{i}"), "x".into(), me)
            .unwrap();
    }
    let first = drain(&host).await;
    assert_eq!(first.sealed, 1, "{first:?}");
    assert_eq!(first.snapshot_rows, live, "{first:?}");

    // One more write: the snapshot's rows do not count, so no re-seal.
    host.store.set(NS, "live/0", "y".into(), me).unwrap();
    let next = drain(&host).await;
    assert_eq!((next.appended, next.sealed), (1, 0), "{next:?}");

    // A restart has no cached base and reads the mark off the admission.
    let again = host_at(dir.path(), &mesh);
    again.project_all_on_disk().await;
    again.store.set(NS, "live/1", "y".into(), me).unwrap();
    let after_restart = drain(&again).await;
    assert_eq!(
        (after_restart.appended, after_restart.sealed),
        (1, 0),
        "{after_restart:?}"
    );

    // The bar still holds for real writes, at exactly SEAL_AFTER_OWN_OPS
    // since the mark. The padding goes straight onto the journal in one
    // batch rather than through the store and its outbox.
    let journal = again.rail.journal(NS).unwrap();
    let roster = again.rail.roster(&journal).await.unwrap();
    let pad: Vec<RailAct> = (0..SEAL_AFTER_OWN_OPS - 4)
        .map(|i| RailAct::Record {
            payload: rail_kv::to_payload(&format!("pad/{i}"), None, 1).unwrap(),
        })
        .collect();
    journal
        .append_all(
            pad,
            again.rail.signer(),
            &roster,
            None,
            &commonwealth_rail::Ed25519Verifier,
        )
        .unwrap();
    again.store.set(NS, "live/2", "z".into(), me).unwrap();
    let short = drain(&again).await;
    assert_eq!(short.sealed, 0, "one write short of the bar: {short:?}");
    again.store.set(NS, "live/3", "z".into(), me).unwrap();
    let due = drain(&again).await;
    assert_eq!(due.sealed, 1, "{due:?}");
    assert_eq!(due.snapshot_rows, live, "{due:?}");
}
