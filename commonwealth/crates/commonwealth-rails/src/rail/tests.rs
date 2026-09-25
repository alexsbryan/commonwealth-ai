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
use commonwealth_rail::{
    AttestRefusal, Digest, GuestAttestation, Person, RingRail, RingSigner, SigningKey,
};
use tokio::sync::RwLock;

use super::{
    admit_answer, append_act, compact_answer, derive_roster, digest_answer, drain_answer,
    ingest_answer, journal_of, missing_answer, push_answer, read_answer, roster_answer, LiveBuffer,
    MembershipRosterSource, MissingBody, RosterBody,
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

/// A mesh of this node (key 7) and one other member, "halo" (key 3), who
/// signs the guest door's attestations; key 99 is claimed by nobody.
fn attested(seed: u8, namespace: &str, expires_at: i64) -> serde_json::Value {
    let a = GuestAttestation::sign(&key(seed), "guest-ana", namespace, expires_at);
    serde_json::json!({
        "op": "record",
        "payload": { "amount": 4 },
        "attestation": a,
    })
}

fn guest_mesh() -> Arc<RwLock<Mesh>> {
    mesh_with(vec![
        member(ME, "alex", None, false),
        member(2, "halo", Some(pubkey_of(&key(3))), false),
    ])
}

const LATER: i64 = 4_000_000_000;

/// A roster member's attestation lands the act signed by the node and
/// attributed to the guest.
#[tokio::test]
async fn a_valid_attestation_lands_attributed_to_the_guest() {
    let (_dir, rail, _mesh) = rail_with(guest_mesh());
    let journal = journal_of(&rail, "ledger").unwrap();
    let resp = append_act(&rail, &journal, attested(3, "ledger", LATER)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let out = body_of(resp).await;
    assert_eq!(out["actor"], RingSigner::actor(&key(7)), "the node signs");
    let (ops, _) = journal.read().unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].kind.on_behalf_of.as_deref(), Some("guest-ana"));
}

/// Forged, expired and foreign-key attestations are each a 403 naming the
/// refusal, and the journal is untouched.
#[tokio::test]
async fn a_refused_attestation_names_its_refusal_and_writes_nothing() {
    let (_dir, rail, _mesh) = rail_with(guest_mesh());
    let journal = journal_of(&rail, "ledger").unwrap();
    let mut forged = attested(3, "ledger", LATER);
    forged["attestation"]["name"] = "guest-anb".into();
    let cases = [
        (forged, AttestRefusal::Forged),
        (attested(3, "ledger", 1), AttestRefusal::Expired),
        (
            attested(99, "ledger", LATER),
            AttestRefusal::SignerNotInRoster,
        ),
        (attested(3, "lending", LATER), AttestRefusal::WrongNamespace),
    ];
    for (body, want) in cases {
        let resp = append_act(&rail, &journal, body).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{want:?}");
        assert_eq!(body_of(resp).await["kind"], want.name());
    }
    let (ops, _) = journal.read().unwrap();
    assert!(ops.is_empty(), "a refused attestation writes nothing");
}

/// An unstamped append is unchanged: signed as the node, no name on it.
#[tokio::test]
async fn an_unstamped_append_carries_no_name() {
    let (_dir, rail, _mesh) = rail_with(guest_mesh());
    let journal = journal_of(&rail, "ledger").unwrap();
    let body = serde_json::json!({ "op": "record", "payload": { "amount": 4 } });
    let resp = append_act(&rail, &journal, body).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let (ops, _) = journal.read().unwrap();
    assert_eq!(ops[0].kind.on_behalf_of, None);
}

/// The signer-identity census (REVIEW-fp54-signer-identity). On a default
/// install rails and the daemon hold TWO node keys (`~/.commonwealth-rails`,
/// `~/.svrnmesh`), and rails is a member in its own right: `run` refuses
/// without a mesh, and `join` stamps rails' key into its record. Both name a
/// member by hostname by default and both derivations group keys by name, so
/// rails' lines admit — at home and at a peer — under the person the
/// daemon's lines already render as, and the daemon's key attests as a
/// roster member. The person holds only while the two names agree.
#[tokio::test]
async fn rails_and_the_daemon_sign_with_two_keys_under_one_person() {
    let daemon = key(1);
    let records = |rails_name: &str| {
        vec![
            member(ME, rails_name, Some(pubkey_of(&key(7))), false),
            member(0xD, "host-a", Some(pubkey_of(&daemon)), false),
            member(0xB, "host-b", Some(pubkey_of(&key(2))), false),
        ]
    };
    let (_dir, rail, mesh) = rail_with(mesh_with(records("host-a")));
    let journal = journal_of(&rail, "ledger").unwrap();
    let body = serde_json::json!({ "op": "record", "payload": { "amount": 4 } });
    assert_eq!(
        append_act(&rail, &journal, body).await.status(),
        StatusCode::OK
    );
    let resp = append_act(&rail, &journal, attested(1, "ledger", LATER)).await;
    assert_eq!(resp.status(), StatusCode::OK, "the daemon's key attests");

    let persons = |m: &Mesh, at: u128, k: &SigningKey| {
        let roster = derive_roster(m, NodeId::from_u128(at), Some(pubkey_of(k)));
        let journal = journal.clone();
        async move {
            let out = body_of(admit_answer(&journal, RosterBody { roster })).await;
            assert_eq!(out["complete"], true, "{out}");
            out["ops"]
                .as_array()
                .unwrap()
                .iter()
                .map(|o| o["person"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };
    let m = mesh.read().await;
    assert_eq!(persons(&m, ME, &key(7)).await, ["host-a", "host-a"]);
    assert_eq!(persons(&m, 0xB, &key(2)).await, ["host-a", "host-a"]);
    let renamed = mesh_with(records("living-room"));
    assert_eq!(
        persons(&*renamed.read().await, 0xB, &key(2)).await,
        ["living-room", "living-room"],
        "a rails named apart renders apart"
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

/// The sync doors are the round's read/write surface: a caller that holds
/// nothing is offered everything; a caller that holds it all is offered
/// nothing; ingesting what is already held moves nothing; and the roster
/// door answers WITH its origin so a caller never pairs two reads that could
/// disagree.
#[tokio::test]
async fn the_sync_doors_digest_missing_and_ingest_one_journal() {
    let mesh = mesh_with(vec![member(ME, "alex", None, false)]);
    let (_dir, rail, _mesh) = rail_with(mesh);
    let journal = journal_of(&rail, "ledger").unwrap();
    for amount in 1..=2 {
        let body = serde_json::json!({ "op": "record", "payload": { "amount" : amount } });
        assert_eq!(
            append_act(&rail, &journal, body).await.status(),
            StatusCode::OK
        );
    }

    // digest: one actor, contiguous through seq 1. A digest IS the
    // actor→high-water map, so the JSON is that map and nothing else.
    let out = body_of(digest_answer(&journal)).await;
    assert_eq!(out["namespace"], "ledger");
    assert_eq!(
        out["digest"][RingSigner::actor(&key(7)).as_str()],
        1,
        "one actor, two ops, high-water seq 1"
    );

    // missing against an empty digest: everything, and `more` is false.
    let out = body_of(missing_answer(
        &journal,
        MissingBody {
            digest: Digest::default(),
            budget: None,
        },
    ))
    .await;
    assert_eq!(out["ops"].as_array().unwrap().len(), 2);
    assert_eq!(out["more"], false);
    let offered = out["ops"].clone();

    // missing against the digest it just got: nothing left to send.
    let full: Digest =
        serde_json::from_value(body_of(digest_answer(&journal)).await["digest"].clone()).unwrap();
    let out = body_of(missing_answer(
        &journal,
        MissingBody {
            digest: full.clone(),
            budget: None,
        },
    ))
    .await;
    assert_eq!(out["ops"].as_array().unwrap().len(), 0);
    assert_eq!(out["more"], false);

    // ingest of what is already held: zero new — the steady state.
    let out = body_of(ingest_answer(
        &journal,
        serde_json::from_value(serde_json::json!({ "ops": offered })).unwrap(),
    ))
    .await;
    assert_eq!(out["ingested"], 0);

    // read: the raw lines, refused or not.
    let out = body_of(read_answer(&journal)).await;
    assert_eq!(out["ops"].as_array().unwrap().len(), 2);
    assert_eq!(out["skipped"], 0);

    // roster WITH its origin — derived here, no roster file anywhere.
    let out = body_of(roster_answer(&rail, &journal).await).await;
    assert_eq!(out["origin"], "derived");
    assert!(out["roster"]["members"]["alex"].is_array());

    // admit against THAT roster: complete, both lines held.
    let out = body_of(admit_answer(
        &journal,
        serde_json::from_value(serde_json::json!({ "roster": out["roster"].clone() })).unwrap(),
    ))
    .await;
    assert_eq!(out["held"], 2);
    assert_eq!(out["complete"], true);
    assert_eq!(out["gaps"].as_array().unwrap().len(), 0);
}

/// The budget is a budget, not a cap: a byte-starved `missing` returns what
/// fits and says `more`, so the caller knows to come back — the shape that
/// makes one refused whole-journal send impossible to misread as an empty
/// ring.
#[tokio::test]
async fn a_budgeted_missing_returns_what_fits_and_names_more() {
    let mesh = mesh_with(vec![member(ME, "alex", None, false)]);
    let (_dir, rail, _mesh) = rail_with(mesh);
    let journal = journal_of(&rail, "ledger").unwrap();
    for amount in 1..=3 {
        let body = serde_json::json!({ "op": "record", "payload": { "amount" : amount } });
        assert_eq!(
            append_act(&rail, &journal, body).await.status(),
            StatusCode::OK
        );
    }
    let out = body_of(missing_answer(
        &journal,
        MissingBody {
            digest: Digest::default(),
            budget: Some(1),
        },
    ))
    .await;
    assert_eq!(
        out["ops"].as_array().unwrap().len(),
        1,
        "one op always ships — a budget that returned nothing would read as \
         an empty ring"
    );
    assert_eq!(out["more"], true, "there IS more, and the answer says so");
}

/// Compact through the door retires exactly what the roster's seals
/// authorise — the same numbers the append door's `retired` renders, because
/// they are one prune seen from two doors.
#[tokio::test]
async fn compact_retires_below_the_authenticated_floors() {
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
    let out = body_of(compact_answer(
        &journal,
        // No seal yet: nothing is below any floor, so nothing may move.
        serde_json::from_value(serde_json::json!({ "roster": body_of(roster_answer(&rail, &journal).await).await["roster"].clone() })).unwrap(),
    ))
    .await;
    assert_eq!(out["removed"], 0);
    assert_eq!(out["kept"], 2);
    assert_eq!(out["gaps_cleared"], 0);
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
