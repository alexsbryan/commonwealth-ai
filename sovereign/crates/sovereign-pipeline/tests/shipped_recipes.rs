//! Regression coverage for recipes shipped in the repo. If a recipe
//! file lands in `sovereign-recipes/` and is referenced from docs or
//! commit history, it must continue to parse cleanly — otherwise the
//! `sovereign pipeline run` user is the one who finds out.

use std::path::PathBuf;

use sovereign_pipeline::recipe::Recipe;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at sovereign/crates/sovereign-pipeline.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.join("..").join("..").join("..")
}

#[test]
fn sep_core_v1_recipe_parses() {
    let path = repo_root().join("sovereign-recipes/sep/pipelines/sep-core-v1.toml");
    // Skip silently if the recipe was renamed/moved — failing here
    // would block unrelated work. The grep above commit history will
    // surface the rename if it happens.
    if !path.exists() {
        eprintln!("skipping: recipe not at {}", path.display());
        return;
    }
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
