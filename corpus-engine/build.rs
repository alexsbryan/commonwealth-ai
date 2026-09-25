// SPDX-License-Identifier: AGPL-3.0-or-later
//! Build-time vendoring of the canonical `sovereign-recipes/` tree into
//! `OUT_DIR`.
//!
//! `sovereign-recipes/` (the sibling workspace dir) is the SINGLE SOURCE
//! OF TRUTH for corpus recipes and the registry catalog. corpus-engine
//! bundles them at compile time
//! by copying into the per-build `OUT_DIR` Cargo provides, then
//! `include_str!`-ing from there.
//!
//! There is NO second checked-in copy of recipes in this crate: the
//! bundle is a pure function of `sovereign-recipes/`, regenerated every
//! build and invalidated by `cargo:rerun-if-changed`, so the canonical
//! tree and the bundled fallback cannot drift.
//!
//! What gets vendored:
//!   - every `sovereign-recipes/<id>/recipe.toml` → `OUT_DIR/recipes/<id>/recipe.toml`
//!   - `sovereign-recipes/registry.toml`          → `OUT_DIR/registry_snapshot.toml`
//!   - `sovereign-recipes/schema/recipe_schema_descriptor.json`
//!                                                → `OUT_DIR/recipe_schema_descriptor.json`
//!
//! Standalone clones (corpus-engine built without the sibling repo
//! present, e.g. air-gapped CI): set
//! `CORPUS_ENGINE_RECIPES_DIR=<path-to-sovereign-recipes>` to point the
//! vendoring at an alternate copy of the tree.

use std::path::{Path, PathBuf};

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
    let templates_root = recipes_root.join("_templates").join("ontology-v1");
    vendor_recipes(
        &templates_root,
        &out_dir
            .join("recipes")
            .join("_templates")
            .join("ontology-v1"),
    );
    // A NEW template directory must re-run this script; the per-file lines
    // `vendor_recipes` emits watch only files that already existed, so a new
    // `template!("x")` row would fail with "couldn't read" until `build.rs`
    // itself changed. The dir is ten small recipes, so the recursive scan is
    // cheap; the recipes root is NOT watched this way because it would
    // rebuild the crate on every doc edit under sovereign-recipes/.
    println!("cargo:rerun-if-changed={}", templates_root.display());
    vendor_registry(&recipes_root, &out_dir);
    vendor_descriptor(&recipes_root, &out_dir);

    println!("cargo:rerun-if-changed=build.rs");
    // The recipes ROOT too, not only each recipe.toml found under it: a
    // directory's mtime changes when a subdirectory is added, and without
    // this a NEW `<id>/recipe.toml` was not vendored until something else
    // forced a rebuild — the registry snapshot picked the entry up while
    // `bundled_recipe_covers_every_snapshot_entry` found no recipe for it
    // (2026-09-11, federalist-starter).
    println!("cargo:rerun-if-changed={}", recipes_root.display());
    println!("cargo:rerun-if-env-changed=CORPUS_ENGINE_RECIPES_DIR");
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

        // A recipe may ship an exhaustive ground-truth manifest beside it
        // (`wessex-hoard/truth.json`). Vendored on the same path so a test can
        // `include_str!` the ONE copy instead of re-typing its rows in Rust.
        let truth = path.join("truth.json");
        if truth.is_file() {
            let dest = dest_dir.join("truth.json");
            std::fs::copy(&truth, &dest)
                .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", truth.display(), dest.display()));
            println!("cargo:rerun-if-changed={}", truth.display());
        }
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

/// Copy the generated recipe variant-catalog descriptor into `OUT_DIR`.
///
/// The descriptor is produced by `tests/main/recipe_schema.rs` from the recipe
/// AST; vendoring it here (beside the registry snapshot) is what lets
/// `src/recipe_schema.rs` embed it with `env!("OUT_DIR")` rather than climbing
/// three levels out of the crate root to the `sovereign-recipes/` tree.
fn vendor_descriptor(recipes_root: &Path, out_dir: &Path) {
    let src = recipes_root
        .join("schema")
        .join("recipe_schema_descriptor.json");
    let dest = out_dir.join("recipe_schema_descriptor.json");
    std::fs::copy(&src, &dest)
        .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", src.display(), dest.display()));
    println!("cargo:rerun-if-changed={}", src.display());
}
