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

use super::{extract_body_from_lines, is_enrichable_kind, PromptKind, SymbolMeta, SymbolSource};
use crate::error::Result;
use corpus_engine_scip::{descriptor_kind, DescriptorKind};

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
/// Which prompt a descriptor deserves, or `None` to skip it entirely.
///
/// One decider, reusing `corpus_engine_scip::descriptor_kind` rather than
/// re-deriving the SCIP grammar here (ARCH §10.6).
pub fn prompt_kind_for(qualified_name: &str) -> Option<PromptKind> {
    let k = descriptor_kind(qualified_name);
    if k.is_callable() {
        return Some(PromptKind::Callable);
    }
    match k {
        DescriptorKind::Type => Some(PromptKind::Type),
        // POSITIVE IDENTIFICATION TO DROP, fallback to keep. Only a descriptor
        // we can positively read as non-callable is skipped; an unreadable one
        // keeps its prior treatment.
        //
        // The SCIP descriptor grammar is the CROSS-LANGUAGE signal, which is
        // why routing on it does not tie this pass to one exporter. Measured
        // on this graph 2026-08-24: every symbol carries a descriptor (zero
        // empty), and scip-python emits the same shapes as rust-analyzer —
        // 14,219 callable and 3,303 type descriptors of its own. This arm is
        // therefore not a language special-case but a refusal to drop what we
        // could not read, for whatever exporter turns up next.
        DescriptorKind::Unrecognized => Some(PromptKind::Callable),
        _ => None,
    }
}

pub fn enumerate_from_rows(
    rows: &[SymbolRow],
    source_root: &Path,
    file_filter: &[String],
    caller_set: &HashSet<String>,
) -> Vec<SymbolSource> {
    // Cache each file's lines split ONCE (not per-symbol). Body extraction below
    // reads from these, turning the per-file cost from O(functions × file_len) to
    // O(file_len + bodies) — what makes the whole-corpus enumerate tractable.
    let mut file_cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
    let mut seen: HashSet<(String, i32, String)> = HashSet::new();
    let mut out = Vec::new();
    let (mut enrichable, mut emitted, mut skipped_short, mut unreadable) = (0usize, 0, 0, 0);
    let mut skipped_kind = 0usize;

    for row in rows {
        // Optional scope: when `file_filter` is non-empty, keep only symbols
        // whose file path contains one of the substrings (e.g. a §4 subsystem
        // grade enriches just `streaming.rs,engine.rs,...`). Empty = whole corpus.
        if !file_filter.is_empty()
            && !file_filter
                .iter()
                .any(|f| row.file_path.contains(f.as_str()))
        {
            continue;
        }
        // ROUTE ON THE DESCRIPTOR FIRST. It is the one reliable signal for what
        // a symbol IS; the two screens below are function-shaped heuristics and
        // are therefore applied to CALLABLES ONLY.
        //
        // Types are KEPT (with their own prompt): "what does this represent" is
        // exactly the question a destination-first audit asks. Modules and the
        // rest are DROPPED — a module's body is the whole file, the most
        // expensive call in the run and the least useful answer.
        let Some(kind) = prompt_kind_for(&row.qualified_name) else {
            skipped_kind += 1;
            continue;
        };
        if kind == PromptKind::Callable {
            // Precise function screen: when the call graph carries refs, enrich
            // only symbols that appear as a CALLER (provably have a body that
            // calls something) — the reliable "real function/method" signal the
            // SCIP `kind` field is not. Empty set (a graph with no refs, e.g. a
            // test fixture) = no filter.
            //
            // THIS MUST NOT GATE TYPES, and until 2026-08-31 it did, because it
            // ran BEFORE the router. A type reached the type prompt only when
            // `#[derive(..)]` expansions happened to emit refs whose CALLER was
            // the type — so types were in the corpus BY ACCIDENT. Measured on
            // this graph 2026-08-31: 4,366 of 9,115 type descriptors (48%)
            // appear as a caller; the other 4,749 were dropped before the
            // router that exists to route them ever ran. `AdmissionReason`,
            // `IngestProgress` and `StepKind` all sit at zero caller refs and
            // were all absent.
            //
            // 9,115 counts `Type` ONLY. `EnumVariant` (`Enum#Variant#`) is a
            // separate descriptor class that `prompt_kind_for` drops; folding
            // the two together gives 13,426 and overstates this gap by 2x.
            if !caller_set.is_empty() && !caller_set.contains(&row.qualified_name) {
                continue;
            }
            // Same reasoning: this reject list is TYPE-SHAPED
            // ("enum" | "struct" | "class" | "type" | ...), so applying it to a
            // descriptor-confirmed type would drop exactly what we just decided
            // to keep. Rust types only ever passed it because rust-analyzer
            // labels them `constructor`, which the list does not name — a
            // coincidence, not a decision.
            if !is_enrichable_kind(&row.kind) {
                continue;
            }
        }
        enrichable += 1;
        if !seen.insert((row.file_path.clone(), row.line_start, row.name.clone())) {
            continue; // duplicate symbol row
        }
        let lines = file_cache.entry(row.file_path.clone()).or_insert_with(|| {
            std::fs::read_to_string(source_root.join(&row.file_path))
                .ok()
                .map(|c| c.lines().map(str::to_string).collect::<Vec<String>>())
        });
        let Some(lines) = lines.as_ref() else {
            unreadable += 1;
            continue;
        };
        let body = extract_body_from_lines(lines, row.line_start, row.line_end);
        if body.trim().chars().count() < MIN_BODY_CHARS {
            skipped_short += 1;
            continue;
        }
        out.push(SymbolSource {
            kind,
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
    // Per-step timing — the whole-corpus enumerate is the per-run setup cost
    // (re-paid on every resume), so make each stage observable.
    let t = std::time::Instant::now();
    let rows = scip.symbols_in_crate("", "").await?;
    tracing::info!(target: "enrichment.code_intel", rows = rows.len(), ms = t.elapsed().as_millis() as u64, "enum step: symbols_in_crate");
    // The call-graph caller-set is the precise function population (the SCIP
    // `kind` field is unreliable). Empty for a graph with no refs → no filter.
    let t = std::time::Instant::now();
    let caller_set = scip.caller_qualified_names().await?;
    tracing::info!(target: "enrichment.code_intel", callers = caller_set.len(), ms = t.elapsed().as_millis() as u64, "enum step: caller_qualified_names");
    let t = std::time::Instant::now();
    let out = enumerate_from_rows(&rows, source_root, file_filter, &caller_set);
    tracing::info!(target: "enrichment.code_intel", out = out.len(), ms = t.elapsed().as_millis() as u64, "enum step: enumerate_from_rows");
    Ok(out)
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
    /// A struct walks straight through the caller screen, because
    /// `#[derive(..)]` expansions emit refs whose CALLER is the type. It must
    /// be routed to the TYPE prompt, not asked "what does this function do".
    #[test]
    fn a_type_is_routed_to_the_type_prompt_not_the_function_one() {
        assert_eq!(
            prompt_kind_for("rust-analyzer cargo c 0.1.0 m/Label#"),
            Some(PromptKind::Type)
        );
        assert_eq!(
            prompt_kind_for("rust-analyzer cargo c 0.1.0 m/select_route()."),
            Some(PromptKind::Callable)
        );
    }

    /// Routing a type correctly is useless if the type never reaches the router.
    ///
    /// The caller screen ran FIRST until 2026-08-31, so a type was enumerated
    /// only when `#[derive(..)]` happened to emit a ref whose CALLER was the
    /// type. Measured on this repo's graph 2026-08-31: 4,366 of 9,115 type
    /// descriptors (48%) appear as a caller — the other 4,749 were dropped
    /// before `prompt_kind_for` ran. `AdmissionReason` (0 caller refs) is a
    /// real example that was absent from a 21,862-summary cache.
    #[test]
    fn a_type_with_no_caller_refs_is_still_enumerated() {
        let dir = scratch("type_no_callers");
        std::fs::write(
            dir.join("a.rs"),
            "// 0\npub enum AdmissionReason {\n    Accepted,\n    RejectedBecauseFull,\n}\n",
        )
        .unwrap();
        // A non-empty caller set that does NOT name the type: exactly the
        // production shape, where refs exist but no derive named this one.
        let mut callers = std::collections::HashSet::new();
        callers.insert("crate::some_unrelated_fn".to_string());

        let mut r = row("AdmissionReason", "constructor", "a.rs", 1, 4);
        r.qualified_name = "rust-analyzer cargo c 0.1.0 admission/AdmissionReason#".to_string();

        let out = enumerate_from_rows(&[r], &dir, &[], &callers);
        assert_eq!(
            out.len(),
            1,
            "a type with zero caller refs must still be enumerated"
        );
        assert_eq!(
            out[0].kind,
            PromptKind::Type,
            "and routed to the TYPE prompt"
        );
    }

    /// The other half of the same change: the function screen still screens
    /// FUNCTIONS. Loosening it for types must not loosen it for callables.
    #[test]
    fn a_callable_absent_from_the_caller_set_is_still_dropped() {
        let dir = scratch("callable_screened");
        std::fs::write(
            dir.join("b.rs"),
            "// 0\nfn select_route() {\n    decide_where_the_request_runs();\n}\n",
        )
        .unwrap();
        let mut callers = std::collections::HashSet::new();
        callers.insert("crate::some_unrelated_fn".to_string());

        let mut r = row("select_route", "function", "b.rs", 1, 3);
        r.qualified_name = "rust-analyzer cargo c 0.1.0 m/select_route().".to_string();

        let out = enumerate_from_rows(&[r], &dir, &[], &callers);
        assert!(
            out.is_empty(),
            "a callable that never appears as a caller is still screened out"
        );
    }

    /// A module's body is the whole file — the most expensive call in the run
    /// and the least useful answer. Dropped, not described.
    /// An exporter whose descriptors this grammar cannot read must KEEP its
    /// symbols, not lose them. Every exporter on today's graph emits readable
    /// descriptors, so this arm guards the next one rather than a current gap.
    #[test]
    fn an_unreadable_descriptor_keeps_the_symbol_rather_than_dropping_it() {
        assert_eq!(prompt_kind_for("handle"), Some(PromptKind::Callable));
        assert_eq!(prompt_kind_for(""), Some(PromptKind::Callable));
    }

    #[test]
    fn modules_and_other_non_callables_are_skipped_entirely() {
        for d in [
            "rust-analyzer cargo c 0.1.0 refactor_cmd/labels/",
            "rust-analyzer cargo c 0.1.0 m/CONST.",
            "rust-analyzer cargo c 0.1.0 m/Type#field.",
        ] {
            assert_eq!(prompt_kind_for(d), None, "{d} should be skipped");
        }
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
        std::fs::write(
            dir.join("y.rs"),
            "fn f() {\n    do_a_real_thing_here();\n}\nfn tiny() {}\n",
        )
        .unwrap();

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
        assert_eq!(
            out.len(),
            1,
            "duplicate collapses and the tiny body is skipped"
        );
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

        let out = enumerate_symbol_sources(&scip, &dir, &[])
            .await
            .expect("enumerate");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].meta.name, "handle");
        assert!(out[0].body.contains("route_and_run_the_request"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
