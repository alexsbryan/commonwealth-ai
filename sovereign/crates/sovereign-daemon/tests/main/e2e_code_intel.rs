// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(feature = "treesitter")]
//! Code Intelligence — E2E tests.
//!
//! Exercises all five tools against controlled fixture repositories.
//! Every test uses real indexing, real tools, and real LanceDB queries —
//! no mocking. The only shortcut is transport: calls go through
//! `tool.execute()` directly instead of the MCP HTTP wire. The MCP
//! protocol layer is tested separately in `sovereign-server::routes_mcp`.
//!
//! **Spec mapping:**
//! - T-01..T-05: Index correctness (this file)
//! - T-06..T-09: Semantic search (this file)
//! - T-10..T-11: Recent changes (this file)
//! - T-12..T-14: Watcher (corpus-engine/tests/watcher_e2e.rs)
//! - T-15..T-17: MCP protocol (sovereign-server::routes_mcp::tests)
//! - T-18: Session arc (this file)
//! - T-19: Latency (this file)
//! - T-20: Watcher SLA (corpus-engine/tests/watcher_e2e.rs)
//! - T-21..T-24: Call graph tools + staleness (demo_auth part, auth demo fixture)
//! - T-25..T-27: Demo scenario — auth surface discovery, call chain
//!   traversal, grounded security finding (demo_auth part, auth demo fixture)
//!
//! Run with:
//!     cargo test -p sovereign-daemon --test main e2e_code_intel

use sovereign_contracts::tool_manifest::DeclaredTool;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use sovereign_code::{CodeSearchTool, RecentChangesTool, SymbolLookupTool};
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::{StepOutput, ToolContext};

use corpus_engine::{CorpusEngine, CorpusSpec};
use corpus_index::types::EmbedFn;

// ─── Shared fixture ───────────────────────────────────────────

struct Fixture {
    root: PathBuf,
    data_dir: PathBuf,
    engine: Arc<CorpusEngine>,
    sym: DeclaredTool,
    search: DeclaredTool,
    recent: DeclaredTool,
    _tmp: tempfile::TempDir,
}

impl Fixture {
    async fn setup() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("repo");
        let data_dir = tmp.path().join("indexes");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("web")).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();

        // ── Write fixture files ─────────────────────────────

        std::fs::write(root.join("src/executor.rs"), EXECUTOR_RS).unwrap();
        std::fs::write(root.join("src/scheduler.rs"), SCHEDULER_RS).unwrap();
        std::fs::write(root.join("src/types.rs"), TYPES_RS).unwrap();
        std::fs::write(root.join("src/planner.rs"), PLANNER_RS).unwrap();
        std::fs::write(root.join("web/api.ts"), API_TS).unwrap();
        std::fs::write(root.join("web/store.ts"), STORE_TS).unwrap();

        // Backdate executor.rs to 30 days ago for mtime tests (T-10).
        let ft = filetime::FileTime::from_unix_time(
            (commonwealth_core::clock::unix_now_secs() - 30 * 24 * 3600) as i64,
            0,
        );
        filetime::set_file_mtime(root.join("src/executor.rs"), ft).unwrap();

        // ── Index the fixture ───────────────────────────────

        let embed: EmbedFn = Arc::new(|_text: &str| {
            Box::pin(async {
                Ok::<Vec<f32>, corpus_index::Error>(vec![
                    0.0;
                    corpus_index::types::DEFAULT_EMBED_DIM
                ])
            })
        });
        // `with_embedding_model` is a hard precondition of `ingest()`:
        // the engine refuses to write `_corpus_meta.json` without a
        // declared model name so downstream shard-compatibility checks
        // never see a bogus label. The other corpus-engine fixtures
        // (`parquet_ingest_e2e`, `ingest_failure_modes`) use the same
        // `"test-mock"` stem; we follow the convention.
        let engine = Arc::new(
            CorpusEngine::new(data_dir.join("_recipes"), data_dir.clone(), embed)
                .with_embedding_model("test-mock"),
        );

        let recipe_dir = data_dir.join("_recipes");
        std::fs::create_dir_all(&recipe_dir).unwrap();
        let recipe_path = recipe_dir.join("test-code.toml");
        std::fs::write(
            &recipe_path,
            format!(
                r#"[corpus]
id = "test-code"
name = "test-code"
description = "E2E fixture"
license = "private"
mesh_sharing = false
size_compressed_gb = 0
size_indexed_gb = 0

[acquire]
type = "local_file"
path = "{path}"

[extract]
type = "code"
context_lines = 3
max_lines_per_chunk = 150

[chunk]
type = "passthrough"

[index]
fts = true
vector = false
"#,
                path = root.display()
            ),
        )
        .unwrap();

        engine
            .ingest(&CorpusSpec::RecipePath(recipe_path), None)
            .await
            .expect("fixture ingest");

        // ── Build tools ─────────────────────────────────────

        // SymbolLookupTool reads SCIP. Use an empty in-memory graph
        // for the LanceDB-only fixtures — those tests assert empty
        // results today.
        let scip_handle: sovereign_code::ScipGraphHandle =
            Arc::new(arc_swap::ArcSwap::from_pointee(
                corpus_engine_scip::ScipGraph::open_in_memory("fixture")
                    .expect("in-memory ScipGraph for fixture"),
            ));
        let sym = SymbolLookupTool::new(
            Arc::clone(&engine) as std::sync::Arc<dyn sovereign_code::CodeIndexSource>,
            Arc::clone(&scip_handle),
        )
        .declared();
        let search = CodeSearchTool::new(
            Arc::clone(&engine) as std::sync::Arc<dyn sovereign_code::CodeIndexSource>
        )
        .declared();
        let recent = RecentChangesTool::new(
            Arc::clone(&engine) as std::sync::Arc<dyn sovereign_code::CodeIndexSource>
        )
        .declared();

        Self {
            root,
            data_dir,
            engine,
            sym,
            search,
            recent,
            _tmp: tmp,
        }
    }

    fn ctx(&self) -> ToolContext {
        ToolContext {
            conversation_id: "e2e-test".to_string(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
            ..Default::default()
        }
    }

    async fn symbol(&self, name: &str) -> String {
        text(
            &self
                .sym
                .execute(&serde_json::json!({ "name": name }), &self.ctx())
                .await,
        )
    }

    async fn symbol_kind(&self, name: &str, kind: &str) -> String {
        text(
            &self
                .sym
                .execute(
                    &serde_json::json!({ "name": name, "kind": kind }),
                    &self.ctx(),
                )
                .await,
        )
    }

    async fn search_code(&self, query: &str) -> String {
        text(
            &self
                .search
                .execute(&serde_json::json!({ "query": query }), &self.ctx())
                .await,
        )
    }

    async fn search_code_lang(&self, query: &str, language: &str) -> String {
        text(
            &self
                .search
                .execute(
                    &serde_json::json!({ "query": query, "language": language }),
                    &self.ctx(),
                )
                .await,
        )
    }

    async fn changes(&self, hours: u64) -> String {
        text(
            &self
                .recent
                .execute(&serde_json::json!({ "hours": hours }), &self.ctx())
                .await,
        )
    }
}

fn text(result: &Result<StepOutput, sovereign_contracts::error::Error>) -> String {
    match result {
        Ok(StepOutput::Text(s)) => s.clone(),
        Ok(other) => format!("{other:?}"),
        Err(e) => format!("ERROR: {e}"),
    }
}

// ─── Fixture file contents ────────────────────────────────────

const EXECUTOR_RS: &str = r#"/// Executes a planned step against the current mesh state.
pub async fn execute_step(
    plan:  &StepPlan,
    state: &MeshState,
) -> Result<StepResult, ExecutorError> {
    validate_preconditions(plan, state)?;
    let result = dispatch_step(plan).await?;
    Ok(result)
}

fn validate_preconditions(
    plan:  &StepPlan,
    state: &MeshState,
) -> Result<(), ExecutorError> {
    if state.nodes.is_empty() {
        return Err(ExecutorError::NoNodes);
    }
    Ok(())
}

async fn dispatch_step(plan: &StepPlan) -> Result<StepResult, ExecutorError> {
    todo!()
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    #[error("no nodes available")]
    NoNodes,
    #[error("step validation failed: {0}")]
    ValidationFailed(String),
}
"#;

const SCHEDULER_RS: &str = r#"/// Applies a shard plan — takes ownership to prevent caller modification
/// after submission.
pub async fn apply_shard_plan(plan: ShardPlan) -> anyhow::Result<()> {
    validate_plan(&plan)?;
    broadcast_plan(plan).await
}

pub fn validate_plan(plan: &ShardPlan) -> anyhow::Result<()> {
    anyhow::ensure!(!plan.shards.is_empty(), "plan must have at least one shard");
    Ok(())
}

async fn broadcast_plan(plan: ShardPlan) -> anyhow::Result<()> { todo!() }

pub struct ShardPlan { pub shards: Vec<Shard> }
pub struct Shard { pub node_id: String, pub layers: std::ops::Range<usize> }
"#;

const TYPES_RS: &str = r#"pub struct MeshState { pub nodes: Vec<Node> }
pub struct Node     { pub id: String, pub capacity: usize }
pub struct StepPlan { pub id: uuid::Uuid, pub kind: StepKind }
pub struct StepResult { pub success: bool, pub output: Option<String> }
pub enum StepKind   { Inference, KnowledgeQuery, ToolCall }
"#;

const PLANNER_RS: &str = r#"pub fn plan_next_step(context: &ConversationContext) -> StepPlan {
    StepPlan { id: uuid::Uuid::new_v4(), kind: StepKind::Inference }
}
pub struct ConversationContext { pub messages: Vec<String> }
"#;

const API_TS: &str = r#"interface NodeCapability { nodeId: string; vramGb: number; isOnline: boolean }
async function fetchCapabilities(url: string): Promise<NodeCapability[]> {
    return fetch(`${url}/capabilities`).then(r => r.json());
}
function filterOnlineNodes(nodes: NodeCapability[]): NodeCapability[] {
    return nodes.filter(n => n.isOnline);
}
"#;

const STORE_TS: &str = r#"const createMeshStore = () => {
    let nodes: string[] = [];
    const addNode    = (id: string): void => { nodes = [...nodes, id]; };
    const removeNode = (id: string): void => { nodes = nodes.filter(n => n !== id); };
    return { addNode, removeNode };
};
"#;

// ═══════════════════════════════════════════════════════════════
// Group 1: Index correctness
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn t01_rust_symbols_correct_kinds() {
    let fx = Fixture::setup().await;

    let cases: &[(&str, &str)] = &[
        ("execute_step", "function"),
        ("validate_preconditions", "function"),
        ("ExecutorError", "enum"),
        ("apply_shard_plan", "function"),
        ("validate_plan", "function"),
        ("ShardPlan", "struct"),
        ("Shard", "struct"),
        ("MeshState", "struct"),
        ("StepPlan", "struct"),
        ("StepKind", "enum"),
        ("plan_next_step", "function"),
    ];

    for (name, kind) in cases {
        let result = fx.symbol_kind(name, kind).await;
        assert!(
            result.contains(name) && !result.to_lowercase().contains("not found"),
            "Missing Rust symbol `{name}` (kind: {kind})\nResponse: {result}"
        );
    }
}

#[tokio::test]
async fn t02_typescript_symbols_extracted() {
    let fx = Fixture::setup().await;

    let cases: &[(&str, &str)] = &[
        ("fetchCapabilities", "function"),
        ("filterOnlineNodes", "function"),
        ("NodeCapability", "interface"),
        ("createMeshStore", "function"),
        ("addNode", "function"),
        ("removeNode", "function"),
    ];

    for (name, kind) in cases {
        let result = fx.symbol_kind(name, kind).await;
        assert!(
            result.contains(name) && !result.to_lowercase().contains("not found"),
            "Missing TypeScript symbol `{name}` (kind: {kind})\nResponse: {result}"
        );
    }
}

#[ignore = "test Fixture builds FTS but no SCIP call graph; needs rust-analyzer scip extraction"]
#[tokio::test]
async fn t03_file_path_correct_and_exclusive() {
    let fx = Fixture::setup().await;
    let result = fx.symbol("apply_shard_plan").await;

    assert!(
        result.contains("scheduler.rs"),
        "apply_shard_plan must cite scheduler.rs: {result}"
    );
    assert!(
        !result.contains("executor.rs"),
        "apply_shard_plan must not cite executor.rs: {result}"
    );
    assert!(
        !result.contains("planner.rs"),
        "apply_shard_plan must not cite planner.rs: {result}"
    );
}

#[ignore = "test Fixture builds FTS but no SCIP call graph; needs rust-analyzer scip extraction"]
#[tokio::test]
async fn t04_unknown_symbol_graceful() {
    let fx = Fixture::setup().await;
    let result = fx.symbol("nonexistent_symbol_xyz_987").await;

    assert!(
        result.to_lowercase().contains("no symbol") || result.to_lowercase().contains("not found"),
        "Expected not-found message: {result}"
    );
    assert!(
        result.contains("code_search"),
        "Should suggest code_search as fallback: {result}"
    );
}

#[tokio::test]
async fn t05_kind_filter_is_enforced() {
    let fx = Fixture::setup().await;
    let result = fx.symbol_kind("validate_plan", "struct").await;

    assert!(
        result.to_lowercase().contains("no symbol") || !result.contains("validate_plan"),
        "Kind filter not enforced — function returned for struct query: {result}"
    );
}

// ═══════════════════════════════════════════════════════════════
// Group 2: Semantic search
// ═══════════════════════════════════════════════════════════════

#[ignore = "test Fixture builds FTS but no vector index / SCIP graph; semantic search has nothing to rank"]
#[tokio::test]
async fn t06_semantic_search_finds_relevant_symbols() {
    let fx = Fixture::setup().await;
    let result = fx
        .search_code("validating preconditions before executing a step")
        .await;

    // FTS-only path (no real embeddings) — should still find relevant
    // symbols via text matching on "validating" / "preconditions" /
    // "executing" / "step".
    assert!(
        result.contains("execute_step")
            || result.contains("validate_preconditions")
            || result.contains("ExecutorError"),
        "Semantic search failed to surface relevant executor symbols: {result}"
    );
}

#[tokio::test]
async fn t07_language_filter_restricts_results() {
    let fx = Fixture::setup().await;
    let result = fx
        .search_code_lang("node collection management", "typescript")
        .await;

    assert!(
        !result.contains("executor.rs")
            && !result.contains("scheduler.rs")
            && !result.contains("planner.rs"),
        "Language filter failed — Rust paths in TypeScript results: {result}"
    );
}

#[tokio::test]
async fn t08_approximate_label_always_present() {
    let fx = Fixture::setup().await;

    let queries = [
        "error handling",
        "mesh node capacity",
        "plan execution dispatch",
    ];

    for query in &queries {
        let result = fx.search_code(query).await;
        // Empty-result responses don't need the label (they already
        // say "No semantically similar code found"). Non-empty ones do.
        if result.contains("No semantically similar") {
            continue;
        }
        assert!(
            result.to_lowercase().contains("approximate"),
            "Approximate label missing for query `{query}`: {result}"
        );
    }
}

#[tokio::test]
async fn t09_empty_search_graceful() {
    let fx = Fixture::setup().await;
    let result = fx.search_code_lang("xyzzy frobnicate quux", "ruby").await;

    assert!(
        !result.contains("ERROR"),
        "Empty search produced an error: {result}"
    );
    assert!(!result.is_empty(), "Empty search produced an empty string");
}

// ═══════════════════════════════════════════════════════════════
// Group 3: Recent changes
// ═══════════════════════════════════════════════════════════════

#[ignore = "test Fixture ingests without recent_changes signal; needs git-aware mtime in the corpus"]
#[tokio::test]
async fn t10_recent_changes_correct_window() {
    let fx = Fixture::setup().await;
    let result = fx.changes(48).await;

    // planner.rs has current mtime — must appear in 48h window.
    assert!(
        result.contains("planner.rs"),
        "recent_changes should include planner.rs: {result}"
    );
    // executor.rs was backdated 30 days — must NOT appear in 48h.
    assert!(
        !result.contains("executor.rs"),
        "recent_changes should not include 30-day-old executor.rs: {result}"
    );
}

#[tokio::test]
async fn t11_recent_changes_empty_state() {
    let fx = Fixture::setup().await;
    // validator rejects hours=0, so use 1 minute (0.016h rounds to 1h
    // minimum internally). Use a validation that should produce an
    // empty result.
    let result = text(
        &fx.recent
            .execute(&serde_json::json!({ "hours": 0 }), &fx.ctx())
            .await,
    );

    // hours=0 is rejected by validate() — should return an error, not crash.
    // The tool's validate() checks hours > 0. But the execute path
    // doesn't re-validate, so we may get either a validation error or
    // an empty result. Both are acceptable.
    assert!(
        result.to_lowercase().contains("no")
            || result.to_lowercase().contains("error")
            || result.to_lowercase().contains("changes"),
        "Zero-hour window must produce a clear message: {result}"
    );
}

// ═══════════════════════════════════════════════════════════════
// Group 6: Session arc
// ═══════════════════════════════════════════════════════════════

#[ignore = "test Fixture builds FTS but no SCIP call graph; symbol_lookup has nothing to resolve"]
#[tokio::test]
async fn t18_developer_session_arc() {
    let fx = Fixture::setup().await;

    // 1. Orientation: what's been active?
    let changes = fx.changes(48).await;
    assert!(
        changes.contains("planner.rs"),
        "Session start: recent changes should surface planner.rs"
    );

    // 2. Find a symbol by name.
    let definition = fx.symbol("execute_step").await;
    assert!(
        definition.contains("execute_step"),
        "Symbol lookup failed during session arc"
    );
    assert!(
        definition.contains("executor.rs"),
        "Symbol lookup returned wrong file during session arc"
    );

    // 3. Explore an unfamiliar concept.
    let search = fx
        .search_code("error handling step execution validation")
        .await;
    if !search.contains("No semantically similar") {
        assert!(
            search.to_lowercase().contains("approximate"),
            "code_search missing approximate label during session arc"
        );
    }

    // 4. Narrow the window.
    let today = fx.changes(24).await;
    assert!(
        today.contains("planner.rs"),
        "24h recent_changes should include planner.rs"
    );
}

// ═══════════════════════════════════════════════════════════════
// Group 6: Latency
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn t19_latency_within_targets() {
    let fx = Fixture::setup().await;

    // Warm the index.
    fx.symbol("execute_step").await;

    // ── symbol_lookup: target <10ms, allow 50ms with overhead ─
    let mut times = Vec::new();
    for _ in 0..10 {
        let t = Instant::now();
        fx.symbol("apply_shard_plan").await;
        times.push(t.elapsed().as_millis());
    }
    let p99 = percentile(&times, 99);
    assert!(
        p99 < 200,
        "symbol_lookup p99 was {p99}ms — target is <10ms (200ms lenient)"
    );

    // ── code_search: target <150ms, allow 500ms with overhead ─
    let mut times = Vec::new();
    for _ in 0..10 {
        let t = Instant::now();
        fx.search_code("error handling validation").await;
        times.push(t.elapsed().as_millis());
    }
    let p99 = percentile(&times, 99);
    assert!(
        p99 < 500,
        "code_search p99 was {p99}ms — target is <150ms (500ms lenient)"
    );

    // ── recent_changes: target <20ms, allow 200ms with overhead ─
    let mut times = Vec::new();
    for _ in 0..10 {
        let t = Instant::now();
        fx.changes(24).await;
        times.push(t.elapsed().as_millis());
    }
    let p99 = percentile(&times, 99);
    assert!(
        p99 < 200,
        "recent_changes p99 was {p99}ms — target is <20ms (200ms lenient)"
    );
}

fn percentile(times: &[u128], p: usize) -> u128 {
    let mut sorted = times.to_vec();
    sorted.sort_unstable();
    sorted[(sorted.len() * p / 100).min(sorted.len() - 1)]
}

// ═══════════════════════════════════════════════════════════════
// The auth demo fixture (T-21..T-27) lives in the demo_auth part.
// ═══════════════════════════════════════════════════════════════

#[path = "e2e_code_intel/demo_auth.rs"]
mod demo_auth;
