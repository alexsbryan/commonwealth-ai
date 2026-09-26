// SPDX-License-Identifier: AGPL-3.0-or-later
//! NER loads once per daemon process, through the served NER kind
//! (`sovereign_compute::ner::served_ner`), and neither svrn crate that
//! composes a daemon turn links sovereign-gliner (phase-b pb-serving-ner).
//!
//! Before that row a daemon held two GLiNER models: its own boot load, and
//! runtime-recipe's `load_gliner`, which always took the v1 default while the
//! boot load honoured `SOVEREIGN_GLINER_MODEL_ID`. The compiler cannot keep
//! that from coming back — a restored loader and its dependency compile
//! clean — so this reads the two crates' manifests and sources.
//!
//! Watched red by restoring runtime-recipe's `load_gliner` with its
//! dependency, and by a daemon load that skips the kind
//! (`sovereign_gliner::load_gliner_extractor()` in `bootstrap.rs`).

use std::path::{Path, PathBuf};

/// The svrn crates the census covers: the daemon, and the recipe it hands
/// the NER handle to.
fn crates() -> Vec<PathBuf> {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    vec![here.to_path_buf(), here.join("../sovereign-runtime-recipe")]
}

/// Loads and links of GLiNER, assembled so this file does not match itself.
fn needles() -> Vec<String> {
    let crate_path = ["sovereign", "_gliner::"].concat();
    vec![
        crate_path,
        ["Gliner", "Extractor::new"].concat(),
        ["Gliner2", "Extractor::new"].concat(),
        ["load_labeled", "_extractor("].concat(),
        ["LazyGliner", "Extractor"].concat(),
    ]
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_svrn_crate_links_or_loads_gliner() {
    let dep = ["sovereign", "-gliner"].concat();
    let needles = needles();
    let mut found = Vec::new();
    let mut scanned = 0usize;
    for krate in crates() {
        let manifest = std::fs::read_to_string(krate.join("Cargo.toml"))
            .unwrap_or_else(|e| panic!("read {}/Cargo.toml: {e}", krate.display()));
        for line in manifest.lines() {
            if !line.trim_start().starts_with('#') && line.trim_start().starts_with(&dep) {
                found.push(format!("{}/Cargo.toml: {}", krate.display(), line.trim()));
            }
        }
        let mut files = Vec::new();
        rust_files(&krate.join("src"), &mut files);
        rust_files(&krate.join("tests"), &mut files);
        for file in files {
            let text = std::fs::read_to_string(&file)
                .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
            scanned += 1;
            for (n, line) in text.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                if let Some(needle) = needles.iter().find(|needle| code.contains(needle.as_str())) {
                    found.push(format!("{}:{}: `{needle}`", file.display(), n + 1));
                }
            }
        }
    }
    assert!(
        scanned > 50,
        "the census read only {scanned} files — it is not looking"
    );
    assert!(
        found.is_empty(),
        "GLiNER is linked or loaded outside the NER served kind; a daemon turn would hold a \
         second model. Take the handle from `sovereign_compute::ner::served_ner` (the daemon) \
         or `RecipeInputs::ner` (the recipe):\n  {}",
        found.join("\n  ")
    );
}
