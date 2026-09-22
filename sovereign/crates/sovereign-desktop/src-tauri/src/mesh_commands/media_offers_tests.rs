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
        "media_available",
    ] {
        assert!(wire.get(key).is_some(), "LibraryView reads `{key}`");
    }
}

/// A library its holder is watching is a row the rail can render and CANNOT
/// play: `media_available` 0.0 carries the reason and `player_url` is absent,
/// so "does not start a stream" is a fact about the row rather than a rule
/// the view has to remember. The route is asserted too — the reach is never
/// requested for that peer, so the bridge is not even built.
#[tokio::test]
async fn a_library_in_use_by_its_holder_is_shown_and_not_reachable() {
    let reached = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = reached.clone();
    let app = Router::new().route(
        "/v1/mesh/media",
        get(move |Query(q): Query<HashMap<String, String>>| {
            let seen = seen.clone();
            async move {
                match q.get("peer").map(String::as_str) {
                    None => (
                        axum::http::StatusCode::OK,
                        axum::Json(serde_json::json!({ "offering": [
                            {"peer":"little","node_id":"llll","status":"online",
                             "media_available": 0.0},
                            {"peer":"free","node_id":"ffff","status":"online",
                             "media_available": 1.0}
                        ]})),
                    ),
                    Some(_) => {
                        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        (
                            axum::http::StatusCode::OK,
                            axum::Json(serde_json::json!({
                                "peer":"x","node_id":"xxxx",
                                "url":"http://127.0.0.1:41231","via":"iroh"
                            })),
                        )
                    }
                }
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
    assert_eq!(got.len(), 2, "an in-use library is still a row");

    assert_eq!(got[0].peer, "little");
    assert_eq!(got[0].media_available, Some(0.0));
    assert_eq!(
        got[0].player_url, None,
        "an in-use library must carry no URL for the view to open"
    );
    assert_eq!(
        got[0].unreachable, None,
        "in use is not a refusal to reach — the view says why from media_available"
    );

    assert_eq!(got[1].peer, "free");
    assert_eq!(got[1].media_available, Some(1.0));
    assert_eq!(
        got[1].player_url.as_deref(),
        Some("http://127.0.0.1:41231"),
        "a free library still plays"
    );

    assert_eq!(
        reached.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the in-use holder must not be dialed at all; only the free one was"
    );
}

// ─── mesh_media_probe ──────────────────────────────────────────────────────

/// The probe is loopback-http only: it must never become a fetch gadget
/// aimed at arbitrary hosts, so everything else is refused BY NAME.
#[tokio::test]
async fn the_probe_refuses_non_loopback_or_non_http_urls() {
    for url in [
        "https://127.0.0.1:8096/",
        "http://10.0.0.5:8096/",
        "http://example.com/",
        "file:///etc/passwd",
        "not a url",
    ] {
        let err = probe_media_url(url)
            .await
            .expect_err("only loopback http may be probed");
        assert!(
            err.contains("loopback"),
            "`{url}` refused with a reason naming loopback, got: {err}"
        );
    }
}

/// The happy path: a loopback origin that answers ANY HTTP status is a
/// playable library — the question was "does it answer", not "is it
/// healthy". A listener that accepts and immediately closes is the
/// RuggedFox failure shape and must be an Err.
#[tokio::test]
async fn the_probe_reports_the_origin_status_and_names_a_silent_origin() {
    use tokio::io::AsyncWriteExt as _;

    // An origin that answers 204.
    let answering = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let answer_addr = answering.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match answering.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = sock
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await;
        }
    });
    assert_eq!(
        probe_media_url(&format!("http://{answer_addr}")).await,
        Ok(204),
        "an answering origin is playable whatever its status"
    );

    // An origin that accepts and closes without a byte — the bridge's
    // dead-far-end shape.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent_addr = silent.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (sock, _) = match silent.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            drop(sock);
        }
    });
    let err = probe_media_url(&format!("http://{silent_addr}"))
        .await
        .expect_err("a silent origin must not open a browser tab");
    assert!(
        err.contains("did not answer"),
        "the refusal names the silent-origin shape, got: {err}"
    );
}
