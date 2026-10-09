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
use sovereign_daemon::client_tokens::{ClientTokenStore, Loopback, LoopbackPosture};
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::{AppState, NodeSeed};
use tower::ServiceExt;

const LOOPBACK: &str = "127.0.0.1:55002";

fn state(dir: &std::path::Path) -> (AppState, Arc<ClientTokenStore>) {
    let tokens = Arc::new(ClientTokenStore::load(
        Some(dir.join("client-tokens")),
        LoopbackPosture {
            loopback: Loopback::Owner,
            declared: true,
        },
    ));
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
    tokens.mint("harness", &[], token.clone()).unwrap();
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
    tokens.mint("claude-code", &[], token.clone()).unwrap();
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

/// POST `/internal/client/token` from loopback, minting `name`, presenting
/// `bearer` when given.
async fn mint_from_loopback(state: AppState, bearer: Option<&str>, name: &str) -> StatusCode {
    let mut b = Request::post("/internal/client/token")
        .header("host", "127.0.0.1:9741")
        .header("content-type", "application/json");
    if let Some(t) = bearer {
        b = b.header(axum::http::header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let mut req = b
        .body(Body::from(format!(r#"{{"name":"{name}"}}"#)))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(LOOPBACK.parse::<SocketAddr>().unwrap()));
    client_router(state).oneshot(req).await.unwrap().status()
}

/// A named client on loopback is a client, not the owner: it may not mint a
/// credential (ADDRESSED_TEXT §5.5 rule 3, appendix defect 5). The owner, a
/// local process presenting nothing, still may. Red before the owner check:
/// the harness minted (200).
#[tokio::test]
async fn a_named_client_minting_a_token_is_403() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, tokens) = state(tmp.path());
    let harness = generate_bearer_token().unwrap();
    tokens.mint("claude-code", &[], harness.clone()).unwrap();
    assert_eq!(
        mint_from_loopback(state.clone(), Some(&harness), "smuggled").await,
        StatusCode::FORBIDDEN,
        "a named client minted a credential"
    );
    assert!(
        tokens.list().iter().all(|r| r.name != "smuggled"),
        "and the credential exists"
    );
    assert_eq!(
        mint_from_loopback(state, None, "laptop").await,
        StatusCode::OK,
        "the owner lost minting"
    );
}
// ── /mcp behind the one auth layer ───────────────────────────────

/// An svrn tool that answers with the caller its `ToolContext` names.
struct EchoCaller;

#[async_trait::async_trait]
impl sovereign_contracts::traits::Tool for EchoCaller {
    fn descriptor(&self) -> sovereign_contracts::types::ToolDescriptor {
        use sovereign_contracts::types::{Effect, Idempotency, Latency, Scope};
        sovereign_contracts::types::ToolDescriptor {
            // An id svrn's MCP surface exposes, so the call runs and is logged.
            id: "corpus_search".into(),
            name: "corpus_search".into(),
            description: "echoes the caller".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: None,
        }
    }
    async fn execute(
        &self,
        _args: &serde_json::Value,
        ctx: &sovereign_contracts::types::ToolContext,
    ) -> Result<sovereign_contracts::types::StepOutput, sovereign_contracts::Error> {
        Ok(sovereign_contracts::types::StepOutput::Text(format!(
            "caller={:?}",
            ctx.caller
        )))
    }
    fn required_permissions(&self) -> Vec<sovereign_contracts::types::Permission> {
        Vec::new()
    }
}

/// `/mcp` as the daemon mounts it: behind `client_auth` on the operator
/// policy, with svrn's own store as its call log.
fn mounted_mcp(
    state: &AppState,
    notes: Arc<sovereign_store::sqlite::SqliteStateStore>,
) -> axum::Router {
    let mut registry = sovereign_contracts::ToolRegistry::new();
    registry.register(Box::new(EchoCaller));
    sovereign_daemon::client_auth::behind(
        sovereign_daemon::mcp_router::mcp_router(
            Arc::new(registry),
            notes,
            "named-client".into(),
            None,
            sovereign_daemon::mcp_router::McpNotifier::new(),
        ),
        state,
        sovereign_daemon::server::ClientSurface::Operator.auth_policy(),
    )
}

async fn mcp_call(router: axum::Router, bearer: Option<&str>) -> axum::response::Response {
    let mut b = Request::post("/mcp")
        .header("host", "127.0.0.1:9741")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    if let Some(t) = bearer {
        b = b.header(axum::http::header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let mut req = b
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"corpus_search","arguments":{}}}"#,
        ))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(LOOPBACK.parse::<SocketAddr>().unwrap()));
    router.oneshot(req).await.unwrap()
}

/// A named client's MCP call carries its name into `ToolContext` and into
/// svrn's call log (ADDRESSED_TEXT §5.5 rule 3). Watched red against a
/// framing that ignores the caller: the tool saw `None` and the row named no
/// one.
#[tokio::test]
async fn a_named_clients_mcp_call_carries_its_name_to_the_tool_and_the_log() {
    use sovereign_contracts::notes::AgentNotes;
    let tmp = tempfile::tempdir().unwrap();
    let (state, tokens) = state(tmp.path());
    let token = generate_bearer_token().unwrap();
    tokens.mint("claude-code", &[], token.clone()).unwrap();
    let notes = Arc::new(
        sovereign_store::sqlite::SqliteStateStore::open(&tmp.path().join("sovereign.db")).unwrap(),
    );
    let resp = mcp_call(mounted_mcp(&state, Arc::clone(&notes)), Some(&token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap(),
    )
    .unwrap();
    let text = body["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert_eq!(
        text, r#"caller=Some("asserted:claude-code")"#,
        "the tool's context did not name the client: {body}"
    );
    // The log write is fire-and-forget; give it its turn.
    let mut rows = Vec::new();
    for _ in 0..50 {
        rows = notes.tool_call_log_rows(0, 10).await.unwrap();
        if !rows.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(rows.len(), 1, "one call, one row: {rows:?}");
    assert_eq!(rows[0].caller.as_deref(), Some("asserted:claude-code"));
}

/// `/mcp` sits behind the same layer: a credential of ours that verifies
/// nothing is refused there too, and the owner's own MCP client, presenting
/// nothing, is still served.
#[tokio::test]
async fn mcp_refuses_a_bogus_credential_of_ours_and_still_serves_the_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, _) = state(tmp.path());
    let notes = Arc::new(
        sovereign_store::sqlite::SqliteStateStore::open(&tmp.path().join("sovereign.db")).unwrap(),
    );
    let bogus = format!("svrn_{}", "ab".repeat(32));
    let refused = mcp_call(mounted_mcp(&state, Arc::clone(&notes)), Some(&bogus)).await;
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    let owner = mcp_call(mounted_mcp(&state, notes), None).await;
    assert_eq!(owner.status(), StatusCode::OK);
}
