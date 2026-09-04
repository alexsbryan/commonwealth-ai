// SPDX-License-Identifier: AGPL-3.0-or-later
//! Census: declarations of the `atlas/atoms.json` shape. ARCH §10.7 step 4 —
//! "pin the count in a census that only shrinks" — and the count Step 2 of
//! the enrichment-as-plugin plan owes, because a carve-out is mass-neutral by
//! construction (`DECOMPOSITION.md`: "owes a count that went down").
//!
//! On 2026-09-03 there were FOUR: `AtomsFile` itself, a private
//! `AtomsFile { atoms: Vec<RawAtom> }` in `corpus_scrub_cmd.rs`, an
//! `AtomsFileLite` in `bench_cmd/scaffold.rs`, and an untyped
//! `serde_json::Value` walk in `sovereign-core`'s `anchoring.rs` that
//! re-derived the schema by key name (and asked for a `statement` key no
//! atom kind has). Now there is one, here, and every reader names it.

use std::path::{Path, PathBuf};

fn roots() -> Vec<PathBuf> {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let mut roots = vec![
        ws.join("corpus-engine/src"),
        ws.join("corpus-engine-vocab/src"),
    ];
    for entry in std::fs::read_dir(ws.join("sovereign/crates")).unwrap() {
        let src = entry.unwrap().path().join("src");
        if src.is_dir() {
            roots.push(src);
        }
    }
    roots
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A struct declaration that is the atoms-file shape under another name:
/// `struct AtomsFile`, `struct AtomsFile<Suffix>` (the `Lite` lookalike), or
/// `struct RawAtom` (the untyped envelope). `DocToAtomsFile` is a different
/// artifact (`doc_to_atoms.json`) and is deliberately not matched.
fn is_atoms_file_decl(line: &str) -> bool {
    let t = line.trim_start();
    let t = t.strip_prefix("pub ").unwrap_or(t);
    let t = t.strip_prefix("pub(crate) ").unwrap_or(t);
    let Some(rest) = t.strip_prefix("struct ") else {
        return false;
    };
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    name == "RawAtom" || name.starts_with("AtomsFile")
}

#[test]
fn the_atoms_json_shape_is_declared_exactly_once() {
    let mut files = Vec::new();
    for root in roots() {
        rs_files(&root, &mut files);
    }
    assert!(
        files.len() > 100,
        "census swept only {} files — wrong root?",
        files.len()
    );

    let mut hits = Vec::new();
    for file in &files {
        let Ok(src) = std::fs::read_to_string(file) else {
            continue;
        };
        for (i, line) in src.lines().enumerate() {
            if is_atoms_file_decl(line) {
                hits.push(format!("{}:{}: {}", file.display(), i + 1, line.trim()));
            }
        }
    }

    let home = "corpus-engine-vocab/src/atoms.rs";
    assert_eq!(
        hits.len(),
        1,
        "the atoms.json shape must be declared once, in `{home}`; found:\n  {}",
        hits.join("\n  ")
    );
    assert!(hits[0].contains(home), "{}", hits[0]);
}

/// Positive control (§18.1): the matcher recognises each deleted lookalike.
#[test]
fn the_matcher_sees_the_three_shapes_that_were_deleted() {
    assert!(is_atoms_file_decl("struct AtomsFile {"));
    assert!(is_atoms_file_decl("struct AtomsFileLite {"));
    assert!(is_atoms_file_decl("struct RawAtom {"));
    assert!(is_atoms_file_decl("pub struct AtomsFile {"));
    assert!(!is_atoms_file_decl("pub struct AtomEnvelope {"));
    assert!(!is_atoms_file_decl("struct EdgesFile {"));
    assert!(!is_atoms_file_decl("pub struct DocToAtomsFile {"));
}
