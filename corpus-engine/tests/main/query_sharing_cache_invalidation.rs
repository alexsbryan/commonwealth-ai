// SPDX-License-Identifier: AGPL-3.0-or-later
//! `query_sharing` is a privacy control, so flipping it has to take effect.
//!
//! **The thing being fixed.** `CorpusEngine::installed_indexes` caches
//! `IndexInfo` per index dir and validates the cache against an mtime. Until
//! 2026-08-29 that mtime was `chunks.lance/_versions` when it existed, with
//! `_corpus_meta.json` only as a FALLBACK for indexes that had no
//! `chunks.lance`. Every real corpus has one — so on every real corpus, an
//! edit to `_corpus_meta.json` was invisible until the next committed write
//! of a chunk.
//!
//! That matters because `query_sharing` lives only in that file and has no
//! setter: editing the meta is the only way to change it. It is the
//! per-corpus dial that decides whether mesh peers may run federated searches
//! against this copy — `sovereign-mesh`'s `build_hosted_corpora` filters on
//! exactly this flag when deciding what to advertise in `hosted_corpora`. So
//! a corpus switched to `query_sharing = false` went on being advertised, and
//! went on serving peers, for as long as nobody wrote to it. A guard nobody
//! has watched deny is not a guard (ARCH §18.1), and one that silently does
//! not take effect is worse than none.
//!
//! **What this test pins.** Two reads of `installed_indexes()` around a meta
//! edit must disagree. Revert `installed_indexes`'s mtime key to
//! `_versions`-or-meta (rather than the max of both) and the second read
//! returns the cached `query_sharing = true`, and this fails.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::{Corpus, CorpusEngine, CorpusSpec, EmbedFn};

fn write_source(dir: &Path) -> PathBuf {
    let path = dir.join("source.txt");
    let text = "The invite was written down once and read twice, which is \
                how most of these stories start.\n\n\
                The second reader was not the one the first had in mind.\n";
    std::fs::write(&path, text).unwrap();
    path
}

fn write_recipe(recipes_dir: &Path, source: &Path) -> PathBuf {
    let recipe_path = recipes_dir.join("shared_corpus.toml");
    let source_str = source.to_string_lossy();
    std::fs::write(
        &recipe_path,
        format!(
            r#"
[corpus]
id = "shared_corpus"
name = "Shared Corpus"
description = "query_sharing cache-invalidation fixture"
license = "CC0"
mesh_sharing = false
query_sharing = true

[acquire]
type = "local_file"
path = "{source_str}"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
max_chars = 512
overlap_chars = 64

[index]
embedding_model = "test-mock"
embedding_dimensions = 8
"#
        ),
    )
    .unwrap();
    recipe_path
}

fn working_embed_fn() -> EmbedFn {
    Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 8]) }))
}

async fn query_sharing_of(engine: &CorpusEngine, corpus_id: &str) -> bool {
    engine
        .installed_indexes()
        .await
        .expect("installed_indexes must succeed on a completed ingest")
        .into_iter()
        .find(|i| i.corpus_id == corpus_id)
        .unwrap_or_else(|| panic!("{corpus_id} must be installed"))
        .query_sharing
}

#[tokio::test]
async fn flipping_query_sharing_on_disk_is_visible_to_the_next_capability_read() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();
    let source = write_source(dir.path());
    let recipe_path = write_recipe(&recipes_dir, &source);

    let engine = CorpusEngine::new(recipes_dir, indexes_dir.clone(), working_embed_fn())
        .with_embedding_model("test-mock");
    engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("ingest completes");

    // Validate the instrument before the verdict (§18.4). Two preconditions,
    // and without either one this test would pass while testing nothing:
    //   - the flag starts TRUE, so a flip has somewhere to go;
    //   - the index really does carry a `chunks.lance`, which is what made
    //     the meta mtime a dead fallback in the first place. A fixture with
    //     no `chunks.lance` exercises the path that always worked.
    assert!(
        query_sharing_of(&engine, "shared_corpus").await,
        "fixture must start shared, or the flip below proves nothing"
    );
    let index_dir = indexes_dir.join("shared_corpus");
    assert!(
        index_dir.join("chunks.lance").join("_versions").exists(),
        "fixture must have a chunks.lance/_versions, or it does not exercise \
         the cache key that was wrong"
    );

    // The only way to change this flag: rewrite the meta. There is no setter.
    let meta_path = Corpus::meta_in(&index_dir);
    let mut meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
    meta["query_sharing"] = serde_json::Value::Bool(false);
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();

    assert!(
        !query_sharing_of(&engine, "shared_corpus").await,
        "the corpus was withdrawn from federated search on disk and the next \
         capability read still says it is shared — peers would go on being \
         served from it"
    );
}
