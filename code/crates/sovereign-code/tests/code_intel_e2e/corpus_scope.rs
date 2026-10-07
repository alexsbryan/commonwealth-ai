// SPDX-License-Identifier: AGPL-3.0-or-later
//! The code-intel-repo-scope order, steps 2 and 3: a call whose
//! `ToolContext::corpus_scope` names one corpus answers from that corpus
//! alone, through the real merged-graph loader and the real chunk indexes.
//!
//! Two corpora, `scope-a` and `scope-b`, each with a chunk index and a
//! `scip_graph.db` defining `Foo`, called by `use_foo_a` / `use_foo_b`.
//! Scoped to `scope-a`, `symbols`, `callers`, `blast`, `code_search` and
//! `recent_changes` return only `scope-a`'s rows; unscoped, both.

use std::path::Path;

use corpus_engine_scip::scip_graph::{ScipGraph, ScipRefRecord, ScipSymbolRecord};
use sovereign_code::{
    BlastRadiusTool, CodeSearchTool, FindCallersTool, LazyScipGraph, RecentChangesTool,
    SymbolLookupTool,
};
use sovereign_contracts::tool_manifest::DeclaredTool;
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::ToolContext;

use super::text;

/// `corpus`'s graph on disk: `Foo` in its own file, called by `use_foo_<c>`.
async fn graph(data_dir: &Path, corpus: &str, c: &str) {
    let g = ScipGraph::open(&data_dir.join(corpus).join("scip_graph.db"), corpus).unwrap();
    let file = format!("src/{c}_foo.rs");
    let def = |name: &str, kind: &str, line: i32| ScipSymbolRecord {
        name: name.into(),
        qualified_name: String::new(),
        kind: kind.into(),
        file_path: file.clone(),
        line_start: line,
        line_end: line,
        language: "rust".into(),
    };
    let caller = format!("use_foo_{c}");
    g.ingest_symbols_and_refs(
        vec![def("Foo", "struct", 0), def(&caller, "function", 2)],
        vec![ScipRefRecord {
            caller_symbol: caller.clone(),
            callee_symbol: "Foo".into(),
            caller_qualified: String::new(),
            callee_qualified: String::new(),
            file_path: file.clone(),
            line: 3,
            start_col: -1,
            end_line: -1,
            end_col: -1,
            ref_kind: "direct".into(),
        }],
    )
    .await
    .unwrap();
}

struct Tools {
    symbols: DeclaredTool,
    callers: DeclaredTool,
    blast: DeclaredTool,
    search: DeclaredTool,
    recent: DeclaredTool,
    _tmp: tempfile::TempDir,
}

impl Tools {
    async fn setup() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("indexes");
        std::fs::create_dir_all(&data_dir).unwrap();
        for c in ["a", "b"] {
            super::build_fixture_index(&data_dir, &format!("scope-{c}"), None).await;
            graph(&data_dir, &format!("scope-{c}"), c).await;
        }
        let source = super::fixture_source(&data_dir);
        let merged = LazyScipGraph::deferred(data_dir.clone());
        Self {
            symbols: SymbolLookupTool::new(source.clone(), merged.clone()).declared(),
            callers: FindCallersTool::new(source.clone(), merged.clone()).declared(),
            blast: BlastRadiusTool::new(merged).declared(),
            search: CodeSearchTool::new(source.clone()).declared(),
            recent: RecentChangesTool::new(source).declared(),
            _tmp: tmp,
        }
    }

    async fn run(
        &self,
        tool: &DeclaredTool,
        params: serde_json::Value,
        scope: Option<&str>,
    ) -> String {
        let ctx = ToolContext {
            conversation_id: "corpus-scope".into(),
            corpus_scope: scope.map(str::to_string),
            ..Default::default()
        };
        text(&tool.execute(&params, &ctx).await)
    }
}

/// What each tool answers, scoped to `scope-a` and unscoped.
async fn answers(t: &Tools, scope: Option<&str>) -> Vec<(&'static str, String)> {
    vec![
        (
            "symbols",
            t.run(&t.symbols, serde_json::json!({ "name": "Foo" }), scope)
                .await,
        ),
        (
            "callers",
            t.run(&t.callers, serde_json::json!({ "symbol": "Foo" }), scope)
                .await,
        ),
        (
            "blast",
            t.run(&t.blast, serde_json::json!({ "symbol": "Foo" }), scope)
                .await,
        ),
        (
            "code_search",
            t.run(&t.search, serde_json::json!({ "query": "Foo" }), scope)
                .await,
        ),
        (
            "recent_changes",
            t.run(&t.recent, serde_json::json!({ "hours": 24 }), scope)
                .await,
        ),
    ]
}

/// FAILING INPUT (step 2): the corpus predicate dropped from a graph query,
/// so `scope-b`'s row appears; (step 3): `code_search`'s own enumeration
/// left ignoring the scope.
#[tokio::test]
async fn a_scoped_call_answers_from_its_corpus_alone() {
    let t = Tools::setup().await;
    let leaked: Vec<String> = answers(&t, Some("scope-a"))
        .await
        .into_iter()
        .filter(|(_, out)| !out.contains("a_foo.rs") || out.contains("b_foo.rs"))
        .map(|(tool, out)| format!("{tool}: {out}"))
        .collect();
    assert!(
        leaked.is_empty(),
        "scoped to scope-a, these answered from scope-b or lost scope-a's row:\n{}",
        leaked.join("\n---\n")
    );
}

/// Unscoped, every corpus answers, and the graph tools say which corpus
/// each row is from.
#[tokio::test]
async fn an_unscoped_call_answers_from_every_corpus_labelled() {
    let t = Tools::setup().await;
    for (tool, out) in answers(&t, None).await {
        assert!(
            out.contains("a_foo.rs") && out.contains("b_foo.rs"),
            "{tool} unscoped did not answer from both corpora: {out}"
        );
        if matches!(tool, "symbols" | "callers" | "blast") {
            assert!(
                out.contains("scope-a") && out.contains("scope-b"),
                "{tool} does not label its rows' corpora: {out}"
            );
        }
    }
}
