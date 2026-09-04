// SPDX-License-Identifier: AGPL-3.0-or-later
//! Publish -> restore, over REAL Lance datasets, asserting the store still
//! answers both of its faces afterwards.
//!
//! The unit tests in `snapshot` write `chunks.lance` as a FILE containing
//! `b"fake lance bytes"`, which is why they never exercised the Lance-aware
//! capture and never noticed that the atlas datasets beside it were riding the
//! naive recursive walk. This module uses datasets the writer actually
//! produced, and the corpus it uses has NO `chunks.lance` at all — so the only
//! Lance tables in the archive are the atlas's, which is precisely the case
//! `lance_datasets_under` was added to cover.
//!
//! A tarred Lance dataset is not obviously broken when it is broken: a dropped
//! fragment or a missing `_indices/<uuid>` subtree still opens, and answers
//! with less. So the assertions are on ANSWERS — the same neighbor sets and the
//! same provider atoms, before and after — not on file presence.
//!
//! WHAT THIS CAN AND CANNOT FAIL ON (ARCH §18.1), because the distinction
//! matters and I do not want the green read as more than it is:
//!
//! - It CAN fail if the Lance-aware capture is incomplete — a fragment or an
//!   `_indices/<uuid>` subtree the manifest names but the archive omits. The
//!   `edges.lance` BTree on `source_title` is exercised by every `neighbors`
//!   call in the fingerprint, so losing it changes answers.
//! - It CAN fail if the atlas datasets fell back to the naive walk:
//!   `append_lance_snapshot` tars the manifest, the data files and the index
//!   subtrees, and NOT `_transactions/`, which the naive walk copies. The
//!   absence of `_transactions/` in the restored dataset is asserted, and is
//!   the discriminator between the two paths.
//! - It CANNOT fail on the RACE the Lance-aware path exists to prevent. That
//!   needs a writer appending while the tar walks, which this test does not
//!   have. The concurrency argument is structural, not tested here.

use corpus_engine::enrichment::atlas::provider::AtlasProvider;
use corpus_engine::enrichment::atlas::wiki_store::{
    build_wikipedia_columnar_store_from_chunks, wiki_atom_id,
};
use corpus_engine::extractors::wikipedia_types::{WikiLink, WikipediaChunkMetadata};
use corpus_engine::index::StoredChunkWithMetadata;
use corpus_engine::restore_snapshot_archive;
use corpus_engine::snapshot::{publish_snapshot, PublishOptions};
use corpus_engine::{ColumnarWikipediaGraph, WikiAtlasProvider};

fn meta(section: &str, links: Vec<(&str, &str)>) -> String {
    let m = WikipediaChunkMetadata {
        section_name: section.into(),
        section_path: vec![section.into()],
        section_depth: 0,
        section_type: if section == "Criticism" {
            "controversy".into()
        } else {
            "lead".into()
        },
        citation_needed_count: None,
        pov_count: if section == "Criticism" {
            Some(3)
        } else {
            None
        },
        clarification_needed_count: None,
        update_count: None,
        is_flagged_stable: None,
        outgoing_links: links
            .into_iter()
            .map(|(t, l)| WikiLink {
                target_title: t.into(),
                link_text: l.into(),
            })
            .collect(),
        revision_id: Some(77),
        wikidata_qid: None,
        page_id: None,
    };
    serde_json::to_string(&m).unwrap()
}

fn ch(id: u64, title: &str, m: String) -> StoredChunkWithMetadata {
    StoredChunkWithMetadata {
        id,
        title: Some(title.into()),
        url: None,
        metadata_raw: Some(m),
    }
}

/// Enough articles and sections that a dropped fragment or a lost scalar index
/// changes an answer rather than going unnoticed.
fn corpus_chunks() -> Vec<StoredChunkWithMetadata> {
    let mut v = Vec::new();
    let titles = ["Alpha", "Beta", "Gamma", "Delta", "Epsilon"];
    for (i, t) in titles.iter().enumerate() {
        let next = titles[(i + 1) % titles.len()];
        let prev = titles[(i + titles.len() - 1) % titles.len()];
        v.push(ch(
            (i * 10 + 1) as u64,
            t,
            meta("Lead", vec![(next, "the next one"), ("Offsite", "offsite")]),
        ));
        v.push(ch(
            (i * 10 + 2) as u64,
            t,
            meta("Criticism", vec![(prev, "criticism of the previous")]),
        ));
    }
    v
}

/// Every answer the two faces give, as one comparable value.
async fn fingerprint(atlas_dir: &std::path::Path, corpus: &str) -> Vec<String> {
    let g = ColumnarWikipediaGraph::open(atlas_dir).await.unwrap();
    let p = WikiAtlasProvider::open(atlas_dir, corpus).await.unwrap();
    let mut out = Vec::new();
    out.push(format!(
        "counts articles={} edges={} provider_atoms={} provider_edges={}",
        g.article_count().await,
        g.edge_count().await,
        p.atom_count(),
        p.edge_count()
    ));
    for t in ["Alpha", "Beta", "Gamma", "Delta", "Epsilon"] {
        // Neighbor face, including the axis filter that reads the per-edge
        // strings — the columns a fragment drop would take with it.
        let mut ns: Vec<String> = g
            .neighbors(t, 50)
            .await
            .into_iter()
            .map(|n| {
                format!(
                    "{t}->{} [{}] occ={} in={}",
                    n.title, n.relationship_type, n.occurrence_count, n.in_scope
                )
            })
            .collect();
        ns.sort();
        out.extend(ns);
        let mut axis: Vec<String> = g
            .neighbors_for_axis(t, &["criticism".to_string()], 50)
            .await
            .into_iter()
            .map(|n| format!("{t}~criticism->{}", n.title))
            .collect();
        axis.sort();
        out.extend(axis);
        out.push(format!(
            "{t} contested={}",
            g.has_contested_section(t).await
        ));

        // Walk face: identity, evidence anchor, adjacency.
        let id = wiki_atom_id(t, corpus);
        let atom = p.atom(&id).map(|a| a.name().to_string());
        let ev: Vec<String> = p
            .atom_evidence(&id)
            .iter()
            .map(|e| e.chunk_id().to_string())
            .collect();
        let mut es: Vec<String> = p
            .edges_from(&id)
            .iter()
            .map(|e| format!("{}=>{}", e.source, e.target))
            .collect();
        es.sort();
        out.push(format!("{t} atom={atom:?} evidence={ev:?} out={es:?}"));
    }
    out
}

/// Publish a wiki-class corpus and restore it under a throwaway id: both faces
/// must answer identically to the source.
#[tokio::test]
async fn wiki_store_survives_publish_and_restore_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let index_dir = tmp.path().join("indexes/wikitest");
    let atlas_dir = index_dir.join("atlas");
    std::fs::create_dir_all(&atlas_dir).unwrap();
    std::fs::write(
        corpus_engine::corpus::Corpus::meta_in(&index_dir),
        serde_json::to_vec_pretty(&serde_json::json!({
            "corpus_id": "wikitest",
            "corpus_name": "Wiki Roundtrip",
            "embedding_model": "qwen3-embedding-0.6b",
            "embedding_dimensions": 1024,
        }))
        .unwrap(),
    )
    .unwrap();
    build_wikipedia_columnar_store_from_chunks(&atlas_dir, "wikitest", corpus_chunks())
        .await
        .unwrap();

    let before = fingerprint(&atlas_dir, "wikitest").await;
    // The fixture has to be rich enough that a silent loss would show.
    assert!(
        before.len() > 20,
        "fingerprint too thin to detect a drop: {before:?}"
    );

    let archive = tmp.path().join("out.tar.zst");
    let outcome = publish_snapshot(PublishOptions {
        index_dir: index_dir.clone(),
        enrichment_dir: None,
        output_path: archive.clone(),
        snapshot_id: "wikitest-roundtrip".into(),
        chunk_count: 10,
        residual_gap_pct: None,
        notes: None,
        source_recipe_sha256: None,
        producer_version: "test".into(),
        zstd_level: 1,
        sibling_index_dirs: Vec::new(),
    })
    .await
    .unwrap();

    let restore_root = tmp.path().join("restored");
    std::fs::create_dir_all(&restore_root).unwrap();
    let restored = restore_snapshot_archive(
        &archive,
        &restore_root,
        "wikitest",
        Some(&outcome.archive_sha256),
        "qwen3-embedding-0.6b",
        1024,
    )
    .unwrap();

    let restored_atlas = restored.index_dir.join("atlas");
    let after = fingerprint(&restored_atlas, "wikitest").await;
    assert_eq!(
        before, after,
        "the restored store must answer identically to the source"
    );

    // The discriminator between the Lance-aware capture and the naive walk.
    // `append_lance_snapshot` tars the manifest, the data files and the index
    // subtrees; the naive `append_dir_recursive` would also carry
    // `_transactions/`, which the source datasets DO have on disk.
    for table in ["articles.lance", "edges.lance"] {
        assert!(
            atlas_dir.join(table).join("_transactions").is_dir(),
            "{table}: the source must have _transactions, or this check proves nothing"
        );
        assert!(
            !restored_atlas.join(table).join("_transactions").exists(),
            "{table}: _transactions in the archive means the atlas dataset rode the \
             naive walk instead of the Lance-aware capture"
        );
        // What the manifest DOES name must be there.
        assert!(
            restored_atlas.join(table).join("_versions").is_dir(),
            "{table}: the manifest version must be captured"
        );
    }
    // The scalar index `write_wikipedia_columnar_store` builds on
    // `edges.source_title` is what makes `neighbors` a seek; it lives under
    // `_indices/<uuid>` and is captured by uuid, not by walking the directory.
    assert!(
        restored_atlas.join("edges.lance/_indices").is_dir(),
        "the edges BTree index must survive the round trip"
    );
}

/// The same archive restored under a DIFFERENT corpus id — the "install into a
/// throwaway id" path, and the one that proves an atom id travels with the
/// article rather than with the directory it landed in.
///
/// The ids under the new corpus are DIFFERENT, by construction: `corpus_id` is
/// part of the essence. So the assertion is not "the ids match" — it is that
/// the walk still resolves every article under ids a peer could compute, which
/// is the property `entity_content_hash` exists to give.
#[tokio::test]
async fn restore_under_a_new_corpus_id_still_serves_the_walk() {
    let tmp = tempfile::tempdir().unwrap();
    let index_dir = tmp.path().join("indexes/wikitest");
    let atlas_dir = index_dir.join("atlas");
    std::fs::create_dir_all(&atlas_dir).unwrap();
    std::fs::write(
        corpus_engine::corpus::Corpus::meta_in(&index_dir),
        serde_json::to_vec_pretty(&serde_json::json!({
            "corpus_id": "wikitest",
            "corpus_name": "Wiki Roundtrip",
            "embedding_model": "qwen3-embedding-0.6b",
            "embedding_dimensions": 1024,
        }))
        .unwrap(),
    )
    .unwrap();
    build_wikipedia_columnar_store_from_chunks(&atlas_dir, "wikitest", corpus_chunks())
        .await
        .unwrap();

    let archive = tmp.path().join("out.tar.zst");
    let outcome = publish_snapshot(PublishOptions {
        index_dir,
        enrichment_dir: None,
        output_path: archive.clone(),
        snapshot_id: "wikitest-roundtrip".into(),
        chunk_count: 10,
        residual_gap_pct: None,
        notes: None,
        source_recipe_sha256: None,
        producer_version: "test".into(),
        zstd_level: 1,
        sibling_index_dirs: Vec::new(),
    })
    .await
    .unwrap();

    let restore_root = tmp.path().join("restored");
    std::fs::create_dir_all(&restore_root).unwrap();
    let restored = restore_snapshot_archive(
        &archive,
        &restore_root,
        "wikitest-throwaway",
        Some(&outcome.archive_sha256),
        "qwen3-embedding-0.6b",
        1024,
    )
    .unwrap();

    let dir = restored.index_dir.join("atlas");
    // The store still carries the ids it was BUILT with — a restore copies
    // bytes, it does not re-key. So the walk over the throwaway id resolves by
    // the ORIGINAL corpus's ids, and that is the honest reading: a snapshot's
    // atom ids belong to the corpus that produced it.
    let p = WikiAtlasProvider::open(&dir, "wikitest-throwaway")
        .await
        .unwrap();
    assert_eq!(p.atom_count(), 5);
    for t in ["Alpha", "Beta", "Gamma", "Delta", "Epsilon"] {
        let built_id = wiki_atom_id(t, "wikitest");
        assert!(
            p.atom(&built_id).is_some(),
            "{t}: the id the archive was built with must still resolve"
        );
        assert!(!p.atom_evidence(&built_id).is_empty(), "{t}: evidence");
        // And an id computed for the NEW corpus does not resolve, which is the
        // failing input that makes the line above mean something.
        assert!(
            p.atom(&wiki_atom_id(t, "wikitest-throwaway")).is_none(),
            "{t}: a re-keyed id must NOT resolve against un-re-keyed bytes"
        );
    }
    // The neighbor face survives the rename too.
    let g = ColumnarWikipediaGraph::open(&dir).await.unwrap();
    assert_eq!(g.article_count().await, 5);
    assert!(!g.neighbors("Alpha", 10).await.is_empty());
}
