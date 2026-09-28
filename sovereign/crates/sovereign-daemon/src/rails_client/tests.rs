// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the dialing rail puts on the wire for a guest's append (decision
//! five-programs-34), against a stand-in for cw-rails' append door that
//! records each body and then does what the door does with it.

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use axum::Json;
use commonwealth_rail_core::{
    AttestRefusal, GuestAttestation, Payload, Person, RailAct, RailError, RingSigner, Roster,
    SigningKey,
};
use sovereign_mesh::rail_port::{LocalRingRail, RingRailPort};

use super::RailsRingRail;

const NS: &str = "house";

type Seen = Arc<Mutex<Vec<serde_json::Value>>>;

/// A rail whose `house` roster names key 1 as alex, and the base URL of an
/// append door over it.
async fn door(root: &std::path::Path, seen: Seen) -> (Arc<LocalRingRail>, String) {
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let rail = Arc::new(LocalRingRail::new(root, Arc::new(key.clone())));
    let mut members = std::collections::BTreeMap::new();
    members.insert(Person::from("alex"), vec![key.actor()]);
    rail.inner()
        .journal(NS)
        .unwrap()
        .set_roster(&Roster::new(members))
        .unwrap();
    let served = Arc::clone(&rail);
    let app = axum::Router::new().route(
        "/v1/rail/append",
        axum::routing::post(move |Json(body): Json<serde_json::Value>| {
            let rail = Arc::clone(&served);
            let seen = Arc::clone(&seen);
            async move {
                seen.lock().unwrap().push(body.clone());
                let roster = rail.roster(NS).await.unwrap();
                let attestation = body
                    .get("attestation")
                    .map(|v| serde_json::from_value::<GuestAttestation>(v.clone()).unwrap());
                let act = RailAct::from_json(body).unwrap();
                let appended = match &attestation {
                    Some(a) => rail.journal_append_attested(NS, act, &roster, a).await,
                    None => rail.journal_append(NS, act, &roster).await,
                };
                match appended {
                    Ok(op) => (StatusCode::OK, Json(serde_json::json!({ "op": op }))),
                    Err(RailError::AttestRefused(r)) => (
                        StatusCode::FORBIDDEN,
                        Json(serde_json::json!({ "error": r.to_string(), "kind": r.name() })),
                    ),
                    Err(e) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({ "error": e.to_string() })),
                    ),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (rail, format!("http://{addr}"))
}

fn act() -> RailAct {
    RailAct::Record {
        payload: Payload::new(serde_json::json!({ "kind": "note", "text": "hi" })).unwrap(),
    }
}

fn in_an_hour() -> i64 {
    sovereign_time::unix_now_u64() as i64 + 3_600
}

#[tokio::test]
async fn a_stamped_append_reaches_rails_with_an_attestation_that_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let seen = Seen::default();
    let (local, base) = door(dir.path(), Arc::clone(&seen)).await;
    let roster = local.roster(NS).await.unwrap();
    let signed =
        GuestAttestation::sign(&SigningKey::from_bytes(&[1u8; 32]), "ana", NS, in_an_hour());

    let op = RailsRingRail::new(base)
        .journal_append_attested(NS, act(), &roster, &signed)
        .await
        .unwrap();

    assert_eq!(op.kind.on_behalf_of.as_deref(), Some("ana"));
    let body = seen.lock().unwrap().pop().expect("the door saw the append");
    let wire: GuestAttestation = serde_json::from_value(body["attestation"].clone()).unwrap();
    assert_eq!(wire, signed);
    let now = sovereign_time::unix_now_u64() as i64;
    assert_eq!(wire.verify(&roster, NS, now), Ok(()));
}

#[tokio::test]
async fn an_unstamped_append_sends_no_attestation() {
    let dir = tempfile::tempdir().unwrap();
    let seen = Seen::default();
    let (local, base) = door(dir.path(), Arc::clone(&seen)).await;
    let roster = local.roster(NS).await.unwrap();

    let op = RailsRingRail::new(base)
        .journal_append(NS, act(), &roster)
        .await
        .unwrap();

    assert_eq!(op.kind.on_behalf_of, None);
    let body = seen.lock().unwrap().pop().expect("the door saw the append");
    assert!(body.get("attestation").is_none(), "{body}");
}

/// The door's refusal keeps its name across the dial, so the route can hand
/// it to the caller verbatim instead of a 422 of prose.
#[tokio::test]
async fn a_refused_attestation_comes_back_typed() {
    let dir = tempfile::tempdir().unwrap();
    let (local, base) = door(dir.path(), Seen::default()).await;
    let roster = local.roster(NS).await.unwrap();
    let stranger = GuestAttestation::sign(
        &SigningKey::from_bytes(&[42u8; 32]),
        "ana",
        NS,
        in_an_hour(),
    );

    let refused = RailsRingRail::new(base)
        .journal_append_attested(NS, act(), &roster, &stranger)
        .await
        .unwrap_err();

    assert!(
        matches!(
            refused,
            RailError::AttestRefused(AttestRefusal::SignerNotInRoster)
        ),
        "{refused:?}"
    );
    assert!(local.journal_read(NS).await.unwrap().is_empty());
}
