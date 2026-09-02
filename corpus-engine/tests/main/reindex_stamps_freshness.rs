// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-document recency is EMERGENT FROM INDEXING, not reported by each
//! source. One sidecar, `_doc_freshness.json`, stamped at the single point
//! every re-index path converges on — `reindex_by_source_doc_id`. That is
//! what lets the newsworthy watcher, a watched-folder edit and a delta update
//! all make their content "fresh" with no per-source code.
//!
//! `freshness.rs` already tests stamp-then-load in isolation, which proves the
//! sidecar round-trips and nothing about whether anything calls it. The
//! failure this file catches is the one that leaves both halves passing: a new
//! content source whose re-index path inserts chunks and never reaches the
//! stamp. Its documents then never sort fresh-first, silently and forever —
//! the Atlas just shows them at baseline, which is exactly what a
//! never-refreshed document looks like.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::freshness::load_doc_freshness;
use corpus_engine::recipe::{ChunkerConfig, ExtractorConfig};
use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn};

const CORPUS: &str = "recency";

fn working_embed_fn() -> EmbedFn {
    Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 8]) }))
}

fn write_recipe(recipes_dir: &Path, source: &Path) -> PathBuf {
    let recipe_path = recipes_dir.join("recency.toml");
    let source_str = source.to_string_lossy();
    std::fs::write(
        &recipe_path,
        format!(
            r#"
[corpus]
id = "{CORPUS}"
name = "Recency"
description = "doc-freshness fixture"
license = "CC0"
mesh_sharing = false

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

/// covers: ST-41
#[tokio::test]
async fn reindexing_a_document_stamps_its_recency_at_the_shared_convergence_point() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let source = dir.path().join("source.txt");
    std::fs::write(
        &source,
        "The harbour master keeps two ledgers and admits to one.\n\n\
         The second is not secret, only inconvenient to explain.\n\n\
         Both agree about the tides and about nothing else.\n",
    )
    .unwrap();
    let recipe_path = write_recipe(&recipes_dir, &source);

    let engine = CorpusEngine::new(recipes_dir, indexes_dir.clone(), working_embed_fn())
        .with_embedding_model("test-mock");
    engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("bulk install");

    let corpus_dir = indexes_dir.join(CORPUS);

    // Validate the instrument before the result (§18.4). A fresh full install
    // stamps NOTHING — freshness means "touched since the bulk index" — so if
    // the map already held this id, the assertion below would pass without the
    // reindex having done anything.
    assert!(
        load_doc_freshness(&corpus_dir).is_empty(),
        "a bulk install must leave every document at baseline recency"
    );

    let before = corpus_engine::freshness::now_unix();
    let result = engine
        .reindex_by_source_doc_id(
            CORPUS,
            "Ardrossan_Harbour",
            "The harbour master has since closed the second ledger.\n\n\
             The tides are unchanged.\n",
            &ExtractorConfig::Plaintext {
                title_pattern: None,
                strip_boilerplate: None,
            },
            &ChunkerConfig::Paragraph {
                max_chars: 512,
                overlap_chars: 64,
            },
        )
        .await
        .expect("reindex");

    // The reindex really wrote chunks — otherwise a stamp on an empty write
    // would be the thing under test, and that is a different question.
    match result {
        corpus_engine::engine::reindex::ReindexResult::Updated { chunks_written, .. } => {
            assert!(chunks_written > 0, "the reindex must have written chunks");
        }
        other => panic!("expected an Updated reindex, got {other:?}"),
    }

    let after = corpus_engine::freshness::now_unix();
    let map = load_doc_freshness(&corpus_dir);
    let stamped = map
        .get("Ardrossan_Harbour")
        .copied()
        .unwrap_or_else(|| panic!("reindex left no freshness stamp; map = {map:?}"));
    assert!(
        stamped >= before && stamped <= after,
        "stamp {stamped} must fall inside the reindex window [{before}, {after}]"
    );

    // Only the document that was re-indexed becomes fresh — a whole-corpus
    // stamp would make recency meaningless as a sort key.
    assert_eq!(
        map.len(),
        1,
        "exactly the re-indexed document is fresh; map = {map:?}"
    );

    // A second reindex moves the stamp forward in place rather than adding a
    // row — the sidecar is a map keyed by document identity, not a log.
    let second = engine
        .reindex_by_source_doc_id(
            CORPUS,
            "Ardrossan_Harbour",
            "A third ledger has appeared.\n",
            &ExtractorConfig::Plaintext {
                title_pattern: None,
                strip_boilerplate: None,
            },
            &ChunkerConfig::Paragraph {
                max_chars: 512,
                overlap_chars: 64,
            },
        )
        .await;
    assert!(second.is_ok());
    let map2 = load_doc_freshness(&corpus_dir);
    assert_eq!(map2.len(), 1, "re-stamping must not duplicate the entry");
    assert!(
        map2["Ardrossan_Harbour"] >= stamped,
        "a later reindex must not move recency backwards"
    );
}
