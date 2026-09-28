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

use corpus_engine::CorpusEngine;
use corpus_engine_notes::NoteStore;
use sovereign_daemon::mcp_router::{mcp_router, McpNotifier};

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
    let engine = Arc::new(CorpusEngine::new(
        dir.join("recipes"),
        dir.join("indexes"),
        embed,
    ));
    let solve_jobs = Arc::new(sovereign_daemon::solve_http::SolveJobs::new(1));
    sovereign_daemon::tool_registry::build_tool_registry(engine, solve_jobs).await
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
    reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
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
    let notes = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());
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

    let projects = spawn(sovereign_daemon::hosted_code::projects_absent_router()).await;
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

/// A stand-in for code's mounted host: one tool, `symbols`.
struct CodeStandIn;

impl McpMountedTools for CodeStandIn {
    fn list(&self) -> Value {
        json!([{ "name": "symbols", "description": "code's", "inputSchema": {} }])
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        _args: &'a Value,
        _ctx: &'a McpRequestContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolOutcome>> + Send + 'a>> {
        Box::pin(async move {
            (name == "symbols").then(|| ToolOutcome::answer("answered by code".to_string(), None))
        })
    }
}

#[tokio::test]
async fn a_composed_code_program_answers_its_tools_on_the_same_mcp_once() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(svrn_registry(dir.path()).await);
    let notes = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());
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
        call["result"]["content"][0]["text"], "answered by code",
        "{call}"
    );
    let unknown = rpc(&base, 3, "tools/call", json!({ "name": "nope" })).await;
    assert_eq!(unknown["error"]["code"], -32601, "{unknown}");
}
