// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine half of sovereign-daemon's last composed-ingest e2es
//! (pb-ingest-dial-daemon-tests, phase-b-52).
//!
//! `d9a_corpus_catalog_e2e`, `ingest_origin_e2e` and `wikipedia_fetch_e2e`
//! drive `IngestPortDouble` and assert what the daemon asks of ingest. What
//! ingest does with the same asks is proven here, through the ports svrn
//! holds (`&dyn IngestPort`, `&dyn CorpusReadPort`, `&dyn
//! CatalogIngestPort`), never the same-named inherent methods, over the
//! fixtures those files built on a real engine until the split:
//!
//! - the catalogue is the registry snapshot, and every entry in it has the
//!   registry listing the catalog route reads beside it;
//! - an `ingest:v1` slice (the harbour-master text, a range of 0..100, unit
//!   0) lands in the engine's own `partition_path`, chunked;
//! - a Wikipedia catalog work, fetched from a fixture origin, folds into the
//!   shared `wikipedia-fetched` corpus where the next read finds a phrase
//!   only the article carries, and the catalog's literal-id lookup the tool
//!   makes finds the work's row.

use std::path::Path;
use std::sync::Arc;

use corpus_engine::{CorpusEngine, CorpusSpec};
use corpus_index::ingest_port::daemon::IngestPort;
use corpus_index::ingest_port::{CatalogIngestPort, CatalogWork};
use corpus_index::source::{CorpusReadPort, IndexSource};
use corpus_index::types::{CorpusKind, EmbedFn};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn embed(dim: usize) -> EmbedFn {
    Arc::new(move |_t: &str| Box::pin(async move { Ok(vec![0.1_f32; dim]) }))
}

fn engine_at(dir: &Path, dim: usize) -> CorpusEngine {
    std::fs::create_dir_all(dir.join("indexes")).unwrap();
    std::fs::create_dir_all(dir.join("recipes")).unwrap();
    CorpusEngine::new(dir.join("recipes"), dir.join("indexes"), embed(dim))
        .with_embedding_model("test-mock")
}

/// d9a's catalog route unions `builtin_corpora` with the installed set and
/// reads `registry_listing` per built-in; the double answers one built-in
/// and no listing. Here: the engine's catalogue is its registry snapshot,
/// never empty, and no entry of it lacks the listing the route reads.
#[test]
fn the_catalogue_is_the_registry_snapshot_each_entry_with_its_listing() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_at(dir.path(), 8);
    let builtins = CorpusReadPort::builtin_corpora(&engine);
    assert!(!builtins.is_empty(), "the bundled snapshot lists corpora");
    let unlisted: Vec<&str> = builtins
        .iter()
        .filter(|b| IngestPort::registry_listing(&engine, &b.id).is_none())
        .map(|b| b.id.as_str())
        .collect();
    assert!(
        unlisted.is_empty(),
        "catalogue entries with no registry listing: {unlisted:?}"
    );
}

const SLICE_CORPUS: &str = "origin-slice";

/// ingest_origin_e2e's recipe: a plain-text file, paragraph chunks, a fixed
/// 8-dimension embedding.
fn write_slice_recipe(dir: &Path) {
    let source = dir.join("source.txt");
    std::fs::write(
        &source,
        "The harbour master keeps two ledgers and admits to one.\n\n\
         The second is not secret, only inconvenient to explain.\n\n\
         Both agree about the tides and about nothing else.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("recipes").join(format!("{SLICE_CORPUS}.toml")),
        format!(
            "[corpus]\nid = \"{SLICE_CORPUS}\"\nname = \"Origin slice\"\n\
             description = \"pb-work-donor fixture\"\nlicense = \"CC0\"\nmesh_sharing = false\n\n\
             [acquire]\ntype = \"local_file\"\npath = \"{}\"\n\n\
             [extract]\ntype = \"plaintext\"\n\n\
             [chunk]\ntype = \"paragraph\"\nmax_chars = 512\noverlap_chars = 64\n\n\
             [index]\nembedding_model = \"test-mock\"\nembedding_dimensions = 8\n",
            source.display()
        ),
    )
    .unwrap();
}

/// ingest_origin_e2e asserts the daemon's `IngestExecutor` asks for ONE
/// slice, of the unit's recipe and range, into `partition_path`, and
/// reports the partition's chunk total. Here: that ask, made of the engine
/// through the port, writes a chunked index at exactly that path.
#[tokio::test]
async fn a_slice_ingests_into_the_engines_own_partition() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_at(dir.path(), 8);
    write_slice_recipe(dir.path());
    let port: &dyn IngestPort = &engine;
    let partition = port.partition_path(SLICE_CORPUS);
    assert!(
        partition.starts_with(dir.path().join("indexes")),
        "the partition is on this engine's disk: {}",
        partition.display()
    );

    let result = port
        .ingest_with_overrides(
            SLICE_CORPUS,
            None,
            Some((0, 100)),
            &partition,
            None,
            Some(0),
        )
        .await
        .expect("the slice ingests");
    assert!(result.chunks_created > 0, "{result:?}");
    assert!(partition.is_dir(), "{}", partition.display());
    let index = port
        .open_index(&partition)
        .await
        .expect("the partition opens");
    assert_eq!(index.chunk_count().await.unwrap(), result.chunks_created);
}

/// A word that appears in the fixture's article and nowhere else.
const MARKER: &str = "zyzzyvalattice";

/// wikipedia_fetch_e2e's fixture origin: the one-row catalog and the
/// Action API `parse` body for it.
async fn fixture_origin() -> MockServer {
    let server = MockServer::start().await;
    let catalog = serde_json::json!({
        "title": "Fixture Topic",
        "url": "https://en.wikipedia.org/wiki/Fixture_Topic",
        "abstract": "A topic that exists only in this test.",
        "sections": ["History"],
    })
    .to_string();
    Mock::given(method("GET"))
        .and(path("/catalog.jsonl"))
        .respond_with(ResponseTemplate::new(200).set_body_string(catalog))
        .mount(&server)
        .await;
    let article = serde_json::json!({ "parse": {
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
    .to_string();
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("page", "Fixture_Topic"))
        .respond_with(ResponseTemplate::new(200).set_body_string(article))
        .mount(&server)
        .await;
    server
}

/// wikipedia_fetch_e2e's two recipes as local overrides, pointed at the
/// fixture origin.
fn write_wikipedia_recipes(recipes: &Path, origin: &str) {
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

/// wikipedia_fetch_e2e asserts the tool finds the work in an installed
/// `Catalog` corpus, reads its `[catalog]` block, and asks ingest for one
/// work: the content recipe at the templated url, staged, folded into the
/// shared target. Here, on the engine: the catalog installs as a `Catalog`,
/// the tool's literal-id lookup finds its row, the block reads back, and
/// the work lands the article where the next read finds [`MARKER`], with
/// the staging corpus gone.
#[tokio::test]
async fn a_catalog_work_folds_into_the_shared_target_where_the_next_read_finds_it() {
    let dir = tempfile::tempdir().unwrap();
    let origin = fixture_origin().await;
    let dim = corpus_index::types::DEFAULT_EMBED_DIM;
    let engine = engine_at(dir.path(), dim);
    write_wikipedia_recipes(&dir.path().join("recipes"), &origin.uri());
    engine
        .ingest(&CorpusSpec::Builtin("wikipedia-catalog".into()), None)
        .await
        .expect("the fixture catalog installs");

    let installed = CorpusReadPort::installed_indexes(&engine).await.unwrap();
    let catalog = installed
        .iter()
        .find(|i| i.corpus_id == "wikipedia-catalog")
        .expect("the catalog is installed");
    assert_eq!(catalog.kind, CorpusKind::Catalog);
    let lookup = IndexSource::open_index(&engine, &catalog.path)
        .await
        .unwrap()
        .search(&[], "\"Gutenberg ID: Fixture_Topic\"", 1)
        .await
        .unwrap();
    assert!(
        !lookup.is_empty(),
        "the tool's literal-id lookup finds the work's catalog row"
    );

    let config = CorpusReadPort::catalog_config(&engine, "wikipedia-catalog")
        .await
        .unwrap()
        .expect("the catalog declares a [catalog] block");
    assert_eq!(config.content_recipe, "wikipedia-article");
    assert_eq!(
        config.target_corpus_id.as_deref(),
        Some("wikipedia-fetched")
    );

    let work = CatalogWork {
        content_recipe: config.content_recipe.clone(),
        catalog_corpus_id: "wikipedia-catalog".into(),
        download_url: config
            .download_url_template
            .replace("{id}", "Fixture_Topic"),
        staging_corpus_id: "_fetch_wikipedia-fetched-Fixture_Topic".into(),
        shared_target: Some("wikipedia-fetched".into()),
    };
    let ingested = CatalogIngestPort::ingest_catalog_work(&engine, &work, None)
        .await
        .unwrap_or_else(|e| panic!("the work ingests: {e:?}"));
    assert!(
        ingested.opts_out_of_auto_enrichment,
        "the article recipe declares [enrichment] enabled = false"
    );

    let fetched = CorpusReadPort::open_index_for_corpus(&engine, "wikipedia-fetched")
        .await
        .expect("the shared target corpus exists after the fetch");
    let hits = fetched.search(&[], MARKER, 5).await.expect("FTS read");
    assert!(
        hits.iter().any(|h| h.content.contains(MARKER)),
        "the fetched article is not readable: {} hit(s)",
        hits.len()
    );
    assert!(
        !dir.path()
            .join("indexes")
            .join(&work.staging_corpus_id)
            .exists(),
        "the staging corpus is removed after the fold"
    );
}
