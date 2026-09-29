// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine half of sovereign-daemon's tests that READ through ingest's
//! ports (pb-ingest-dial-daemon-tests-reads, phase-b-47/52). The daemon's
//! surface_parity, recipe_surface and lc_surface now drive
//! `IngestPortDouble`; what those tests program the double to answer is
//! pinned here, through the port svrn holds (`&dyn IngestPort`,
//! `&dyn LocalCorpusPort`), never the same-named inherent methods, over the
//! same fixtures.
//!
//! - `corpus_status_rows` is `scan_corpus_rows` on the index dir, bytes and
//!   labels: a ready canonical and an in-flight partition, one row each.
//! - A valid recipe installs verbatim under `<recipes>/<id>/recipe.toml`
//!   with a registry digest, and its `[parameters]` read back through the
//!   same registry; a text that is not a recipe has no id.
//! - The dry run's verdict is the strict one: an empty `corpus.name` fails
//!   naming the field, offline leaves reachability unasked, and a sample of
//!   one local file is one chunked record.
//! - `source_file_progress` counts `Complete` entries of the corpus dir's
//!   manifest, and a dir with none answers `None`.
//!
//! Reads that are pure delegation to the leaf (`installed_indexes`,
//! `usable_indexes`, the opens) are proven where `FsIndexSource` lives; the
//! handle cache the engine adds past `CorpusIndex::open` is
//! index_cache_residency's.

use std::path::Path;
use std::sync::Arc;

use corpus_engine::engine::status::scan_corpus_rows;
use corpus_engine::{CorpusEngine, SourceFileManifest, SourceFileRecord, SourceFileStatus};
use corpus_index::corpus::Corpus;
use corpus_index::ingest_port::daemon::IngestPort;
use corpus_index::ingest_port::LocalCorpusPort;
use corpus_index::types::EmbedFn;

fn engine_at(dir: &Path) -> CorpusEngine {
    let embed: EmbedFn = Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.0_f32; 8]) }));
    std::fs::create_dir_all(dir.join("indexes")).unwrap();
    std::fs::create_dir_all(dir.join("recipes")).unwrap();
    CorpusEngine::new(dir.join("recipes"), dir.join("indexes"), embed)
}

fn port(engine: &CorpusEngine) -> &dyn IngestPort {
    engine
}

/// surface_parity's fixture: a corpus meta at `dir`, mid-ingest or built.
fn write_fixture_meta(dir: &Path, corpus_id: &str, ingestion_in_progress: bool) {
    std::fs::create_dir_all(dir).unwrap();
    let meta = serde_json::json!({
        "corpus_id": corpus_id,
        "corpus_name": format!("{corpus_id} (fixture)"),
        "embedding_model": "qwen-embedding-0.6b",
        "embedding_dimensions": 1024,
        "mesh_sharing": false,
        "license": "private",
        "created_at": 1_786_548_248_u64,
        "last_updated": 1_786_548_248_u64,
        "schema_version": 3,
        "is_shard": false,
        "ingestion_in_progress": ingestion_in_progress,
        "indexes_built": !ingestion_in_progress,
    });
    std::fs::write(
        Corpus::meta_in(dir),
        serde_json::to_string_pretty(&meta).unwrap(),
    )
    .unwrap();
}

#[test]
fn the_status_rows_are_the_deciders_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = engine_at(tmp.path());
    let indexes = tmp.path().join("indexes");
    write_fixture_meta(&indexes.join("ready-corpus"), "ready-corpus", false);
    let partition = engine.partition_path("building-corpus");
    write_fixture_meta(&partition, "building-corpus", true);

    let served = port(&engine).corpus_status_rows().unwrap();
    let decided = serde_json::to_value(scan_corpus_rows(&indexes).unwrap()).unwrap();
    assert_eq!(served, decided, "the port answers the one decider's bytes");

    let label = |id: &str| {
        served
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["corpus_id"] == id)
            .unwrap_or_else(|| panic!("no row for {id} in {served}"))["state_label"]
            .clone()
    };
    assert_eq!(served.as_array().unwrap().len(), 2, "{served}");
    assert_eq!(label("ready-corpus"), "ready");
    assert_eq!(label("building-corpus"), "building");
}

/// recipe_surface's `RECIPE_TOML`, byte for byte.
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

#[tokio::test]
async fn an_installed_recipe_lands_verbatim_and_its_parameters_read_back() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = engine_at(tmp.path());
    let recipes = tmp.path().join("recipes");

    assert_eq!(
        port(&engine).recipe_corpus_id(RECIPE_TOML).unwrap(),
        "author-test"
    );
    assert!(
        port(&engine)
            .recipe_corpus_id("[corpus]\nid = \"broken\"\n")
            .is_err(),
        "a text with no `[acquire]`/`[extract]`/`[chunk]` is not a recipe"
    );

    let landed = port(&engine).install_local_recipe(RECIPE_TOML).unwrap();
    assert_eq!(landed, recipes.join("author-test").join("recipe.toml"));
    assert_eq!(std::fs::read_to_string(&landed).unwrap(), RECIPE_TOML);
    let registry = std::fs::read_to_string(recipes.join("registry.toml")).unwrap();
    assert!(
        registry.contains("id = \"author-test\"") && registry.contains("sha256 = \""),
        "the local registry carries the entry and its digest:\n{registry}"
    );

    let schema = port(&engine)
        .recipe_parameter_schema("author-test")
        .await
        .unwrap();
    assert_eq!(schema.corpus_id, "author-test");
    assert_eq!(schema.parameters.len(), 2);
    let find = |name: &str| schema.parameters.iter().find(|p| p.name == name).unwrap();
    let since = find("since");
    assert_eq!((since.kind.as_str(), since.required), ("date", false));
    assert_eq!(since.default, Some(serde_json::json!("2020-01-01")));
    let tickers = find("tickers");
    assert_eq!((tickers.kind.as_str(), tickers.required), ("list", true));
    assert_eq!(tickers.default, None);

    assert!(
        port(&engine)
            .recipe_parameter_schema("no-such-recipe")
            .await
            .is_err(),
        "an unknown recipe has no schema"
    );
}

/// recipe_surface's `local_recipe`: a local-file acquirer, so a sampled run
/// acquires, extracts and chunks with no network.
fn local_recipe(source: &Path, name: &str) -> String {
    format!(
        r#"
[corpus]
id = "dry-run-test"
name = "{name}"
description = "a recipe tested over a local file"
license = "Public Domain"
size_compressed_gb = 0.001
size_indexed_gb = 0.001

[acquire]
type = "local_file"
path = "{path}"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
max_chars = 2048
overlap_chars = 0

[index]
fts = true
vector = false
"#,
        path = source.display()
    )
}

const SAMPLE_TEXT: &str = "Alpha paragraph, long enough to survive a chunker.\n\n\
                           Beta paragraph, likewise long enough to be kept.\n";

#[tokio::test]
async fn the_dry_run_verdict_is_the_strict_one() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = engine_at(tmp.path());
    let source = tmp.path().join("source.txt");
    std::fs::write(&source, SAMPLE_TEXT).unwrap();
    let staged = |name: &str, file: &str| {
        let path = tmp.path().join(file);
        std::fs::write(&path, local_recipe(&source, name)).unwrap();
        path
    };

    let ok = staged("Dry run test", "ok.toml");
    let report = port(&engine).dry_run_recipe(&ok, 0, true).await.unwrap();
    assert!(report.passed, "{report:#?}");
    assert_eq!(report.recipe_id, "dry-run-test");
    assert_eq!(report.recipe_name, "Dry run test");
    assert_eq!(report.records_attempted, 0, "validation only");
    assert_eq!(report.source_reachable, None, "offline: not asked");
    assert!(report.report_markdown.contains("Dry run test"));

    let online = port(&engine).dry_run_recipe(&ok, 0, false).await.unwrap();
    assert!(online.source_reachable.is_some(), "not offline: asked");

    let unnamed = staged("", "unnamed.toml");
    let report = port(&engine)
        .dry_run_recipe(&unnamed, 0, true)
        .await
        .unwrap();
    assert!(!report.passed, "an empty `corpus.name` fails: {report:#?}");
    assert!(
        report.errors.iter().any(|e| e.contains("corpus.name")),
        "the error names the field: {report:#?}"
    );

    let sampled = port(&engine).dry_run_recipe(&ok, 5, true).await.unwrap();
    assert!(sampled.passed, "{sampled:#?}");
    assert_eq!(sampled.records_attempted, 1, "one local file, one record");
    assert!(sampled.total_chunks >= 1, "the sample really chunked");
}

#[test]
fn source_file_progress_counts_the_manifests_complete_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = engine_at(tmp.path());
    let dir = tmp.path().join("indexes").join("c");
    std::fs::create_dir_all(&dir).unwrap();
    let local: &dyn LocalCorpusPort = &engine;
    assert!(
        local.source_file_progress(&dir).is_none(),
        "no manifest, no progress"
    );

    let record = |i: usize, status| SourceFileRecord {
        file_index: i,
        filename: format!("f{i}.md"),
        size_bytes: 1,
        status,
    };
    let done = || SourceFileStatus::Complete {
        chunks_indexed: 1,
        completed_at: chrono::Utc::now(),
    };
    SourceFileManifest::new(
        "c",
        "c",
        vec![
            record(0, done()),
            record(1, done()),
            record(2, SourceFileStatus::Pending),
        ],
    )
    .save(&dir)
    .unwrap();
    let progress = local.source_file_progress(&dir).unwrap();
    assert_eq!((progress.done, progress.total), (2, 3));
}
