// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine half of install carrying its recipe (ADDRESSED_TEXT §5.6),
//! through `&dyn IngestPort`: the daemon's `oicp_install_recipe_e2e` drives a
//! double, and what the engine does with the same ask is proven here.
//!
//! - a recipe that does not load, or names another corpus, is
//!   `InstallRefusal::InvalidRecipe` from the one load boundary
//!   (`Recipe::from_toml`), before anything is written;
//! - a recipe that loads is registered where every later reader resolves it
//!   by id, ingested to a canonical, and stamped with the sha256 of its text.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use corpus_engine::CorpusEngine;
use corpus_index::corpus::{recipe_sha256, Corpus};
use corpus_index::ingest_port::daemon::{IngestPort, InstallRefusal};
use corpus_index::types::EmbedFn;

fn embed() -> EmbedFn {
    Arc::new(|text: &str| {
        let mut v = vec![0.0_f32; 8];
        for (i, b) in text.bytes().enumerate() {
            v[i % 8] += f32::from(b) / 255.0;
        }
        Box::pin(async move { Ok::<Vec<f32>, corpus_index::Error>(v) })
    })
}

fn engine(dir: &Path) -> Arc<dyn IngestPort> {
    std::fs::create_dir_all(dir.join("recipes")).unwrap();
    std::fs::create_dir_all(dir.join("indexes")).unwrap();
    Arc::new(
        CorpusEngine::new(dir.join("recipes"), dir.join("indexes"), embed())
            .with_embedding_model("mock-8d")
            .with_self_node_id("node-test"),
    )
}

/// A local JSONL recipe over `docs` paragraph docs, as `corpus_id`.
fn recipe(dir: &Path, corpus_id: &str, docs: usize) -> String {
    let source = dir.join(format!("{corpus_id}.jsonl"));
    let body: String = (0..docs)
        .map(|i| {
            serde_json::json!({
                "title": format!("Note {i}"),
                "text": format!(
                    "This is note {i}, long enough for the paragraph chunker to keep \
                     it rather than drop it as noise, with a few ordinary words."
                ),
            })
            .to_string()
                + "\n"
        })
        .collect();
    std::fs::write(&source, body).unwrap();
    format!(
        r#"[corpus]
id = "{corpus_id}"
name = "Notes"
description = "install-carrying-its-recipe fixture"
license = "MIT"
mesh_sharing = false
size_compressed_gb = 0.0
size_indexed_gb = 0.0

[acquire]
type = "local_file"
path = "{}"

[extract]
type = "jsonl"
content_field = "text"
title_field = "title"

[chunk]
type = "paragraph"
max_chars = 400
overlap_chars = 40

[index]
fts = true
vector = true
embedding_model = "mock-8d"
embedding_dimensions = 8
"#,
        source.display()
    )
}

/// The failing input `ingest.recipe` names: a recipe that does not parse is
/// refused as an invalid recipe, not as a registry miss.
#[tokio::test]
async fn a_recipe_that_does_not_load_is_an_invalid_recipe() {
    let dir = tempfile::tempdir().unwrap();
    let port = engine(dir.path());
    match port
        .prepare_recipe_install("notes", "this is [not a recipe", &BTreeMap::new())
        .await
    {
        Err(InstallRefusal::InvalidRecipe(_)) => {}
        Err(other) => panic!("refused, but not as an invalid recipe: {other:?}"),
        Ok(_) => panic!("a recipe that does not parse was prepared"),
    }
    assert!(
        !dir.path().join("recipes/notes").exists(),
        "nothing is registered for a refused recipe"
    );
}

/// One corpus, one name: a recipe declaring another id is refused.
#[tokio::test]
async fn a_recipe_naming_another_corpus_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let port = engine(dir.path());
    let toml = recipe(dir.path(), "other", 2);
    match port
        .prepare_recipe_install("notes", &toml, &BTreeMap::new())
        .await
    {
        Err(InstallRefusal::InvalidRecipe(reason)) => {
            assert!(
                reason.contains("'other'") && reason.contains("'notes'"),
                "{reason}"
            )
        }
        Err(other) => panic!("refused, but not as an invalid recipe: {other:?}"),
        Ok(_) => panic!("a recipe naming another corpus was prepared"),
    }
}

/// A recipe that loads installs under its id, lands in the registry, and
/// carries the sha256 of its exact text on the index. Failing input: drop
/// the stamp from the run and `recipe_sha256_in` reads `None`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_installed_recipe_is_registered_and_stamped() {
    let dir = tempfile::tempdir().unwrap();
    let port = engine(dir.path());
    let toml = recipe(dir.path(), "notes", 3);
    let prepared = Arc::clone(&port)
        .prepare_recipe_install("notes", &toml, &BTreeMap::new())
        .await
        .unwrap_or_else(|e| panic!("the recipe prepares: {e:?}"));
    (prepared.run)(None).await.expect("the install runs");
    let corpus = Corpus::named(dir.path().join("indexes"), "notes").unwrap();
    assert!(corpus.is_installed(), "the canonical is finalised");
    assert_eq!(
        Corpus::recipe_sha256_in(corpus.root()),
        Some(recipe_sha256(&toml))
    );
    assert!(
        port.recipe_parameter_schema("notes").await.is_ok(),
        "the registry resolves the installed recipe by id"
    );
}
