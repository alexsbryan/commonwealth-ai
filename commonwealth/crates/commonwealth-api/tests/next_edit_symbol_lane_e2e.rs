// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end proof of the symbol-lane navigation affordance
//! (`commonwealth_api::next_edit_symbols`), against a REAL `ScipGraph`
//! and real files on disk — not a mock of either.
//!
//! What each test drives: an indexed function whose signature the
//! developer then edits in an unsaved buffer, and the jump list the
//! lane produces. The graph is built with the exporter's own ingest
//! path, so the columns and descriptors are shaped the way the live
//! index shapes them.
//!
//! WHY THESE ASSERTIONS. The lane's pre-registered bar is RECALL — it
//! must name the call sites of the function under the cursor — and its
//! measured hazards are the three classes M1a found dominating the M0
//! population (`sovereign/docs/specs/NEXT_EDIT_SYMBOL_LANE.md`): a
//! brand-new function, a file that merely moved, and an occurrence
//! that is a `use` import rather than a call. There is a test for each,
//! and each fails if the corresponding guard is removed.

use commonwealth_api::next_edit_symbols::{navigate, Decline};
use corpus_engine_scip::scip_graph::{ScipGraph, ScipRefRecord, ScipSymbolRecord};

const CORPUS: &str = "e2e-symbol-lane";

/// `fn helper(a: usize, b: usize)` declared in `lib.rs`, called from
/// two other files, imported by a third.
const DECL: &str = "\
pub fn helper(a: usize, b: usize) -> usize {
    a + b
}
";

const CALLER_A: &str = "\
use crate::helper;

pub fn one() -> usize {
    helper(1, 2)
}
";

const CALLER_B: &str = "\
pub fn two() -> usize {
    crate::helper(3, 4)
}
";

fn sym(name: &str, qual: &str, file: &str, ls: i32, le: i32) -> ScipSymbolRecord {
    ScipSymbolRecord {
        name: name.to_string(),
        qualified_name: qual.to_string(),
        kind: "function".to_string(),
        file_path: file.to_string(),
        line_start: ls,
        line_end: le,
        language: "rust".to_string(),
    }
}

fn r#ref(callee: &str, qual: &str, file: &str, line: i32, end_col: i32) -> ScipRefRecord {
    ScipRefRecord {
        caller_symbol: "site".to_string(),
        callee_symbol: callee.to_string(),
        caller_qualified: String::new(),
        callee_qualified: qual.to_string(),
        file_path: file.to_string(),
        line,
        start_col: end_col - callee.len() as i32,
        end_line: line,
        end_col,
        ref_kind: "direct".to_string(),
    }
}

const QUAL: &str = "rust-analyzer cargo e2e 0.1.0 lib/helper().";

/// Writes the three files and ingests the graph that describes them.
async fn fixture() -> (tempfile::TempDir, ScipGraph) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), DECL).unwrap();
    std::fs::write(dir.path().join("a.rs"), CALLER_A).unwrap();
    std::fs::write(dir.path().join("b.rs"), CALLER_B).unwrap();

    let graph = ScipGraph::open_in_memory(CORPUS).expect("graph");
    graph
        .ingest_symbols_and_refs(
            vec![sym("helper", QUAL, "lib.rs", 0, 2)],
            vec![
                // the declaration's own occurrence — never a destination
                r#ref("helper", QUAL, "lib.rs", 0, 13),
                // `use crate::helper;` — an occurrence, NOT a call
                r#ref("helper", QUAL, "a.rs", 0, 17),
                // `    helper(1, 2)` — a real call
                r#ref("helper", QUAL, "a.rs", 3, 10),
                // `    crate::helper(3, 4)` — a real call
                r#ref("helper", QUAL, "b.rs", 1, 17),
            ],
        )
        .await
        .expect("ingest");
    (dir, graph)
}

/// The buffer: the developer has added a third parameter and not saved.
const EDITED: &str = "\
pub fn helper(a: usize, b: usize, c: u8) -> usize {
    a + b
}
";

fn reader(dir: &std::path::Path) -> impl Fn(&str) -> Option<String> + '_ {
    move |p: &str| std::fs::read_to_string(dir.join(p)).ok()
}

#[tokio::test]
async fn a_signature_edit_names_every_call_site_and_no_import() {
    let (dir, graph) = fixture().await;
    let cursor = EDITED.find("c: u8").expect("cursor in the new parameter");

    let nav = navigate(&graph, Some("lib.rs"), EDITED, cursor, reader(dir.path()))
        .await
        .expect("the lane should fire on a changed parameter list");

    assert_eq!(nav.symbol, "helper");
    let jumps: Vec<(String, i32)> = nav.sites.iter().map(|s| (s.path.clone(), s.line)).collect();
    // RECALL: both real call sites are named.
    assert!(
        jumps.contains(&("a.rs".to_string(), 3)),
        "missing a.rs:3 — {jumps:?}"
    );
    assert!(
        jumps.contains(&("b.rs".to_string(), 1)),
        "missing b.rs:1 — {jumps:?}"
    );
    // PRECISION, the measured free filter: the `use` import and the
    // declaration itself are not destinations.
    assert!(
        !jumps.iter().any(|(p, l)| p == "a.rs" && *l == 0),
        "offered the import — {jumps:?}"
    );
    assert!(
        !jumps.iter().any(|(p, _)| p == "lib.rs"),
        "offered the declaration — {jumps:?}"
    );
    assert_eq!(nav.sites.len(), 2, "{jumps:?}");
    assert_eq!(
        nav.dropped, 1,
        "the import should be counted as dropped, not hidden"
    );
    assert!(!nav.truncated);

    // A jump list without context is a list of numbers.
    let a = nav.sites.iter().find(|s| s.path == "a.rs").unwrap();
    assert_eq!(a.preview, "helper(1, 2)");
    assert_eq!(a.col, 4, "column should point AT the name, not past it");
}

#[tokio::test]
async fn an_unchanged_signature_does_not_fire() {
    // The gate that replaced a vacuous one: next-edit fires on
    // edit-settle with the cursor AT the edit, so "the user just
    // edited where the cursor is" is true by construction and could
    // never fail. Comparing against the last save can.
    let (dir, graph) = fixture().await;
    let cursor = DECL.find("b: usize").unwrap();
    assert_eq!(
        navigate(&graph, Some("lib.rs"), DECL, cursor, reader(dir.path())).await,
        Err(Decline::SignatureUnchanged)
    );
}

#[tokio::test]
async fn a_function_the_index_does_not_know_declines_by_name() {
    // 89% of M0's measured population was this class — a function that
    // did not exist at the parent commit, or a file that merely moved.
    // It has no callers to name and the lane must say so rather than
    // return an empty list that reads like "no work to do".
    let (dir, graph) = fixture().await;
    let src = "pub fn brand_new(a: usize, b: u8) {}\n";
    let cursor = src.find("b: u8").unwrap();
    assert_eq!(
        navigate(&graph, Some("lib.rs"), src, cursor, reader(dir.path())).await,
        Err(Decline::SymbolNotIndexed)
    );
}

#[tokio::test]
async fn editing_the_body_never_reaches_the_graph() {
    let (dir, graph) = fixture().await;
    let cursor = EDITED.find("a + b").unwrap();
    assert_eq!(
        navigate(&graph, Some("lib.rs"), EDITED, cursor, reader(dir.path())).await,
        Err(Decline::CursorNotInParameterList)
    );
}

#[tokio::test]
async fn an_overload_set_refuses_rather_than_offering_the_union() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lib.rs"), DECL).unwrap();
    let graph = ScipGraph::open_in_memory(CORPUS).unwrap();
    graph
        .ingest_symbols_and_refs(
            vec![
                sym("helper", QUAL, "lib.rs", 0, 2),
                sym(
                    "helper",
                    "rust-analyzer cargo e2e 0.1.0 lib/Other#helper().",
                    "lib.rs",
                    8,
                    9,
                ),
            ],
            vec![r#ref("helper", QUAL, "a.rs", 3, 10)],
        )
        .await
        .unwrap();
    let cursor = EDITED.find("c: u8").unwrap();
    assert_eq!(
        navigate(&graph, Some("lib.rs"), EDITED, cursor, reader(dir.path())).await,
        Err(Decline::AmbiguousSymbol)
    );
}

#[tokio::test]
async fn a_call_site_whose_line_moved_since_the_save_is_dropped_not_pointed_at() {
    // The index describes the last save. If a site's line no longer
    // names the symbol, jumping there lands the developer somewhere
    // arbitrary — worse than one entry fewer.
    let (dir, graph) = fixture().await;
    std::fs::write(
        dir.path().join("b.rs"),
        "pub fn two() -> usize {\n    0\n}\n",
    )
    .unwrap();
    let cursor = EDITED.find("c: u8").unwrap();
    let nav = navigate(&graph, Some("lib.rs"), EDITED, cursor, reader(dir.path()))
        .await
        .unwrap();
    assert_eq!(nav.sites.len(), 1);
    assert_eq!(nav.sites[0].path, "a.rs");
    assert_eq!(nav.dropped, 2, "the import and the moved line");
}
