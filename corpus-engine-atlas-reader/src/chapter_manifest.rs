// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-corpus chapter manifest — the type and its READ half.
//!
//! Lives at `~/.svrnmesh/indexes/<corpus>/chapters.json` — corpus
//! state, not enrichment state, so it's in the index root alongside
//! `_corpus_meta.json` rather than under the enrichment tree.
//!
//! Carved from corpus-engine's `enrichment::pipeline::chapter_manifest` by
//! fp-60 (FIVE_PROGRAMS §12 decision 1) so [`crate::context::read_section_rows`]
//! can parse it here. The writes (`save`, `from_detected_sections`,
//! `merge_characters_present`) stay in corpus-engine on its
//! `ChapterManifestWrite` trait; corpus-engine re-exports this type at its
//! historical path.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use corpus_index::error::{Error, Result};
/// Stable on-disk manifest of chapters (or the domain-equivalent unit
/// of composition) for one corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterManifest {
    pub corpus_id: String,
    pub schema_version: u32,
    pub chapters: Vec<ChapterEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterEntry {
    pub id: String,
    pub title: String,

    /// Structured hierarchy when the detector surfaced it. Free to be
    /// `None` for flat corpora (e.g. Moby Dick only has chapters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapter: Option<u32>,

    pub first_line: String,
    pub word_count: u64,

    /// Chunk IDs (in the corpus's LanceDB index) that fall inside
    /// this chapter's body. Populated post-ingest.
    #[serde(default)]
    pub chunk_ids: Vec<u64>,

    /// Thematic carriers identified by the phase 1 extractor.
    /// Populated post-run; safely no-ops on fresh manifests.
    #[serde(default)]
    pub characters_present: Vec<String>,

    /// Remaining detector metadata the runner didn't elevate to a
    /// structured column (e.g. detector ordinal, byte offsets).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub metadata: std::collections::BTreeMap<String, String>,
}

impl ChapterManifest {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn new(corpus_id: impl Into<String>) -> Self {
        Self {
            corpus_id: corpus_id.into(),
            schema_version: Self::SCHEMA_VERSION,
            chapters: Vec::new(),
        }
    }

    pub fn default_path(index_root: &Path) -> PathBuf {
        index_root.join("chapters.json")
    }

    /// Load a manifest from disk, tolerating a missing file.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(path)?;
        let m: Self = serde_json::from_str(&raw).map_err(|e| {
            Error::Serialization(format!(
                "chapter manifest {} parse error: {}",
                path.display(),
                e
            ))
        })?;
        if m.schema_version > Self::SCHEMA_VERSION {
            return Err(Error::Serialization(format!(
                "chapter manifest {} has schema_version {} but this binary supports {}",
                path.display(),
                m.schema_version,
                Self::SCHEMA_VERSION
            )));
        }
        Ok(Some(m))
    }

    pub fn get(&self, chapter_id: &str) -> Option<&ChapterEntry> {
        self.chapters.iter().find(|c| c.id == chapter_id)
    }

    pub fn get_mut(&mut self, chapter_id: &str) -> Option<&mut ChapterEntry> {
        self.chapters.iter_mut().find(|c| c.id == chapter_id)
    }

    pub fn chapter_ids(&self) -> Vec<&str> {
        self.chapters.iter().map(|c| c.id.as_str()).collect()
    }

    pub fn len(&self) -> usize {
        self.chapters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chapters.is_empty()
    }
}
