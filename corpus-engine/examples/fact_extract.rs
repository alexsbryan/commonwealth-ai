//! Phase 1 fact extractor — walks the relevant crates and dumps the deterministic
//! tree-sitter fact base to JSON for the query pipeline. Extends the fact_spike
//! primitives with corpus-walking + enclosing-function scoping.
//!
//! Run: cargo run -p corpus-engine --example fact_extract --features treesitter

use std::path::{Path, PathBuf};

use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

const REPO: &str = "/home/alexbryan/dev/commonwealth-ai";
const OUT: &str = "/home/alexbryan/.sovereign/indexes/commonwealth-ai/facts.json";
// crates whose src covers the 25-claim bank's evidence
const ROOTS: &[&str] = &[
    "sovereign/crates/sovereign-core/src",
    "sovereign/crates/sovereign-mesh/src",
    "sovereign/crates/sovereign-tools/src",
    "sovereign/crates/sovereign-cli-daemon/src",
    "corpus-engine/src",
    "corpus-engine-scip/src",
];

#[derive(serde::Serialize)]
struct CtorField {
    struct_type: String,
    field: String,
    value: String,
    enclosing_fn: String,
    file: String,
    line: usize,
}
#[derive(serde::Serialize)]
struct StrLit {
    content: String,
    enclosing_fn: String,
    file: String,
    line: usize,
}
#[derive(serde::Serialize)]
struct FnDef {
    name: String,
    file: String,
    line: usize,
}
#[derive(serde::Serialize, Default)]
struct Facts {
    ctor_fields: Vec<CtorField>,
    str_lits: Vec<StrLit>,
    fn_defs: Vec<FnDef>,
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().map_or(false, |x| x == "rs") {
                out.push(p);
            }
        }
    }
}

fn enclosing_fn(mut n: Node, src: &[u8]) -> String {
    while let Some(p) = n.parent() {
        if p.kind() == "function_item" {
            if let Some(name) = p.child_by_field_name("name") {
                return name.utf8_text(src).unwrap_or("").to_string();
            }
        }
        n = p;
    }
    String::new()
}

fn extract(rel: &str, src: &str, lang: &tree_sitter::Language, f: &mut Facts) {
    let mut parser = Parser::new();
    if parser.set_language(lang).is_err() {
        return;
    }
    let tree = match parser.parse(src, None) {
        Some(t) => t,
        None => return,
    };
    let b = src.as_bytes();
    let root = tree.root_node();

    // construction fields (the data-flow primitive)
    let q = Query::new(lang, "(struct_expression name: (_) @s body: (field_initializer_list (field_initializer field: (field_identifier) @f value: (_) @v)))").unwrap();
    let (si, fi, vi) = (
        q.capture_index_for_name("s").unwrap(),
        q.capture_index_for_name("f").unwrap(),
        q.capture_index_for_name("v").unwrap(),
    );
    let mut c = QueryCursor::new();
    let mut ms = c.matches(&q, root, b);
    while let Some(m) = ms.next() {
        if let (Some(s), Some(fl), Some(v)) = (
            m.nodes_for_capture_index(si).next(),
            m.nodes_for_capture_index(fi).next(),
            m.nodes_for_capture_index(vi).next(),
        ) {
            f.ctor_fields.push(CtorField {
                struct_type: s.utf8_text(b).unwrap_or("").to_string(),
                field: fl.utf8_text(b).unwrap_or("").to_string(),
                value: v.utf8_text(b).unwrap_or("").chars().take(60).collect(),
                enclosing_fn: enclosing_fn(v, b),
                file: rel.to_string(),
                line: v.start_position().row + 1,
            });
        }
    }

    // string literals (the literal primitive) — cap content, keep enclosing fn for scoping
    let q = Query::new(lang, "(string_literal) @s").unwrap();
    let si = q.capture_index_for_name("s").unwrap();
    let mut c = QueryCursor::new();
    let mut ms = c.matches(&q, root, b);
    while let Some(m) = ms.next() {
        if let Some(n) = m.nodes_for_capture_index(si).next() {
            let content: String = n.utf8_text(b).unwrap_or("").chars().take(200).collect();
            if content.len() > 3 {
                f.str_lits.push(StrLit {
                    content,
                    enclosing_fn: enclosing_fn(n, b),
                    file: rel.to_string(),
                    line: n.start_position().row + 1,
                });
            }
        }
    }

    // function definitions (the existence primitive)
    let q = Query::new(lang, "(function_item name: (identifier) @n)").unwrap();
    let ni = q.capture_index_for_name("n").unwrap();
    let mut c = QueryCursor::new();
    let mut ms = c.matches(&q, root, b);
    while let Some(m) = ms.next() {
        if let Some(n) = m.nodes_for_capture_index(ni).next() {
            f.fn_defs.push(FnDef {
                name: n.utf8_text(b).unwrap_or("").to_string(),
                file: rel.to_string(),
                line: n.start_position().row + 1,
            });
        }
    }
}

fn main() {
    // args: [repo_root] [out.json] [src_root ...]  — all optional; default to commonwealth-ai.
    // Lets the SAME binary extract a fresh repo with zero code edits (held-out generalization test).
    let args: Vec<String> = std::env::args().collect();
    let repo = args.get(1).map(String::as_str).unwrap_or(REPO).to_string();
    let out = args.get(2).map(String::as_str).unwrap_or(OUT).to_string();
    let roots: Vec<String> = if args.len() > 3 {
        args[3..].to_vec()
    } else {
        ROOTS.iter().map(|s| s.to_string()).collect()
    };

    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut files = Vec::new();
    for r in &roots {
        walk(&PathBuf::from(format!("{repo}/{r}")), &mut files);
    }
    let mut f = Facts::default();
    for path in &files {
        if let Ok(src) = std::fs::read_to_string(path) {
            let rel = path
                .strip_prefix(&repo)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            extract(&rel, &src, &lang, &mut f);
        }
    }
    let json = serde_json::to_string(&f).unwrap();
    std::fs::write(&out, &json).unwrap();
    println!(
        "extracted from {} files: {} ctor-fields · {} string-lits · {} fn-defs → {} ({} MB)",
        files.len(),
        f.ctor_fields.len(),
        f.str_lits.len(),
        f.fn_defs.len(),
        out,
        json.len() / 1_000_000
    );
    // sanity spot-checks
    let tools = f
        .ctor_fields
        .iter()
        .filter(|c| c.struct_type == "CompletionRequest" && c.field == "tools")
        .count();
    let sr = f.fn_defs.iter().any(|d| d.name == "select_route");
    println!("spot-check: CompletionRequest.tools sites={tools}, select_route present={sr}");
}
