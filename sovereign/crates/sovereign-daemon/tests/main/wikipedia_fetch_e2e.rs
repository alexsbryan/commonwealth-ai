// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(feature = "treesitter")]
//! `wikipedia_fetch` ingests through ingest's port (pb-ingest-dial-tools-close).
//!
//! The daemon's real `/mcp` registry (the one the stock process serves),
//! over ingest's port double (pb-ingest-dial-daemon-tests): the catalog
//! corpus is a fixture index on disk the double lists and opens with the
//! leaf's own reader, its `[catalog]` block is what the double answers,
//! `wikipedia_fetch` is called through the registry, and the double records
//! the one catalog work the tool asks ingest for — the content recipe, the
//! work's url from the template, the staging corpus, and the fold into the
//! shared `wikipedia-fetched` corpus.
//!
//! What ingest does with that work — fetch the article, index it, fold it
//! into the shared corpus where the next read finds a phrase only the
//! article carries — is corpus-engine's catalog_fetch_port_parity, over the
//! same fixture recipes and origin this file served until the split.

use std::sync::{Arc, Mutex};

use corpus_index::index::{CorpusIndex, InsertChunk};
use corpus_index::ingest_port::{CatalogWork, CatalogWorkIngested};
use corpus_index::types::{CorpusKind, EmbedFn};
use serde_json::json;
use sovereign_contracts::types::{StepOutput, ToolContext};

const DIM: usize = corpus_index::types::DEFAULT_EMBED_DIM;
const TEMPLATE: &str = "http://fixture.invalid/api?page={id}";

/// The one-row catalog as ingest leaves it: a `Catalog`-kind index whose
/// row the tool's literal-id lookup finds.
async fn install_catalog(indexes: &std::path::Path) {
    let index = CorpusIndex::create(
        &indexes.join("wikipedia-catalog"),
        "wikipedia-catalog",
        "Wikipedia Catalog (fixture)",
        "fixture-embed",
        DIM,
        false,
        "CC-BY-SA-4.0",
    )
    .await
    .unwrap();
    index
        .set_kind_and_parent(Some(CorpusKind::Catalog), Some("wikipedia"))
        .unwrap();
    index
        .insert_batch(&[(
            InsertChunk {
                content: "Fixture Topic\nGutenberg ID: Fixture_Topic\nA topic that exists only \
                          in this test."
                    .into(),
                title: Some("Fixture Topic".into()),
                url: None,
                metadata: None,
                content_hash: None,
                source_doc_id: Some("Fixture_Topic".into()),
                source_file: None,
                code: Default::default(),
                unit_id: None,
            },
            vec![0.5; DIM],
        )])
        .await
        .unwrap();
    index.build_indexes(false, true, None).await.unwrap();
    index.mark_ingestion_complete().unwrap();
}

/// Fails if `wikipedia_fetch` stops reaching ingest through the registry,
/// or stops asking it for the work the catalog names (drop the fold into
/// the shared target, or the url substitution, and the recorded work
/// differs).
#[tokio::test]
async fn wikipedia_fetch_through_the_registry_asks_ingest_for_the_catalogs_work() {
    let dir = tempfile::tempdir().unwrap();
    let indexes = dir.path().join("indexes");
    install_catalog(&indexes).await;
    let embed: EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async { Ok::<Vec<f32>, corpus_index::Error>(vec![0.5; DIM]) })
    });
    let catalog = serde_json::from_value(json!({
        "id_field": "title",
        "download_url_template": TEMPLATE,
        "content_recipe": "wikipedia-article",
        "target_corpus_id": "wikipedia-fetched",
        "expansion_enabled": false,
    }))
    .unwrap();
    let asked: Arc<Mutex<Vec<CatalogWork>>> = Arc::default();
    let record = Arc::clone(&asked);
    let engine = Arc::new(
        crate::common::reading_double(indexes, embed)
            .with_catalog_configs(vec![("wikipedia-catalog".into(), catalog)])
            .on_ingest_catalog_work(move |work| {
                record.lock().unwrap().push(work);
                Box::pin(async {
                    Ok(CatalogWorkIngested {
                        chunks_created: 2,
                        opts_out_of_auto_enrichment: true,
                    })
                })
            }),
    );

    let solve_jobs = Arc::new(sovereign_daemon::solve_http::SolveJobs::new(1));
    let registry =
        sovereign_daemon::tool_registry::build_tool_registry(
            Some((
                engine.clone(),
                // Unasked: the fixture recipe opts out of auto-enrichment, so
                // no structural atlas is built.
                Arc::new(corpus_engine_atlas_reader::ports::double::AtlasPortDouble::new()),
            )),
            solve_jobs,
        )
        .await;
    let out = registry
        .get("wikipedia_fetch")
        .expect("wikipedia_fetch is on the daemon's registry")
        .execute(
            &json!({ "title": "Fixture Topic", "expand_links": false }),
            &ToolContext::default(),
        )
        .await
        .expect("wikipedia_fetch answered");
    let StepOutput::Text(text) = out else {
        panic!("wikipedia_fetch answered with a non-text output: {out:?}");
    };
    assert!(text.contains("wikipedia-fetched"), "{text}");

    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 1, "one work asked of ingest: {asked:?}");
    let work = &asked[0];
    assert_eq!(work.content_recipe, "wikipedia-article");
    assert_eq!(work.catalog_corpus_id, "wikipedia-catalog");
    assert_eq!(
        work.download_url, "http://fixture.invalid/api?page=Fixture_Topic",
        "the template's id is the title, underscored"
    );
    assert_eq!(
        work.staging_corpus_id,
        "_fetch_wikipedia-fetched-Fixture_Topic"
    );
    assert_eq!(work.shared_target.as_deref(), Some("wikipedia-fetched"));
}
