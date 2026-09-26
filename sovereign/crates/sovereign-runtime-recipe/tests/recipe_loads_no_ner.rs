// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe loads no NER model (phase-b pb-serving-ner).
//!
//! It used to: `load_gliner` took the v1 default on every host, so a daemon
//! that had already loaded its own GLiNER (honouring
//! `SOVEREIGN_GLINER_MODEL_ID`) held a second one. NER is a host port now,
//! `RecipeInputs::ner`, and the daemon hands in its served NER kind's handle.
//! A restored loader and its dependency compile clean, so this reads the
//! crate's manifest and source.
//!
//! Watched to fail: restoring a `load_gliner` that calls
//! `GlinerExtractor::new_default()`, with the `sovereign-gliner` dependency.

use std::path::Path;

fn read(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn the_recipe_links_and_loads_no_ner_model() {
    let dep = ["sovereign", "-gliner"].concat();
    let needles = [
        ["sovereign", "_gliner::"].concat(),
        ["Gliner", "Extractor::new"].concat(),
        ["load_labeled", "_extractor("].concat(),
    ];
    let mut found = Vec::new();
    for line in read("Cargo.toml").lines() {
        if line.trim_start().starts_with(&dep) {
            found.push(format!("Cargo.toml: {}", line.trim()));
        }
    }
    let src = read("src/lib.rs");
    assert!(src.lines().count() > 100, "src/lib.rs read short");
    for (n, line) in src.lines().enumerate() {
        let code = line.trim_start();
        if code.starts_with("//") {
            continue;
        }
        if let Some(needle) = needles.iter().find(|needle| code.contains(needle.as_str())) {
            found.push(format!("src/lib.rs:{}: `{needle}`", n + 1));
        }
    }
    assert!(
        found.is_empty(),
        "the recipe links or loads a NER model of its own; read the host's \
         `RecipeInputs::ner` instead:\n  {}",
        found.join("\n  ")
    );
}
