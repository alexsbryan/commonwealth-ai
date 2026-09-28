// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for [`super`]: the listing a program reads with no engine in its
//! tree. The dedupe test moved here from corpus-engine's engine tests with the
//! code it pins (pb-corpus-mcp-reads).

use super::*;

/// covers: ST-5
///
/// Pins the dedup invariant: when two physical indexes advertise
/// the same corpus_id, `installed_indexes` returns exactly one
/// entry per corpus_id, and the kept entry is the one whose
/// directory basename equals the corpus_id (the canonical that
/// `open_index_for_corpus` will dereference). Without this,
/// retrieval can return chunks from a partition while the reading
/// desk re-resolves their `(corpus_id, chunk_id)` against the
/// canonical, silently misrouting citations to unrelated content.
#[tokio::test]
async fn installed_indexes_dedupes_corpus_id_collisions_preferring_canonical() {
    let dir = tempfile::tempdir().unwrap();
    let idx_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&idx_dir).unwrap();

    // Real canonical wikipedia index.
    let canonical = idx_dir.join("wikipedia");
    CorpusIndex::create(
        &canonical,
        "wikipedia",
        "Wikipedia",
        "test-model",
        4,
        true,
        "MIT",
    )
    .await
    .unwrap();
    std::fs::write(
        crate::corpus::Corpus::meta_in(&canonical),
        r#"{"corpus_id":"wikipedia","corpus_name":"Wikipedia","embedding_model":"test-model",
                 "embedding_dimensions":4,"mesh_sharing":true,"license":"MIT",
                 "created_at":0,"last_updated":0,"schema_version":3,"is_shard":false,
                 "ingestion_in_progress":false,"indexes_built":true,
                 "vector_index_built":true,"content_fts_built":true,
                 "title_fts_built":true,"committed_iter_pos":0,
                 "committed_shard_set":[]}"#,
    )
    .unwrap();

    // Lingering per-peer partition advertising the same corpus_id.
    let partition = idx_dir.join("wikipedia-partition-peerX");
    CorpusIndex::create(
        &partition,
        "wikipedia",
        "Wikipedia",
        "test-model",
        4,
        true,
        "MIT",
    )
    .await
    .unwrap();
    std::fs::write(
        crate::corpus::Corpus::meta_in(&partition),
        r#"{"corpus_id":"wikipedia","corpus_name":"Wikipedia","embedding_model":"test-model",
                 "embedding_dimensions":4,"mesh_sharing":true,"license":"MIT",
                 "created_at":0,"last_updated":0,"schema_version":3,"is_shard":false,
                 "ingestion_in_progress":false,"indexes_built":true,
                 "vector_index_built":true,"content_fts_built":true,
                 "title_fts_built":true,"committed_iter_pos":0,
                 "committed_shard_set":[]}"#,
    )
    .unwrap();

    let source = FsIndexSource::new(idx_dir.clone());
    let listed = source.installed_indexes().await.unwrap();

    let by_id: Vec<&str> = listed.iter().map(|i| i.corpus_id.as_str()).collect();
    assert_eq!(
        by_id,
        vec!["wikipedia"],
        "expected exactly one wikipedia entry after dedup, got: {by_id:?}"
    );
    let kept = listed.iter().find(|i| i.corpus_id == "wikipedia").unwrap();
    assert_eq!(
        kept.path, canonical,
        "dedup must prefer the canonical-named dir (matches open_index_for_corpus)"
    );
}

/// A program that links no engine lists an installed index and opens it by
/// id, and the second open is served from the handle cache.
#[tokio::test]
async fn lists_and_opens_a_fixture_index_without_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let idx_dir = dir.path().join("indexes");
    let sep = idx_dir.join("sep");
    CorpusIndex::create(&sep, "sep", "SEP", "test-model", 4, true, "MIT")
        .await
        .unwrap();
    std::fs::write(
        crate::corpus::Corpus::meta_in(&sep),
        r#"{"corpus_id":"sep","corpus_name":"SEP","embedding_model":"test-model",
             "embedding_dimensions":4,"mesh_sharing":true,"license":"MIT",
             "created_at":0,"last_updated":0,"schema_version":3,"is_shard":false,
             "ingestion_in_progress":false,"indexes_built":true,
             "vector_index_built":true,"content_fts_built":true,
             "title_fts_built":true,"committed_iter_pos":0,
             "committed_shard_set":[]}"#,
    )
    .unwrap();

    let source = FsIndexSource::new(idx_dir);
    let usable = IndexSource::usable_indexes(&source).await.unwrap();
    let ids: Vec<&str> = usable.iter().map(|i| i.corpus_id.as_str()).collect();
    assert_eq!(ids, vec!["sep"]);

    let index = source.open_index_for_corpus("sep").await.unwrap();
    assert_eq!(index.corpus_id(), "sep");
    assert_eq!(index.embedding_dim(), 4);
    source.open_index_for_corpus("sep").await.unwrap();
    assert_eq!(
        source.index_cache_len(),
        1,
        "the reopen was not a cache hit"
    );
}
