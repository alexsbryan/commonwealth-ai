// SPDX-License-Identifier: AGPL-3.0-or-later
//! SCIP-sourced symbol enumeration for code-intel enrichment (slice 2).
//!
//! Turns a code corpus's SCIP graph + on-disk source into the
//! [`SymbolSource`] list that [`super::enrich_symbols_incremental`] consumes.
//! Gated on `treesitter` (the feature that pulls in `corpus-engine-scip`),
//! matching `atlas::strategies::code_walk`.
//!
//! Split for testability: [`enumerate_from_rows`] is pure glue (sync file IO,
//! no graph access) exercised with hand-built rows + a temp source tree;
//! [`enumerate_symbol_sources`] is the thin async wrapper that asks the graph
//! for every symbol and hands the rows to it.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use corpus_engine_scip::{ScipGraph, SymbolRow};

use super::{extract_body, is_enrichable_kind, SymbolMeta, SymbolSource};
use crate::error::Result;

/// Bodies shorter than this (trimmed) are skipped — one-line getters and the
/// like don't carry enough intent to summarize usefully.
const MIN_BODY_CHARS: usize = 24;

/// Map a SCIP row to our decoupled [`SymbolMeta`]. Line numbers are converted
/// from SCIP's 0-based to 1-based (editor / `file:line` clickable) display.
fn symbol_meta_from_row(row: &SymbolRow) -> SymbolMeta {
    SymbolMeta {
        name: row.name.clone(),
        qualified_name: row.qualified_name.clone(),
        file_path: row.file_path.clone(),
        line_start: (row.line_start.max(0) + 1) as u32,
        line_end: (row.line_end.max(row.line_start) + 1) as u32,
        language: row.language.clone(),
    }
}

/// Build the [`SymbolSource`] list from SCIP rows + on-disk source. Reads each
/// file once (cache), skips non-functions, unreadable files, and trivially
/// short bodies, and dedups by `(file, line, name)` (SCIP can double-list a
/// symbol under two path prefixes). Pure + sync so it is unit-testable.
pub fn enumerate_from_rows(
    rows: &[SymbolRow],
    source_root: &Path,
    file_filter: &[String],
    caller_set: &HashSet<String>,
) -> Vec<SymbolSource> {
    let mut file_cache: HashMap<String, Option<String>> = HashMap::new();
    let mut seen: HashSet<(String, i32, String)> = HashSet::new();
    let mut out = Vec::new();
    let (mut enrichable, mut emitted, mut skipped_short, mut unreadable) = (0usize, 0, 0, 0);

    for row in rows {
        // Optional scope: when `file_filter` is non-empty, keep only symbols
        // whose file path contains one of the substrings (e.g. a §4 subsystem
        // grade enriches just `streaming.rs,engine.rs,...`). Empty = whole corpus.
        if !file_filter.is_empty()
            && !file_filter.iter().any(|f| row.file_path.contains(f.as_str()))
        {
            continue;
        }
        // Precise function screen: when the call graph carries refs, enrich only
        // symbols that appear as a CALLER (provably have a body that calls
        // something) — the reliable "real function/method" signal that the SCIP
        // `kind` field is not. An empty set (a graph with no refs, e.g. a test
        // fixture) falls back to the kind screen below.
        if !caller_set.is_empty() && !caller_set.contains(&row.qualified_name) {
            continue;
        }
        if !is_enrichable_kind(&row.kind) {
            continue;
        }
        enrichable += 1;
        if !seen.insert((row.file_path.clone(), row.line_start, row.name.clone())) {
            continue; // duplicate symbol row
        }
        let contents = file_cache
            .entry(row.file_path.clone())
            .or_insert_with(|| std::fs::read_to_string(source_root.join(&row.file_path)).ok());
        let Some(contents) = contents.as_ref() else {
            unreadable += 1;
            continue;
        };
        let body = extract_body(contents, row.line_start, row.line_end);
        if body.trim().chars().count() < MIN_BODY_CHARS {
            skipped_short += 1;
            continue;
        }
        out.push(SymbolSource {
            meta: symbol_meta_from_row(row),
            body,
        });
        emitted += 1;
    }

    tracing::info!(
        target: "enrichment.code_intel",
        rows = rows.len(),
        enrichable,
        emitted,
        skipped_short,
        unreadable,
        "code_intel: enumerated symbol sources from SCIP",
    );
    out
}

/// Enumerate every enrichable symbol in a corpus's SCIP graph, reading each
/// body from `source_root`. The all-symbols query uses empty prefixes —
/// `symbols_in_crate("", "")` matches the whole corpus via `file_path LIKE '%'`.
pub async fn enumerate_symbol_sources(
    scip: &ScipGraph,
    source_root: &Path,
    file_filter: &[String],
) -> Result<Vec<SymbolSource>> {
    let rows = scip.symbols_in_crate("", "").await?;
    // The call-graph caller-set is the precise function population (the SCIP
    // `kind` field is unreliable). Empty for a graph with no refs → no filter.
    let caller_set = scip.caller_qualified_names().await?;
    Ok(enumerate_from_rows(&rows, source_root, file_filter, &caller_set))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, kind: &str, file: &str, ls: i32, le: i32) -> SymbolRow {
        SymbolRow {
            corpus_id: "c".to_string(),
            name: name.to_string(),
            qualified_name: format!("crate::{name}"),
            kind: kind.to_string(),
            file_path: file.to_string(),
            line_start: ls,
            line_end: le,
            language: "rust".to_string(),
        }
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("code_intel_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn enumerate_reads_function_bodies_and_filters() {
        let dir = scratch("enum");
        let src = "// 0\nfn select_route() {\n    decide_where_the_request_runs();\n}\nstruct Cfg { a: u8 }\n";
        std::fs::write(dir.join("x.rs"), src).unwrap();

        let rows = vec![
            row("select_route", "function", "x.rs", 1, 3), // 0-based lines 1..=3
            row("Cfg", "struct", "x.rs", 4, 4),            // not a function -> filtered
            row("ghost", "function", "missing.rs", 0, 2),  // unreadable file -> skipped
        ];
        let out = enumerate_from_rows(&rows, &dir, &[], &std::collections::HashSet::new());

        assert_eq!(out.len(), 1, "only the readable function is emitted");
        let s = &out[0];
        assert_eq!(s.meta.name, "select_route");
        assert_eq!(s.meta.line_start, 2, "0-based 1 -> 1-based 2 display");
        assert!(s.body.contains("fn select_route"));
        assert!(s.body.contains("decide_where_the_request_runs"));
        assert!(!s.body.contains("struct Cfg"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dedups_double_listed_rows_and_skips_short_bodies() {
        let dir = scratch("dedup");
        std::fs::write(dir.join("y.rs"), "fn f() {\n    do_a_real_thing_here();\n}\nfn tiny() {}\n").unwrap();

        let out = enumerate_from_rows(
            &[
                row("f", "function", "y.rs", 0, 2),
                row("f", "function", "y.rs", 0, 2), // exact duplicate -> collapses
                row("tiny", "function", "y.rs", 3, 3), // body "fn tiny() {}" < 24 chars -> skipped
            ],
            &dir,
            &[],
            &std::collections::HashSet::new(),
        );
        assert_eq!(out.len(), 1, "duplicate collapses and the tiny body is skipped");
        assert_eq!(out[0].meta.name, "f");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn enumerate_symbol_sources_reads_from_in_memory_graph() {
        let dir = scratch("graph");
        std::fs::write(
            dir.join("z.rs"),
            "fn handle() {\n    route_and_run_the_request();\n}\n",
        )
        .unwrap();

        let scip = ScipGraph::open_in_memory("c").expect("in-memory graph");
        scip.ingest_symbols_and_refs(
            vec![corpus_engine_scip::ScipSymbolRecord {
                name: "handle".to_string(),
                qualified_name: "crate::handle".to_string(),
                kind: "function".to_string(),
                file_path: "z.rs".to_string(),
                line_start: 0,
                line_end: 2,
                language: "rust".to_string(),
            }],
            vec![],
        )
        .await
        .expect("ingest");

        let out = enumerate_symbol_sources(&scip, &dir, &[]).await.expect("enumerate");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].meta.name, "handle");
        assert!(out[0].body.contains("route_and_run_the_request"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
