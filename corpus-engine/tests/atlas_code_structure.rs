//! Integration coverage for the `structure_first` code-corpus
//! branch.
//!
//! The full end-to-end pipeline (recipe → code extraction → LanceDB
//! index → structure_first → atlas/atoms.json) is exercised by the
//! day-6 demo dry-run against the real workspace. This file pins the
//! pieces that are too narrow to be unit-tested cleanly but too
//! brittle to leave for the dry-run:
//!
//!   1. The code-extractor change captures Rust visibility and doc
//!      comments correctly. Atlas entity descriptions ride directly
//!      on these fields, so a regression here silently empties the
//!      atlas.
//!   2. The `metadata_looks_like_code` dispatch signature
//!      distinguishes code chunks from Wikipedia chunks. A regression
//!      here misroutes the strategy and the atlas comes out empty
//!      with no obvious error.
//!
//! Both checks are gated on the `treesitter` feature — the same
//! feature that brings in the code extractor and SCIP graph.

#![cfg(feature = "treesitter")]

use corpus_engine::enrichment::atlas::strategies::code_walk::metadata_looks_like_code;
use corpus_engine::extractors::code::CodeExtractor;

const RUST_FIXTURE: &str = "\
//! Crate-level docs.
//!
//! Second paragraph of the crate-level docs.

/// Public function with a one-line description.
pub fn documented_fn() -> usize {
    42
}

fn private_fn() {}

/// Multi-line docs.
/// Continues here.
pub struct Documented {
    pub field: i32,
}

pub struct Undocumented;

/// A trait carrying a doc comment.
pub trait Speaks {
    fn say(&self);
}
";

#[test]
fn code_extractor_captures_visibility_and_docs() {
    let ex = CodeExtractor::default();
    let chunks = ex
        .extract_file(RUST_FIXTURE, "src/lib.rs", 1_700_000_000)
        .expect("extract_file");
    let by_name: std::collections::HashMap<_, _> =
        chunks.iter().map(|c| (c.symbol_name.as_str(), c)).collect();

    let documented = by_name.get("documented_fn").expect("documented_fn missing");
    assert!(documented.is_public, "documented_fn must be flagged pub");
    assert_eq!(
        documented.doc_comment.as_deref(),
        Some("Public function with a one-line description.")
    );

    let private = by_name.get("private_fn").expect("private_fn missing");
    assert!(!private.is_public, "private_fn must not be flagged pub");
    assert!(private.doc_comment.is_none());

    let documented_struct = by_name.get("Documented").expect("Documented missing");
    assert!(documented_struct.is_public);
    assert_eq!(
        documented_struct.doc_comment.as_deref(),
        Some("Multi-line docs.\nContinues here.")
    );

    let undoc = by_name.get("Undocumented").expect("Undocumented missing");
    assert!(undoc.is_public);
    assert!(undoc.doc_comment.is_none());

    let speaks = by_name.get("Speaks").expect("Speaks missing");
    assert!(speaks.is_public);
    assert_eq!(
        speaks.doc_comment.as_deref(),
        Some("A trait carrying a doc comment.")
    );
}

#[test]
fn metadata_json_round_trips_visibility_and_docs() {
    let ex = CodeExtractor::default();
    let chunks = ex
        .extract_file(RUST_FIXTURE, "src/lib.rs", 0)
        .expect("extract_file");
    let documented = chunks
        .iter()
        .find(|c| c.symbol_name == "documented_fn")
        .expect("documented_fn missing");
    let json = documented.metadata_json();
    let obj = json.as_object().expect("metadata_json is an object");
    assert_eq!(obj.get("is_public").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        obj.get("doc_comment").and_then(|v| v.as_str()),
        Some("Public function with a one-line description.")
    );
    // Existing keys must remain untouched (back-compat for the
    // typed-column promotion in the LanceDB insert path).
    assert_eq!(
        obj.get("symbol_name").and_then(|v| v.as_str()),
        Some("documented_fn")
    );
    assert_eq!(obj.get("language").and_then(|v| v.as_str()), Some("rust"));
}

#[test]
fn metadata_dispatch_signature_distinguishes_corpus_kinds() {
    let code_meta = r#"{"symbol_name":"foo","symbol_kind":"function","file_path":"src/lib.rs","language":"rust","is_public":true}"#;
    assert!(
        metadata_looks_like_code(code_meta),
        "code chunk metadata should be recognised"
    );

    // Wikipedia-style metadata uses section_path + section_type +
    // outgoing_links — none of the code-specific keys.
    let wiki_meta =
        r#"{"section_path":["Lead"],"section_type":"lead","outgoing_links":[],"section_depth":0}"#;
    assert!(
        !metadata_looks_like_code(wiki_meta),
        "Wikipedia chunk metadata should not be misrouted"
    );

    // Blob with no metadata at all (legacy chunks predating typed
    // metadata) should not be misrouted to code.
    let empty = r#"{}"#;
    assert!(!metadata_looks_like_code(empty));

    // Garbage should not panic and should not match.
    assert!(!metadata_looks_like_code("not json"));
}

#[test]
fn private_items_default_off_in_extractor_metadata() {
    // The extractor itself does NOT filter — it always emits private
    // items, with `is_public = false`. The atlas walker is what
    // applies the `--include-private` filter. Pinning the contract
    // here so a future refactor doesn't quietly start dropping
    // private items at the extractor layer (which would break code
    // search + reading desk).
    let ex = CodeExtractor::default();
    let chunks = ex
        .extract_file(RUST_FIXTURE, "src/lib.rs", 0)
        .expect("extract_file");
    let private_present = chunks
        .iter()
        .any(|c| c.symbol_name == "private_fn" && !c.is_public);
    assert!(
        private_present,
        "extractor must emit private items so other consumers see them"
    );
}
