// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for `reindex.rs`, moved out of it unchanged.

#[test]
fn rescope_falls_back_to_section_links_when_bullet_has_no_markup() {
    // The production extractor strips [[..]] markup before chunking,
    // so bullet_text carries plain prose. Section outgoing_links
    // must survive as outbound_links instead of filtering to [].
    let section_meta = Some(serde_json::json!({
        "outgoing_links": [
            {"target_title": "Gaza war"},
            {"target_title": "Benjamin Netanyahu"}
        ]
    }));
    let meta = rescope_outgoing_links_for_bullet(
        &section_meta,
        "Israeli prime minister Benjamin Netanyahu says reconstruction waits.",
    )
    .expect("meta");
    let links: Vec<&str> = meta["outbound_links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(links, vec!["Gaza war", "Benjamin Netanyahu"]);
}

#[test]
fn rescope_stays_bullet_scoped_when_markup_present() {
    let section_meta = Some(serde_json::json!({
        "outgoing_links": [
            {"target_title": "Kyiv"},
            {"target_title": "Elsewhere"}
        ]
    }));
    let meta = rescope_outgoing_links_for_bullet(&section_meta, "At least 12 killed in [[Kyiv]].")
        .expect("meta");
    let flat: Vec<&str> = meta["outbound_links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(flat, vec!["Kyiv"]);
    let filtered = meta["outgoing_links"].as_array().unwrap();
    assert_eq!(filtered.len(), 1, "section links narrowed to the bullet's");
}
use super::*;
use crate::index::CorpusIndex;
use crate::recipe::{ChunkerConfig, ExtractorConfig};
use std::path::Path;
use std::sync::Arc;

fn mock_embed_fn() -> crate::types::EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.1_f32; 4]) }))
}

async fn fixture_engine(index_dir: &Path) -> (CorpusEngine, CorpusIndex) {
    let recipes_dir = index_dir.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();
    let engine = CorpusEngine::new(recipes_dir, index_dir.to_path_buf(), mock_embed_fn());

    let idx_path = index_dir.join("test-corpus");
    let index = CorpusIndex::create(
        &idx_path,
        "test-corpus",
        "Test Corpus",
        "test-model",
        4,
        false,
        "MIT",
    )
    .await
    .expect("create index");
    (engine, index)
}

#[tokio::test]
async fn reindex_by_source_doc_id_inserts_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    let idx_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&idx_dir).unwrap();
    let (engine, _index) = fixture_engine(&idx_dir).await;

    let result = engine
        .reindex_by_source_doc_id(
            "test-corpus",
            "Donald_Trump",
            "Body of the Donald Trump article. One paragraph.",
            &ExtractorConfig::Plaintext {
                title_pattern: None,
                strip_boilerplate: None,
            },
            &ChunkerConfig::Passthrough,
        )
        .await
        .expect("reindex absent doc");

    match result {
        ReindexResult::Updated { chunks_written, .. } => {
            assert_eq!(chunks_written, 1, "passthrough chunker → one chunk")
        }
        other => panic!("expected Updated, got {other:?}"),
    }

    // Reopen the index and verify the chunk is queryable.
    let reopened = CorpusIndex::open(&idx_dir.join("test-corpus"))
        .await
        .unwrap();
    assert_eq!(reopened.chunk_count().await.unwrap(), 1);
    let ids = reopened.list_indexed_source_doc_ids().await.unwrap();
    assert!(ids.contains("Donald_Trump"));
}

#[tokio::test]
async fn reindex_by_source_doc_id_replaces_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let idx_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&idx_dir).unwrap();
    let (engine, _index) = fixture_engine(&idx_dir).await;

    // First call — initial insert.
    engine
        .reindex_by_source_doc_id(
            "test-corpus",
            "Joe_Biden",
            "Original revision of the Biden article.",
            &ExtractorConfig::Plaintext {
                title_pattern: None,
                strip_boilerplate: None,
            },
            &ChunkerConfig::Passthrough,
        )
        .await
        .expect("initial insert");

    // Second call with new content — must replace, not append.
    engine
        .reindex_by_source_doc_id(
            "test-corpus",
            "Joe_Biden",
            "Updated revision with substantially different content body.",
            &ExtractorConfig::Plaintext {
                title_pattern: None,
                strip_boilerplate: None,
            },
            &ChunkerConfig::Passthrough,
        )
        .await
        .expect("refresh");

    let reopened = CorpusIndex::open(&idx_dir.join("test-corpus"))
        .await
        .unwrap();
    // Total chunk count is 1 — old must be replaced, not duplicated.
    // (If the delete-by-source-doc step had been skipped, count
    //  would be 2 with both revisions co-resident.)
    assert_eq!(
        reopened.chunk_count().await.unwrap(),
        1,
        "old chunk must be replaced, not duplicated",
    );
    let ids = reopened.list_indexed_source_doc_ids().await.unwrap();
    assert!(ids.contains("Joe_Biden"));
    assert_eq!(ids.len(), 1);
}

#[tokio::test]
async fn reindex_by_source_doc_id_returns_error_when_corpus_missing() {
    let dir = tempfile::tempdir().unwrap();
    let idx_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&idx_dir).unwrap();
    let (engine, _index) = fixture_engine(&idx_dir).await;

    let err = engine
        .reindex_by_source_doc_id(
            "no-such-corpus",
            "Anything",
            "body",
            &ExtractorConfig::Plaintext {
                title_pattern: None,
                strip_boilerplate: None,
            },
            &ChunkerConfig::Passthrough,
        )
        .await;
    assert!(matches!(err, Err(Error::IndexNotFound(_))));
}
