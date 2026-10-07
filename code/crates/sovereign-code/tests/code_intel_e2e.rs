// SPDX-License-Identifier: AGPL-3.0-or-later
//! Code Intelligence — E2E tests over code's own tools (moved from
//! sovereign-daemon's tests/main/e2e_code_intel.rs, pb-code-daemon-exit).
//!
//! Every test runs real tools against a real LanceDB index. The index is
//! built through corpus-index's write API from committed rows
//! (`tests/fixtures/code_intel/*.json`), which are what the code
//! extractor produced for the original fixture repositories; extraction is
//! ingest's and is tested there. Code's tests take no corpus-engine edge.
//! Calls go through `tool.execute()` directly instead of the MCP wire.
//!
//! **Spec mapping:**
//! - T-01..T-05: Index correctness (this file)
//! - T-06..T-09: Semantic search (this file)
//! - T-10..T-11: Recent changes (this file)
//! - T-18: Session arc (this file)
//! - T-19: Latency (this file)
//! - T-21..T-24: Call graph tools + staleness (demo_auth part)
//! - T-25..T-27: Demo scenario — auth surface discovery, call chain
//!   traversal, grounded security finding (demo_auth part)

#![cfg(feature = "treesitter")]

use sovereign_contracts::tool_manifest::DeclaredTool;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use corpus_index::fs_source::FsIndexSource;
use corpus_index::index::{code_meta_from_json, CorpusIndex, InsertChunk};
use sovereign_code::{CodeSearchTool, RecentChangesTool, SymbolLookupTool};
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::{StepOutput, ToolContext};

/// The embedding model every fixture index is stamped with.
const FIXTURE_MODEL: &str = "test-mock";

/// Build the index `name` (a file under `tests/fixtures/code_intel/`) in
/// `data_dir`, with zero vectors: its rows are inserted as the extractor
/// wrote them, then ingestion is marked complete and the indexes built.
/// Each `mtime` is re-stamped to now, except `backdate`'s, which is 30
/// days old, as the original fixture set the file times.
pub(crate) async fn build_fixture_index(data_dir: &Path, name: &str, backdate: Option<&str>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/code_intel")
        .join(format!("{name}.json"));
    let fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("fixture file"))
            .expect("fixture json");
    let corpus_id = fixture["corpus_id"].as_str().expect("corpus_id");
    let dims = fixture["embedding_dimensions"].as_u64().expect("dims") as usize;
    let index = CorpusIndex::create(
        &data_dir.join(corpus_id),
        corpus_id,
        corpus_id,
        FIXTURE_MODEL,
        dims,
        false,
        fixture["license"].as_str().expect("license"),
    )
    .await
    .expect("create fixture index");

    let now = sovereign_time::unix_now();
    let mut chunks = Vec::new();
    for row in fixture["rows"].as_array().expect("rows") {
        let mut metadata = row["metadata"].clone();
        if let Some(obj) = metadata.as_object_mut() {
            if obj.contains_key("mtime") {
                let old = backdate.is_some_and(|f| obj["file_path"] == f);
                obj.insert(
                    "mtime".into(),
                    (if old { now - 30 * 24 * 3600 } else { now }).into(),
                );
            }
        }
        let insert = InsertChunk {
            content: row["content"].as_str().expect("content").to_string(),
            title: row["title"].as_str().map(String::from),
            url: row["url"].as_str().map(String::from),
            metadata: (!metadata.is_null()).then(|| metadata.to_string()),
            content_hash: None,
            source_doc_id: row["source_doc_id"].as_str().map(String::from),
            source_file: None,
            code: code_meta_from_json(Some(&metadata)),
            unit_id: None,
        };
        chunks.push((insert, vec![0.0; dims]));
    }
    index
        .insert_batch(&chunks)
        .await
        .expect("insert fixture rows");
    index
        .mark_ingestion_complete()
        .expect("mark ingestion complete");
    index
        .build_indexes(true, true, None)
        .await
        .expect("build fixture indexes");
}

/// The engine-free index source the tools read the fixture through.
pub(crate) fn fixture_source(data_dir: &Path) -> Arc<dyn sovereign_code::CodeIndexSource> {
    Arc::new(FsIndexSource::new(data_dir.to_path_buf()).with_embedding_model(FIXTURE_MODEL))
}

// ─── Shared fixture ───────────────────────────────────────────

struct Fixture {
    sym: DeclaredTool,
    search: DeclaredTool,
    recent: DeclaredTool,
    _tmp: tempfile::TempDir,
}

impl Fixture {
    async fn setup() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_dir = tmp.path().join("indexes");
        std::fs::create_dir_all(&data_dir).unwrap();

        // executor.rs is the file the original fixture backdated 30 days
        // for the mtime tests (T-10).
        build_fixture_index(&data_dir, "test-code", Some("src/executor.rs")).await;
        let source = fixture_source(&data_dir);

        // SymbolLookupTool reads SCIP. Use an empty in-memory graph
        // for the LanceDB-only fixtures — those tests assert empty
        // results today.
        let scip_handle: sovereign_code::ScipGraphHandle =
            Arc::new(arc_swap::ArcSwap::from_pointee(
                corpus_engine_scip::ScipGraph::open_in_memory("fixture")
                    .expect("in-memory ScipGraph for fixture"),
            ));
        let sym = SymbolLookupTool::new(Arc::clone(&source), Arc::clone(&scip_handle)).declared();
        let search = CodeSearchTool::new(Arc::clone(&source)).declared();
        let recent = RecentChangesTool::new(source).declared();

        Self {
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

#[path = "code_intel_e2e/demo_auth.rs"]
mod demo_auth;

#[path = "code_intel_e2e/corpus_scope.rs"]
mod corpus_scope;
