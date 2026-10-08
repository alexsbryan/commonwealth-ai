// SPDX-License-Identifier: AGPL-3.0-or-later
//! A web page is not a local process.
//!
//! A browser on the owner's machine connects from loopback, so every route
//! that trusts a loopback peer address answered any page the owner had open:
//! `/mcp` replied to `OPTIONS` from `https://evil.example` with
//! `access-control-allow-origin: *` and served `tools/list` to a cross-site
//! `POST` (observed against the running daemon, 2026-10-08). A page under a
//! name that resolves to 127.0.0.1 needs no CORS at all: to the browser it IS
//! the daemon's origin (DNS rebinding). MCP's Streamable HTTP transport
//! requires the check: "Servers MUST validate the `Origin` header on all
//! incoming connections" (revision 2025-06-18, which `/mcp` negotiates).
//!
//! These tests drive the real routers with the three shapes a page produces
//! (a foreign `Origin`, `Sec-Fetch-Site: cross-site`, a foreign `Host`) and
//! the shapes the owner's own tools produce, which must keep their trust.
//! `ConnectInfo` is injected, as in `client_auth.rs`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use kernel_types::NodeId;
use sovereign_daemon::mcp_router::{mcp_router, McpNotifier};
use sovereign_daemon::server::{client_router, internal_router};
use sovereign_daemon::state::{AppState, NodeSeed};
use sovereign_store::sqlite::SqliteStateStore;
use tower::ServiceExt;

const LOOPBACK: &str = "127.0.0.1:55001";
const EVIL: &str = "https://evil.example";

/// What Node's `fetch` sends to a loopback MCP server (recorded 2026-10-08,
/// node v20.20.2 against a listener on 127.0.0.1): no `Origin`, no
/// `Sec-Fetch-Site`, and `sec-fetch-mode: cors`. Claude Code's MCP client
/// speaks through this `fetch`; it must stay a local process.
const NODE_FETCH: &[(&str, &str)] = &[
    ("host", "127.0.0.1:9741"),
    ("connection", "keep-alive"),
    ("accept", "application/json, text/event-stream"),
    ("accept-language", "*"),
    ("sec-fetch-mode", "cors"),
    ("user-agent", "node"),
    ("accept-encoding", "gzip, deflate"),
];

/// No token configured: a caller that loses loopback trust has no other way
/// in, so a refusal is visible as a status.
fn tokenless() -> AppState {
    AppState::new_with_node(NodeId::from_u128(1), NodeSeed::default())
}

fn from_loopback(mut req: Request<Body>) -> Request<Body> {
    let addr: SocketAddr = LOOPBACK.parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

fn get(path: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut b = Request::get(path);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    from_loopback(b.body(Body::empty()).unwrap())
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

fn is_auth_rejection(s: StatusCode) -> bool {
    s == StatusCode::UNAUTHORIZED || s == StatusCode::FORBIDDEN
}

// ── the operator bind ────────────────────────────────────────────

#[tokio::test]
async fn a_cross_site_page_on_loopback_is_refused_by_name() {
    let resp = client_router(tokenless())
        .oneshot(get(
            "/v1/models",
            &[("host", "127.0.0.1:9741"), ("origin", EVIL)],
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "a page from {EVIL} reached /v1/models as a local process"
    );
    let body = body_json(resp).await;
    assert_eq!(
        body["error"], "cross-origin",
        "the refusal must name why: {body}"
    );
    assert_eq!(body["origin"], EVIL, "and whose origin: {body}");
}

#[tokio::test]
async fn a_cross_site_fetch_with_no_origin_is_refused() {
    // A no-cors GET carries no `Origin`; the browser still says where it came
    // from in `Sec-Fetch-Site`.
    let status = client_router(tokenless())
        .oneshot(get(
            "/v1/models",
            &[("host", "127.0.0.1:9741"), ("sec-fetch-site", "cross-site")],
        ))
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_rebinding_host_gets_no_loopback_trust() {
    // Same-origin in the browser's eyes: the page was served from
    // evil.example:9741, which resolved to 127.0.0.1.
    let status = client_router(tokenless())
        .oneshot(get(
            "/v1/models",
            &[
                ("host", "evil.example:9741"),
                ("origin", "http://evil.example:9741"),
                ("sec-fetch-site", "same-origin"),
            ],
        ))
        .await
        .unwrap()
        .status();
    assert!(
        is_auth_rejection(status),
        "a request addressed to evil.example was trusted as local: {status}"
    );
}

#[tokio::test]
async fn the_owners_own_tools_and_pages_keep_loopback_trust() {
    let cases: &[(&str, &[(&str, &str)])] = &[
        ("no browser headers at all (curl, reqwest)", &[]),
        ("node fetch, as recorded", NODE_FETCH),
        (
            "a page the daemon itself serves, same origin",
            &[
                ("host", "localhost:9741"),
                ("origin", "http://localhost:9741"),
                ("sec-fetch-site", "same-origin"),
            ],
        ),
        (
            "a URL typed into the address bar",
            &[("host", "127.0.0.1:9741"), ("sec-fetch-site", "none")],
        ),
        ("IPv6 loopback by name", &[("host", "[::1]:9741")]),
    ];
    for (what, headers) in cases {
        let status = client_router(tokenless())
            .oneshot(get("/v1/models", headers))
            .await
            .unwrap()
            .status();
        assert!(
            !is_auth_rejection(status),
            "{what}: a local tool lost loopback trust ({status})"
        );
    }
}

// ── /mcp ─────────────────────────────────────────────────────────

fn mcp(dir: &std::path::Path) -> axum::Router {
    let notes = Arc::new(SqliteStateStore::open(&dir.join("sovereign.db")).unwrap());
    mcp_router(
        Arc::new(sovereign_contracts::ToolRegistry::new()),
        notes,
        "browser-origin".into(),
        None,
        McpNotifier::new(),
    )
}

fn tools_list(headers: &[(&str, &str)]) -> Request<Body> {
    let mut b = Request::post("/mcp")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    from_loopback(
        b.body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        ))
        .unwrap(),
    )
}

#[tokio::test]
async fn mcp_grants_no_foreign_origin_a_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let req = from_loopback(
        Request::options("/mcp")
            .header("host", "127.0.0.1:9741")
            .header("origin", EVIL)
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "content-type")
            .body(Body::empty())
            .unwrap(),
    );
    let resp = mcp(dir.path()).oneshot(req).await.unwrap();
    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "/mcp let {EVIL} read its replies: {:?}",
        resp.headers()
    );
}

#[tokio::test]
async fn mcp_refuses_a_cross_site_post() {
    let dir = tempfile::tempdir().unwrap();
    let resp = mcp(dir.path())
        .oneshot(tools_list(&[
            ("host", "127.0.0.1:9741"),
            ("origin", EVIL),
            ("sec-fetch-site", "cross-site"),
        ]))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "a page from {EVIL} listed the MCP tools"
    );
}

#[tokio::test]
async fn mcp_still_serves_the_owners_mcp_client() {
    let dir = tempfile::tempdir().unwrap();
    let resp = mcp(dir.path())
        .oneshot(tools_list(NODE_FETCH))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "node fetch lost /mcp");
}

// ── the internal port ────────────────────────────────────────────

#[tokio::test]
async fn the_internal_port_admits_no_cross_site_page() {
    let state = AppState::new(NodeId::from_u128(1));
    let req = from_loopback(
        Request::post("/internal/mesh/quiesce")
            .header("content-type", "application/json")
            .header("host", "127.0.0.1:9742")
            .header("origin", EVIL)
            .body(Body::from(r#"{"quiesced":true}"#))
            .unwrap(),
    );
    let resp = internal_router(state.clone()).oneshot(req).await.unwrap();
    assert!(
        is_auth_rejection(resp.status()),
        "a page from {EVIL} reached the internal port: {}",
        resp.status()
    );
    assert!(!state.mesh_quiesced(), "and the flag moved");
}
