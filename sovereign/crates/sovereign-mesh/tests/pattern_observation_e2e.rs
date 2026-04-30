//! End-to-end test for Phase 7.1 ToolPatternMatcher wired through
//! `mcp_router::handle_tool_call`.
//!
//! Spawns a real `mcp_router` over an ephemeral localhost port,
//! POSTs two `tools/call` invocations (`blast` then `build`), and
//! confirms that:
//!
//! 1. The `tool_call_log` ring buffer records both calls.
//! 2. The `ToolPatternMatcher` Extension fires after the second
//!    call, writes a `kind='reflection'` + `source='observed'`
//!    note describing the blast→build sequence.
//!
//! The matcher runs on a background tokio task (fire-and-forget
//! after `log_tool_call`), so we poll briefly until the note
//! shows up — typical latency is under 50ms.

use std::sync::Arc;
use std::time::Duration;

use corpus_engine::{NoteSource, NoteStore};
use sovereign_core::registry::ToolRegistry;
use sovereign_core::types::{
    Effect, Idempotency, Latency, Scope, StepOutput, ToolContext, ToolDescriptor,
};
use sovereign_core::Tool;
use sovereign_mesh::mcp_router::{mcp_router, FeatureRoot, McpNotifier};

/// Stub tool — we only care that the registry resolves the id and
/// the dispatch fires; the body is a single-line text response.
struct StubTool {
    descriptor: ToolDescriptor,
}

#[async_trait::async_trait]
impl Tool for StubTool {
    fn descriptor(&self) -> ToolDescriptor {
        self.descriptor.clone()
    }
    async fn execute(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput, sovereign_core::Error> {
        Ok(StepOutput::Text("ok".into()))
    }
    fn required_permissions(&self) -> Vec<sovereign_core::types::Permission> {
        Vec::new()
    }
}

fn stub(id: &str) -> Box<dyn Tool> {
    Box::new(StubTool {
        descriptor: ToolDescriptor {
            id: id.into(),
            name: id.into(),
            description: format!("stub for {id}"),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: None,
        },
    })
}

async fn post_tools_call(addr: std::net::SocketAddr, name: &str) {
    let url = format!("http://127.0.0.1:{}/mcp", addr.port());
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": {} }
    });
    let _ = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .expect("tools/call POST");
}

/// Full Phase 7.1 loop: a `blast` followed by a `build` tool call
/// fires the InvestigateThenAct pattern, which the matcher
/// records as a `source='observed'` note via the live MCP wire.
#[tokio::test]
async fn blast_then_build_writes_observed_note_via_live_mcp_wire() {
    let dir = tempfile::tempdir().unwrap();
    let notes_path = dir.path().join("notes.db");
    let notes = Arc::new(NoteStore::open(&notes_path).expect("notes open"));

    let mut registry = ToolRegistry::new();
    registry.register(stub("blast"));
    registry.register(stub("build"));
    let registry = Arc::new(registry);

    let notifier = McpNotifier::new();
    let feature_root = dir.path().to_path_buf();
    let app = mcp_router(
        registry,
        Arc::clone(&notes),
        "obs-test-session".into(),
        FeatureRoot::new(Some(feature_root)),
        notifier,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let svc = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, svc).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Sequence: blast then build. Matcher should fire on the
    // second call.
    post_tools_call(addr, "blast").await;
    post_tools_call(addr, "build").await;

    // Matcher runs on a tokio::spawn'd task; poll the DB until
    // the observed note appears (or timeout).
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let rows = notes
            .read_notes(
                None,
                &[],
                &[],
                &["reflection".to_string()],
                100,
                false,
            )
            .await
            .unwrap();
        let observed: Vec<_> = rows
            .iter()
            .filter(|n| n.source == NoteSource::Observed.as_str())
            .collect();
        if !observed.is_empty() {
            // Body should describe the blast→build sequence.
            let body = observed[0].content.to_lowercase();
            assert!(
                body.contains("investigated") || body.contains("blast"),
                "observed note body should describe pattern; got: {body}"
            );
            // Ring buffer sanity: both tool calls landed.
            let log = notes.tool_call_log_rows(0, 100).await.unwrap();
            let names: Vec<&str> = log.iter().map(|r| r.tool_name.as_str()).collect();
            assert!(names.contains(&"blast"), "log missing blast: {names:?}");
            assert!(names.contains(&"build"), "log missing build: {names:?}");
            return;
        }
        if std::time::Instant::now() >= deadline {
            let log = notes.tool_call_log_rows(0, 100).await.unwrap();
            panic!(
                "observed note never appeared. tool_call_log: {} rows; \
                 reflection rows: {}",
                log.len(),
                rows.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
