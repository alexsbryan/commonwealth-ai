//! Smoke test: exercise the Code Intelligence tools against a
//! live LanceDB code index produced by `sovereign code index`.
//!
//! Usage:
//!     cargo run --example exercise_code_tools -p sovereign-tools -- \
//!         /tmp/sov-code-test
//!
//! The path argument is the parent directory that contains one corpus
//! subdirectory (e.g. `/tmp/sov-code-test/sovereign`). Expects a code
//! corpus already indexed via `sovereign code index`.
//!
//! Not part of the normal build path — lives under `examples/` as a
//! throwaway validation binary.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_core::traits::Tool;
use sovereign_core::types::{StepOutput, ToolContext};
use sovereign_tools::{CodeSearchTool, RecentChangesTool, SymbolLookupTool};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: exercise_code_tools <data-dir>");

    // Zero-vector embed fn — matches the P1 indexing strategy. The tools
    // don't use inference in this smoke test (CodeSearchTool falls back
    // to FTS-only when no inference provider is wired).
    let embed: EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 768]) })
    });
    let engine = Arc::new(CorpusEngine::new(
        data_dir.clone().join("_recipes"),
        data_dir.clone(),
        embed,
    ));

    let ctx = ToolContext {
        conversation_id: "test".to_string(),
        task_id: None,
        working_directory: None,
        in_reasoning_loop: false,
        agent_session_token: None,
    };

    // ─── symbol_lookup ────────────────────────────────────────
    println!("─── symbol_lookup(name = \"Runtime\") ───");
    // SymbolLookupTool now reads SCIP. Example uses an empty
    // in-memory graph; lookups will report "not found" honestly.
    let graph: sovereign_tools::ScipGraphHandle = Arc::new(
        arc_swap::ArcSwap::from_pointee(
            corpus_engine::ScipGraph::open_in_memory("example")
                .expect("in-memory ScipGraph"),
        ),
    );
    let sym_tool = SymbolLookupTool::new(Arc::clone(&engine), Arc::clone(&graph));
    let out = sym_tool
        .execute(&serde_json::json!({ "name": "Runtime" }), &ctx)
        .await?;
    print_text(&out);
    println!();

    println!("─── symbol_lookup(name = \"compress_working_memory\", kind = \"function\") ───");
    let out = sym_tool
        .execute(
            &serde_json::json!({
                "name": "compress_working_memory",
                "kind": "function"
            }),
            &ctx,
        )
        .await?;
    print_text(&out);
    println!();

    println!("─── symbol_lookup rejects injection ───");
    match sym_tool
        .execute(&serde_json::json!({ "name": "foo' OR 1=1 --" }), &ctx)
        .await
    {
        Ok(_) => println!("  (unexpected success — injection slipped through)"),
        Err(e) => println!("  refused: {e}"),
    }
    println!();

    // ─── code_search (FTS-only path, no inference) ─────────────
    println!("─── code_search(query = \"parse the recipe\") [FTS fallback] ───");
    let search_tool = CodeSearchTool::new(Arc::clone(&engine));
    let out = search_tool
        .execute(
            &serde_json::json!({ "query": "parse the recipe" }),
            &ctx,
        )
        .await?;
    print_text(&out);
    println!();

    // ─── recent_changes ────────────────────────────────────────
    // Use a huge window so we definitely see something from the corpus
    // we indexed today.
    println!("─── recent_changes(hours = 168) ───");
    let recent_tool = RecentChangesTool::new(Arc::clone(&engine));
    let out = recent_tool
        .execute(&serde_json::json!({ "hours": 168_u64 }), &ctx)
        .await?;
    print_text_short(&out, 60);
    println!();

    Ok(())
}

fn print_text(out: &StepOutput) {
    if let StepOutput::Text(s) = out {
        for line in s.lines().take(40) {
            println!("  {line}");
        }
        let total = s.lines().count();
        if total > 40 {
            println!("  … ({} more lines)", total - 40);
        }
    } else {
        println!("  (non-text output: {out:?})");
    }
}

fn print_text_short(out: &StepOutput, max_lines: usize) {
    if let StepOutput::Text(s) = out {
        for line in s.lines().take(max_lines) {
            println!("  {line}");
        }
        let total = s.lines().count();
        if total > max_lines {
            println!("  … ({} more lines)", total - max_lines);
        }
    }
}
