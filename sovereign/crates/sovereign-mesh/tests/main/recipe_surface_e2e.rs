// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe-registry write and read the desktop stops doing in-process
//! (thin-desktop order, 2026-09-11, `recipe_http`): importing an authored
//! recipe into the local registry and reading a recipe's `[parameters]`.
//!
//! Against a REAL daemon whose engine owns a temp recipes dir — the fault
//! these routes exist to prevent is the desktop writing a recipe into a
//! directory the daemon's registry does not resolve through.

use std::sync::Arc;

use corpus_engine::{CorpusEngine, EmbedFn};
use sovereign_core::setup_config::SetupConfig;
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::recipe_http::recipe_router;

use crate::common;
use crate::common::spawn_router;

const EMBED_DIM: usize = 8;

/// A recipe that passes offline validation (no reachability check on the
/// download URL) and declares two parameters, one with a default.
const RECIPE_TOML: &str = r#"
[corpus]
id = "author-test"
name = "Author test"
description = "an authored recipe"
license = "Public Domain"
size_compressed_gb = 0.001
size_indexed_gb = 0.001

[parameters.since]
type = "date"
required = false
description = "Only records after this date."
default = "2020-01-01"

[parameters.tickers]
type = "list"
required = true
description = "Tickers to include."

[acquire]
type = "bulk_download"
url = "https://example.invalid/author-test.txt"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
max_chars = 2048
overlap_chars = 256

[index]
fts = true
vector = true
"#;

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.0_f32; EMBED_DIM]) }))
}

/// Fixture construction: a failure here is a broken fixture, not a
/// finding, and must abort loudly (`meshapp_surface_e2e`'s allow).
#[allow(clippy::unwrap_used)]
async fn build_daemon() -> (Arc<EmbeddedDaemon>, tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&indexes).unwrap();
    std::fs::create_dir_all(&recipes).unwrap();
    let engine = Arc::new(
        CorpusEngine::new(recipes.clone(), indexes, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        common::desktop_services_with_engine(engine),
    );
    (daemon, tmp, recipes)
}

/// A valid recipe lands under the DAEMON's recipes dir as
/// `<id>/recipe.toml` with a `registry.toml` entry carrying its sha256,
/// and is then resolvable by the parameters route through the same
/// registry — the round trip the install form depends on.
#[tokio::test]
async fn import_writes_the_daemons_registry_and_parameters_read_it_back() {
    let (daemon, _tmp, recipes) = build_daemon().await;
    let addr = spawn_router(recipe_router(Arc::clone(&daemon))).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{addr}/internal/corpus/recipes/import"))
        .json(&serde_json::json!({ "toml_text": RECIPE_TOML }))
        .send()
        .await
        .expect("recipe_router reachable");
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    assert_eq!(status, 200, "{body:#?}");
    assert_eq!(body["success"], true, "a valid recipe imports: {body:#?}");
    assert_eq!(body["corpus_id"], "author-test");
    let landed = recipes.join("author-test").join("recipe.toml");
    assert_eq!(
        body["recipe_path"],
        landed.display().to_string(),
        "the path answered is the daemon's, not the client's"
    );
    assert_eq!(std::fs::read_to_string(&landed).unwrap(), RECIPE_TOML);
    let registry = std::fs::read_to_string(recipes.join("registry.toml")).unwrap();
    assert!(
        registry.contains("id = \"author-test\"") && registry.contains("sha256 = \""),
        "the local registry carries the entry and its digest:\n{registry}"
    );

    let resp = client
        .get(format!(
            "http://{addr}/internal/corpus/recipes/author-test/parameters"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let schema: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(schema["corpus_id"], "author-test");
    let params = schema["parameters"].as_array().unwrap();
    assert_eq!(params.len(), 2);
    let since = params.iter().find(|p| p["name"] == "since").unwrap();
    assert_eq!(since["kind"], "date");
    assert_eq!(since["required"], false);
    assert_eq!(
        since["default"], "2020-01-01",
        "the TOML default crosses as JSON"
    );
    let tickers = params.iter().find(|p| p["name"] == "tickers").unwrap();
    assert_eq!(tickers["kind"], "list");
    assert_eq!(tickers["required"], true);
    assert_eq!(tickers["default"], serde_json::Value::Null);
}

/// A recipe that parses but fails validation is a 200 with
/// `success: false` and the errors — the form's own verdict shape — and
/// nothing is written; a body that is not a recipe is a 400.
#[tokio::test]
async fn import_refuses_an_invalid_recipe_without_writing_and_400s_on_non_toml() {
    let (daemon, _tmp, recipes) = build_daemon().await;
    let addr = spawn_router(recipe_router(Arc::clone(&daemon))).await;
    let client = reqwest::Client::new();

    // No `[acquire]`/`[extract]`/`[chunk]` — parses as a recipe? It must
    // not: the schema requires them, so this is the 400 arm.
    let resp = client
        .post(format!("http://{addr}/internal/corpus/recipes/import"))
        .json(&serde_json::json!({ "toml_text": "[corpus]\nid = \"broken\"\n" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400, "not a recipe is a refusal");
    assert!(
        !recipes.join("broken").exists(),
        "nothing is written on refusal"
    );

    // Parameters for a recipe nobody imported: a 404 naming it.
    let resp = client
        .get(format!(
            "http://{addr}/internal/corpus/recipes/no-such-recipe/parameters"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("no-such-recipe"));
}
