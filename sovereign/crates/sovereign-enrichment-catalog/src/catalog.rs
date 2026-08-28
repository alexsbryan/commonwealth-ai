// SPDX-License-Identifier: AGPL-3.0-or-later
//! What is in the enrichment store — the inventory side of the catalog.
//!
//! `sovereign-desktop` carried this loop inline, reading each `config.json`
//! through a `serde_json::Value` and naming `pipeline_id`, `source_path` and
//! `created_at` as string literals, so a rename in [`crate::config`] could not
//! reach it and a malformed file was skipped in silence. It now goes through
//! [`EnrichConfig`], which is the one decider for what that file means.
//!
//! ONE BEHAVIOUR CHANGE, named rather than smuggled (ARCH_PRINCIPLES §18.3):
//! the hand-rolled reader accepted a config missing any required field and
//! listed the corpus with empty strings in its place. [`list_enriched_corpora`]
//! skips it and says so at `warn`. A config `svrn enrich build` cannot load is
//! not a workspace the user can act on, and listing it invites a click that
//! fails with a worse message.

use std::path::Path;

use corpus_engine::error::{Error, Result};
use serde::Serialize;

use crate::config::EnrichConfig;
use crate::paths;

/// One enrichment workspace, as the corpus list wants to show it.
#[derive(Debug, Clone, Serialize)]
pub struct EnrichedCorpusSummary {
    pub corpus_id: String,
    pub pipeline_id: String,
    /// The configured source, rendered for display. `EnrichConfig::source_path`
    /// is a `PathBuf`; this is the string a UI puts on screen.
    pub source_path: String,
    pub created_at: String,
}

impl EnrichedCorpusSummary {
    fn from_config(cfg: &EnrichConfig) -> Self {
        Self {
            corpus_id: cfg.corpus_id.clone(),
            pipeline_id: cfg.pipeline_id.clone(),
            source_path: cfg.source_path.display().to_string(),
            created_at: cfg.created_at.clone(),
        }
    }
}

/// Every corpus id that has an enrichment workspace directory, sorted.
///
/// Directory presence is the ONLY test — a workspace mid-`enrich init` has a
/// directory before it has a config. Callers that need a loadable config want
/// [`list_enriched_corpora`] instead.
///
/// Returns `Ok(vec![])` when the enrichment tree does not exist yet: no
/// corpora is a fact about the store, not a failure to read it.
pub fn enriched_corpus_ids() -> Result<Vec<String>> {
    ids_in(&paths::enrichment_dir())
}

/// Every enrichment workspace with a config this binary can load, newest
/// first by `created_at`.
///
/// Returns `Ok(vec![])` when the enrichment tree does not exist yet.
pub fn list_enriched_corpora() -> Result<Vec<EnrichedCorpusSummary>> {
    list_in(&paths::enrichment_dir())
}

fn ids_in(root: &Path) -> Result<Vec<String>> {
    if !root.exists() {
        tracing::debug!(root = %root.display(), "enrichment catalog: no store on disk yet");
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(root).map_err(|e| io_at(e, root))?;
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
                "enrichment catalog: skipping non-UTF-8 workspace name"
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
            Ok(cfg) => out.push(EnrichedCorpusSummary::from_config(&cfg)),
            Err(e) => {
                unreadable += 1;
                tracing::warn!(
                    corpus_id = %corpus_id,
                    path = %config_path.display(),
                    error = %e,
                    "enrichment catalog: skipping workspace with an unloadable config"
                );
            }
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    tracing::debug!(
        root = %root.display(),
        listed = out.len(),
        unreadable,
        "enrichment catalog: inventory"
    );
    Ok(out)
}

/// `corpus_engine::Error::Io` carries the OS error but not what was being
/// read; a bare `?` here yields "Permission denied" with no path. Re-wrapping
/// keeps the `ErrorKind` (callers may still match on it) and names the file.
fn io_at(e: std::io::Error, path: &Path) -> Error {
    Error::Io(std::io::Error::new(
        e.kind(),
        format!("{}: {e}", path.display()),
    ))
}

/// Load a config from an explicit path. [`EnrichConfig::load`] resolves the
/// path from the corpus id via the shared accessor, which is what every
/// production caller wants; the inventory already holds the path and must not
/// re-resolve it, so that it reads exactly the file it enumerated.
fn load_at(path: &Path) -> Result<EnrichConfig> {
    let raw = std::fs::read_to_string(path).map_err(|e| io_at(e, path))?;
    EnrichConfig::parse_checked(&raw, path)
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
            "chapter_regex": "^Chapter",
            "chat_model": "c",
            "embed_model": "e",
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
    fn lists_newest_first_by_created_at() {
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
    }

    #[test]
    fn a_workspace_with_no_config_is_an_id_but_not_a_listing() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), "ready", "2026-01-01T00:00:00Z");
        std::fs::create_dir_all(tmp.path().join("mid-init")).unwrap();
        assert_eq!(ids_in(tmp.path()).unwrap(), vec!["mid-init", "ready"]);
        let rows = list_in(tmp.path()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].corpus_id, "ready");
    }

    #[test]
    fn an_unloadable_config_is_skipped_not_listed_with_blanks() {
        // The behaviour change named in the module docs, pinned as a test so
        // the next reader finds the decision rather than re-deriving it.
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), "good", "2026-01-01T00:00:00Z");
        let bad = tmp.path().join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        // Valid JSON, missing every required field — exactly what the
        // hand-rolled desktop reader used to list with empty strings.
        std::fs::write(bad.join("config.json"), r#"{"corpus_id":"bad"}"#).unwrap();
        let rows = list_in(tmp.path()).unwrap();
        assert_eq!(rows.len(), 1, "the malformed workspace must not be listed");
        assert_eq!(rows[0].corpus_id, "good");
    }

    #[test]
    fn a_config_from_a_newer_binary_is_not_listed() {
        // One decider for "loadable" (`EnrichConfig::parse_checked`). Listing a
        // workspace whose schema_version this binary refuses would offer the
        // user a corpus `svrn enrich build` then declines — the reader/writer
        // disagreement this crate exists to remove, in miniature.
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), "current", "2026-01-01T00:00:00Z");
        let future = tmp.path().join("from-the-future");
        std::fs::create_dir_all(&future).unwrap();
        let body = serde_json::json!({
            "schema_version": crate::CONFIG_SCHEMA_VERSION + 1,
            "corpus_id": "from-the-future",
            "pipeline_id": "literary_atlas",
            "source_path": "/tmp/f.txt",
            "chapter_regex": "^Chapter",
            "chat_model": "c",
            "embed_model": "e",
            "created_at": "2026-07-01T00:00:00Z",
        });
        std::fs::write(
            future.join("config.json"),
            serde_json::to_string_pretty(&body).unwrap(),
        )
        .unwrap();
        let rows = list_in(tmp.path()).unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| r.corpus_id.as_str())
                .collect::<Vec<_>>(),
            vec!["current"],
            "a config this binary cannot load must not be listed"
        );
    }

    #[test]
    fn a_file_in_the_store_root_is_not_a_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("stray.json"), "{}").unwrap();
        assert!(ids_in(tmp.path()).unwrap().is_empty());
    }
}
