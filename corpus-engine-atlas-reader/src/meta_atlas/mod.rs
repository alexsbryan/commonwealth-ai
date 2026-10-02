// SPDX-License-Identifier: AGPL-3.0-or-later
//! Meta-atlas READ half — the persisted file's types, its reader, and the
//! retrieval-time [`MetaAtlasIndex`]. The builder (walk every installed atlas,
//! write `canonical_atoms.json`) stays in corpus-engine, which re-exports
//! these at `corpus_engine::meta_atlas::` (five-programs fp-63).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use corpus_index::stream_axes::Stability;
use serde::{Deserialize, Serialize};
use understanding_vocab::articulation::ArticulationVector;
use understanding_vocab::atoms::{AtomId, ChunkRef};

pub mod index;

pub use index::MetaAtlasIndex;

/// One MetaAtom = one canonical-name equivalence class across the
/// installed atlases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaAtom {
    pub canonical_key: String,
    pub display: String,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub aliases: BTreeSet<String>,
    pub anchors: Vec<Anchor>,
}

/// One per (atlas, atom) the meta-atom is attested in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anchor {
    pub corpus_id: String,
    pub atom_id: AtomId,
    pub primary_chunk: ChunkRef,
    pub articulation: ArticulationVector,
    /// `None` when the atlas's owning corpus had no `stream` block in
    /// its `_corpus_meta.json` at build time. `sovereign corpus
    /// stream-axes` is the backfill path; meta-atlas writes the
    /// anchor anyway so retrieval still gets the per-atom
    /// articulation tag.
    pub stability: Option<Stability>,
    pub salience: f32,
    pub atlas_content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasSeen {
    pub corpus_id: String,
    pub content_hash: String,
    pub eligible_entities: usize,
    pub stability: Option<Stability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaAtlasFile {
    pub schema_version: String,
    pub built_at: u64,
    pub atlases_seen: Vec<AtlasSeen>,
    pub atoms: Vec<MetaAtom>,
}

impl MetaAtlasFile {
    pub const SCHEMA_VERSION: &'static str = "1.0";
}

/// Default path the persisted meta-atlas lives at.
pub fn default_meta_atlas_path() -> Option<PathBuf> {
    Some(
        sovereign_contracts::rebrand::svrnmesh_root()
            .join("meta-atlas")
            .join("canonical_atoms.json"),
    )
}

pub fn read_meta_atlas(path: &Path) -> std::io::Result<MetaAtlasFile> {
    let s = std::fs::read_to_string(path)?;
    serde_json::from_str(&s).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}
