// SPDX-License-Identifier: AGPL-3.0-or-later
//! Census: sites that decide behaviour by comparing `enrichment_type` against
//! a string literal. ARCH §10.7 step 4 — "pin the count in a census that only
//! shrinks."
//!
//! Before 2026-09-03 there were FIVE, in three crates, and they gave four
//! different answers for an unknown type (ingest ran `field_model`, the
//! health stamp said "expected", drift said "unverifiable", the desktop's
//! "enrich now" ran `tiered`, boot-resume said "no"). They collapsed into
//! `corpus_engine::enrichment::pass::EnrichmentPassRegistry`, and the only
//! place the literal ids may live is that module's `pub const`s. A site that
//! needs to name a pass compares against the const.
//!
//! This test greps rather than resolves because the smell is textual: a
//! `"atlas"` next to `enrichment_type` is the thing §2.1 names, whatever
//! type it has.

use std::path::{Path, PathBuf};

/// Production source roots the census sweeps. Tests are excluded by
/// `is_test_line` below, not by path, because `#[cfg(test)]` modules live
/// inline in this workspace.
fn roots() -> Vec<PathBuf> {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let mut roots = vec![ws.join("corpus-engine/src")];
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

/// A line that COMPARES `enrichment_type` against a quoted literal:
/// `enrichment_type == "x"`, `!= "x"`, `== Some("x")`, a `match
/// enrichment_type.as_str()` arm, or `matches!(enrichment_type…, "x" | …)`.
fn is_literal_switch(line: &str, prev: &str) -> bool {
    let t = line.trim();
    let mentions = t.contains("enrichment_type");
    let cmp = t.contains("== \"") || t.contains("!= \"") || t.contains("Some(\"");
    if mentions && cmp {
        return true;
    }
    if mentions && t.contains("matches!(") && t.contains('"') {
        return true;
    }
    // `match x.enrichment_type.as_str() {` followed by literal arms — the
    // arm line itself does not mention the field, so look one line back.
    if prev.trim().contains("enrichment_type.as_str()") && t.starts_with('"') {
        return true;
    }
    false
}

fn is_test_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("assert") || t.starts_with("//")
}

#[test]
fn enrichment_type_literal_switches_live_only_in_the_registry() {
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
        let mut prev = "";
        for (i, line) in src.lines().enumerate() {
            if !is_test_line(line) && is_literal_switch(line, prev) {
                hits.push(format!("{}:{}: {}", file.display(), i + 1, line.trim()));
            }
            prev = line;
        }
    }

    // The census target. Every entry that remains here must say what would
    // remove it. Today: nothing — the registry's `pub const` ids are
    // definitions, not comparisons, so they do not match.
    const ALLOWED: &[&str] = &[];

    let unexpected: Vec<&String> = hits
        .iter()
        .filter(|h| !ALLOWED.iter().any(|a| h.contains(a)))
        .collect();
    assert!(
        unexpected.is_empty(),
        "{} site(s) compare `enrichment_type` against a string literal. Resolve the \
         pass through `EnrichmentPassRegistry` and ask it, or compare against a \
         `corpus_engine::enrichment::pass::*` const:\n  {}",
        unexpected.len(),
        unexpected
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// The positive control (§18.1): the matcher must recognise each of the five
/// shapes the census exists to catch, or a green run means nothing.
#[test]
fn the_matcher_sees_every_shape_it_was_built_for() {
    let shapes = [
        ("if enrichment_config.enrichment_type == \"tiered\" {", ""),
        ("if enrichment.enrichment_type != \"investigation\" {", ""),
        ("enrichment_type == Some(\"tiered\")", ""),
        (
            "!matches!(enrichment_type, \"investigation\" | \"atlas\")",
            "",
        ),
        (
            "\"field_model\" => Some(\"field_skeleton.json\"),",
            "match self.enrichment_type.as_str() {",
        ),
    ];
    for (line, prev) in shapes {
        assert!(is_literal_switch(line, prev), "matcher missed: {line}");
    }
    // And it stays quiet on the shapes that are allowed.
    assert!(!is_literal_switch("pub const ATLAS: &str = \"atlas\";", ""));
    assert!(!is_literal_switch(
        "if enrichment.enrichment_type != pass::FIELD_MODEL {",
        ""
    ));
    assert!(is_test_line(
        "        assert_eq!(enr.enrichment_type, \"atlas\");"
    ));
}
