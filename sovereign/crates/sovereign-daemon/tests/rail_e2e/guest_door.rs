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
use sovereign_daemon::guest_door::{door_router, serve, GuestPages, PAGE_PREFIX};
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
    // `None`: this suite drives the door's route topology, not a turn. The
    // ask route answers 503 naming the missing host, which is what the
    // "a guest reaches nothing else" assertions below expect from it.
    let resp = door_router(
        state,
        std::sync::Arc::new(GuestPages::new(
            Some(page.to_path_buf()),
            Default::default(),
        )),
        None,
    )
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
    tokio::spawn(serve(
        state.clone(),
        Some(addr.to_string()),
        std::sync::Arc::new(GuestPages::default()),
        None,
    ));
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

/// **The name is claimed at the door, once, and the phone carries a handle.**
/// One QR serves a room, so the grant cannot say who is holding the phone —
/// the session does. Everything a page could get wrong about it is refused
/// HERE, where the name is claimed: a member's name, a name somebody in this
/// room already has, and a handle this grant never issued.
#[tokio::test]
async fn the_door_claims_a_name_once_and_refuses_the_three_collisions() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let a = with_guest(
        state_with_rail(dir.path(), &key),
        vec![Scope::Rails(NS.into())],
    );
    let (_root, page) = page_dir();
    let claim = |name: &str| {
        request(
            "POST",
            "/v1/guest/session",
            LAN_PEER,
            Some(GUEST_TOKEN),
            Some(serde_json::json!({ "name": name })),
        )
    };

    // A member's name, in any case — the refusal the append route has spoken
    // since the rail shipped, now at the moment the name is claimed.
    for member in ["bo", "Alex"] {
        let (status, body) = door(a.clone(), &page, claim(member)).await;
        assert_eq!(status, StatusCode::CONFLICT, "claimed member {member}");
        assert!(
            body.contains("is a member of this ring"),
            "the member refusal lost its sentence: {body}"
        );
    }

    // A name of their own is bound, and the handle comes back.
    let (status, body) = door(a.clone(), &page, claim("ana")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let claimed: serde_json::Value = serde_json::from_str(&body).unwrap();
    let handle = claimed["session"].as_str().expect("a handle").to_string();
    assert_eq!(claimed["name"], "ana");

    // Two guests must never be shown as one person, so the second phone
    // typing it is refused rather than quietly renaming the first.
    let (status, body) = door(a.clone(), &page, claim(" Ana ")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("already someone else in this room"), "{body}");

    // The handle is accepted on the routes behind the door…
    let act = serde_json::json!({
        "op": "record",
        "payload": { "kind": "doc-change", "doc": "ring-doc", "update": "AA==" },
    });
    let mut req = request(
        "POST",
        "/v1/rail/append",
        LAN_PEER,
        Some(GUEST_TOKEN),
        Some(act),
    );
    req.headers_mut().insert(
        axum::http::HeaderName::from_static("x-ring-session"),
        axum::http::HeaderValue::from_str(&handle).unwrap(),
    );
    let (status, body) = door(a.clone(), &page, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // …and one this grant never issued is refused by name, rather than read as
    // "this phone has not claimed a name yet" (ARCH 6).
    let mut req = request("GET", "/v1/rail/log", LAN_PEER, Some(GUEST_TOKEN), None);
    req.headers_mut().insert(
        axum::http::HeaderName::from_static("x-ring-session"),
        axum::http::HeaderValue::from_static("not-a-handle"),
    );
    let (status, body) = door(a, &page, req).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("stale_session"), "{body}");
}

/// **The door writes whose words an act was, and a lying page gets nowhere.**
/// The page here sends a `guest` of its own in the payload — the field the
/// rail used to believe — and the act is still attributed to the name the
/// session holds. The log hands the finished name back, so an app renders
/// `person` and is right without knowing guests exist.
///
/// This replaces the append-time member-name refusal: that check read a field
/// the page supplied, and its subject now lives where the name is CLAIMED
/// (`the_door_claims_a_name_once_and_refuses_the_three_collisions`).
#[tokio::test]
async fn the_door_stamps_the_guest_and_the_page_cannot() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let a = with_guest(
        state_with_rail(dir.path(), &key),
        vec![Scope::Rails(NS.into())],
    );
    let (_root, page) = page_dir();

    let (status, body) = door(
        a.clone(),
        &page,
        request(
            "POST",
            "/v1/guest/session",
            LAN_PEER,
            Some(GUEST_TOKEN),
            Some(serde_json::json!({ "name": "ana" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let handle = serde_json::from_str::<serde_json::Value>(&body).unwrap()["session"]
        .as_str()
        .expect("a handle")
        .to_string();

    let with_handle = |method: &str, path: &str, body: Option<serde_json::Value>| {
        let mut req = request(method, path, LAN_PEER, Some(GUEST_TOKEN), body);
        req.headers_mut().insert(
            axum::http::HeaderName::from_static("x-ring-session"),
            axum::http::HeaderValue::from_str(&handle).unwrap(),
        );
        req
    };

    // The page names somebody else, in the payload and beside it. Neither is
    // read: the door already knows who is holding this phone.
    let (status, body) = door(
        a.clone(),
        &page,
        with_handle(
            "POST",
            "/v1/rail/append",
            Some(serde_json::json!({
                "op": "record",
                "on_behalf_of": "zoe",
                "payload": {
                    "kind": "doc-change", "doc": "ring-doc", "update": "AA==", "guest": "zoe",
                },
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, log) = door(a, &page, with_handle("GET", "/v1/rail/log", None)).await;
    let log: serde_json::Value = serde_json::from_str(&log).unwrap();
    let op = &log["ops"][0];
    let person = op["person"].as_str().expect("a person");
    assert!(
        person.starts_with("ana, guest of "),
        "the door's name did not reach the log: {person}"
    );
    assert_eq!(op["guest"]["name"], "ana");
    assert_eq!(
        op["on_behalf_of"], "ana",
        "the name must be what was SIGNED"
    );
    assert!(!person.contains("zoe"), "the page's claim won: {person}");
}

/// The second app on this wall: its own grant, its own bearer, its own QR —
/// because a grant names exactly one rail namespace.
const DOC_TOKEN: &str = "1a2b3c4d5e6f70819a2b3c4d5e6f70819a2b3c4d5e6f70819a2b3c4d5e6f7081";
const DOC_NS: &str = "ring-doc";

/// Put a second app's namespace on the same wall: a roster it can admit
/// against, and a live grant of its own.
fn second_app(state: &AppState, key: &SigningKey) {
    let mut members = std::collections::BTreeMap::new();
    members.insert(Person::from("alex"), vec![key.actor()]);
    members.insert(
        Person::from("bo"),
        vec!["bo-has-not-joined-yet".to_string()],
    );
    state
        .ring_rail()
        .expect("a rail")
        .journal(DOC_NS)
        .unwrap()
        .set_roster(&Roster::new(members))
        .unwrap();
    state.inner.node.guest_grants.issue(
        DOC_TOKEN,
        vec![Scope::Rails(DOC_NS.into())],
        Some("doc".into()),
        3_600,
        commonwealth_core::clock::unix_now_millis(),
    );
}

/// Present `handle` with `bearer` — a phone that named itself on one app,
/// walking to the other one on the same wall.
fn as_guest(
    method: &str,
    path: &str,
    bearer: &str,
    handle: &str,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let mut req = request(method, path, LAN_PEER, Some(bearer), body);
    req.headers_mut().insert(
        axum::http::HeaderName::from_static("x-ring-session"),
        axum::http::HeaderValue::from_str(handle).unwrap(),
    );
    req
}

/// Claim `name` under `bearer` and return the handle.
async fn claimed(state: AppState, page: &std::path::Path, bearer: &str, name: &str) -> String {
    let (status, body) = door(
        state,
        page,
        request(
            "POST",
            "/v1/guest/session",
            LAN_PEER,
            Some(bearer),
            Some(serde_json::json!({ "name": name })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["session"]
        .as_str()
        .expect("a handle")
        .to_string()
}

/// **The grant is the scope; the session is the person.** A wall's second app
/// is a second grant with a second bearer, and the phone that already typed
/// its name is the same person there — it is not asked again.
///
/// **And the handle decides nothing about reach.** The act written while
/// presenting the doc's bearer lands in the DOC's namespace, not in the one
/// the name was claimed under: `permits_path` on the bearer presented is the
/// sole decider, and a handle only ever names.
#[tokio::test]
async fn a_name_claimed_on_one_app_is_the_same_person_on_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let a = with_guest(
        state_with_rail(dir.path(), &key),
        vec![Scope::Rails(NS.into())],
    );
    second_app(&a, &key);
    let (_root, page) = page_dir();

    let handle = claimed(a.clone(), &page, GUEST_TOKEN, "ana").await;

    // The doc, on the same wall, under its own bearer: no second prompt, and
    // the door names the writer from the handle it already knows.
    let (status, body) = door(
        a.clone(),
        &page,
        as_guest(
            "POST",
            "/v1/rail/append",
            DOC_TOKEN,
            &handle,
            Some(serde_json::json!({
                "op": "record",
                "payload": { "kind": "doc-change", "doc": "ring-doc", "update": "AA==" },
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, log) = door(
        a.clone(),
        &page,
        as_guest("GET", "/v1/rail/log", DOC_TOKEN, &handle, None),
    )
    .await;
    let log: serde_json::Value = serde_json::from_str(&log).unwrap();
    assert_eq!(
        log["namespace"], DOC_NS,
        "reach followed the handle instead of the bearer presented"
    );
    let person = log["ops"][0]["person"].as_str().expect("a person");
    assert!(
        person.starts_with("ana, guest of "),
        "the name did not walk to the second app: {person}"
    );

    // And nothing of it reached the app the name was claimed on.
    let (_, other) = door(
        a,
        &page,
        as_guest("GET", "/v1/rail/log", GUEST_TOKEN, &handle, None),
    )
    .await;
    let other: serde_json::Value = serde_json::from_str(&other).unwrap();
    assert_eq!(other["namespace"], NS);
    assert_eq!(
        other["ops"].as_array().map(Vec::len),
        Some(0),
        "an act written under the doc's bearer landed on the other app"
    );
}

/// **The knob, watched working.** Under `[daemon] guest_sessions = "grant"` —
/// the strict setting — the same walk is refused by name: the handle is not
/// live under the second app's bearer, and the shim's answer is to ask for a
/// name again rather than to present a handle this link does not know.
#[tokio::test]
async fn under_the_strict_binding_the_second_app_asks_again() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let a = with_guest(
        state_with_rail_sessions(
            dir.path(),
            &key,
            sovereign_grants::GuestSessionBinding::Grant,
            Default::default(),
        ),
        vec![Scope::Rails(NS.into())],
    );
    second_app(&a, &key);
    let (_root, page) = page_dir();

    let handle = claimed(a.clone(), &page, GUEST_TOKEN, "ana").await;

    // Still the same person on the app it was claimed on.
    let (status, body) = door(
        a.clone(),
        &page,
        as_guest("GET", "/v1/rail/log", GUEST_TOKEN, &handle, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = door(
        a,
        &page,
        as_guest("GET", "/v1/rail/log", DOC_TOKEN, &handle, None),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("stale_session"), "{body}");
}

/// **ONE bearer, the whole wall — and the wall is what the OWNER declared.**
///
/// The resource declares and the credential identifies: this grant names no
/// namespace at all, so what it reaches is `[daemon.guest_pages]`, read at the
/// route. Clause (e) of `rg-one-person-across-apps` is the three refusals here
/// — a namespace nobody declared, one this daemon owns, and an append to an
/// entry registered `guests = "read"` — each with a sentence naming it.
#[tokio::test]
async fn one_wall_bearer_reaches_every_declared_app_and_is_refused_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let owned = sovereign_core::mesh_measurements::MEASUREMENTS_APP_ID;
    let a = with_guest(
        state_with_wall(
            dir.path(),
            &key,
            &[
                (
                    NS,
                    sovereign_core::guest_pages::GuestPage::Open("/srv/a".into()),
                ),
                (
                    DOC_NS,
                    sovereign_core::guest_pages::GuestPage::Narrowed {
                        dir: "/srv/b".into(),
                        guests: sovereign_core::guest_pages::GuestAccess::Read,
                    },
                ),
                // A config that should never have been written. The route
                // refuses it anyway — `GuestPages::from_config` is not the
                // only guard (ARCH 5).
                (
                    owned,
                    sovereign_core::guest_pages::GuestPage::Open("/srv/c".into()),
                ),
            ],
        ),
        vec![Scope::Wall],
    );
    second_app(&a, &key);
    let (_root, page) = page_dir();
    let handle = claimed(a.clone(), &page, GUEST_TOKEN, "ana").await;

    let append = |ns: &str| {
        as_guest(
            "POST",
            &format!("/v1/rail/append?namespace={ns}"),
            GUEST_TOKEN,
            &handle,
            Some(serde_json::json!({ "op": "record", "payload": { "n": 1 } })),
        )
    };
    let log = |ns: &str| {
        as_guest(
            "GET",
            &format!("/v1/rail/log?namespace={ns}"),
            GUEST_TOKEN,
            &handle,
            None,
        )
    };

    // The declared, writable app: one bearer, and the guest's name on the act.
    let (status, body) = door(a.clone(), &page, append(NS)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = door(a.clone(), &page, log(NS)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("ana, guest of alex"), "{body}");

    // The second app on the same wall, registered read-only. The SAME bearer
    // reads it — narrowing what a guest may DO is not narrowing what they see.
    let (status, body) = door(a.clone(), &page, log(DOC_NS)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // ...and its append is refused by name, with the mode the operator wrote.
    let (status, body) = door(a.clone(), &page, append(DOC_NS)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains(DOC_NS) && body.contains("read"), "{body}");

    // A namespace nobody put on the wall: refused, and never served as one of
    // the two that ARE on it.
    let (status, body) = door(a.clone(), &page, log("someone-elses")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("someone-elses"), "{body}");

    // The hard edge: a ring this daemon owns, declared by a config that was
    // wrong, refused at the route regardless.
    let (status, body) = door(a.clone(), &page, log(owned)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains(owned), "{body}");

    // And a wall grant that names nothing has not said what it wants.
    let (status, body) = door(
        a.clone(),
        &page,
        as_guest("GET", "/v1/rail/log", GUEST_TOKEN, &handle, None),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}
