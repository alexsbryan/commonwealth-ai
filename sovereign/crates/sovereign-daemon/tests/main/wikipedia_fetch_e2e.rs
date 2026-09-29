// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(feature = "treesitter")]
//! `wikipedia_fetch` ingests through ingest's port (pb-ingest-dial-tools-close).
//!
//! The daemon's real `/mcp` registry (the one the stock process serves),
//! over an engine whose catalog and content recipes point at a local fixture
//! in place of en.wikipedia.org: the catalog corpus is installed from a
//! one-row JSONL, `wikipedia_fetch` is called through the registry, the
//! engine ingests the article through `CatalogIngestPort` and folds it into
//! the shared `wikipedia-fetched` corpus, and a read of that corpus finds a
//! phrase that exists only in the fixture's article body.

use std::sync::Arc;

use axum::extract::Query;
use axum::routing::get;
use axum::Router;
use corpus_engine::{CorpusEngine, CorpusSpec};
use serde_json::json;
use sovereign_contracts::types::{StepOutput, ToolContext};

/// A word that appears in the fixture's article and nowhere else.
const MARKER: &str = "zyzzyvalattice";

/// The one catalog row and the Action API `parse` body for it.
async fn fixture_origin() -> String {
    let catalog = json!({
        "title": "Fixture Topic",
        "url": "https://en.wikipedia.org/wiki/Fixture_Topic",
        "abstract": "A topic that exists only in this test.",
        "sections": ["History"],
    })
    .to_string();
    let article = move |Query(q): Query<std::collections::HashMap<String, String>>| async move {
        assert_eq!(q.get("page").map(String::as_str), Some("Fixture_Topic"));
        json!({ "parse": {
            "title": "Fixture Topic",
            "pageid": 1,
            "revid": 1,
            "wikitext": format!(
                "Fixture Topic is a subject studied only by this test. Its defining \
                 structure is the {MARKER}, which the fetch must carry into the index.\n\n\
                 == History ==\nThe {MARKER} was first described in a fixture."
            ),
            "sections": [{ "line": "History", "level": "2", "index": "1" }],
            "links": [],
            "properties": [],
        }})
        .to_string()
    };
    let app = Router::new()
        .route("/catalog.jsonl", get(move || async move { catalog }))
        .route("/api", get(article));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// The two recipes as local overrides: the catalog's acquire URL and its
/// download template, and the content recipe's, point at the fixture.
fn write_recipes(recipes: &std::path::Path, origin: &str) {
    std::fs::create_dir_all(recipes).unwrap();
    std::fs::write(
        recipes.join("wikipedia-catalog.toml"),
        format!(
            r#"[corpus]
id = "wikipedia-catalog"
name = "Wikipedia Catalog (fixture)"
description = "One-row fixture catalog."
license = "CC-BY-SA-4.0"
kind = "catalog"
parent_corpus_id = "wikipedia"

[acquire]
type = "bulk_download"
url = "{origin}/catalog.jsonl"

[extract]
type = "wikipedia_catalog"

[chunk]
type = "passthrough"

[index]
fts = true
vector = true

[catalog]
id_field = "title"
download_url_template = "{origin}/api?page={{id}}"
content_recipe = "wikipedia-article"
target_corpus_id = "wikipedia-fetched"
expansion_enabled = false
"#
        ),
    )
    .unwrap();
    std::fs::write(
        recipes.join("wikipedia-article.toml"),
        format!(
            r#"[corpus]
id = "wikipedia-article"
name = "Wikipedia — Single Article (fixture)"
description = "On-demand single-article ingest from the fixture."
license = "CC-BY-SA-4.0"
kind = "knowledge"
on_demand = true
parent_corpus_id = "wikipedia"

[acquire]
type = "bulk_download"
url = "{origin}/api?page=PLACEHOLDER"

[extract]
type = "wikipedia_api_article"

[chunk]
type = "paragraph"
max_chars = 1024
overlap_chars = 128

[index]
fts = true
vector = true

[enrichment]
enabled = false
"#
        ),
    )
    .unwrap();
}

/// Fails if `wikipedia_fetch` stops reaching ingest through the registry,
/// or the port's ingest stops landing the article where the next read looks
/// (drop the fold into the shared target, or the fetch URL substitution,
/// and the marker is not found).
#[tokio::test]
async fn wikipedia_fetch_through_the_registry_ingests_the_article_and_the_next_read_finds_it() {
    let dir = tempfile::tempdir().unwrap();
    let origin = fixture_origin().await;
    write_recipes(&dir.path().join("recipes"), &origin);
    let embed: corpus_index::types::EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async {
            Ok::<Vec<f32>, corpus_index::Error>(vec![0.5; corpus_index::types::DEFAULT_EMBED_DIM])
        })
    });
    let engine = Arc::new(
        CorpusEngine::new(
            dir.path().join("recipes"),
            dir.path().join("indexes"),
            embed,
        )
        .with_embedding_model("fixture-embed"),
    );
    engine
        .ingest(&CorpusSpec::Builtin("wikipedia-catalog".into()), None)
        .await
        .expect("the fixture catalog installs");

    let solve_jobs = Arc::new(sovereign_daemon::solve_http::SolveJobs::new(1));
    let registry =
        sovereign_daemon::tool_registry::build_tool_registry(engine.clone(), solve_jobs).await;
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

    let fetched = corpus_index::index::CorpusIndex::open(
        &dir.path().join("indexes").join("wikipedia-fetched"),
    )
    .await
    .expect("the shared target corpus exists after the fetch");
    let hits = fetched.search(&[], MARKER, 5).await.expect("FTS read");
    assert!(
        hits.iter().any(|h| h.content.contains(MARKER)),
        "the fetched article is not readable: {} hit(s)",
        hits.len()
    );
}
