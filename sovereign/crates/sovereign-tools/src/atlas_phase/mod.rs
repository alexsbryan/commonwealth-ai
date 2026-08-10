// SPDX-License-Identifier: AGPL-3.0-or-later
//! Literary-atlas enrichment phases as workflow leaves.
//!
//! Each *deterministic* atlas phase (cluster, resolve, tensions, gaps) is one
//! atomic algorithm over the resolved-atlas JSON — a legitimate `tool:` leaf
//! wrapping the real corpus-engine function (NOT a subsystem-in-a-tool: it's a
//! single op, like `corpus_store` wraps `insert_batch`). The LLM phases stay
//! `model:` step compositions, never leaves. Chaining these leaves + the model
//! steps as a workflow re-expresses the whole bespoke `enrich build` pipeline as
//! a composition — the migration the substrate was built for.
//!
//! Phases operate on the canonical corpus dirs (`~/.svrnmesh/indexes/<corpus>/
//! atlas/`) — the same files the bespoke `enrich` commands and the retrieval
//! path read — so a workflow-built atlas is a drop-in for the bespoke one.

pub mod gaps;
pub mod tensions;

use std::path::PathBuf;

/// `~/.svrnmesh/indexes` (or the `index_dir` param) — the canonical corpus root,
/// derived from the same home-dir resolution as the setup config.
pub(crate) fn default_index_dir() -> PathBuf {
    sovereign_core::setup_config::SetupConfig::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join("indexes")
}

/// The atlas dir for a corpus: `<index_dir>/<corpus>/atlas`.
pub(crate) fn atlas_dir_for(params: &serde_json::Value, corpus: &str) -> PathBuf {
    let index_dir = params
        .get("index_dir")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(default_index_dir);
    index_dir
        .join(corpus)
        .join(corpus_engine::enrichment::atlas::ATLAS_DIRNAME)
}
