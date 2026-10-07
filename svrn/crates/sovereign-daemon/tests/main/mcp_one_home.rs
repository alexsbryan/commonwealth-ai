// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(feature = "treesitter")]
//! One home for each tool on `:9741/mcp` (pb-code-daemon-exit): svrn serves
//! no code tool and hosts no code runtime, so `symbols` is served by the
//! code program alone.
//!
//! - svrn's own registry, built by the daemon's real builder, holds none of
//!   code's tools;
//! - svrn alone (no code program composed) lists no code tool, answers a
//!   code tool with a pointer to `svrn code mcp`, and answers
//!   `/v1/projects` with the same named absence;
//! - with the code program composed, its tools are listed beside svrn's
//!   exactly once and a call reaches code's host.
//!
//! The stock binary's composition is proven end to end in sovereign-stock's
//! `the_stock_install_serves_code_on_its_one_mcp`; `svrn code mcp` answering
//! `symbols` alone in sovereign-cli-dev's `code_mcp_e2e`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use host_kit::mcp::{McpMountedTools, McpRequestContext, ToolOutcome};
use serde_json::{json, Value};

use corpus_index::ingest_port::double::IngestPortDouble;
use sovereign_daemon::mcp_router::{mcp_router, McpNotifier};
use sovereign_store::sqlite::SqliteStateStore;

/// Every tool id the code program serves (sovereign-code's bundles and the
/// work atlas). The daemon cannot name code's exposure list — it links no
/// sovereign-code — so the ids are data here.
const CODE_TOOL_IDS: &[&str] = &[
    "symbols",
    "code_search",
    "recent_changes",
    "callers",
    "callees",
    "blast",
    "capability_map",
    "note",
    "notes",
    "retire_note",
    "delete_note",
    "read_note_by_id",
    "promote_note",
    "read_note_digest",
    "write_redteam_finding",
    "session_reflection",
    "session_state",
    "build",
    "lint_status",
    "get_lint_output",
    "test_status",
    "get_run_output",
    "run_tests",
    "arch_report",
    "arch_posture",
    "drift_posture",
    "drift_findings",
    "declare_scope",
    "release_scope",
    "work_in_flight",
    "resource_may_i",
    "facts",
    "briefing",
    "capability_posture",
    "capability_findings",
];

/// The daemon's real `/mcp` registry, over an empty index root.
async fn svrn_registry(dir: &std::path::Path) -> sovereign_contracts::ToolRegistry {
    let embed: corpus_index::types::EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async {
            Ok::<Vec<f32>, corpus_index::Error>(vec![0.0; corpus_index::types::DEFAULT_EMBED_DIM])
        })
    });
    let engine = Arc::new(
        IngestPortDouble::new()
            .with_index_dir(dir.join("indexes"))
            .with_embed_fn(embed),
    );
    let atlas = Arc::new(corpus_engine_atlas_reader::ports::double::AtlasPortDouble::new());
    sovereign_daemon::tool_registry::build_tool_registry(Some((engine, atlas))).await
}

/// Serve `router` on 127.0.0.1:0 with `ConnectInfo`; the URL prefix.
async fn spawn(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

async fn rpc(base: &str, id: u64, method: &str, params: Value) -> Value {
    rpc_as(base, &[], id, method, params).await
}

/// [`rpc`] from a connection that sends `headers`.
async fn rpc_as(
    base: &str,
    headers: &[(&str, &str)],
    id: u64,
    method: &str,
    params: Value,
) -> Value {
    let mut req = reqwest::Client::new().post(format!("{base}/mcp"));
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    req.json(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
        .send()
        .await
        .expect("post /mcp")
        .json()
        .await
        .expect("a JSON-RPC reply")
}

fn names(list: &Value) -> Vec<String> {
    list["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list has no tools: {list}"))
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect()
}

/// The one-home guard: svrn's registry, as the daemon builds it, holds none
/// of code's tools.
#[tokio::test]
async fn svrns_registry_holds_no_code_tool() {
    let dir = tempfile::tempdir().unwrap();
    let registry = svrn_registry(dir.path()).await;
    let ids: Vec<String> = registry.descriptors().into_iter().map(|d| d.id).collect();
    let second_home: Vec<&String> = ids
        .iter()
        .filter(|id| CODE_TOOL_IDS.contains(&id.as_str()))
        .collect();
    assert!(
        second_home.is_empty(),
        "svrn's /mcp registry holds code's tools {second_home:?}: a second home for \
         what the code program serves (pb-code-daemon-exit). svrn's ids: {ids:?}"
    );
    assert!(
        ids.iter().any(|id| id == "wikipedia_fetch"),
        "svrn's own tools are missing from its registry: {ids:?}"
    );
}

#[tokio::test]
async fn svrn_alone_points_a_code_tool_at_the_code_server() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(svrn_registry(dir.path()).await);
    let notes = Arc::new(SqliteStateStore::open(&dir.path().join("sovereign.db")).unwrap());
    let base = spawn(mcp_router(
        registry,
        notes,
        "one-home".into(),
        None,
        McpNotifier::new(),
    ))
    .await;

    let listed = names(&rpc(&base, 1, "tools/list", json!({})).await);
    assert!(
        listed.iter().any(|n| n == "wikipedia_fetch"),
        "svrn's tools: {listed:?}"
    );
    assert!(
        !listed.iter().any(|n| CODE_TOOL_IDS.contains(&n.as_str())),
        "svrn alone lists a code tool: {listed:?}"
    );

    let call = rpc(
        &base,
        2,
        "tools/call",
        json!({ "name": "symbols", "arguments": { "name": "main" } }),
    )
    .await;
    assert_eq!(call["error"]["code"], -32601, "{call}");
    let message = call["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("svrn code mcp"),
        "the absence does not name the code server: {call}"
    );

    let projects = spawn(sovereign_daemon::hosted_code::projects_absent_router(
        sovereign_daemon::process::Posture::Open,
    ))
    .await;
    let resp = reqwest::get(format!("{projects}/v1/projects"))
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("svrn code mcp")),
        "/v1/projects' absence does not name the code server: {body}"
    );
}

/// A stand-in for code's mounted host: one tool, `symbols`, which answers
/// with the corpus scope it was called under; it holds corpus `a` alone.
struct CodeStandIn;

impl McpMountedTools for CodeStandIn {
    fn list(&self, _ctx: &McpRequestContext) -> Value {
        json!([{ "name": "symbols", "description": "code's", "inputSchema": {} }])
    }

    fn admit(&self, ctx: &McpRequestContext) -> Result<(), String> {
        match ctx.corpus.as_deref() {
            None | Some("a") => Ok(()),
            Some(other) => Err(format!("`{other}` is not an indexed code corpus: a")),
        }
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        _args: &'a Value,
        ctx: &'a McpRequestContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolOutcome>> + Send + 'a>> {
        Box::pin(async move {
            (name == "symbols").then(|| {
                ToolOutcome::answer(format!("answered by code for {:?}", ctx.corpus), None)
            })
        })
    }
}

#[tokio::test]
async fn a_composed_code_program_answers_its_tools_on_the_same_mcp_once() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(svrn_registry(dir.path()).await);
    let notes = Arc::new(SqliteStateStore::open(&dir.path().join("sovereign.db")).unwrap());
    let base = spawn(mcp_router(
        registry,
        notes,
        "one-home".into(),
        Some(Arc::new(CodeStandIn)),
        McpNotifier::new(),
    ))
    .await;

    let listed = names(&rpc(&base, 1, "tools/list", json!({})).await);
    assert_eq!(
        listed.iter().filter(|n| *n == "symbols").count(),
        1,
        "{listed:?}"
    );
    assert!(listed.iter().any(|n| n == "wikipedia_fetch"), "{listed:?}");

    let call = rpc(
        &base,
        2,
        "tools/call",
        json!({ "name": "symbols", "arguments": { "name": "main" } }),
    )
    .await;
    assert_eq!(
        call["result"]["content"][0]["text"], "answered by code for None",
        "{call}"
    );
    let unknown = rpc(&base, 3, "tools/call", json!({ "name": "nope" })).await;
    assert_eq!(unknown["error"]["code"], -32601, "{unknown}");
}

/// The code-intel-repo-scope order, step 1: the `x-svrn-corpus` header
/// reaches code's host on the one `/mcp`. A corpus code holds is passed
/// through; no header is every corpus, as before; a corpus code does not hold
/// is -32602 naming the ones it does, for svrn's own tools too, before any
/// tool runs. FAILING INPUT: a mount that drops the header answers the scoped
/// call `for None`.
#[tokio::test]
async fn the_corpus_header_reaches_code_and_an_unknown_corpus_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(svrn_registry(dir.path()).await);
    let notes = Arc::new(SqliteStateStore::open(&dir.path().join("sovereign.db")).unwrap());
    let base = spawn(mcp_router(
        registry,
        notes,
        "scoped".into(),
        Some(Arc::new(CodeStandIn)),
        McpNotifier::new(),
    ))
    .await;
    let symbols = json!({ "name": "symbols", "arguments": { "name": "Foo" } });

    let scoped = rpc_as(
        &base,
        &[("x-svrn-corpus", "a")],
        1,
        "tools/call",
        symbols.clone(),
    )
    .await;
    assert_eq!(
        scoped["result"]["content"][0]["text"], "answered by code for Some(\"a\")",
        "{scoped}"
    );

    let unscoped = rpc(&base, 2, "tools/call", symbols.clone()).await;
    assert_eq!(
        unscoped["result"]["content"][0]["text"], "answered by code for None",
        "{unscoped}"
    );

    for (id, name) in [(3, "symbols"), (4, "wikipedia_fetch")] {
        let refused = rpc_as(
            &base,
            &[("x-svrn-corpus", "nope")],
            id,
            "tools/call",
            json!({ "name": name, "arguments": {} }),
        )
        .await;
        assert_eq!(refused["error"]["code"], -32602, "{name}: {refused}");
        let message = refused["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("`nope`") && message.contains(": a"),
            "{name}: the refusal does not name the indexed corpora: {refused}"
        );
    }
}

/// The order's step 4: a read-only connection (`x-svrn-effects: read`)
/// lists no tool whose MANIFEST effect is not `Read`, judged against the
/// registry's own descriptors rather than a hand list, and a call of
/// `corpus_store` is refused naming its effect. FAILING INPUT: the list
/// filtered but the call not, so `corpus_store` runs; the last assertion
/// catches it.
#[tokio::test]
async fn a_read_only_connection_neither_lists_nor_calls_a_write_tool() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(svrn_registry(dir.path()).await);
    let effects: std::collections::HashMap<String, sovereign_contracts::Effect> = registry
        .descriptors()
        .into_iter()
        .map(|d| (d.id, d.effect))
        .collect();
    let notes = Arc::new(SqliteStateStore::open(&dir.path().join("sovereign.db")).unwrap());
    let base = spawn(mcp_router(
        registry,
        notes,
        "read-only".into(),
        None,
        McpNotifier::new(),
    ))
    .await;
    let read_only = [("x-svrn-effects", "read")];

    let full = names(&rpc(&base, 1, "tools/list", json!({})).await);
    let listed = names(&rpc_as(&base, &read_only, 2, "tools/list", json!({})).await);
    assert!(
        full.iter().any(|n| n == "corpus_store"),
        "the full list must hold corpus_store for this test to mean anything: {full:?}"
    );
    assert!(!listed.is_empty(), "a read-only list keeps the read tools");
    for name in &listed {
        assert_eq!(
            effects.get(name),
            Some(&sovereign_contracts::Effect::Read),
            "a read-only connection lists `{name}`, whose manifest effect is not Read"
        );
    }
    let hidden: Vec<&String> = full.iter().filter(|n| !listed.contains(n)).collect();
    assert!(
        hidden.iter().any(|n| *n == "corpus_store"),
        "corpus_store is still listed: {listed:?}"
    );

    // Arguments that validate, so only the effect gate stands between the
    // call and a write.
    let embedding = vec![0.0f32; corpus_index::types::DEFAULT_EMBED_DIM];
    let call = rpc_as(
        &base,
        &read_only,
        3,
        "tools/call",
        json!({ "name": "corpus_store",
                "arguments": { "corpus": "x", "chunks": json!(["hello"]).to_string(),
                               "embeddings": json!([embedding]).to_string() } }),
    )
    .await;
    assert_eq!(call["result"]["isError"], true, "corpus_store ran: {call}");
    assert!(
        !dir.path().join("indexes").join("x").exists(),
        "corpus_store wrote corpus `x` on a read-only connection: {call}"
    );
    let text = call["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains("Write") && text.contains("x-svrn-effects"),
        "the refusal does not name the effect: {call}"
    );
}
