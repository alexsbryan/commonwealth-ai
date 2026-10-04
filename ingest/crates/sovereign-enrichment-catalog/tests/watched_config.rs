// SPDX-License-Identifier: AGPL-3.0-or-later
//! The watched-folder config the enrichment-config port writes
//! (pb-ingest-dial-tools-close). Its own test binary because it re-roots
//! the process-global data dir (`SVRNMESH_DATA_DIR`), which the unit tests'
//! path assertions read.

use std::path::Path;

use corpus_index::ingest_port::enrich_config::{EnrichConfigPort, WatchedEnrichConfig};
use sovereign_enrichment_catalog::port::CatalogEnrichConfig;
use sovereign_enrichment_catalog::{EnrichConfig, CONFIG_SCHEMA_VERSION};

/// The bytes `sovereign-tools`' driver wrote for this folder at 451cc7ee2,
/// before the write moved behind the port, with `created_at` held out (it is
/// the clock). Derived from that commit's `synthesize_watched_config` and
/// `EnrichConfig`, both unchanged since (`git diff 451cc7ee2` is empty on the
/// schema, and the function's body hashes the same).
fn at_451cc7ee2(created_at: &str) -> String {
    format!(
        r#"{{
  "schema_version": {CONFIG_SCHEMA_VERSION},
  "corpus_id": "watched-test",
  "pipeline_id": "philosophy_atlas",
  "source_path": "/tmp/notes",
  "chapter_regex": "^.*$",
  "chat_model": "qwen3-32b",
  "embed_model": "qwen3-embedding-0.6b",
  "base_url": "http://localhost:9741",
  "min_section_body_words": 0,
  "max_output_tokens": 16384,
  "created_at": "{created_at}"
}}"#
    )
}

/// Fails if the port writes anywhere but the path `svrn enrich build` reads,
/// or writes different bytes than the driver did before the move (reorder a
/// field of the policy, or drop the watched-folder `chapter_regex`, and the
/// comparison goes red).
#[test]
fn the_port_writes_the_watched_config_the_driver_wrote_at_its_canonical_path() {
    // Re-root the data dir so the test never writes the operator's
    // ~/.svrnmesh.
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("SVRNMESH_DATA_DIR", dir.path());
    let path = CatalogEnrichConfig
        .write_watched(&WatchedEnrichConfig {
            corpus_id: "watched-test",
            pipeline_id: "philosophy_atlas",
            source_path: Path::new("/tmp/notes"),
            chat_model: "qwen3-32b",
            embed_model: "qwen3-embedding-0.6b",
            base_url: "http://localhost:9741",
        })
        .unwrap();
    assert_eq!(
        path,
        dir.path()
            .join("enrichment")
            .join("watched-test")
            .join("config.json")
    );
    let raw = std::fs::read_to_string(&path).unwrap();
    let parsed: EnrichConfig = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed.corpus_id, "watched-test");
    chrono::DateTime::parse_from_rfc3339(&parsed.created_at).unwrap();
    assert_eq!(raw, at_451cc7ee2(&parsed.created_at));

    let summary = CatalogEnrichConfig.load("watched-test").unwrap().unwrap();
    assert_eq!(summary.pipeline_id, "philosophy_atlas");
    assert!(!summary.declares_ontology);
    assert_eq!(CatalogEnrichConfig.load("never-written").unwrap(), None);
    std::env::remove_var("SVRNMESH_DATA_DIR");
}
