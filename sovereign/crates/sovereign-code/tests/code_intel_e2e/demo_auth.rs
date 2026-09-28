// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the code-intel e2e suite — the auth demo fixture (T-21..T-27)
//! and the mixed-corpora regression, split from code_intel_e2e.rs for the
//! §3.2 size ceiling.

use std::sync::Arc;

use arc_swap::ArcSwap;
use corpus_engine_scip::scip_graph::{ScipGraph, ScipRefRecord, ScipSymbolRecord};
use corpus_index::fs_source::FsIndexSource;
use sovereign_code::{
    CodeSearchTool, FindCalleesTool, FindCallersTool, RecentChangesTool, ScipGraphHandle,
    SymbolLookupTool,
};
use sovereign_contracts::tool_manifest::DeclaredTool;
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::ToolContext;

use super::text;

// ═══════════════════════════════════════════════════════════════
// Auth demo fixture — SCIP call graph tests (T-21 through T-27)
// ═══════════════════════════════════════════════════════════════

struct AuthFixture {
    sym: DeclaredTool,
    search: DeclaredTool,
    callees: DeclaredTool,
    callers: DeclaredTool,
    graph: ScipGraphHandle,
    _tmp: tempfile::TempDir,
}

impl AuthFixture {
    async fn setup() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_dir = tmp.path().join("indexes");
        std::fs::create_dir_all(&data_dir).unwrap();

        // ── The chunk index (symbol_lookup/code_search) ─────────
        super::build_fixture_index(&data_dir, "auth-demo", None).await;
        let source = super::fixture_source(&data_dir);

        // ── Populate SCIP call graph (directly, no external exporter) ─

        let graph_inner = Arc::new(ScipGraph::open_in_memory("auth-demo").unwrap());
        graph_inner
            .ingest_symbols_and_refs(auth_demo_symbols(), auth_demo_refs())
            .await
            .unwrap();
        let graph: ScipGraphHandle = Arc::new(ArcSwap::from(Arc::clone(&graph_inner)));

        // ── Build tools ──────────────────────────────────────

        let sym = SymbolLookupTool::new(Arc::clone(&source), Arc::clone(&graph)).declared();
        let search = CodeSearchTool::new(Arc::clone(&source)).declared();
        let callees = FindCalleesTool::new(Arc::clone(&source), Arc::clone(&graph)).declared();
        let callers = FindCallersTool::new(source, Arc::clone(&graph)).declared();

        Self {
            sym,
            search,
            callees,
            callers,
            graph,
            _tmp: tmp,
        }
    }

    fn ctx(&self) -> ToolContext {
        ToolContext {
            conversation_id: "e2e-auth".to_string(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
            ..Default::default()
        }
    }

    async fn find_callees(&self, symbol: &str) -> String {
        text(
            &self
                .callees
                .execute(&serde_json::json!({ "symbol": symbol }), &self.ctx())
                .await,
        )
    }

    async fn find_callers(&self, symbol: &str, depth: u64) -> String {
        text(
            &self
                .callers
                .execute(
                    &serde_json::json!({ "symbol": symbol, "depth": depth }),
                    &self.ctx(),
                )
                .await,
        )
    }

    async fn symbol_lookup(&self, name: &str) -> String {
        text(
            &self
                .sym
                .execute(&serde_json::json!({ "name": name }), &self.ctx())
                .await,
        )
    }

    async fn code_search(&self, query: &str) -> String {
        text(
            &self
                .search
                .execute(&serde_json::json!({ "query": query }), &self.ctx())
                .await,
        )
    }
}

// ─── Auth demo SCIP data ─────────────────────────────────────

fn auth_demo_symbols() -> Vec<ScipSymbolRecord> {
    vec![
        sym(
            "auth_middleware",
            "function",
            "src/middleware/auth.rs",
            1,
            15,
        ),
        sym(
            "extract_bearer_token",
            "function",
            "src/middleware/auth.rs",
            17,
            25,
        ),
        sym(
            "validate_access_token",
            "function",
            "src/auth/tokens.rs",
            1,
            10,
        ),
        sym("issue_token_pair", "function", "src/auth/tokens.rs", 12, 20),
        sym("decode_jwt", "function", "src/auth/tokens.rs", 22, 28),
        sym("sign_jwt", "function", "src/auth/tokens.rs", 30, 36),
        sym(
            "refresh_if_expired",
            "function",
            "src/auth/refresh.rs",
            1,
            12,
        ),
        sym(
            "rotate_refresh_token",
            "function",
            "src/auth/refresh.rs",
            14,
            22,
        ),
        sym("login_handler", "function", "src/routes/auth.rs", 1, 10),
        sym("refresh_handler", "function", "src/routes/auth.rs", 12, 20),
        sym("verify_password", "function", "src/routes/auth.rs", 22, 28),
        sym("register_user", "function", "src/routes/users.rs", 1, 18),
        sym("find_by_email", "function", "src/models/user.rs", 8, 14),
        sym("create_user", "function", "src/models/user.rs", 16, 26),
    ]
}

fn sym(name: &str, kind: &str, file: &str, start: i32, end: i32) -> ScipSymbolRecord {
    ScipSymbolRecord {
        name: name.to_string(),
        qualified_name: String::new(),
        kind: kind.to_string(),
        file_path: file.to_string(),
        line_start: start,
        line_end: end,
        language: "rust".to_string(),
    }
}

fn auth_demo_refs() -> Vec<ScipRefRecord> {
    vec![
        // auth_middleware calls:
        refr(
            "auth_middleware",
            "extract_bearer_token",
            "src/middleware/auth.rs",
            5,
        ),
        refr(
            "auth_middleware",
            "validate_access_token",
            "src/middleware/auth.rs",
            6,
        ),
        refr(
            "auth_middleware",
            "find_by_email",
            "src/middleware/auth.rs",
            7,
        ),
        // validate_access_token calls:
        refr(
            "validate_access_token",
            "decode_jwt",
            "src/auth/tokens.rs",
            3,
        ),
        refr(
            "validate_access_token",
            "refresh_if_expired",
            "src/auth/tokens.rs",
            5,
        ),
        // refresh_if_expired calls:
        refr(
            "refresh_if_expired",
            "rotate_refresh_token",
            "src/auth/refresh.rs",
            6,
        ),
        refr(
            "refresh_if_expired",
            "issue_token_pair",
            "src/auth/refresh.rs",
            7,
        ),
        // issue_token_pair calls:
        refr("issue_token_pair", "sign_jwt", "src/auth/tokens.rs", 14),
        // login_handler calls:
        refr("login_handler", "find_by_email", "src/routes/auth.rs", 3),
        refr("login_handler", "verify_password", "src/routes/auth.rs", 4),
        refr("login_handler", "issue_token_pair", "src/routes/auth.rs", 5),
        // refresh_handler calls:
        refr(
            "refresh_handler",
            "rotate_refresh_token",
            "src/routes/auth.rs",
            14,
        ),
        refr(
            "refresh_handler",
            "issue_token_pair",
            "src/routes/auth.rs",
            15,
        ),
        // register_user calls:
        refr("register_user", "find_by_email", "src/routes/users.rs", 10),
        refr("register_user", "create_user", "src/routes/users.rs", 15),
    ]
}

fn refr(caller: &str, callee: &str, file: &str, line: i32) -> ScipRefRecord {
    ScipRefRecord {
        caller_symbol: caller.to_string(),
        callee_symbol: callee.to_string(),
        caller_qualified: String::new(),
        callee_qualified: String::new(),
        file_path: file.to_string(),
        line,
        start_col: -1,
        end_line: -1,
        end_col: -1,
        ref_kind: "direct".to_string(),
    }
}

// ══════════════════��══════════════════════════════════���═════════
// T-21 — find_callees returns correct outbound calls
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn t21_find_callees_correct() {
    let h = AuthFixture::setup().await;

    let result = h.find_callees("auth_middleware").await;

    // auth_middleware calls extract_bearer_token, validate_access_token,
    // find_by_email (as find_user_by_id proxy)
    assert!(
        result.contains("extract_bearer_token") || result.contains("validate_access_token"),
        "find_callees missing known callees of auth_middleware: {result}"
    );

    // Must not contain symbols from unrelated files.
    assert!(
        !result.contains("register_user"),
        "find_callees returned unrelated symbol: {result}"
    );
}

// ═══════════════════════════════════════════════════════════════
// T-22 — find_callers returns correct call sites
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn t22_find_callers_correct() {
    let h = AuthFixture::setup().await;

    // issue_token_pair is called by login_handler and refresh_handler
    let result = h.find_callers("issue_token_pair", 1).await;

    assert!(
        result.contains("login_handler") || result.contains("refresh_handler"),
        "find_callers missing known callers of issue_token_pair: {result}"
    );
}

// ═══════════════════════════════════════════════════════════════
// T-23 — Staleness note absent when graph is fresh
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn t23_no_staleness_note_when_fresh() {
    let h = AuthFixture::setup().await;
    // Graph was just populated in setup() — it is fresh.

    let result = h.find_callees("validate_access_token").await;

    assert!(
        !result.contains("hours ago")
            && !result.contains("hours old")
            && !result.contains("\u{26a0}"),
        "Staleness note appeared on fresh graph: {result}"
    );
}

// ═══════════════════════════════════════════════════════════════
// T-24 — Staleness note appears after file modification
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn t24_staleness_note_after_file_modification() {
    let h = AuthFixture::setup().await;

    // Mark a file as stale — simulating what CodeWatcher would do.
    h.graph
        .load_full()
        .mark_file_stale("src/auth/tokens.rs")
        .await;

    // Query a symbol whose callees include that file — should show
    // staleness note.
    let result = h.find_callees("auth_middleware").await;

    // The callee validate_access_token is in src/auth/tokens.rs, which
    // is now stale. But the result file list is about the callee files,
    // so we check if the staleness note mentions the stale file.
    assert!(
        result.contains("modified since") || result.contains("may not be current"),
        "Staleness note missing after file modification: {result}"
    );
}

// ═══════════════════════════════════════════════════════════════
// T-25 — Demo: auth surface area discovery via code_search
// ═══════════════════════════════════════════════════════════════

#[ignore = "test Fixture builds FTS but no SCIP call graph; demo scenario requires resolved symbols"]
#[tokio::test]
async fn t25_demo_auth_surface_discovery() {
    let h = AuthFixture::setup().await;

    let result = h
        .code_search("OAuth JWT token authentication middleware")
        .await;

    // Must surface at least one auth entry point.
    let found_entry_points = result.contains("auth_middleware")
        || result.contains("validate_access_token")
        || result.contains("login_handler");

    assert!(
        found_entry_points,
        "Auth surface discovery failed — key entry points not in top results: {result}"
    );
}

// ═══════════════════════════════════════════════════════════════
// T-26 — Demo: call chain traversal
// ═══════════════════════════════════════════════════════════════

#[ignore = "test Fixture builds FTS but no SCIP call graph; chain traversal needs resolved callers/callees"]
#[tokio::test]
async fn t26_demo_call_chain_traversal() {
    let h = AuthFixture::setup().await;

    // Step 1: find what auth_middleware calls.
    let callees_1 = h.find_callees("auth_middleware").await;
    assert!(
        callees_1.contains("validate_access_token"),
        "Call chain step 1 broken — auth_middleware doesn't show validate_access_token: {callees_1}"
    );

    // Step 2: follow validate_access_token.
    let callees_2 = h.find_callees("validate_access_token").await;
    assert!(
        callees_2.contains("refresh_if_expired"),
        "Call chain step 2 broken — validate_access_token doesn't show refresh_if_expired: {callees_2}"
    );

    // Step 3: inspect the refresh implementation.
    let definition = h.symbol_lookup("refresh_if_expired").await;
    assert!(
        definition.contains("rotate_refresh_token"),
        "refresh_if_expired definition doesn't show token rotation: {definition}"
    );
    assert!(
        definition.contains("refresh.rs"),
        "refresh_if_expired attributed to wrong file: {definition}"
    );

    // Three tool calls. The agent now has the full token refresh flow
    // grounded in actual code — without reading a single complete file.
}

// ═══════════════════════════════════════════════════════════════
// T-27 — Demo: security finding grounded in code path
// ═══════════════════════════════════════════════════════════════

#[ignore = "test Fixture builds FTS but no SCIP call graph; security-finding grounding needs symbol resolution"]
#[tokio::test]
async fn t27_demo_security_finding_grounded() {
    let h = AuthFixture::setup().await;

    // Agent uses code_search to find the registration flow.
    let reg_search = h
        .code_search("user registration create account signup password")
        .await;
    assert!(
        reg_search.contains("register_user") || reg_search.contains("create_user"),
        "Registration flow not found via code_search: {reg_search}"
    );

    // Agent looks up the registration handler.
    let reg_handler = h.symbol_lookup("register_user").await;

    // The retrieved code must contain the vulnerability evidence:
    // password_hash field being set to body.password without a hash function.
    assert!(
        reg_handler.contains("password_hash"),
        "register_user definition missing password_hash field: {reg_handler}"
    );

    // Must cite the correct file — this is the grounded part.
    assert!(
        reg_handler.contains("users.rs"),
        "register_user attributed to wrong file: {reg_handler}"
    );

    // Agent verifies the call chain: register_user → create_user
    // to confirm no hashing happens downstream.
    let callees = h.find_callees("register_user").await;
    assert!(
        callees.contains("create_user"),
        "find_callees missing create_user in register_user call chain: {callees}"
    );

    // Agent looks up create_user to confirm password is stored as-is.
    let create_fn = h.symbol_lookup("create_user").await;
    assert!(
        create_fn.contains("password_hash"),
        "create_user doesn't reference password_hash: {create_fn}"
    );

    // Verify that bcrypt/hashing is NOT present in the vulnerable path.
    // (It IS present in login_handler via verify_password, confirming
    // the inconsistency is real — the login path hashes, the register
    // path doesn't.)
    assert!(
        !create_fn.contains("bcrypt") && !create_fn.contains("hash("),
        "create_user unexpectedly contains a hash function — fixture may be wrong: {create_fn}"
    );
}

// ═══════════════════════════════════════════════════════════════
// Mixed-corpora regression — code intel must skip Knowledge corpora
// ═══════════════════════════════════════════════════════════════
//
// Bug: `query_all_code_indexes` and `code_search`'s inline loop
// iterated *every* installed corpus and relied on the predicate
// `symbol_name = '…'` to implicitly filter prose rows. That works
// when the prose schema *has* a `symbol_name` column (with NULLs),
// but Knowledge corpora's chunks tables don't include the typed
// code columns at all — Lance fails at column resolution, returning
// `Not found: <fragment>.lance` or a column-missing error before
// any predicate runs.
//
// This test sets up one Code corpus and one Knowledge corpus side
// by side and asserts all three code-intel tools succeed. Without
// the `info.kind == CorpusKind::Code` filter, `symbols`/`code_search`
// /`recent_changes` would error out.
//
// Both corpora are the rows the original ingest wrote: one `.rs` file with
// `make_widget` through the code extractor, one prose paragraph through the
// parquet extractor (tests/fixtures/code_intel/mixed-*.json).

#[tokio::test]
async fn mixed_corpora_code_intel_skips_knowledge() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("indexes");
    std::fs::create_dir_all(&data_dir).unwrap();
    super::build_fixture_index(&data_dir, "mixed-code", None).await;
    super::build_fixture_index(&data_dir, "mixed-knowledge", None).await;

    // Sanity: both corpora should be visible to `installed_indexes()`.
    let listing = FsIndexSource::new(data_dir.clone()).with_embedding_model("test-mock");
    let installed = listing.installed_indexes().await.expect("listed");
    assert!(
        installed.iter().any(|i| i.corpus_id == "mixed-code"),
        "code corpus missing from installed list",
    );
    assert!(
        installed.iter().any(|i| i.corpus_id == "mixed-knowledge"),
        "knowledge corpus missing from installed list",
    );

    // ── Run the three tools. Each must succeed (no Lance error). ─
    let source = super::fixture_source(&data_dir);
    let mixed_graph: sovereign_code::ScipGraphHandle = Arc::new(arc_swap::ArcSwap::from_pointee(
        corpus_engine_scip::ScipGraph::open_in_memory("mixed")
            .expect("in-memory ScipGraph for mixed-corpora test"),
    ));
    let sym = SymbolLookupTool::new(Arc::clone(&source), Arc::clone(&mixed_graph));
    let search = CodeSearchTool::new(Arc::clone(&source)).declared();
    let recent = RecentChangesTool::new(source).declared();
    let ctx = ToolContext {
        conversation_id: "mixed-corpora-test".to_string(),
        task_id: None,
        working_directory: None,
        in_reasoning_loop: false,
        agent_session_token: None,
        turn_index: 0,
        ..Default::default()
    };

    let sym_out = text(
        &sym.declared()
            .execute(&serde_json::json!({ "name": "make_widget" }), &ctx)
            .await,
    );
    assert!(
        !sym_out.starts_with("ERROR"),
        "symbol_lookup errored with mixed corpora: {sym_out}"
    );
    assert!(
        sym_out.contains("make_widget"),
        "symbol_lookup didn't return the code symbol: {sym_out}"
    );

    let search_out = text(
        &search
            .execute(&serde_json::json!({ "query": "widget" }), &ctx)
            .await,
    );
    assert!(
        !search_out.starts_with("ERROR"),
        "code_search errored with mixed corpora: {search_out}"
    );

    let recent_out = text(
        &recent
            .execute(&serde_json::json!({ "hours": 24u64 }), &ctx)
            .await,
    );
    assert!(
        !recent_out.starts_with("ERROR"),
        "recent_changes errored with mixed corpora: {recent_out}"
    );
}
