// SPDX-License-Identifier: AGPL-3.0-or-later
//! A presented credential decides, from any address (ADDRESSED_TEXT §5.5
//! rules 1-3), driven through the real `client_router`.
//!
//! Until 2026-10-08 a loopback caller was admitted before its bearer was
//! read: a bogus bearer from 127.0.0.1 passed as the owner, a revoked token
//! still worked from the machine it was minted on, and a named token used
//! locally was never logged by its name. Every bearer the daemon mints starts
//! with `svrn_`, so a credential of ours that verifies nothing is refused,
//! while `docs/INTEROP.md` §1's `OPENAI_API_KEY=local` keeps working.
//!
//! Peer address is injected as `ConnectInfo`, as `client_auth.rs` does.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use kernel_types::NodeId;
use sovereign_daemon::client_auth::generate_bearer_token;
use sovereign_daemon::client_tokens::ClientTokenStore;
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::{AppState, NodeSeed};
use tower::ServiceExt;

const LOOPBACK: &str = "127.0.0.1:55002";

fn state(dir: &std::path::Path) -> (AppState, Arc<ClientTokenStore>) {
    let tokens = Arc::new(ClientTokenStore::load(Some(dir.join("client-tokens"))));
    let state = AppState::new_with_node(
        NodeId::from_u128(1),
        NodeSeed {
            named_client_tokens: Arc::clone(&tokens),
            ..Default::default()
        },
    );
    (state, tokens)
}

/// GET `/v1/models` from loopback, as curl or an SDK sends it, with
/// `authorization` as the whole header value when given.
async fn models_from_loopback(state: AppState, authorization: Option<&str>) -> StatusCode {
    let mut b = Request::get("/v1/models").header("host", "127.0.0.1:9741");
    if let Some(value) = authorization {
        b = b.header(axum::http::header::AUTHORIZATION, value);
    }
    let mut req = b.body(Body::empty()).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(LOOPBACK.parse::<SocketAddr>().unwrap()));
    client_router(state).oneshot(req).await.unwrap().status()
}

/// A credential of this daemon's form that it never issued is refused from
/// the machine itself. Red against the loopback-first order: 200.
#[tokio::test]
async fn a_bogus_credential_of_ours_from_loopback_is_401() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, _) = state(tmp.path());
    let bogus = format!("Bearer svrn_{}", "c0ffee00".repeat(8));
    assert_eq!(
        models_from_loopback(state, Some(&bogus)).await,
        StatusCode::UNAUTHORIZED,
        "a bearer of ours that verifies nothing passed as the owner"
    );
}

/// INTEROP §1, verbatim: any OpenAI client, base URL and a non-empty key.
/// `local` is not in this daemon's form, so loopback ignores it.
#[tokio::test]
async fn interop_recipe_bearer_local_from_loopback_is_200() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, _) = state(tmp.path());
    assert_eq!(
        models_from_loopback(state, Some("Bearer local")).await,
        StatusCode::OK,
        "docs/INTEROP.md §1's OPENAI_API_KEY=local stopped working"
    );
}

/// A token the operator revoked stops working from loopback too, in the same
/// daemon lifetime. Red against the loopback-first order: 200.
#[tokio::test]
async fn a_revoked_token_from_loopback_is_401() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, tokens) = state(tmp.path());
    let token = generate_bearer_token().unwrap();
    tokens.mint("harness", token.clone()).unwrap();
    let header = format!("Bearer {token}");
    assert_eq!(
        models_from_loopback(state.clone(), Some(&header)).await,
        StatusCode::OK
    );
    assert!(tokens.revoke("harness"));
    assert_eq!(
        models_from_loopback(state, Some(&header)).await,
        StatusCode::UNAUTHORIZED,
        "a revoked token still admitted from loopback"
    );
}

#[derive(Clone)]
struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl tracing_subscriber::fmt::MakeWriter<'_> for Captured {
    type Writer = Captured;
    fn make_writer(&self) -> Captured {
        self.clone()
    }
}

/// Run `f` with every tracing line at DEBUG captured, and return them.
async fn captured_lines<F: std::future::Future<Output = ()>>(f: F) -> String {
    let buf = Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(Captured(Arc::clone(&buf)))
        .with_ansi(false)
        .finish();
    {
        let _guard = tracing::subscriber::set_default(subscriber);
        f.await;
    }
    let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    out
}

/// A named token used from this machine is logged by its name, never by its
/// secret. Red against the loopback-first order: no line names it.
#[tokio::test]
async fn a_named_token_from_loopback_is_logged_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, tokens) = state(tmp.path());
    let token = generate_bearer_token().unwrap();
    tokens.mint("claude-code", token.clone()).unwrap();
    let header = format!("Bearer {token}");
    let lines = captured_lines(async {
        assert_eq!(
            models_from_loopback(state, Some(&header)).await,
            StatusCode::OK
        );
    })
    .await;
    assert!(
        lines.lines().any(|l| l.contains("claude-code")),
        "no log line named the client:\n{lines}"
    );
    assert!(
        !lines.contains(&token),
        "a log line carried the secret:\n{lines}"
    );
}
