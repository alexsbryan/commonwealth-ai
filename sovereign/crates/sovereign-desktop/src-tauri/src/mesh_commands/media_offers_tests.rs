use super::*;
use axum::{extract::Query, routing::get, Router};
use std::collections::HashMap;

/// Exactly the route's two answers: the no-peer list (one online holder
/// admitting two names, one offline holder admitting everyone) and the
/// `?peer=` reach, which the host refuses with a 409 for the offline one.
#[tokio::test]
async fn the_rail_lists_every_offer_with_its_player_url_or_its_refusal() {
    let app = Router::new().route(
        "/v1/mesh/media",
        get(|Query(q): Query<HashMap<String, String>>| async move {
            match q.get("peer").map(String::as_str) {
                None => (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({ "offering": [
                        {"peer":"b","node_id":"bbbb","status":"online",
                         "offered_to":["a","c"]},
                        {"peer":"d","node_id":"dddd","status":"offline"}
                    ]})),
                ),
                Some("bbbb") => (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({
                        "peer":"b","node_id":"bbbb",
                        "url":"http://127.0.0.1:41231","via":"iroh"
                    })),
                ),
                Some(_) => (
                    axum::http::StatusCode::CONFLICT,
                    axum::Json(serde_json::json!({
                        "error":"'d' is offline — a bridge to it would accept and then never answer"
                    })),
                ),
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let got = media_offers(&sovereign_turn_client::TurnClient::new(base))
        .await
        .expect("the route's own answers parse");
    assert_eq!(got.len(), 2, "a refused reach is a row, not a dropped one");

    assert_eq!(got[0].peer, "b");
    assert_eq!(got[0].offered_to, ["a", "c"]);
    assert_eq!(got[0].player_url.as_deref(), Some("http://127.0.0.1:41231"));
    assert_eq!(got[0].unreachable, None);

    assert_eq!(got[1].peer, "d");
    assert_eq!(got[1].status, "offline");
    assert!(got[1].offered_to.is_empty(), "absent = everyone here");
    assert_eq!(got[1].player_url, None);
    assert!(
        got[1]
            .unreachable
            .as_deref()
            .unwrap_or("")
            .contains("offline"),
        "the host's refusal reaches the rail; got {:?}",
        got[1].unreachable
    );

    let wire = serde_json::to_value(&got[0]).unwrap();
    for key in [
        "peer",
        "node_id",
        "status",
        "offered_to",
        "player_url",
        "unreachable",
    ] {
        assert!(wire.get(key).is_some(), "LibraryView reads `{key}`");
    }
}
