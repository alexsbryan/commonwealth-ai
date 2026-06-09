// SPDX-License-Identifier: AGPL-3.0-or-later
//! Doc→atoms sidecar — Move 6 Phase 1.
//!
//! `atlas/doc_to_atoms.json` records which atoms each source
//! document produced. The atoms-delta primitive (Phase 2) reads this
//! to find "every atom owned by article X" in O(1) when X is being
//! re-extracted or deleted.
//!
//! ## Schema
//!
//! ```jsonc
//! {
//!   "schema_version": "1.0",
//!   "by_doc": {
//!     "Albert_Einstein": ["entity-ab12cd34ef567890", "event-..."],
//!     "Isaac_Newton":    ["entity-cd56ef78901234ab", ...]
//!   }
//! }
//! ```
//!
//! ## How `doc_id` is derived
//!
//! For each atom variant, we extract a single doc-level handle from
//! the atom's primary anchor field:
//!   - Entity / Position / Opposition: `first_appearance.chunk_id`.
//!   - Event / ArgumentReconstruction: `section_position.section_id`.
//!   - State / Relation: `section_range.start`.
//!   - Claim / Question / Configuration: first `evidence[0].chunk_id`
//!     or first `raised_at[0].chunk_id` (fallback `"unknown"` when
//!     evidence list is empty).
//!
//! For structural-first wiki atlases, `chunk_id` IS the article slug,
//! so the sidecar groups by article naturally. For literary /
//! philosophy atlases where `chunk_id` is a section id, grouping is
//! at section grain — finer than article, which is correct for
//! incremental updates (a section edit only re-extracts that section).
//!
//! Future Move (Phase 5+) can refine this via the LanceDB chunk
//! index's `source_doc_id` column when a finer-than-section grain
//! turns out to matter.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::atoms::{AtomEnvelope, AtomId, AtomsFile};

pub const DOC_TO_ATOMS_FILENAME: &str = "doc_to_atoms.json";

/// On-disk shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocToAtomsFile {
    pub schema_version: String,
    /// `doc_id → ordered list of atom ids produced from that doc`.
    /// BTreeMap so the on-disk file is deterministic across runs.
    pub by_doc: BTreeMap<String, Vec<AtomId>>,
}

impl DocToAtomsFile {
    pub const SCHEMA_VERSION: &'static str = "1.0";

    pub fn new() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION.to_string(),
            by_doc: BTreeMap::new(),
        }
    }

    /// Atoms produced by `doc_id`. Returns empty slice if the doc is
    /// not present.
    pub fn atoms_for(&self, doc_id: &str) -> &[AtomId] {
        self.by_doc.get(doc_id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// All doc ids the sidecar knows about.
    pub fn docs(&self) -> impl Iterator<Item = &String> {
        self.by_doc.keys()
    }

    pub fn len(&self) -> usize {
        self.by_doc.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_doc.is_empty()
    }
}

impl Default for DocToAtomsFile {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract the doc-level handle for an atom from its primary anchor
/// field. Returns `None` when the atom has no anchor (a degenerate
/// case the caller should treat as a "skip from doc index" signal).
pub fn extract_doc_id(env: &AtomEnvelope) -> Option<String> {
    match env {
        AtomEnvelope::Entity(e) => Some(e.first_appearance.chunk_id.clone()),
        AtomEnvelope::Position(p) => Some(p.first_appearance.chunk_id.clone()),
        AtomEnvelope::Opposition(o) => Some(o.first_appearance.chunk_id.clone()),
        AtomEnvelope::Event(e) => Some(e.section_position.section_id.clone()),
        AtomEnvelope::ArgumentReconstruction(a) => Some(a.section_position.section_id.clone()),
        AtomEnvelope::State(s) => Some(s.section_range.start.clone()),
        AtomEnvelope::Relation(r) => Some(r.section_range.start.clone()),
        AtomEnvelope::Claim(c) => c
            .evidence
            .first()
            .map(|cr| cr.chunk_id.clone())
            .or(c.anchor.clone()),
        AtomEnvelope::Question(q) => q.raised_at.first().map(|cr| cr.chunk_id.clone()),
        AtomEnvelope::Configuration(cfg) => cfg.evidence.first().map(|cr| cr.chunk_id.clone()),
        // Asset atoms are document-level not chunk-level. The
        // `first_seen_source_doc_id` field is the doc handle — when
        // empty the caller should fall back to the corpus root.
        AtomEnvelope::Asset(a) => {
            if a.first_seen_source_doc_id.is_empty() {
                None
            } else {
                Some(a.first_seen_source_doc_id.clone())
            }
        }
    }
}

/// Build a fresh doc→atoms sidecar from an in-memory atoms file.
/// Walks every atom; emits one entry per `(doc_id, atom_id)` pair.
/// Atoms without a resolvable doc_id are skipped (logged).
pub fn build_from_atoms_file(atoms: &AtomsFile) -> DocToAtomsFile {
    let mut file = DocToAtomsFile::new();
    for env in &atoms.atoms {
        let Some(doc_id) = extract_doc_id(env) else {
            tracing::debug!(
                atom_id = env.id().as_str(),
                "doc_to_atoms: atom has no anchor; skipping"
            );
            continue;
        };
        file.by_doc
            .entry(doc_id)
            .or_default()
            .push(env.id().clone());
    }
    file
}

/// Read `doc_to_atoms.json` from the atlas dir. Returns `Ok(None)`
/// when the sidecar is absent (legacy atlas), `Err` on parse failure.
pub fn read(atlas_dir: &Path) -> io::Result<Option<DocToAtomsFile>> {
    let path = atlas_dir.join(DOC_TO_ATOMS_FILENAME);
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read(&path)?;
    let file: DocToAtomsFile = serde_json::from_slice(&data).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("parse {DOC_TO_ATOMS_FILENAME}: {e}"),
        )
    })?;
    Ok(Some(file))
}

/// Atomic write of the sidecar via tmp+rename.
pub fn write(atlas_dir: &Path, file: &DocToAtomsFile) -> io::Result<()> {
    std::fs::create_dir_all(atlas_dir)?;
    let path = atlas_dir.join(DOC_TO_ATOMS_FILENAME);
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(file).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("serialise doc_to_atoms: {e}"),
        )
    })?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Build-and-persist convenience: read atoms.json, derive the
/// sidecar, write it. Used by the migration CLI to backfill the
/// sidecar for atlases that pre-date Phase 1.
pub fn build_and_write(atlas_dir: &Path) -> io::Result<DocToAtomsFile> {
    let atoms = super::writer::read_atlas_atoms(atlas_dir)?;
    let file = build_from_atoms_file(&atoms);
    write(atlas_dir, &file)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{ChunkRef, Entity};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
    use std::fs;

    fn make_entity(_idx: usize, name: &str, chunk_id: &str) -> AtomEnvelope {
        AtomEnvelope::Entity(Entity {
            id: AtomId::entity_content_hash(name, &EntityType::Person, "c"),
            canonical_name: name.into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new(chunk_id, None),
            description: "d".into(),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: vec![],
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        })
    }

    #[test]
    fn builds_groups_atoms_by_doc_id() {
        let atoms = AtomsFile::new(vec![
            make_entity(1, "Alice", "Albert_Einstein"),
            make_entity(2, "Bob", "Albert_Einstein"),
            make_entity(3, "Carol", "Isaac_Newton"),
        ]);
        let file = build_from_atoms_file(&atoms);
        assert_eq!(file.len(), 2);
        assert_eq!(file.atoms_for("Albert_Einstein").len(), 2);
        assert_eq!(file.atoms_for("Isaac_Newton").len(), 1);
        assert_eq!(file.atoms_for("missing").len(), 0);
    }

    #[test]
    fn entries_serialise_deterministically() {
        let atoms = AtomsFile::new(vec![
            make_entity(1, "Zebra", "doc_z"),
            make_entity(2, "Alpha", "doc_a"),
        ]);
        let file = build_from_atoms_file(&atoms);
        let docs: Vec<&String> = file.docs().collect();
        assert_eq!(docs, vec![&"doc_a".to_string(), &"doc_z".to_string()]);
    }

    #[test]
    fn read_write_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path();
        let atoms = AtomsFile::new(vec![make_entity(1, "Alice", "doc_a")]);
        fs::create_dir_all(atlas_dir).unwrap();
        fs::write(
            atlas_dir.join("atoms.json"),
            serde_json::to_string_pretty(&atoms).unwrap(),
        )
        .unwrap();
        let built = build_and_write(atlas_dir).unwrap();
        let read_back = read(atlas_dir).unwrap().unwrap();
        assert_eq!(read_back.len(), built.len());
        assert_eq!(read_back.atoms_for("doc_a").len(), 1);
    }

    #[test]
    fn read_returns_none_when_sidecar_absent() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path()).unwrap();
        let out = read(tmp.path()).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn write_creates_parent_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("nested/dir/atlas");
        let mut f = DocToAtomsFile::new();
        f.by_doc.insert("doc_x".into(), vec![AtomId::entity(1)]);
        write(&atlas_dir, &f).unwrap();
        assert!(atlas_dir.join(DOC_TO_ATOMS_FILENAME).exists());
    }

    #[test]
    fn build_and_write_persists_to_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let atoms = AtomsFile::new(vec![make_entity(1, "Alice", "doc_a")]);
        fs::create_dir_all(tmp.path()).unwrap();
        fs::write(
            tmp.path().join("atoms.json"),
            serde_json::to_string_pretty(&atoms).unwrap(),
        )
        .unwrap();
        build_and_write(tmp.path()).unwrap();
        assert!(tmp.path().join(DOC_TO_ATOMS_FILENAME).exists());
    }
}
