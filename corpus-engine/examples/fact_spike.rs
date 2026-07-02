//! Fact-base spike: can a SMALL set of deterministic tree-sitter fact primitives
//! answer claims of DIFFERENT shapes — the "does it generalize?" test.
//!
//! Three primitives, extracted in one pass (production walks the whole corpus; this
//! spike curates the evidence files spanning 4 crates to stay fast):
//!   P1 construction-field  — `Type { field: VALUE }`  (the DATA-FLOW fact: tools:None)
//!   P2 string-literal       — literals + location       (config strings, prompt content)
//!   P3 function-definition  — name + location           (existence)
//! Then four claims of four shapes are answered as pure lookups over the facts.
//!
//! Run: cargo run -p corpus-engine --example fact_spike --features treesitter

use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

const REPO: &str = "/home/alexbryan/dev/commonwealth-ai";
const FILES: &[&str] = &[
    "sovereign/crates/sovereign-core/src/runtime/handlers/knowledge_query.rs", // #12 config
    "sovereign/crates/sovereign-cli-daemon/src/daemon_cmd/worker.rs",          // chat URL
    "corpus-engine/src/enrichment/code_intel/mod.rs",                          // SUMMARY:/ASKS:
    "sovereign/crates/sovereign-mesh/src/peer_inference.rs",                   // select_route
];

#[derive(Default)]
struct Facts {
    ctor_fields: Vec<(String, String, String, String, usize)>, // (struct, field, value, file, line)
    str_lits: Vec<(String, String, usize)>,                    // (content, file, line)
    fn_defs: Vec<(String, String, usize)>,                     // (name, file, line)
}

fn extract(file: &str, src: &str, lang: &tree_sitter::Language, f: &mut Facts) {
    let mut parser = Parser::new();
    parser.set_language(lang).expect("set_language");
    let tree = parser.parse(src, None).expect("parse");
    let b = src.as_bytes();
    let root = tree.root_node();

    // P1 — construction fields (the data-flow primitive)
    let q = Query::new(
        lang,
        "(struct_expression name: (_) @s body: (field_initializer_list (field_initializer field: (field_identifier) @f value: (_) @v)))",
    )
    .expect("q1");
    let (si, fi, vi) = (
        q.capture_index_for_name("s").unwrap(),
        q.capture_index_for_name("f").unwrap(),
        q.capture_index_for_name("v").unwrap(),
    );
    let mut c = QueryCursor::new();
    let mut ms = c.matches(&q, root, b);
    while let Some(m) = ms.next() {
        let s = m.nodes_for_capture_index(si).next();
        let fl = m.nodes_for_capture_index(fi).next();
        let v = m.nodes_for_capture_index(vi).next();
        if let (Some(s), Some(fl), Some(v)) = (s, fl, v) {
            f.ctor_fields.push((
                s.utf8_text(b).unwrap_or("").to_string(),
                fl.utf8_text(b).unwrap_or("").to_string(),
                v.utf8_text(b).unwrap_or("").chars().take(40).collect(),
                file.to_string(),
                v.start_position().row + 1,
            ));
        }
    }

    // P2 — string literals (the literal primitive)
    let q = Query::new(lang, "(string_literal) @s").expect("q2");
    let si = q.capture_index_for_name("s").unwrap();
    let mut c = QueryCursor::new();
    let mut ms = c.matches(&q, root, b);
    while let Some(m) = ms.next() {
        if let Some(n) = m.nodes_for_capture_index(si).next() {
            f.str_lits.push((
                n.utf8_text(b).unwrap_or("").to_string(),
                file.to_string(),
                n.start_position().row + 1,
            ));
        }
    }

    // P3 — function definitions (the existence primitive)
    let q = Query::new(lang, "(function_item name: (identifier) @n)").expect("q3");
    let ni = q.capture_index_for_name("n").unwrap();
    let mut c = QueryCursor::new();
    let mut ms = c.matches(&q, root, b);
    while let Some(m) = ms.next() {
        if let Some(n) = m.nodes_for_capture_index(ni).next() {
            f.fn_defs.push((
                n.utf8_text(b).unwrap_or("").to_string(),
                file.to_string(),
                n.start_position().row + 1,
            ));
        }
    }
}

fn verdict(claim: &str, key: &str, got: &str, receipt: &str) {
    let ok = got.eq_ignore_ascii_case(key);
    println!(
        "[{:12}] key={:12} {}  {}",
        got,
        key,
        if ok { "✓" } else { "✗" },
        claim
    );
    println!("       receipt: {receipt}");
}

fn main() {
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut f = Facts::default();
    for rel in FILES {
        match std::fs::read_to_string(format!("{REPO}/{rel}")) {
            Ok(src) => extract(rel, &src, &lang, &mut f),
            Err(e) => eprintln!("skip {rel}: {e}"),
        }
    }
    println!(
        "FACT BASE (one tree-sitter pass, {} files): {} ctor-fields · {} string-lits · {} fn-defs\n",
        FILES.len(),
        f.ctor_fields.len(),
        f.str_lits.len(),
        f.fn_defs.len()
    );

    // ── Claim 1 · WIRING/CONFIG (#12): does the code-query flow expose tools to the model? ──
    let tools: Vec<_> = f
        .ctor_fields
        .iter()
        .filter(|(s, fl, _, file, _)| {
            s == "CompletionRequest" && fl == "tools" && file.contains("knowledge_query")
        })
        .collect();
    let all_none = !tools.is_empty() && tools.iter().all(|(_, _, v, _, _)| v == "None");
    verdict(
        "#12 code-query flow exposes tools to the model (agentic loop)",
        "drift",
        if all_none { "drift" } else { "unverified" },
        &format!(
            "CompletionRequest.tools = None at {} site(s); e.g. {}:{}",
            tools.len(),
            tools.first().map(|t| t.3.as_str()).unwrap_or("?"),
            tools.first().map(|t| t.4).unwrap_or(0)
        ),
    );

    // ── Claim 2 · CALL+LITERAL: chat summaries via POST /v1/chat/completions ──
    let chat = f
        .str_lits
        .iter()
        .find(|(s, _, _)| s.contains("/v1/chat/completions"));
    verdict(
        "chat summaries POST to /v1/chat/completions",
        "corroborated",
        if chat.is_some() {
            "corroborated"
        } else {
            "unverified"
        },
        &chat
            .map(|(_, fi, l)| format!("literal at {fi}:{l}"))
            .unwrap_or_else(|| "not found".into()),
    );

    // ── Claim 3 · LITERAL (summary-recall got this WRONG — false GAP): prompt uses SUMMARY:/ASKS: ──
    let has_s = f.str_lits.iter().find(|(s, _, _)| s.contains("SUMMARY:"));
    let has_a = f.str_lits.iter().any(|(s, _, _)| s.contains("ASKS:"));
    verdict(
        "enrich prompt requires SUMMARY: and ASKS: output",
        "corroborated",
        if has_s.is_some() && has_a {
            "corroborated"
        } else {
            "unverified"
        },
        &has_s
            .map(|(_, fi, l)| format!("SUMMARY: literal at {fi}:{l}; ASKS: present={has_a}"))
            .unwrap_or_else(|| "not found".into()),
    );

    // ── Claim 4 · EXISTENCE: select_route is the routing entry ──
    let sr = f.fn_defs.iter().find(|(n, _, _)| n == "select_route");
    verdict(
        "select_route is the local-vs-remote routing entry",
        "corroborated",
        if sr.is_some() {
            "corroborated"
        } else {
            "unverified"
        },
        &sr.map(|(_, fi, l)| format!("fn defined at {fi}:{l}"))
            .unwrap_or_else(|| "not found".into()),
    );
}
