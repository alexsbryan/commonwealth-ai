// SPDX-License-Identifier: AGPL-3.0-or-later
//! Live end-to-end check for [`HttpRecipeTester`] against a running OICP daemon.
//!
//! `#[ignore]`d so the normal `cargo test` gate stays hermetic; run it against
//! a live daemon (v0.4, corpus-engine present) with:
//!
//! ```text
//! cargo test -p sovereign-recipe-author --test live_recipe_test -- --ignored --nocapture
//! ```
//!
//! It exercises the REAL client path — reqwest POST → `RecipeTestReport` parse →
//! `outcome_from_wire` — against the daemon's `/oicp/v1/recipe/test` (§5.4), so
//! the studio bundle's recipe-testing surface is proven without linking
//! corpus-engine.

use sovereign_contracts::recipe::testing::{RecipeTestParams, RecipeTester};
use sovereign_recipe_author::HttpRecipeTester;

/// A structurally-valid, parameterless recipe. Validate-only (`sample_size 0`,
/// `offline`) needs no reachable source, so the acquire path is never taken.
const FIXTURE: &str = r#"
[corpus]
id = "studio-http-tester-fixture"
name = "Studio HTTP tester fixture"
description = "A trivially-valid recipe for the live recipe-test e2e."
license = "private"
mesh_sharing = false

[acquire]
type = "local_file"
path = "/tmp/does-not-need-to-exist-for-validate-only.md"

[extract]
type = "markdown"

[chunk]
type = "passthrough"

[index]
"#;

#[tokio::test]
#[ignore = "requires a live OICP v0.4 daemon at 127.0.0.1:9741"]
async fn http_tester_validate_only_against_live_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recipe.toml");
    std::fs::write(&path, FIXTURE).unwrap();

    let tester = HttpRecipeTester::new("http://127.0.0.1:9741", None);
    let params = RecipeTestParams {
        sample_size: 0, // validate-only
        offline: true,
        ..Default::default()
    };

    let outcome = tester
        .test(&path, &params)
        .await
        .expect("live recipe-test should return a report (is the daemon up?)");

    // A structurally-valid recipe validates without errors.
    assert!(
        outcome.validation.errors.is_empty(),
        "unexpected validation errors: {:?}",
        outcome.validation.errors
    );
    // Validate-only never chunks, so the end-to-end `passed`/`ok` verdict is
    // false — that's correct, not a failure of the client.
    assert!(
        !outcome.passed,
        "validate-only should not report end-to-end pass"
    );
    eprintln!(
        "live recipe-test OK — errors={} warnings={} extraction={:?} passed={}",
        outcome.validation.errors.len(),
        outcome.validation.warnings.len(),
        outcome.extraction.is_some(),
        outcome.passed
    );
}
