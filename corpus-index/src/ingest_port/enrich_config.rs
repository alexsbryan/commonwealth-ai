// SPDX-License-Identifier: AGPL-3.0-or-later
//! The enrichment-config port (pb-ingest-dial-tools-close, FIVE_PROGRAMS §2c).
//!
//! A corpus's enrichment config (`<data root>/enrichment/<corpus>/config.json`)
//! is ingest's: its schema names engine types, so its owner,
//! `sovereign-enrichment-catalog`, implements this port and svrn reads and
//! writes the file only through it. A process that composes no ingest program
//! holds no implementor, and each svrn site that needs one reports ingest
//! absent by name.

use std::path::{Path, PathBuf};

use crate::Result;

/// What svrn decides on from a corpus's enrichment config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichConfigSummary {
    /// The atlas pipeline the config names.
    pub pipeline_id: String,
    /// The config carries a recipe-declared ontology: its map is the
    /// author's, written by the build.
    pub declares_ontology: bool,
}

/// The inputs of a watched folder's enrichment config. The implementor owns
/// the rest of the schema and the watched-folder policy.
#[derive(Debug, Clone, Copy)]
pub struct WatchedEnrichConfig<'a> {
    /// The corpus the config configures.
    pub corpus_id: &'a str,
    /// The atlas pipeline to build.
    pub pipeline_id: &'a str,
    /// The watched folder.
    pub source_path: &'a Path,
    /// Chat model the build's LLM phases route through.
    pub chat_model: &'a str,
    /// Embedding model the resolution phases use.
    pub embed_model: &'a str,
    /// Base URL of the daemon the build calls.
    pub base_url: &'a str,
}

/// Read and write a corpus's enrichment config.
pub trait EnrichConfigPort: Send + Sync {
    /// The corpus's config, or `Ok(None)` when none is written.
    fn load(&self, corpus_id: &str) -> Result<Option<EnrichConfigSummary>>;

    /// Write a watched folder's config and return the path it landed at.
    fn write_watched(&self, config: &WatchedEnrichConfig<'_>) -> Result<PathBuf>;
}
