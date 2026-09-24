// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rail doors' tests. The fixtures mirror `gossip.rs`'s, and the rules
//! they pin are the ones the derivation above names — each one a rule the
//! inference daemon's `MeshRoster` tests pin there, re-proved here because
//! this copy is the one a rails journal lives under.

use std::sync::Arc;

use axum::http::StatusCode;
use commonwealth_core::capabilities::OriginKind;
use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use commonwealth_rail::{Person, RailAct, RingRail, RingSigner, SigningKey};
use tokio::sync::RwLock;

use super::{
    append_act, derive_roster, drain_answer, journal_of, push_answer, LiveBuffer,
    MembershipRosterSource,
};

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn pubkey_of(k: &SigningKey) -> NodePubkey {
    commonwealth_transport::identity::node_pubkey(k)
}

fn member(id: u128, name: &str, key: Option<NodePubkey>, removed: bool) -> MemberRecord {
    MemberRecord {
        node_id: NodeId::from_u128(id),
        name: name.into(),
        invited_by: NodeId::from_u128(1),
        joined_at: 100,
        last_seen: 100,
        status: NodeStatus::Online,
        capabilities: crate::gossip::minimal_capabilities(100, &[OriginKind::Media], None),
        addresses: Vec::new(),
        node_pubkey: key,
        relay_url: None,
        iroh_direct_addrs: Vec::new(),
        dial_info_version: 0,
        dial_info_sig: None,
        removed_at: removed.then(|| 200),
    }
}

fn mesh_with(records: Vec<MemberRecord>) -> Arc<RwLock<Mesh>> {
    let (mut mesh, _key) =
        commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
    mesh.members.clear();
    for r in records {
        mesh.members.insert(r.node_id, r);
    }
    Arc::new(RwLock::new(mesh))
}

const ME: u128 = 0xA11CE;

/// A rail over a tempdir whose default roster is the mesh's membership.
/// The tempdir and the mesh Arc are returned BY VALUE and must be kept:
/// the rail writes under the dir for as long as it lives, and the roster
/// source holds the mesh only WEAKLY — the caller keeps it alive, exactly
/// as [`crate::RailsDaemon`] does in production.
fn rail_with(mesh: Arc<RwLock<Mesh>>) -> (tempfile::TempDir, Arc<RingRail>, Arc<RwLock<Mesh>>) {
    let dir = tempfile::tempdir().unwrap();
    let k = key(7);
    let rail = Arc::new(RingRail::new(dir.path(), Arc::new(k.clone())));
    MembershipRosterSource::install(&rail, &mesh, NodeId::from_u128(ME), Some(pubkey_of(&k)));
    (dir, rail, mesh)
}

async fn body_of(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// **The bridge, in one line** — the same equality the daemon's roster
/// tests pin there: a member row's advertised pubkey and the `actor` this
/// door signs with are the same 64 characters, because both spell
/// `hex(verifying_key)` over the SAME key. A rail this node writes must
/// verify under the roster every peer already holds.
#[test]
fn the_node_pubkey_and_the_rail_actor_are_the_same_spelling() {
    let k = key(7);
    assert_eq!(pubkey_of(&k).to_string(), RingSigner::actor(&k));
}

/// Tombstones kept; the self key passed in beats an unstamped row; a blank
/// name falls back to the node id; a row with no key is absent, never
/// defaulted.
#[test]
fn the_roster_follows_the_membership_rules() {
    let departed = key(1);
    let blank = key(2);
    let mesh = mesh_with(vec![
        member(ME, "alex", None, false),
        member(2, "beefy", Some(pubkey_of(&departed)), true),
        member(3, "   ", Some(pubkey_of(&blank)), false),
        member(4, "halo", None, false),
    ]);
    let mesh = mesh.try_read().unwrap().clone();
    let k = key(7);
    let roster = derive_roster(&mesh, NodeId::from_u128(ME), Some(pubkey_of(&k)));
    // The self key is in, though the row carries no stamp yet.
    assert_eq!(
        roster.person_for(&RingSigner::actor(&k)).unwrap().as_str(),
        "alex"
    );
    // The departed member's key is still claimed — its history counts.
    let beefy_key = format!("{}", pubkey_of(&departed));
    assert_eq!(
        roster.person_for(&beefy_key).unwrap().as_str(),
        "beefy",
        "a tombstone must not retire the lines it already signed"
    );
    // A blank name renders as the node id.
    let blank_key = format!("{}", pubkey_of(&blank));
    assert_eq!(
        roster.person_for(&blank_key).unwrap().as_str(),
        NodeId::from_u128(3).to_string(),
    );
    // No placeholder for the unidentified row: its name is not a roster row
    // and its node id claims no key.
    assert!(!roster.knows(&Person::from("halo")));
    assert!(roster
        .person_for(&NodeId::from_u128(4).to_string())
        .is_none());
}

/// One act in, one admitted op out — and every field a client could try to
/// choose for itself is this door's, not the body's.
#[tokio::test]
async fn append_signs_as_the_node_and_assigns_the_fields() {
    let mesh = mesh_with(vec![member(ME, "alex", None, false)]);
    let (_dir, rail, _mesh) = rail_with(mesh);
    let journal = journal_of(&rail, "ledger").unwrap();
    let body = serde_json::json!({
        "op": "record",
        "payload": { "amount": 4 },
        // Every one of these is the server's to assign; a body that could
        // choose them could write as somebody else.
        "seq": 999,
        "id": "forged",
        "ts_unix": 1,
        "on_behalf_of": "beefy",
    });
    let resp = append_act(&rail, &journal, body).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let out = body_of(resp).await;
    assert_eq!(out["seq"], 0, "the door assigns the sequence, not the body");
    assert_eq!(out["actor"], RingSigner::actor(&key(7)));
    assert!(out["ts_unix"].as_i64().unwrap_or(0) > 0);
    assert!(out["id"].as_str().is_some());
    assert_eq!(out["namespace"], "ledger");
    assert!(
        out.get("retired").is_none(),
        "a record has no prune to render"
    );
}

/// Nobody in the mesh → every op this node writes would be unreadable to
/// every peer, and the refusal says what to do in THIS daemon's words.
#[tokio::test]
async fn append_outside_the_mesh_refuses_naming_the_join_verb() {
    let mesh = mesh_with(vec![]);
    let (_dir, rail, _mesh) = rail_with(mesh);
    let journal = journal_of(&rail, "ledger").unwrap();
    let body = serde_json::json!({ "op": "record", "payload": { "amount": 4 } });
    let resp = append_act(&rail, &journal, body).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let out = body_of(resp).await;
    let msg = out["error"].as_str().unwrap_or_default();
    assert!(msg.contains("cw-rails join"), "{msg}");
}

/// The namespace is never guessed: absent is a 400 that names the fix, and
/// a name the journal would refuse as a directory is the rail's own 400.
#[tokio::test]
async fn the_namespace_must_be_a_name_the_journal_can_open() {
    let mesh = mesh_with(vec![member(ME, "alex", None, false)]);
    let (_dir, rail, _mesh) = rail_with(mesh);
    let q = super::RailQuery { namespace: None };
    let err = super::namespace_of(&q).unwrap_err();
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    let refused = journal_of(&rail, "Not A Namespace");
    assert!(
        refused.is_err(),
        "an unopenable namespace is refused, not sanitized"
    );
}

/// Log ships the admitted order, the roster it was admitted against, and
/// completeness — acts without their gaps would be a confident answer over
/// a subset.
#[tokio::test]
async fn log_ships_admitted_ops_gaps_and_the_roster() {
    let mesh = mesh_with(vec![member(ME, "alex", None, false)]);
    let (_dir, rail, _mesh) = rail_with(mesh);
    let journal = journal_of(&rail, "ledger").unwrap();
    let body = serde_json::json!({ "op": "record", "payload": { "amount": 4 } });
    let resp = append_act(&rail, &journal, body).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = super::log_answer(&rail, &journal).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let out = body_of(resp).await;
    assert_eq!(out["complete"], true);
    // `held` is the journal-line count, refused lines included: one line on
    // disk, one op admitted, so nothing was refused.
    assert_eq!(out["held"], 1);
    assert_eq!(out["ops"].as_array().unwrap().len(), 1);
    assert_eq!(out["ops"][0]["person"], "alex");
    assert_eq!(out["gaps"].as_array().unwrap().len(), 0);
    assert!(
        out["roster"]["members"]["alex"].is_array(),
        "the answer carries the roster the ops were admitted against"
    );
}

/// A seal renders its prune in the append body: the seal is on disk either
/// way, so a refused prune must never read as a failed seal — and a working
/// one reports what it removed.
#[tokio::test]
async fn a_seal_renders_its_prune() {
    let mesh = mesh_with(vec![member(ME, "alex", None, false)]);
    let (_dir, rail, _mesh) = rail_with(mesh);
    let journal = journal_of(&rail, "ledger").unwrap();
    for _ in 0..2 {
        let body = serde_json::json!({ "op": "record", "payload": { "amount": 1 } });
        assert_eq!(
            append_act(&rail, &journal, body).await.status(),
            StatusCode::OK
        );
    }
    let resp = append_act(&rail, &journal, serde_json::json!({ "op": "seal" })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let out = body_of(resp).await;
    let retired = &out["retired"];
    assert!(
        retired.get("refused").is_none(),
        "a prune this roster admits must not read as refused: {retired}"
    );
    assert_eq!(retired["removed"], 2, "both sealed-over acts retire");
    assert_eq!(retired["kept"], 1, "the seal itself stays");
}

/// The live lane bounds its memory and reports what it drops: 257 pushes
/// leave 256 payloads and one honest `dropped`.
#[test]
fn the_live_buffer_evicts_the_oldest_and_reports_it() {
    let live = LiveBuffer::default();
    for i in 0..257 {
        live.push("room", format!("p{i}"));
    }
    let (payloads, dropped) = live.drain("room");
    assert_eq!(payloads.len(), 256);
    assert_eq!(dropped, 1);
    assert_eq!(payloads[0], "p1", "the OLDEST payload is the one evicted");
    let (again, dropped) = live.drain("room");
    assert!(
        again.is_empty() && dropped == 0,
        "a drain empties the buffer"
    );
    // Another namespace's buffer is untouched.
    live.push("other", "x".into());
    let (mine, _) = live.drain("room");
    assert!(mine.is_empty());
}

/// The live lane's refusals are named, and its success honestly reports
/// that nothing was fanned out — this daemon carries no fabric.
#[test]
fn live_push_reports_the_absence_of_the_fan_out() {
    let live = LiveBuffer::default();
    let oversize = vec![b'x'; 4097];
    let resp = push_answer(&live, "room", oversize.into());
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let not_text = vec![0xff, 0xfe, 0x00];
    let resp = push_answer(&live, "room", not_text.into());
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let fits = vec![b'a'; 4096];
    let resp = push_answer(&live, "room", fits.into());
    assert_eq!(resp.status(), StatusCode::OK);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let out = rt.block_on(body_of(resp));
    assert_eq!(out["bytes"], 4096);
    assert_eq!(out["peers"].as_array().unwrap().len(), 0);
    assert_eq!(out["delivered"], 0);
    let resp = drain_answer(&live, "room");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let out = rt.block_on(body_of(resp));
    assert_eq!(out["payloads"].as_array().unwrap().len(), 1);
    assert_eq!(out["dropped"], 0);
}
