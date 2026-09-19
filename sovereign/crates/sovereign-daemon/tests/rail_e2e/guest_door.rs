// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest door, from a phone on the room's WiFi: the wall grant's bearer
//! reaches the ring page and the rail on A's `guest_bind`, and nothing else —
//! not A's other routes, not B, not after it expires.
//!
//! Two daemons are two `AppState`s: A minted the grant, B is a node of the
//! same ring that did not. The router is `guest_door::door_router`, the one
//! the door binds; the lifecycle drill at the end binds a real socket.

use std::time::Duration;

use axum::http::StatusCode;
use sovereign_daemon::guest_door::{door_router, serve, PAGE_PREFIX};
use tower::ServiceExt;

use super::*;

/// A page directory with a sibling file OUTSIDE it, for the escape probe.
fn page_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let page = root.path().join("ring-doc");
    std::fs::create_dir(&page).unwrap();
    std::fs::write(
        page.join("index.html"),
        "<html><head></head><body>ring</body></html>",
    )
    .unwrap();
    std::fs::write(page.join("app.js"), "export {};").unwrap();
    std::fs::write(root.path().join("secret.txt"), "not the page").unwrap();
    (root, page)
}

async fn door(state: AppState, page: &std::path::Path, req: Request<Body>) -> (StatusCode, String) {
    let resp = door_router(state, Some(page.to_path_buf()))
        .oneshot(req)
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// A refusal, by any of the three names the surface has for one: 401 no or
/// wrong credential, 403 a live grant that does not cover the path, 404 not
/// served here.
fn refused(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    )
}

#[tokio::test]
async fn the_wall_bearer_reaches_the_page_and_the_rail_on_a_and_nothing_else() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let a = with_guest(
        state_with_rail(dir_a.path(), &key),
        vec![Scope::Rails(NS.into())],
    );
    let b = state_with_rail(dir_b.path(), &key);
    let (_root, page) = page_dir();

    // The page, with no bearer: a browser navigating cannot send one, and the
    // token rides the fragment, which it never sends at all.
    let (status, html) = door(
        a.clone(),
        &page,
        request("GET", PAGE_PREFIX, LAN_PEER, None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("<script src=\"/ring/__ring.js\"></script></head>"));
    let (status, shim) = door(
        a.clone(),
        &page,
        request("GET", "/ring/__ring.js", LAN_PEER, None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        shim.contains("const RAIL = \"\";"),
        "the door's shim is the bearer transport"
    );
    let (status, _) = door(
        a.clone(),
        &page,
        request("GET", "/ring/app.js", LAN_PEER, None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The rail, with the bearer: write, then read it back.
    let (status, body) = door(
        a.clone(),
        &page,
        request(
            "POST",
            "/v1/rail/append",
            LAN_PEER,
            Some(GUEST_TOKEN),
            Some(groceries()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, log) = door(
        a.clone(),
        &page,
        request("GET", "/v1/rail/log", LAN_PEER, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{log}");
    assert!(
        log.contains(NS),
        "the log names the granted namespace: {log}"
    );

    // Nothing else on A, with the bearer or without it.
    for (method, path) in [
        ("GET", "/v1/models"),
        ("POST", "/v1/chat/completions"),
        ("POST", "/v1/knowledge/search"),
        ("GET", "/v1/apps"),
        ("GET", "/api/tags"),
        ("GET", "/app/any/x"),
        ("GET", "/internal/guest/grant/list"),
        ("POST", "/internal/guest/grant"),
        ("GET", "/v1/conversations"),
    ] {
        for bearer in [Some(GUEST_TOKEN), None] {
            let (status, body) = door(
                a.clone(),
                &page,
                request(method, path, LAN_PEER, bearer, Some(serde_json::json!({}))),
            )
            .await;
            assert!(
                refused(status),
                "{method} {path} (bearer: {}) answered {status}: {body}",
                bearer.is_some()
            );
        }
    }
    // The two exceptions, named so they cannot grow silently: the paths
    // `client_auth` leaves open to ANY non-loopback caller on every surface
    // (liveness and the federation handshake). The door inherits them with
    // the Guest router; the room's WiFi can read them, as a LAN can on a
    // non-loopback `client_bind`.
    assert_eq!(
        sovereign_daemon::client_auth::AUTH_EXEMPT_PATHS,
        &["/status", "/oicp/v1/capabilities"]
    );
    // Without the bearer the rail is shut too.
    let (status, _) = door(
        a.clone(),
        &page,
        request("GET", "/v1/rail/log", LAN_PEER, None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The page directory is the page and nothing beside it.
    let (status, body) = door(
        a.clone(),
        &page,
        request("GET", "/ring/..%2Fsecret.txt", LAN_PEER, None, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.contains("not the page"));

    // Nothing on B: B never issued the grant, so the bearer is a stranger.
    let (status, body) = door(
        b,
        &page,
        request("GET", "/v1/rail/log", LAN_PEER, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

/// **The door does not serve an operator route even to a caller the auth
/// layer admits.** Driven with the daemon-wide token, so the only variable
/// left is whether the route is mounted — a 404 is the route set, not a
/// credential. The control proves the probe is not a router that 404s all.
#[tokio::test]
async fn the_door_mounts_no_operator_route() {
    let (_root, page) = page_dir();
    for (method, path) in [
        ("GET", "/internal/guest/grant/list"),
        ("POST", "/internal/guest/grant"),
    ] {
        let (status, body) = door(
            bare_state(),
            &page,
            request(
                method,
                path,
                LAN_PEER,
                Some(TOKEN),
                Some(serde_json::json!({})),
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "the guest door served {method} {path}: {body}"
        );
    }
    let (status, _) = call(
        bare_state(),
        request("GET", "/internal/guest/grant/list", LOOPBACK, None, None),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the operator surface still serves it"
    );
}

#[tokio::test]
async fn an_expired_wall_bearer_is_refused_at_the_door() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = state_with_rail(dir.path(), &key);
    let now = commonwealth_core::clock::unix_now_millis();
    // Issued ten seconds ago for one second.
    state.inner.node.guest_grants.issue(
        GUEST_TOKEN,
        vec![Scope::Rails(NS.into())],
        Some("wall".into()),
        1,
        now - 10_000,
    );
    let (_root, page) = page_dir();
    let (status, body) = door(
        state,
        &page,
        request("GET", "/v1/rail/log", LAN_PEER, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

/// **Nothing listens until a rail grant is live, and nothing listens after
/// the last one lapses.** A real socket, because "listening" is the claim.
#[tokio::test]
async fn the_door_opens_at_the_first_rail_grant_and_closes_at_the_last_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = state_with_rail(dir.path(), &key);
    let addr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    tokio::spawn(serve(state.clone(), Some(addr.to_string()), None));
    let http = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    let log = || {
        http.get(format!("http://{addr}/v1/rail/log"))
            .bearer_auth(GUEST_TOKEN)
            .send()
    };
    let within = |secs: u64| tokio::time::Instant::now() + Duration::from_secs(secs);

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(log().await.is_err(), "the door is open with no grant out");

    // A models-only grant is not a wall grant: still shut.
    let now = commonwealth_core::clock::unix_now_millis();
    state.inner.node.guest_grants.issue(
        "models-only-token-models-only-token-models-only-token-0000000000",
        vec![Scope::Models(vec!["m".into()])],
        None,
        60,
        now,
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(log().await.is_err(), "a models grant opened the door");

    state.inner.node.guest_grants.issue(
        GUEST_TOKEN,
        vec![Scope::Rails(NS.into())],
        Some("wall".into()),
        3,
        commonwealth_core::clock::unix_now_millis(),
    );
    let deadline = within(4);
    loop {
        if let Ok(r) = log().await {
            assert_eq!(r.status().as_u16(), 200);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the door never opened"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let deadline = within(8);
    while log().await.is_ok() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the door stayed open past the last expiry"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
