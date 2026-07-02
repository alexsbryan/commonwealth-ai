//! Smoke test for the deterministic dispatch (`facts_check`) on the live commonwealth-ai corpus.
//! Validates CONFIG (#12 → drift), EXISTS, and LITERAL without the daemon (scopes CONFIG from the
//! known code-query entry directly, bypassing embed-based resolution — tested separately).
//!
//! Run: cargo run -p corpus-engine --example fact_check_smoke --features treesitter

use std::path::PathBuf;

use corpus_engine::facts::Facts;
use corpus_engine::facts_check::{
    build_adjacency, check_config, check_exists, check_literal, neighborhood_stems, Verdict,
    VerdictKind,
};
use corpus_engine_scip::scip_graph::ScipGraph;

fn show(name: &str, v: &Verdict, expect: VerdictKind) {
    let ok = v.kind == expect;
    println!(
        "  [{}] {:?}  {}  — {}",
        if ok { "OK" } else { "XX" },
        v.kind,
        name,
        v.receipt
    );
}

#[tokio::main]
async fn main() {
    let home = std::env::var("HOME").expect("HOME");
    let dir = PathBuf::from(&home).join(".sovereign/indexes/commonwealth-ai");
    let facts = Facts::load(&dir.join("facts.json")).expect("load facts.json");
    let graph =
        ScipGraph::open(&dir.join("scip_graph.db"), "commonwealth-ai").expect("open scip_graph.db");
    println!(
        "facts: {} ctor-fields · {} lits · {} fn-defs\n",
        facts.ctor_fields.len(),
        facts.str_lits.len(),
        facts.fn_defs.len()
    );

    println!("CONFIG (data-flow drift, scoped via the qualified call graph):");
    let adj = build_adjacency(&graph).await;
    let stems = neighborhood_stems(&adj, &["handle_code_query".to_string()], 2);
    show(
        "#12 code-query flow exposes tools",
        &check_config(&facts, &stems, "tools", true),
        VerdictKind::Drift,
    );

    println!("EXISTS (function-definition lookup):");
    show(
        "select_route",
        &check_exists(&facts, "select_route", true),
        VerdictKind::Corroborated,
    );
    show(
        "gate_answer",
        &check_exists(&facts, "gate_answer", true),
        VerdictKind::Corroborated,
    );
    show(
        "nonexistent_xyz (should abstain)",
        &check_exists(&facts, "nonexistent_xyz", true),
        VerdictKind::Unverifiable,
    );

    println!("LITERAL (string-literal lookup):");
    show(
        "/v1/chat/completions",
        &check_literal(&facts, "/v1/chat/completions", true),
        VerdictKind::Corroborated,
    );
    show(
        "SUMMARY:",
        &check_literal(&facts, "SUMMARY:", true),
        VerdictKind::Corroborated,
    );
    show(
        "nonexistent-zzz (should abstain)",
        &check_literal(&facts, "nonexistent-zzz-string", true),
        VerdictKind::Unverifiable,
    );
}
