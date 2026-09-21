// SPDX-License-Identifier: AGPL-3.0-or-later
//! Regression coverage for recipes shipped in the repo. If a recipe
//! file lands in `sovereign-recipes/` and is referenced from docs or
//! commit history, it must continue to parse cleanly — otherwise the
//! `sovereign pipeline run` user is the one who finds out.

use std::path::PathBuf;

use sovereign_pipeline::recipe::Recipe;

/// Names the canonical recipes tree — the SAME knob `corpus-engine/build.rs`
/// vendors from, so the two readings cannot point at different trees.
const RECIPES_DIR_ENV: &str = "CORPUS_ENGINE_RECIPES_DIR";

/// The recipes tree, from a knob rather than a climb out of the crate root: a
/// third party who lifts this package carries this test and has no such tree
/// (boundary-gate rule 3c). Absence panics naming the knob.
fn recipes_root() -> PathBuf {
    let raw = std::env::var(RECIPES_DIR_ENV).unwrap_or_else(|_| {
        panic!("{RECIPES_DIR_ENV} is unset — point it at a sovereign-recipes checkout")
    });
    let path = PathBuf::from(raw);
    assert!(
        path.is_dir(),
        "{RECIPES_DIR_ENV} points at {} — no sovereign-recipes tree there",
        path.display()
    );
    path
}

#[test]
fn sep_core_v1_recipe_parses() {
    let path = recipes_root().join("sep/pipelines/sep-core-v1.toml");
    // A moved recipe FAILS here. Skipping was a gate that could not fail (§18.1).
    assert!(
        path.is_file(),
        "sep-core-v1 not at {} — the recipe moved, or {RECIPES_DIR_ENV} points at \
         the wrong tree; repoint the knob or update this path, do not skip",
        path.display()
    );
    let recipe = Recipe::load(&path).expect("sep-core-v1 must parse");
    assert_eq!(recipe.recipe.id, "sep-core-v1");
    assert!(
        recipe.enrich.command.contains("sep-ingest")
            && recipe.enrich.command.contains("enrich build"),
        "command should chain sep-ingest + enrich build, got: {}",
        recipe.enrich.command
    );
    // The default source is the corpus-enumerate command so the
    // recipe works without a curated slug file. We don't execute
    // it here (would require the SEP parquet to be acquired);
    // structural check is enough.
    match recipe.source {
        sovereign_pipeline::recipe::Source::Command { command } => {
            assert!(
                command.contains("sep-ingest") && command.contains("--list"),
                "default source should enumerate via `sep-ingest --list`, got: {command}"
            );
        }
        other => panic!("expected command source by default, got {other:?}"),
    }
}
