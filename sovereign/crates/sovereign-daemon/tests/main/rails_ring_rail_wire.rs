// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the dialing rail puts on the wire for a guest's append (decision
//! five-programs-34), against a recorder in front of a real cw-rails' append
//! door: it keeps each body and forwards it, so the door does what it does.
//! Moved from `src/rails_client/tests.rs` with the port (pb-mesh-exit-mesh):
//! the journal behind the door is cw-rails', not a local one.

use std::sync::{Arc, Mutex};

use axum::extract::RawQuery;
use axum::http::StatusCode;
use axum::Json;
use commonwealth_rail_core::{
    AttestRefusal, GuestAttestation, Payload, Person, RailAct, RailError, RingSigner, Roster,
    SigningKey,
};
use sovereign_daemon::rail_port::RingRailPort;
use sovereign_daemon::rails_client::RailsRingRail;

use crate::common::work_rails::WorkRails;

const NS: &str = "house";

type Seen = Arc<Mutex<Vec<serde_json::Value>>>;

/// A cw-rails signing as key 1, whose `house` roster names key 1 as alex,
/// and the base URL of a recorder in front of its append door. The rail it
/// returns dials cw-rails directly.
async fn door(seen: Seen) -> (Arc<dyn RingRailPort>, String) {
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let mut members = std::collections::BTreeMap::new();
    members.insert(Person::from("alex"), vec![key.actor()]);
    let rails =
        Arc::new(WorkRails::spawn_keyed(None, Some(&key), &[(NS, &Roster::new(members))], "").await);
    let upstream = format!("{}/v1/rail/append", rails.base);
    let app = axum::Router::new().route(
        "/v1/rail/append",
        axum::routing::post(
            move |RawQuery(query): RawQuery, Json(body): Json<serde_json::Value>| {
                let seen = Arc::clone(&seen);
                let url = match query {
                    Some(q) => format!("{upstream}?{q}"),
                    None => upstream.clone(),
                };
                async move {
                    seen.lock().unwrap().push(body.clone());
                    let answer = reqwest::Client::new()
                        .post(url)
                        .json(&body)
                        .send()
                        .await
                        .expect("cw-rails' append door answers");
                    let status = StatusCode::from_u16(answer.status().as_u16()).unwrap();
                    let json: serde_json::Value = answer.json().await.expect("a JSON answer");
                    (status, Json(json))
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (rails.ring_rail(), format!("http://{addr}"))
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
    let seen = Seen::default();
    let (local, base) = door(Arc::clone(&seen)).await;
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
    let seen = Seen::default();
    let (local, base) = door(Arc::clone(&seen)).await;
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
    let (local, base) = door(Seen::default()).await;
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

/// A namespace that is not one comes back typed across the dial, so the
/// daemon's rail routes refuse it with a 400, as they would a local rail's.
/// Failing input: cw-rails' refusal without its `kind`, read as `Rejected`.
#[tokio::test]
async fn a_bad_namespace_comes_back_typed() {
    let (rail, _base) = door(Seen::default()).await;
    let refused = rail.journal_read("../../etc").await.unwrap_err();
    assert!(
        matches!(&refused, RailError::BadNamespace(ns) if ns == "../../etc"),
        "{refused:?}"
    );
}
