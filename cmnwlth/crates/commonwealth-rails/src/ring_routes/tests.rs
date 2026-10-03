// SPDX-License-Identifier: AGPL-3.0-or-later
//! The checkpoint and live routes (pb-mesh-exit-transport; director
//! phase-b-81 (5)): the successors of the daemon's `ring_checkpoint` unit
//! tests and `ring_live_non_durable`, whose routes are cw-rails' since
//! pb-rails-parity.

use std::sync::Arc;

use commonwealth_rail::{Digest, Op, Person, RailAct, RingRail, Roster, SignedOp, SigningKey};
use oicp_types::origin::{Admit, Framing, OriginRegistration};

use super::*;

const NS: &str = "house-expenses";
/// The live namespace a registration holds.
const LIVE_NS: &str = "ring-doc";
const OTHER_LIVE_NS: &str = "tool-lending";

fn key() -> SigningKey {
    SigningKey::from_bytes(&[11u8; 32])
}

/// A rail signing as [`key`], with [`NS`] rostered to it as Ada.
fn rail_with_roster(root: &std::path::Path) -> Arc<RingRail> {
    let rail = Arc::new(RingRail::new(root, Arc::new(key())));
    let mut members = std::collections::BTreeMap::new();
    members.insert(
        Person::from("Ada"),
        vec![commonwealth_rail::actor_of(&key())],
    );
    rail.journal(NS)
        .unwrap()
        .set_roster(&Roster::new(members))
        .unwrap();
    rail
}

async fn append_record(rail: &RingRail, amount: u64) {
    let journal = rail.journal(NS).unwrap();
    let roster = RingRail::roster(rail, &journal).await.unwrap();
    let act = RailAct::from_json(
        serde_json::json!({ "op": "record", "payload": { "kind": "expense", "amount": amount } }),
    )
    .unwrap();
    journal
        .append(
            act,
            rail.signer(),
            &roster,
            None,
            &commonwealth_rail::Ed25519Verifier,
        )
        .unwrap();
}

/// The ring routes over `rail` and `live`, with registrations holding the
/// two live namespaces, served; the address.
async fn serve(rail: Arc<RingRail>, live: Arc<LiveBuffer>) -> std::net::SocketAddr {
    let origins = OriginRegistry::new(commonwealth_media::PublishedApps::default());
    for (port, ns) in [(9900, LIVE_NS), (9901, OTHER_LIVE_NS)] {
        origins
            .register(OriginRegistration {
                alpn: format!("cwth/test-live/{port}"),
                prefixes: Vec::new(),
                port,
                admit: Admit::Members(Vec::new()),
                framing: Framing::Http,
                ttl_secs: None,
                claims: None,
                namespaces: vec![ns.to_string()],
            })
            .unwrap();
    }
    let app = host_kit::shell::mount(vec![router(RingInbound {
        rail,
        live,
        origins,
    })]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
    });
    addr
}

async fn checkpoint(addr: std::net::SocketAddr, ns: &str) -> (u16, serde_json::Value) {
    let resp = reqwest::get(format!("http://{addr}/internal/ring/checkpoint/{ns}"))
        .await
        .unwrap();
    (resp.status().as_u16(), resp.json().await.unwrap())
}

fn fresh(dir: &tempfile::TempDir) -> (Arc<RingRail>, Arc<LiveBuffer>) {
    (
        rail_with_roster(dir.path()),
        Arc::new(LiveBuffer::default()),
    )
}

#[tokio::test]
async fn a_checkpoint_of_a_live_namespace_has_the_v1_shape() {
    let dir = tempfile::tempdir().unwrap();
    let (rail, live) = fresh(&dir);
    append_record(&rail, 12).await;
    append_record(&rail, 7).await;
    let (status, doc) = checkpoint(serve(rail, live).await, NS).await;
    assert_eq!(status, 200, "{doc}");
    assert_eq!(doc["v"], 1);
    assert_eq!(doc["ns"], NS);
    assert!(doc["created_unix"].as_u64().unwrap() > 1_600_000_000);
    let ops = doc["ops"].as_array().expect("ops is an array");
    assert_eq!(ops.len(), 2, "the journal's two lines, carried");
    for line in ops {
        let parsed: serde_json::Value =
            serde_json::from_str(line.as_str().expect("a journal line, verbatim")).unwrap();
        assert!(
            parsed["seq"].is_u64(),
            "a journal line, not a summary: {parsed}"
        );
        assert!(parsed["sig"].is_string(), "signed, as written: {parsed}");
    }
}

#[tokio::test]
async fn the_stated_digest_is_what_the_carried_ops_compute_to() {
    let dir = tempfile::tempdir().unwrap();
    let (rail, live) = fresh(&dir);
    for amount in [12, 7, 3] {
        append_record(&rail, amount).await;
    }
    let (_, doc) = checkpoint(serve(rail, live).await, NS).await;
    let ops: Vec<Op<SignedOp>> = doc["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| serde_json::from_str(l.as_str().unwrap()).unwrap())
        .collect();
    let stated: Digest = serde_json::from_value(doc["digest"].clone()).unwrap();
    assert_eq!(
        commonwealth_rail::digest(&ops, NS, &commonwealth_rail::Ed25519Verifier),
        stated,
        "the completeness claim: every act through every mark it names"
    );
}

#[tokio::test]
async fn the_roster_the_append_path_uses_is_embedded() {
    let dir = tempfile::tempdir().unwrap();
    let (rail, live) = fresh(&dir);
    append_record(&rail, 12).await;
    let (_, doc) = checkpoint(serve(rail.clone(), live).await, NS).await;
    let embedded: Roster = serde_json::from_value(doc["roster"].clone()).unwrap();
    let journal = rail.journal(NS).unwrap();
    assert_eq!(embedded, RingRail::roster(&rail, &journal).await.unwrap());
    assert_eq!(
        embedded
            .members
            .get(&Person::from("Ada"))
            .map(Vec::as_slice),
        Some(&[commonwealth_rail::actor_of(&key())][..]),
        "the signer that wrote the act is named in it"
    );
}

#[tokio::test]
async fn a_namespace_this_node_does_not_hold_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let (rail, live) = fresh(&dir);
    let (status, doc) = checkpoint(serve(rail.clone(), live).await, "no-such-ring").await;
    assert_eq!(status, 404);
    assert!(
        doc["error"].as_str().unwrap().contains("no-such-ring"),
        "{doc}"
    );
    assert!(
        !rail
            .namespaces()
            .unwrap()
            .iter()
            .any(|n| n == "no-such-ring"),
        "a refused namespace must not be created on touch"
    );
}

#[tokio::test]
async fn a_journal_read_error_is_refused_and_not_silently_empty() {
    let dir = tempfile::tempdir().unwrap();
    let (rail, live) = fresh(&dir);
    let journal = rail.journal(NS).unwrap();
    let file = <SignedOp as commonwealth_rail::Journaled>::FILE;
    std::fs::create_dir(journal.dir().join(file)).unwrap();
    let (status, doc) = checkpoint(serve(rail, live).await, NS).await;
    assert_eq!(status, 500);
    assert!(
        !doc["error"].as_str().unwrap().is_empty(),
        "the read's own sentence"
    );
}

fn envelope(namespace: &str, payload: &str) -> String {
    serde_json::json!({ "namespace": namespace, "payload": payload }).to_string()
}

async fn push(addr: std::net::SocketAddr, namespace: &str, payload: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{addr}/internal/ring/live"))
        .body(envelope(namespace, payload))
        .send()
        .await
        .unwrap()
}

/// Every file under `dir`, with its length.
fn snapshot(dir: &std::path::Path) -> std::collections::BTreeMap<String, u64> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path).unwrap() {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                let rel = entry
                    .path()
                    .strip_prefix(dir)
                    .unwrap()
                    .display()
                    .to_string();
                out.insert(rel, meta.len());
            }
        }
    }
    out
}

/// The daemon's `a_live_payload_touches_nothing_on_disk`: three pushes drain
/// back exactly, in order, and no byte under the rail's directory changed.
#[tokio::test]
async fn a_live_payload_touches_nothing_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (rail, live) = fresh(&dir);
    let before = snapshot(dir.path());
    assert!(!before.is_empty(), "control: the roster file is on disk");
    let addr = serve(rail, live.clone()).await;
    let payloads = ["AQID", "BAUG", "BwgJ"];
    for p in payloads {
        let sent = push(addr, LIVE_NS, p).await;
        assert_eq!(sent.status(), 200, "{}", sent.text().await.unwrap());
    }
    assert_eq!(
        live.drain(LIVE_NS),
        (payloads.map(String::from).to_vec(), 0)
    );
    assert_eq!(snapshot(dir.path()), before, "the live lane wrote to disk");
}

/// The daemon's `a_restart_empties_the_buffer`: a process over the same
/// directory drains nothing it did not receive.
#[tokio::test]
async fn a_restart_empties_the_buffer() {
    let dir = tempfile::tempdir().unwrap();
    let (rail, live) = fresh(&dir);
    let addr = serve(rail, live.clone()).await;
    assert_eq!(push(addr, LIVE_NS, "AQID").await.status(), 200);
    let restarted = Arc::new(LiveBuffer::default());
    let _ = serve(rail_with_roster(dir.path()), restarted.clone()).await;
    assert_eq!(restarted.drain(LIVE_NS), (Vec::new(), 0));
    assert_eq!(
        live.drain(LIVE_NS),
        (vec!["AQID".to_string()], 0),
        "control: the first process held it"
    );
}

/// The buffer half of the daemon's `a_grant_drains_only_its_own_namespace`
/// (the grant half is svrn's, `rail_e2e::live_drain`): what arrived under
/// one namespace is drained under it and no other.
#[tokio::test]
async fn a_drain_takes_only_its_own_namespace() {
    let dir = tempfile::tempdir().unwrap();
    let (rail, live) = fresh(&dir);
    let addr = serve(rail, live.clone()).await;
    for p in ["AQID", "BAUG"] {
        assert_eq!(push(addr, LIVE_NS, p).await.status(), 200);
    }
    assert_eq!(live.drain(OTHER_LIVE_NS), (Vec::new(), 0));
    assert_eq!(
        live.drain(LIVE_NS),
        (vec!["AQID".to_string(), "BAUG".to_string()], 0)
    );
}

/// The daemon's `an_envelope_for_an_unknown_namespace_is_refused`: no live
/// registration holds it, so nobody here could drain it; refused by name
/// and buffered nowhere.
#[tokio::test]
async fn an_envelope_for_an_unknown_namespace_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (rail, live) = fresh(&dir);
    let addr = serve(rail, live.clone()).await;
    let unknown = "nobody-registered-this";
    let resp = push(addr, unknown, "AQID").await;
    assert_eq!(resp.status(), 403);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains(unknown),
        "the refusal names the namespace: {body}"
    );
    assert_eq!(live.drain(unknown), (Vec::new(), 0));
    assert_eq!(live.drain(LIVE_NS), (Vec::new(), 0));
}

/// A caller with no stamped key reached this loopback port without the
/// acceptor in front — a process on this machine — and is served; a stamp
/// that is not a key is refused by name. The acceptor's half (a stranger
/// never reaches `/internal/ring`) is commonwealth-media
/// `origins::cwth_http_forwards_by_registered_prefix`. Successor of the
/// daemon's `rail_e2e/roster_refusal` pair, whose per-process mark retired
/// with the daemon's acceptor.
#[tokio::test]
async fn an_unstamped_local_caller_is_served_and_a_bad_stamp_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (rail, live) = fresh(&dir);
    let addr = serve(rail, live).await;
    let ask = |stamp: Option<&str>| {
        let mut req = reqwest::Client::new()
            .post(format!("http://{addr}/internal/ring/sync"))
            .json(&serde_json::json!({ "namespace": NS, "digest": Digest::default(), "ops": [] }));
        if let Some(s) = stamp {
            req = req.header(PUBKEY_HEADER, s);
        }
        req.send()
    };
    assert_eq!(ask(None).await.unwrap().status(), 200);
    let refused = ask(Some("not-a-key")).await.unwrap();
    assert_eq!(refused.status(), 403);
    assert!(refused.text().await.unwrap().contains(NS));
}
