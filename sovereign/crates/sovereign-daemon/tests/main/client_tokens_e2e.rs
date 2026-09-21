// SPDX-License-Identifier: AGPL-3.0-or-later
//! The four clauses of `tg-token-revoked-alone`, driven through the real
//! `client_router` (`quality/campaigns/threat-gaps.toml`).
//!
//! One test per clause, each named for the clause it proves:
//!
//! (a) two named tokens each admit a non-loopback caller;
//! (b) revoking one refuses it IN THE SAME DAEMON LIFETIME while the other
//!     still admits — no restart, no reload;
//! (c) the admitting LABEL appears in the log line, and the token does not;
//! (d) the shared token admits by default and is refused under
//!     `client_tokens = "named-only"`.
//!
//! Peer address is injected as `ConnectInfo` rather than reached over TCP, for
//! the reason `client_auth.rs` gives: the layer reads it from the extension,
//! and a live listener makes the loopback-vs-remote split flaky on a box with
//! no routable NIC.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use sovereign_daemon::client_tokens::{ClientTokenStore, ClientTokens};
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::{AppState, NodeSeed};
use tower::ServiceExt;

const LAN_PEER: &str = "192.168.1.50:44444";
const SHARED: &str = "shared-deadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00d";

/// A node with a shared token, a posture, and a named-token store rooted in
/// `dir` — the three things this bar is about, and nothing else.
fn state(dir: &std::path::Path, posture: ClientTokens) -> (AppState, Arc<ClientTokenStore>) {
    let node = NodeId::from_u128(1);
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(7),
        name: "Test".into(),
        invite_key_hash: [3u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: HashMap::new(),
        peers: vec![],
    };
    let tokens = Arc::new(ClientTokenStore::load(Some(dir.to_path_buf())));
    let state = AppState::new_with_node(
        node,
        mesh,
        NodeSeed {
            client_token: Some(Arc::<str>::from(SHARED)),
            client_tokens: posture,
            named_client_tokens: Arc::clone(&tokens),
            ..Default::default()
        },
    );
    (state, tokens)
}

/// GET a gated route from a REMOTE peer bearing `bearer`.
async fn get_as_remote(state: AppState, bearer: &str) -> StatusCode {
    let mut req = Request::get("/v1/models")
        .header(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {bearer}"),
        )
        .body(Body::empty())
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(LAN_PEER.parse::<SocketAddr>().unwrap()));
    client_router(state).oneshot(req).await.unwrap().status()
}

fn is_auth_rejection(s: StatusCode) -> bool {
    s == StatusCode::UNAUTHORIZED
        || s == StatusCode::FORBIDDEN
        || s == StatusCode::INTERNAL_SERVER_ERROR
}

/// Clause (a): two named tokens, each admitting a caller that is not on this
/// machine and does not hold the shared token.
#[tokio::test]
async fn two_named_tokens_each_admit_a_remote_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, tokens) = state(&tmp.path().join("client-tokens"), ClientTokens::default());
    tokens.mint("laptop", "tok-laptop".into()).unwrap();
    tokens.mint("tablet", "tok-tablet".into()).unwrap();

    for token in ["tok-laptop", "tok-tablet"] {
        assert!(
            !is_auth_rejection(get_as_remote(state.clone(), token).await),
            "{token} must admit a remote caller"
        );
    }
    assert_eq!(
        get_as_remote(state, "tok-nobody").await,
        StatusCode::UNAUTHORIZED,
        "a token nobody minted must not admit"
    );
}

/// Clause (b): revoking `laptop` refuses it on the NEXT request, from the same
/// `AppState` — the daemon was never restarted and the store was never
/// reloaded — and `tablet` is untouched.
#[tokio::test]
async fn revoking_one_refuses_it_in_the_same_lifetime_and_leaves_the_other() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, tokens) = state(&tmp.path().join("client-tokens"), ClientTokens::default());
    tokens.mint("laptop", "tok-laptop".into()).unwrap();
    tokens.mint("tablet", "tok-tablet".into()).unwrap();
    assert!(!is_auth_rejection(
        get_as_remote(state.clone(), "tok-laptop").await
    ));

    assert!(tokens.revoke("laptop"));

    assert_eq!(
        get_as_remote(state.clone(), "tok-laptop").await,
        StatusCode::UNAUTHORIZED,
        "a revoked token must be refused without a restart"
    );
    assert!(
        !is_auth_rejection(get_as_remote(state.clone(), "tok-tablet").await),
        "revoking one device must not withdraw another's credential"
    );
    assert!(
        !is_auth_rejection(get_as_remote(state, SHARED).await),
        "revoking one device must not withdraw the shared token either"
    );
}

/// Clause (c): the admit line names the LABEL. It must also NOT carry the
/// token — a credential in a log is a credential in every scrollback and log
/// shipper downstream of it.
#[tokio::test]
async fn the_admitting_label_appears_in_the_log_line_and_the_token_does_not() {
    #[derive(Clone)]
    struct BufWriter(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl tracing_subscriber::fmt::MakeWriter<'_> for BufWriter {
        type Writer = BufWriter;
        fn make_writer(&self) -> BufWriter {
            self.clone()
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let (state, tokens) = state(&tmp.path().join("client-tokens"), ClientTokens::default());
    tokens.mint("laptop", "tok-laptop".into()).unwrap();

    let buf = Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(BufWriter(Arc::clone(&buf)))
        .with_ansi(false)
        .finish();
    {
        let _guard = tracing::subscriber::set_default(subscriber);
        let _ = get_as_remote(state, "tok-laptop").await;
    }
    let captured = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    let admit: Vec<&str> = captured
        .lines()
        .filter(|l| l.contains("named client token admitted"))
        .collect();
    assert_eq!(
        admit.len(),
        1,
        "exactly one admit line was expected; got:\n{captured}"
    );
    assert!(
        admit[0].contains("laptop"),
        "the admit line must name WHO was admitted: {}",
        admit[0]
    );
    assert!(
        !captured.contains("tok-laptop"),
        "no log line may carry the token itself:\n{captured}"
    );
}

/// Clause (d): the shared token admits on the default posture and is refused,
/// with a sentence, under `named-only` — where a named token still admits.
#[tokio::test]
async fn the_shared_token_admits_by_default_and_is_refused_under_named_only() {
    let tmp = tempfile::tempdir().unwrap();

    let (shared_state, _) = state(&tmp.path().join("default"), ClientTokens::default());
    assert!(
        !is_auth_rejection(get_as_remote(shared_state, SHARED).await),
        "the shared token is the 0-to-1 path and must keep working by default"
    );

    let (strict, tokens) = state(&tmp.path().join("strict"), ClientTokens::NamedOnly);
    tokens.mint("laptop", "tok-laptop".into()).unwrap();
    let mut req = Request::get("/v1/models")
        .header(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {SHARED}"),
        )
        .body(Body::empty())
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(LAN_PEER.parse::<SocketAddr>().unwrap()));
    let resp = client_router(strict.clone()).oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "named-only must refuse the shared token"
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("named-only"),
        "the refusal must say WHY, not just 'authentication required': {body}"
    );

    assert!(
        !is_auth_rejection(get_as_remote(strict, "tok-laptop").await),
        "named-only must still admit a named token"
    );
}
