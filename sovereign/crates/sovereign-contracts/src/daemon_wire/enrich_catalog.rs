// SPDX-License-Identifier: AGPL-3.0-or-later
//! The enrichment store's inventory read, at the contract floor.
//!
//! Enumerates `<store-root>/*/config.json` and projects the four fields the
//! corpus list renders ([`EnrichedCorpusSummary`]). This is the READ half of
//! `sovereign_enrichment_catalog` lifted down so `sovereign-daemon` can answer
//! `GET /internal/corpus/enriched` — and name the wire shape it returns —
//! without an edge into that capabilities crate.
//!
//! The WRITER half stays there, and must: the full
//! `sovereign_enrichment_catalog::EnrichConfig` carries
//! `ontology: Option<CustomAtlasSpec>`, a knowledge-layer type, so naming it
//! here would pull the knowledge layer below the contract seam and give the
//! thin desktop/mobile surfaces a transitive backend link (layer-gate). The
//! projection below reads only the four string fields the wire shape needs;
//! every other field in the file is ignored.
//!
//! Parity with the writer's reader is deliberate: a config whose projected
//! fields are missing, or whose `schema_version` is NEWER than this binary
//! supports, is skipped with a warning — the same two rules
//! `sovereign_enrichment_catalog::EnrichConfig::parse_checked` applies. The one
//! documented difference: a config missing a field this projection does not
//! render still lists, because the inventory renders only these four.

use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};

use super::EnrichedCorpusSummary;

/// The four `config.json` fields [`EnrichedCorpusSummary`] renders, projected
/// off the writer's richer schema. Private — the public surface is the summary.
#[derive(Debug, Deserialize)]
struct EnrichConfigProjection {
    schema_version: u32,
    corpus_id: String,
    pipeline_id: String,
    source_path: String,
    created_at: String,
}

/// The config schema this binary writes and reads. ONE definition: the
/// catalog's `config.rs` re-exports this (ARCH §8).
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

impl EnrichConfigProjection {
    fn into_summary(self) -> EnrichedCorpusSummary {
        EnrichedCorpusSummary {
            corpus_id: self.corpus_id,
            pipeline_id: self.pipeline_id,
            source_path: self.source_path,
            created_at: self.created_at,
        }
    }
}

/// Every enrichment workspace under `root` with a loadable `config.json`,
/// newest first by `created_at`.
///
/// `root` is the store directory (`<data-root>/enrichment`), passed explicitly
/// because the daemon's data root is not this process's default root on an
/// attached boot. An absent or empty store is `Ok(vec![])`: no corpora is a
/// fact about the store, not a failure to read it.
pub fn list_enriched_corpora_in(root: &Path) -> Result<Vec<EnrichedCorpusSummary>> {
    list_in(root)
}

/// Every corpus id with a workspace directory under `root`, sorted.
///
/// Directory presence is the only test — a workspace mid-init has a directory
/// before it has a config, and callers that need a loadable config want
/// [`list_enriched_corpora_in`].
fn ids_in(root: &Path) -> Result<Vec<String>> {
    if !root.exists() {
        tracing::debug!(root = %root.display(), "enrich catalog: no store on disk yet");
        return Ok(Vec::new());
    }
    let entries =
        std::fs::read_dir(root).map_err(|e| Error::Storage(format!("{}: {e}", root.display())))?;
    let mut ids: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        match entry.file_name().to_str() {
            Some(s) => ids.push(s.to_string()),
            // Reported, not swallowed: a non-UTF-8 directory name is a corpus
            // this process cannot address, and a silent skip reads as absence.
            None => tracing::warn!(
                entry = ?entry.file_name(),
                "enrich catalog: skipping non-UTF-8 workspace name"
            ),
        }
    }
    ids.sort();
    Ok(ids)
}

fn list_in(root: &Path) -> Result<Vec<EnrichedCorpusSummary>> {
    let mut out = Vec::new();
    let mut unreadable = 0usize;
    for corpus_id in ids_in(root)? {
        let config_path = root.join(&corpus_id).join("config.json");
        if !config_path.exists() {
            continue;
        }
        match load_at(&config_path) {
            Ok(cfg) => out.push(cfg.into_summary()),
            Err(e) => {
                unreadable += 1;
                tracing::warn!(
                    corpus_id = %corpus_id,
                    path = %config_path.display(),
                    error = %e,
                    "enrich catalog: skipping workspace with an unreadable config"
                );
            }
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    tracing::debug!(
        root = %root.display(),
        listed = out.len(),
        unreadable,
        "enrich catalog: inventory"
    );
    Ok(out)
}

/// Load a projection from an explicit path. The path is already in hand (the
/// caller enumerated it), so this reads exactly the file it found rather than
/// re-resolving one from the corpus id. The writer's `EnrichConfig` remains the
/// authority on what a config MEANS; this reads the four fields the list shows.
fn load_at(path: &Path) -> Result<EnrichConfigProjection> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| Error::Storage(format!("{}: {e}", path.display())))?;
    let cfg: EnrichConfigProjection = serde_json::from_str(&raw).map_err(|e| {
        Error::Serialization(format!(
            "enrich config {} is malformed: {e}",
            path.display()
        ))
    })?;
    if cfg.schema_version > CONFIG_SCHEMA_VERSION {
        return Err(Error::Serialization(format!(
            "enrich config {} has schema_version {} but this binary supports {}",
            path.display(),
            cfg.schema_version,
            CONFIG_SCHEMA_VERSION
        )));
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(root: &Path, corpus_id: &str, created_at: &str) {
        let dir = root.join(corpus_id);
        std::fs::create_dir_all(&dir).unwrap();
        let body = serde_json::json!({
            "schema_version": 1,
            "corpus_id": corpus_id,
            "pipeline_id": "literary_atlas",
            "source_path": format!("/tmp/{corpus_id}.txt"),
            "created_at": created_at,
        });
        std::fs::write(
            dir.join("config.json"),
            serde_json::to_string_pretty(&body).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn absent_store_lists_empty_rather_than_erroring() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("never-created");
        assert!(list_in(&missing).unwrap().is_empty());
        assert!(ids_in(&missing).unwrap().is_empty());
    }

    #[test]
    fn lists_newest_first_and_projects_four_fields() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), "older", "2026-01-01T00:00:00Z");
        write_config(tmp.path(), "newer", "2026-06-01T00:00:00Z");
        let rows = list_in(tmp.path()).unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| r.corpus_id.as_str())
                .collect::<Vec<_>>(),
            vec!["newer", "older"]
        );
        assert_eq!(rows[0].pipeline_id, "literary_atlas");
        assert_eq!(rows[0].source_path, "/tmp/newer.txt");
        assert_eq!(rows[0].created_at, "2026-06-01T00:00:00Z");
    }

    #[test]
    fn a_workspace_with_no_config_is_an_id_but_not_a_listing() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), "ready", "2026-01-01T00:00:00Z");
        std::fs::create_dir_all(tmp.path().join("mid-init")).unwrap();
        assert_eq!(ids_in(tmp.path()).unwrap(), vec!["mid-init", "ready"]);
        assert_eq!(list_in(tmp.path()).unwrap().len(), 1);
    }

    #[test]
    fn invalid_json_is_skipped_not_listed() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), "good", "2026-01-01T00:00:00Z");
        let bad = tmp.path().join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("config.json"), "not json at all").unwrap();
        let rows = list_in(tmp.path()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].corpus_id, "good");
    }

    /// A config missing one of the four projected fields is UNLOADABLE and is
    /// skipped, matching `EnrichConfig::parse_checked` (which requires those
    /// fields too). Pinned so the next reader finds the decision.
    #[test]
    fn a_config_missing_a_projected_field_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("partial");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), r#"{"corpus_id":"partial"}"#).unwrap();
        let rows = list_in(tmp.path()).unwrap();
        assert!(rows.is_empty(), "an unloadable config must not be listed");
    }
}
