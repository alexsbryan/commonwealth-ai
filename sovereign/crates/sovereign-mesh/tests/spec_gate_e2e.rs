//! End-to-end test for the Phase 5 + 5b spec-presence gate.
//!
//! Wires `mcp_router` over an ephemeral localhost port with:
//!
//! - a tempdir as `FeatureRoot`,
//! - an `McpNotifier` shared with a `SpecWatcher` rooted at that
//!   tempdir.
//!
//! Verifies the full loop:
//!
//! 1. `tools/list` with no spec on disk → spec-gated tools (`spec`,
//!    `drift`) are absent.
//! 2. SSE subscriber opens `GET /mcp` (the notification channel).
//! 3. Test writes `.sovereign/features/foo/spec.md`.
//! 4. SpecWatcher fires; cache is invalidated; notifier broadcasts
//!    `notifications/tools/list_changed`; the SSE subscriber receives
//!    the JSON-RPC frame.
//! 5. `tools/list` re-issued → spec-gated tools now present.
//!
//! This is the path a real MCP client (Claude Code, opencode) takes
//! when the user creates a feature spec mid-session.

use std::sync::Arc;
use std::time::Duration;

use corpus_engine_notes::NoteStore;
use futures::StreamExt;
use sovereign_core::registry::ToolRegistry;
use sovereign_core::types::{
    Effect, Idempotency, Latency, Scope, StepOutput, ToolContext, ToolDescriptor,
};
use sovereign_core::Tool;
use sovereign_mesh::mcp_router::{mcp_router, FeatureRoot, McpNotifier};
use sovereign_tools::spec_watcher::SpecWatcher;

/// Tiny stub tool — not actually exercised over the wire in these
/// tests, but the registry has to expose descriptors named
/// `callers` / `note` (always-on) and `spec` / `drift` (gated) for
/// `tools/list` to render them in/out per the gate.
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
        Ok(StepOutput::Text("stub".into()))
    }
    fn required_permissions(&self) -> Vec<sovereign_core::types::Permission> {
        Vec::new()
    }
}

fn stub(id: &str, desc: &str) -> Box<dyn Tool> {
    Box::new(StubTool {
        descriptor: ToolDescriptor {
            id: id.into(),
            name: id.into(),
            description: desc.into(),
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

fn build_registry() -> Arc<ToolRegistry> {
    let mut r = ToolRegistry::new();
    // Always-on
    r.register(stub("callers", "always-on"));
    r.register(stub("note", "always-on"));
    // Spec-gated
    r.register(stub("spec", "gated"));
    r.register(stub("drift", "gated"));
    Arc::new(r)
}

async fn spawn_router(
    registry: Arc<ToolRegistry>,
    feature_root_path: std::path::PathBuf,
    notifier: McpNotifier,
) -> std::net::SocketAddr {
    let notes_path = tempfile::tempdir().unwrap().path().join("notes.db");
    // Tempdir holding the notes path; held alive by leaking — the
    // tokio runtime owns the test lifetime.
    let notes = Arc::new(NoteStore::open(&notes_path).expect("notes open"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = mcp_router(
        registry,
        notes,
        "test-session".into(),
        FeatureRoot::new(Some(feature_root_path)),
        notifier,
    );
    let svc = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, svc).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

async fn list_tools(addr: std::net::SocketAddr) -> Vec<String> {
    let url = format!("http://127.0.0.1:{}/mcp", addr.port());
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list"
    });
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .expect("tools/list POST")
        .json::<serde_json::Value>()
        .await
        .expect("tools/list JSON");
    resp["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|t| t["name"].as_str().map(String::from))
        .collect()
}

/// Full Phase 5 + 5b loop: gated tools absent → spec written →
/// `notifications/tools/list_changed` over SSE → gated tools
/// present on next `tools/list` call.
#[tokio::test]
async fn spec_creation_triggers_list_changed_notification_and_gates_tools_in() {
    let dir = tempfile::tempdir().unwrap();
    let registry = build_registry();
    let notifier = McpNotifier::new();
    let watcher_notifier = notifier.clone();

    // Critical: start the watcher BEFORE spawning the router so
    // any spec write between watcher start and the first tools/list
    // is observed. The watcher's on_change publishes to the same
    // notifier the router subscribes against.
    let _watcher = SpecWatcher::start(dir.path(), move || {
        watcher_notifier.notify_tools_list_changed();
    })
    .expect("spec_watcher start");

    let addr = spawn_router(registry, dir.path().to_path_buf(), notifier.clone()).await;

    // 1. No spec on disk — gated tools must be absent.
    let names = list_tools(addr).await;
    assert!(
        names.contains(&"callers".to_string()),
        "always-on missing: {names:?}"
    );
    assert!(
        names.contains(&"note".to_string()),
        "always-on missing: {names:?}"
    );
    assert!(
        !names.contains(&"spec".to_string()),
        "spec-gated leaked despite no spec on disk: {names:?}"
    );
    assert!(
        !names.contains(&"drift".to_string()),
        "spec-gated leaked despite no spec on disk: {names:?}"
    );

    // 2. Subscribe to the notifier directly. (We use the in-process
    // broadcast surface rather than parsing SSE bytes — the SSE
    // path is exercised in `notifier_fans_out_tools_list_changed_to_subscribers`
    // and the wire test in `sse_pushes_tools_list_changed_via_get_mcp` below.)
    let mut sub = notifier.subscribe();

    // 3. Write the spec.
    let foo = dir.path().join(".sovereign").join("features").join("foo");
    std::fs::create_dir_all(&foo).unwrap();
    // Notify's recursive mode adds inner watches lazily on Linux:
    // creating the dir + the file in rapid succession can race the
    // watch-registration so the file-create event lands on a not-
    // yet-watched subdir. Production callers create the dir long
    // before the file (git checkout, editor save) — settle briefly
    // to mirror that ordering.
    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::write(foo.join("spec.md"), b"# foo\n").unwrap();

    // 4. Wait for the watcher to fire. macOS FSEvents has ~100-500ms
    // latency; CI under load can be slower. Generous deadline.
    let recv = tokio::time::timeout(Duration::from_secs(5), sub.recv())
        .await
        .expect("notifier should publish tools/list_changed within 5s")
        .expect("payload arrives");
    assert_eq!(recv["method"], "notifications/tools/list_changed");
    assert_eq!(recv["jsonrpc"], "2.0");

    // 5. The watcher invalidates the cache before publishing. The
    // next `tools/list` call must see the new gated entries.
    let names_after = list_tools(addr).await;
    assert!(
        names_after.contains(&"spec".to_string()),
        "spec-gated tool should be present after spec.md write: {names_after:?}"
    );
    assert!(
        names_after.contains(&"drift".to_string()),
        "spec-gated tool should be present after spec.md write: {names_after:?}"
    );
}

/// `GET /mcp` SSE endpoint forwards broadcast notifications. Open
/// the stream, publish a `tools/list_changed` to the notifier, and
/// confirm the wire delivers a parseable JSON-RPC frame in a
/// `data:` event. This is the load-bearing client-facing contract.
///
/// We subscribe via the SSE wire (not the in-process broadcast
/// helper) to prove the bridge from `BroadcastStream` → axum SSE →
/// HTTP works end-to-end. A regression here is the difference
/// between "client refetches" and "client never knows."
#[tokio::test]
async fn sse_pushes_tools_list_changed_via_get_mcp() {
    let dir = tempfile::tempdir().unwrap();
    let registry = build_registry();
    let notifier = McpNotifier::new();
    let addr = spawn_router(registry, dir.path().to_path_buf(), notifier.clone()).await;

    // Open SSE.
    let url = format!("http://127.0.0.1:{}/mcp", addr.port());
    let resp = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .expect("GET /mcp");
    assert!(resp.status().is_success());

    // Spawn a task that consumes the byte stream and yields each
    // SSE `data:` payload it sees, until the test's tokio::select!
    // drops the receiver.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut buf = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let Ok(bytes) = chunk else { break };
            buf.extend_from_slice(&bytes);
            // Split on SSE event boundary "\n\n". Anything between
            // is a single event; we extract `data:` lines.
            while let Some(idx) = buf.windows(2).position(|w| w == b"\n\n").map(|p| p + 2) {
                let event_bytes = buf.drain(..idx).collect::<Vec<u8>>();
                let event = String::from_utf8_lossy(&event_bytes);
                for line in event.lines() {
                    if let Some(payload) = line.strip_prefix("data:") {
                        let payload = payload.trim().to_string();
                        let _ = tx.send(payload);
                    }
                }
            }
        }
    });

    // The endpoint event arrives first.
    let endpoint = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("endpoint event")
        .expect("event payload");
    assert_eq!(
        endpoint, "/mcp",
        "first SSE event should be the endpoint URL"
    );

    // Publish a notification on the notifier — the SSE subscriber
    // must see it.
    notifier.notify_tools_list_changed();

    let notif = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("notification arrives over SSE")
        .expect("event payload");
    let parsed: serde_json::Value = serde_json::from_str(&notif).expect("valid JSON");
    assert_eq!(parsed["method"], "notifications/tools/list_changed");
    assert_eq!(parsed["jsonrpc"], "2.0");
}

/// `initialize` advertises `tools.listChanged: true` so MCP clients
/// know they need to subscribe to the notification channel.
/// Without this the client is free to assume `tools/list` is
/// stable — which would defeat the gate.
#[tokio::test]
async fn initialize_advertises_tools_list_changed_capability() {
    let dir = tempfile::tempdir().unwrap();
    let registry = build_registry();
    let notifier = McpNotifier::new();
    let addr = spawn_router(registry, dir.path().to_path_buf(), notifier).await;

    let url = format!("http://127.0.0.1:{}/mcp", addr.port());
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize"
    });
    let resp: serde_json::Value = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .expect("initialize POST")
        .json()
        .await
        .expect("initialize JSON");

    assert_eq!(
        resp["result"]["capabilities"]["tools"]["listChanged"], true,
        "initialize must advertise tools.listChanged: true so clients \
         subscribe to the SSE channel and refetch on the gate flip"
    );
}
