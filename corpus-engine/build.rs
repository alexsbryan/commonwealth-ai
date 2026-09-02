// SPDX-License-Identifier: AGPL-3.0-or-later
//! Build-time vendoring of the canonical `sovereign-recipes/` tree into
//! `OUT_DIR`.
//!
//! `sovereign-recipes/` (the sibling workspace dir) is the SINGLE SOURCE
//! OF TRUTH for corpus recipes, the registry catalog, and the generated
//! Vital-Articles data lists. corpus-engine bundles them at compile time
//! by copying into the per-build `OUT_DIR` Cargo provides, then
//! `include_str!`/`include_bytes!`-ing from there (see
//! `src/recipe_builtin.rs`, `src/registry.rs`, `src/filters/assets.rs`).
//!
//! There is NO second checked-in copy of recipes in this crate: the
//! bundle is a pure function of `sovereign-recipes/`, regenerated every
//! build and invalidated by `cargo:rerun-if-changed`, so the canonical
//! tree and the bundled fallback cannot drift.
//!
//! What gets vendored:
//!   - every `sovereign-recipes/<id>/recipe.toml` → `OUT_DIR/recipes/<id>/recipe.toml`
//!   - `sovereign-recipes/registry.toml`          → `OUT_DIR/registry_snapshot.toml`
//!   - the Vital-Articles data lists              → `OUT_DIR/<asset>`
//!
//! Standalone clones (corpus-engine built without the sibling repo
//! present, e.g. air-gapped CI): set
//! `CORPUS_ENGINE_RECIPES_DIR=<path-to-sovereign-recipes>` to point the
//! vendoring at an alternate copy of the tree. `CORPUS_ENGINE_DATA_DIR`
//! remains a narrower escape hatch for the data lists alone (a flat dir
//! holding the `BUNDLED_ASSETS` filenames).

use std::path::{Path, PathBuf};

/// Data-list filenames consumed via `include_bytes!` in
/// `src/filters/assets.rs`. Any new bundled asset added there must also
/// appear here, otherwise the `include_bytes!` macro fails at the next
/// build with a missing-file error.
const BUNDLED_ASSETS: &[&str] = &[
    "vital_articles_l1.txt",
    "vital_articles_l2.txt",
    "vital_articles_l3.txt",
    "vital_articles_l4.txt",
    "vital_articles_l5.txt",
];

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    // The canonical sovereign-recipes tree. Sibling of corpus-engine in
    // the workspace; overridable for standalone clones.
    let recipes_root: PathBuf = std::env::var("CORPUS_ENGINE_RECIPES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            manifest_dir
                .parent()
                .expect("corpus-engine manifest must have a parent dir")
                .join("sovereign-recipes")
        });

    if !recipes_root.is_dir() {
        panic!(
            "sovereign-recipes tree not found at {}.\n  \
             Build inside the commonwealth-ai workspace, or set \
             CORPUS_ENGINE_RECIPES_DIR to a sovereign-recipes checkout.",
            recipes_root.display()
        );
    }

    vendor_recipes(&recipes_root, &out_dir.join("recipes"));
    // Ontology-v1 recipe templates (`svrn recipe new --ontology <name>`), same
    // shape one level down: `_templates/ontology-v1/<name>/recipe.toml`.
    vendor_recipes(
        &recipes_root.join("_templates").join("ontology-v1"),
        &out_dir
            .join("recipes")
            .join("_templates")
            .join("ontology-v1"),
    );
    vendor_registry(&recipes_root, &out_dir);
    vendor_data_assets(&recipes_root, &out_dir);

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CORPUS_ENGINE_RECIPES_DIR");
    println!("cargo:rerun-if-env-changed=CORPUS_ENGINE_DATA_DIR");
}

/// Copy every `<id>/recipe.toml` directly under `recipes_root` into
/// `dest_root/<id>/recipe.toml`. `recipe_builtin.rs` and
/// `recipe_templates.rs` `include_str!` the bundled subset from there; extra
/// (local-only / example) recipes are copied too but simply never referenced.
fn vendor_recipes(recipes_root: &Path, dest_root: &Path) {
    let entries = std::fs::read_dir(recipes_root)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", recipes_root.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if !path.is_dir() {
            continue;
        }
        let recipe = path.join("recipe.toml");
        if !recipe.is_file() {
            continue;
        }
        let id = path.file_name().expect("recipe dir has a name");
        let dest_dir = dest_root.join(id);
        std::fs::create_dir_all(&dest_dir)
            .unwrap_or_else(|e| panic!("create_dir_all {}: {e}", dest_dir.display()));
        let dest = dest_dir.join("recipe.toml");
        std::fs::copy(&recipe, &dest)
            .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", recipe.display(), dest.display()));
        println!("cargo:rerun-if-changed={}", recipe.display());
    }
}

/// Copy the canonical registry catalog into the bundled-snapshot slot.
fn vendor_registry(recipes_root: &Path, out_dir: &Path) {
    let src = recipes_root.join("registry.toml");
    let dest = out_dir.join("registry_snapshot.toml");
    std::fs::copy(&src, &dest)
        .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", src.display(), dest.display()));
    println!("cargo:rerun-if-changed={}", src.display());
}

/// Copy the Vital-Articles data lists into `OUT_DIR`. Resolution order:
///   1. `CORPUS_ENGINE_DATA_DIR` — flat dir of the asset files (air-gapped
///      escape hatch).
///   2. `<recipes_root>/wikipedia/data/` — the common workspace case.
fn vendor_data_assets(recipes_root: &Path, out_dir: &Path) {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(env_dir) = std::env::var("CORPUS_ENGINE_DATA_DIR") {
        candidates.push(PathBuf::from(env_dir));
    }
    candidates.push(recipes_root.join("wikipedia").join("data"));

    for asset in BUNDLED_ASSETS {
        let src = match find_asset(asset, &candidates) {
            Some(p) => p,
            None => {
                eprintln!("cargo:warning=corpus-engine build.rs: '{asset}' not found in any of:");
                for c in &candidates {
                    eprintln!("cargo:warning=  - {}", c.join(asset).display());
                }
                eprintln!(
                    "cargo:warning=  Regenerate via sovereign-recipes/wikipedia/scripts/build_vital_articles.py"
                );
                panic!("required bundled asset '{asset}' missing");
            }
        };
        let dest = out_dir.join(asset);
        std::fs::copy(&src, &dest)
            .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", src.display(), dest.display()));
        println!("cargo:rerun-if-changed={}", src.display());
    }
}

fn find_asset(name: &str, candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}
