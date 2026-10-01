// SPDX-License-Identifier: AGPL-3.0-or-later
//! The grant half of the live lane's drain (pb-mesh-exit-transport; director
//! phase-b-81 (5)): the buffer is cw-rails' since the flip, and svrn's
//! `GET /v1/rail/live` decides the namespace from the caller's grant, then
//! forwards. Successor of the daemon's
//! `ring_live_non_durable::a_grant_drains_only_its_own_namespace`, whose
//! buffer half is commonwealth-rails `ring_routes::tests::a_drain_takes_only_its_own_namespace`.

use std::sync::{Arc, Mutex};

use super::*;

const NS_B: &str = "tool-lending";

/// A stand-in for cw-rails' `GET /v1/rail/live`: answers one payload naming
/// the namespace it was asked for, and records every namespace asked.
fn live_door() -> (String, Arc<Mutex<Vec<String>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/v1/rail/live",
        axum::routing::get(
            move |axum::extract::Query(q): axum::extract::Query<
                std::collections::HashMap<String, String>,
            >| {
                let seen = seen.clone();
                async move {
                    let ns = q.get("namespace").cloned().unwrap_or_default();
                    seen.lock().unwrap().push(ns.clone());
                    axum::Json(serde_json::json!({
                        "payloads": [format!("{ns}-payload")],
                        "dropped": 0,
                    }))
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), asked)
}

/// A grant for `tool-lending` drains `tool-lending` from cw-rails, and asking
/// for `house-expenses` under it is refused before cw-rails is asked at all.
/// Failing input: the drain forwarding the request's `?namespace=` instead of
/// the grant's.
#[tokio::test]
async fn a_grant_drains_only_its_own_namespace() {
    let (base, asked) = live_door();
    let state = with_guest(
        bare_state_with_seed(
            sovereign_daemon::state::FabricSeed::default(),
            sovereign_grants::GuestSessionBinding::Door,
            Default::default(),
            Some(base),
        ),
        vec![Scope::Rails(NS_B.into())],
    );

    let (status, body) = call(
        state.clone(),
        request("GET", "/v1/rail/live", LAN_PEER, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["payloads"],
        serde_json::json!([format!("{NS_B}-payload")])
    );

    let (status, body) = call(
        state,
        request(
            "GET",
            &format!("/v1/rail/live?namespace={NS}"),
            LAN_PEER,
            Some(GUEST_TOKEN),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        *asked.lock().unwrap(),
        vec![NS_B.to_string()],
        "cw-rails was asked for the grant's namespace, once, and never for another"
    );
}
