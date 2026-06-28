// SPDX-License-Identifier: AGPL-3.0-or-later
//! Zero-copy archive of the atlas structural graph — `atlas/atoms.rkyv`.
//!
//! See `sovereign/docs/specs/ATLAS_STORAGE.md`. `atoms.json` is a 758 MB /
//! 1.67M-atom file whose `serde_json` parse cost ~38s and ~4.5 GB resident on
//! first query (the cold-start traced 2026-06-26). This module defines a
//! **flat archived projection** the reader can `mmap` + access in place:
//!
//! * the fields the query-time consumers actually read are **structured** (so a
//!   1.67M-atom typed enumeration reads them zero-copy, no parse), and
//! * each atom's full `AtomEnvelope` is kept as a **JSON payload blob** for
//!   point-lookup fidelity (`atom_evidence`, deep reads) — re-parsed only for
//!   the handful of atoms actually touched.
//!
//! Crucially this projection derives `rkyv::Archive` on **its own** simple types
//! (String / Vec / f32 / a tag enum), NOT on `AtomEnvelope` — so it sidesteps
//! the `serde_json::Value` (`Entity.attributes`) and `PathBuf`
//! (`Asset.parsed_form`) fields that have no `Archive` impl. Phase 0 measured
//! this exact shape at 11ms load / 27MB RSS / 2ms typed scan.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::atoms::{AtomEnvelope, ChunkRef};
use super::edges::{Edge, EdgeType};

/// Stable schema tag at the head of the archive. Bump when the projected
/// layout changes so a stale `atoms.rkyv` is rejected and re-derived from
/// `atoms.json` (see the reader's convert-on-load).
///
/// v2 (2026-06-27): edges are stored as compact [`ArchEdge`] records
/// (source/target + type tag + confidence) instead of per-edge JSON
/// blobs — the wikipedia graph's 6.8M edges were ~1 GB of JSON; the
/// compact form is ~5× smaller and needs no per-edge parse on the
/// navigate hot path.
pub const ATLAS_ARCHIVE_VERSION: u32 = 2;

/// On-disk filename for the archive, beside `atoms.json` in the atlas dir.
pub const ATLAS_ARCHIVE_FILENAME: &str = "atoms.rkyv";

/// Atom-type discriminant — the structured tag that lets a typed enumeration
/// (`atoms_of_kind`) filter without parsing the payload. Mirrors
/// [`super::atoms::AtomType`].
#[derive(rkyv::Archive, rkyv::Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomKindTag {
    Entity,
    Event,
    State,
    Relation,
    Claim,
    Question,
    Configuration,
    ArgumentReconstruction,
    Position,
    Opposition,
    Asset,
}

/// Flattened evidence ref (the `Option`s collapsed to `""`). Carries the fields
/// the consumers read off `ChunkRef` (`chunk_id`, `passage_preview`,
/// `source_doc_id`).
#[derive(rkyv::Archive, rkyv::Serialize)]
pub struct ArchChunkRef {
    pub chunk_id: String,
    pub passage_preview: String,
    pub source_doc_id: String,
}

/// Edge-type discriminant — the archived mirror of [`super::edges::EdgeType`]
/// (a plain enum). Stored on [`ArchEdge`] so the navigate hot path reads the
/// type without parsing JSON. Keep the variants in sync with `EdgeType`.
#[derive(rkyv::Archive, rkyv::Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchEdgeType {
    Transition,
    Causes,
    Grounds,
    Tension,
    Involves,
    Composes,
    Configures,
    Grounding,
    Framing,
    Provenance,
    EvidenceFor,
    Concedes,
    OpposesIn,
    Attaches,
}

/// One edge, compactly: the two endpoint atom-ids + the type tag +
/// confidence. Replaces the former per-edge JSON blob — the only fields
/// the query-time consumer ([`AtlasGraph::edges_from`]/`edges_to` →
/// `atlas_navigate`) reads are `source`/`target`/`edge_type`/`confidence`,
/// so the full `Edge` envelope (id, evidence, provenance, …) is dropped.
#[derive(rkyv::Archive, rkyv::Serialize)]
pub struct ArchEdge {
    pub source: String,
    pub target: String,
    pub edge_type: ArchEdgeType,
    pub confidence: f32,
}

/// One atom: structured hot fields + the full-fidelity JSON payload.
#[derive(rkyv::Archive, rkyv::Serialize)]
pub struct AtomRecord {
    pub id: String,
    pub kind: AtomKindTag,
    /// `Entity.canonical_name` (else `""`).
    pub name: String,
    /// `Relation.label` (else `""`).
    pub label: String,
    /// `Claim.content` (else `""`).
    pub content: String,
    /// `Entity.entity_type` string repr (else `""`).
    pub subtype: String,
    /// `Entity.description` (else `""`). Feeds the atom-enumeration
    /// `embed_text` (`"{name}. {description}"`) and the Entity branch of
    /// `resolve_atom_id_from_entry`'s re-render — both are zero-copy
    /// hot paths, so the field is projected rather than payload-parsed.
    pub description: String,
    /// `Claim.quotable_excerpt` (else `""`).
    pub excerpt: String,
    /// `Claim.confidence` (0.5 default; 0.0 for non-claims).
    pub confidence: f32,
    /// `Entity.salience` (0.0 for non-entities). Prominence tie-break in
    /// the atom-enumeration path.
    pub salience: f32,
    /// `Entity.aliases` (else empty). The enumeration uses the count as
    /// a prominence tie-break; `resolve_atom_id_from_entry` joins them
    /// back into the Entity `embed_text`.
    pub aliases: Vec<String>,
    /// `Relation.participants` atom ids (else empty).
    pub participants: Vec<String>,
    /// Normalised evidence refs (per-variant, mirrors `AtlasGraph::atom_evidence`).
    pub evidence: Vec<ArchChunkRef>,
    /// The full `AtomEnvelope` as canonical JSON — re-parsed only on the rare
    /// deep/point read, never in the bulk enumeration path.
    pub payload: Vec<u8>,
}

/// The whole archived index. `by_id` / `edges_by_*` map atom-id → indices into
/// the flat `atoms` / `edges` arrays.
#[derive(rkyv::Archive, rkyv::Serialize)]
pub struct AtlasArchiveData {
    pub version: u32,
    pub atlas_corpus_id: String,
    pub article_slug: String,
    pub atoms: Vec<AtomRecord>,
    pub by_id: HashMap<String, u32>,
    pub edges: Vec<ArchEdge>,
    pub edges_by_source: HashMap<String, Vec<u32>>,
    pub edges_by_target: HashMap<String, Vec<u32>>,
}

/// Map the corpus-engine [`EdgeType`] to its archived discriminant.
pub fn arch_edge_type(t: EdgeType) -> ArchEdgeType {
    match t {
        EdgeType::Transition => ArchEdgeType::Transition,
        EdgeType::Causes => ArchEdgeType::Causes,
        EdgeType::Grounds => ArchEdgeType::Grounds,
        EdgeType::Tension => ArchEdgeType::Tension,
        EdgeType::Involves => ArchEdgeType::Involves,
        EdgeType::Composes => ArchEdgeType::Composes,
        EdgeType::Configures => ArchEdgeType::Configures,
        EdgeType::Grounding => ArchEdgeType::Grounding,
        EdgeType::Framing => ArchEdgeType::Framing,
        EdgeType::Provenance => ArchEdgeType::Provenance,
        EdgeType::EvidenceFor => ArchEdgeType::EvidenceFor,
        EdgeType::Concedes => ArchEdgeType::Concedes,
        EdgeType::OpposesIn => ArchEdgeType::OpposesIn,
        EdgeType::Attaches => ArchEdgeType::Attaches,
    }
}

/// Per-variant evidence refs — the single source of truth mirrored by the
/// reader's `atom_evidence`. Keep in sync with `AtlasGraph::atom_evidence`.
fn evidence_refs(atom: &AtomEnvelope) -> Vec<&ChunkRef> {
    match atom {
        AtomEnvelope::Entity(e) => vec![&e.first_appearance],
        AtomEnvelope::Event(ev) => ev.evidence.iter().collect(),
        AtomEnvelope::State(s) => s.evidence.iter().collect(),
        AtomEnvelope::Relation(r) => r.evidence.iter().collect(),
        AtomEnvelope::Claim(c) => c.evidence.iter().collect(),
        AtomEnvelope::Question(q) => q.raised_at.iter().collect(),
        AtomEnvelope::Configuration(cfg) => cfg.evidence.iter().collect(),
        AtomEnvelope::ArgumentReconstruction(a) => a.evidence.iter().collect(),
        AtomEnvelope::Position(p) => vec![&p.first_appearance],
        AtomEnvelope::Opposition(o) => vec![&o.first_appearance],
        AtomEnvelope::Asset(_) => Vec::new(),
    }
}

fn kind_of(atom: &AtomEnvelope) -> AtomKindTag {
    match atom {
        AtomEnvelope::Entity(_) => AtomKindTag::Entity,
        AtomEnvelope::Event(_) => AtomKindTag::Event,
        AtomEnvelope::State(_) => AtomKindTag::State,
        AtomEnvelope::Relation(_) => AtomKindTag::Relation,
        AtomEnvelope::Claim(_) => AtomKindTag::Claim,
        AtomEnvelope::Question(_) => AtomKindTag::Question,
        AtomEnvelope::Configuration(_) => AtomKindTag::Configuration,
        AtomEnvelope::ArgumentReconstruction(_) => AtomKindTag::ArgumentReconstruction,
        AtomEnvelope::Position(_) => AtomKindTag::Position,
        AtomEnvelope::Opposition(_) => AtomKindTag::Opposition,
        AtomEnvelope::Asset(_) => AtomKindTag::Asset,
    }
}

fn project(atom: &AtomEnvelope) -> AtomRecord {
    let id = atom.id().as_str().to_string();
    let kind = kind_of(atom);
    let mut name = String::new();
    let mut label = String::new();
    let mut content = String::new();
    let mut subtype = String::new();
    let mut description = String::new();
    let mut excerpt = String::new();
    let mut confidence = 0.0_f32;
    let mut salience = 0.0_f32;
    let mut aliases = Vec::new();
    let mut participants = Vec::new();
    match atom {
        AtomEnvelope::Entity(e) => {
            name = e.canonical_name.clone();
            subtype = e.entity_type.as_str_repr().to_string();
            description = e.description.clone();
            salience = e.salience;
            aliases = e.aliases.clone();
        }
        AtomEnvelope::Relation(r) => {
            label = r.label.clone();
            participants = r.participants.iter().map(|p| p.as_str().to_string()).collect();
        }
        AtomEnvelope::Claim(c) => {
            content = c.content.clone();
            excerpt = c.quotable_excerpt.clone().unwrap_or_default();
            confidence = c.confidence.unwrap_or(0.5);
        }
        _ => {}
    }
    let evidence = evidence_refs(atom)
        .into_iter()
        .map(|c| ArchChunkRef {
            chunk_id: c.chunk_id.clone(),
            passage_preview: c.passage_preview.clone().unwrap_or_default(),
            source_doc_id: c.source_doc_id.clone().unwrap_or_default(),
        })
        .collect();
    let payload = serde_json::to_vec(atom).unwrap_or_default();
    AtomRecord {
        id,
        kind,
        name,
        label,
        content,
        subtype,
        description,
        excerpt,
        confidence,
        salience,
        aliases,
        participants,
        evidence,
        payload,
    }
}

/// Build the rkyv archive bytes for an atlas. The caller writes the result to
/// `atlas/atoms.rkyv` (build-time dual-write, or the reader's convert-on-load).
pub fn build_atlas_archive_bytes(
    atlas_corpus_id: &str,
    article_slug: &str,
    atoms: &[AtomEnvelope],
    edges: &[Edge],
) -> Result<Vec<u8>, String> {
    let mut atom_records = Vec::with_capacity(atoms.len());
    let mut by_id: HashMap<String, u32> = HashMap::with_capacity(atoms.len());
    for (i, atom) in atoms.iter().enumerate() {
        let rec = project(atom);
        by_id.insert(rec.id.clone(), i as u32);
        atom_records.push(rec);
    }
    let mut edge_records: Vec<ArchEdge> = Vec::with_capacity(edges.len());
    let mut edges_by_source: HashMap<String, Vec<u32>> = HashMap::new();
    let mut edges_by_target: HashMap<String, Vec<u32>> = HashMap::new();
    for (j, edge) in edges.iter().enumerate() {
        let source = edge.source.as_str().to_string();
        let target = edge.target.as_str().to_string();
        edges_by_source.entry(source.clone()).or_default().push(j as u32);
        edges_by_target.entry(target.clone()).or_default().push(j as u32);
        edge_records.push(ArchEdge {
            source,
            target,
            edge_type: arch_edge_type(edge.edge_type),
            confidence: edge.confidence,
        });
    }
    let data = AtlasArchiveData {
        version: ATLAS_ARCHIVE_VERSION,
        atlas_corpus_id: atlas_corpus_id.to_string(),
        article_slug: article_slug.to_string(),
        atoms: atom_records,
        by_id,
        edges: edge_records,
        edges_by_source,
        edges_by_target,
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&data)
        .map_err(|e| format!("rkyv serialize atlas archive: {e}"))?;
    Ok(bytes.to_vec())
}

/// Read `atoms.json` (+ `edges.json` if present) from `atlas_dir`, build the
/// archive, and write `atoms.rkyv` beside them (atomic tmp + rename). This is
/// the **off-query-thread** build step for the install/enrich lifecycle and
/// the `sovereign atlas build-archive` CLI, so a corpus never pays the
/// convert-on-load parse on its first query (ATLAS_STORAGE.md Phase 1.5).
/// The archive's self-description (`corpus_id` / slug) is derived from the
/// caller; the reader re-derives the slug on load.
pub fn build_and_write_archive(atlas_dir: &Path, corpus_id: &str) -> Result<PathBuf, String> {
    let atoms = super::read_atlas_atoms(atlas_dir)
        .map_err(|e| format!("read atoms.json for {corpus_id}: {e}"))?;
    let edges = super::read_atlas_edges(atlas_dir)
        .map(|f| f.edges)
        .unwrap_or_default();
    let slug = corpus_id.strip_prefix("sep-").unwrap_or(corpus_id);
    let bytes = build_atlas_archive_bytes(corpus_id, slug, &atoms.atoms, &edges)?;
    let path = atlas_dir.join(ATLAS_ARCHIVE_FILENAME);
    let tmp = path.with_extension("rkyv.tmp");
    std::fs::write(&tmp, &bytes)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// True if `atoms.rkyv` is missing, or older than `atoms.json` — i.e. the
/// archive should be (re)built. A cheap mtime check: a version-stale archive
/// that is nonetheless newer than the JSON is still caught by the reader's
/// load-time version gate (which re-derives), so this need not read and
/// validate the archive header just to decide whether to rebuild.
pub fn archive_needs_build(atlas_dir: &Path) -> bool {
    let rkyv = atlas_dir.join(ATLAS_ARCHIVE_FILENAME);
    let json = atlas_dir.join("atoms.json");
    let Ok(rkyv_meta) = std::fs::metadata(&rkyv) else {
        return json.exists();
    };
    match (
        rkyv_meta.modified(),
        std::fs::metadata(&json).and_then(|m| m.modified()),
    ) {
        (Ok(rt), Ok(jt)) => jt > rt,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_access_roundtrips_kind_and_payload() {
        // Build a tiny archive from an empty atom set + verify it accesses.
        let bytes = build_atlas_archive_bytes("test-corpus", "test", &[], &[]).unwrap();
        let archived =
            rkyv::access::<ArchivedAtlasArchiveData, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(archived.version, ATLAS_ARCHIVE_VERSION);
        assert_eq!(archived.atlas_corpus_id.as_ref(), "test-corpus");
        assert_eq!(archived.atoms.len(), 0);
    }

    /// Exercises exactly the archived read APIs the L2 reader
    /// (`sovereign-core/src/atlas_context.rs`) builds on, against a
    /// hand-constructed `AtlasArchiveData` (so the probe needs no
    /// `Entity`/`Edge` constructors): `by_id`/`edges_by_*` keyed lookup
    /// by `&str`, the kind tag, projected scalar/vec fields, the
    /// evidence refs, and the JSON payload blob.
    #[test]
    fn archived_read_apis_round_trip() {
        let rec0 = AtomRecord {
            id: "entity-1".to_string(),
            kind: AtomKindTag::Entity,
            name: "Earth".to_string(),
            label: String::new(),
            content: String::new(),
            subtype: "place".to_string(),
            description: "the third planet".to_string(),
            excerpt: String::new(),
            confidence: 0.0,
            salience: 0.7,
            aliases: vec!["Terra".to_string()],
            participants: Vec::new(),
            evidence: vec![ArchChunkRef {
                chunk_id: "sec_0001".to_string(),
                passage_preview: "Earth is the third planet".to_string(),
                source_doc_id: "doc-9".to_string(),
            }],
            payload: serde_json::to_vec(&serde_json::json!({"kind":"entity"})).unwrap(),
        };
        let rec1 = AtomRecord {
            id: "rel-1".to_string(),
            kind: AtomKindTag::Relation,
            name: String::new(),
            label: "orbits".to_string(),
            content: String::new(),
            subtype: String::new(),
            description: String::new(),
            excerpt: String::new(),
            confidence: 0.0,
            salience: 0.0,
            aliases: Vec::new(),
            participants: vec!["entity-1".to_string()],
            evidence: Vec::new(),
            payload: Vec::new(),
        };
        let mut by_id = HashMap::new();
        by_id.insert("entity-1".to_string(), 0u32);
        by_id.insert("rel-1".to_string(), 1u32);
        let mut edges_by_source = HashMap::new();
        edges_by_source.insert("entity-1".to_string(), vec![0u32]);
        let mut edges_by_target = HashMap::new();
        edges_by_target.insert("rel-1".to_string(), vec![0u32]);
        let data = AtlasArchiveData {
            version: ATLAS_ARCHIVE_VERSION,
            atlas_corpus_id: "c".to_string(),
            article_slug: "c".to_string(),
            atoms: vec![rec0, rec1],
            by_id,
            edges: vec![ArchEdge {
                source: "entity-1".to_string(),
                target: "rel-1".to_string(),
                edge_type: ArchEdgeType::Involves,
                confidence: 0.9,
            }],
            edges_by_source,
            edges_by_target,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&data).unwrap();
        let a = rkyv::access::<ArchivedAtlasArchiveData, rkyv::rancor::Error>(&bytes).unwrap();

        // Keyed lookup by &str into the archived id index.
        let idx = a.by_id.get("entity-1").expect("by_id get") ;
        let i: u32 = (*idx).into();
        assert_eq!(i, 0);
        let rec = &a.atoms[i as usize];
        assert!(matches!(rec.kind, ArchivedAtomKindTag::Entity));
        assert_eq!(rec.name.as_ref(), "Earth");
        assert_eq!(rec.description.as_ref(), "the third planet");
        let sal: f32 = rec.salience.into();
        assert!((sal - 0.7).abs() < 1e-6);
        assert_eq!(rec.aliases.len(), 1);
        assert_eq!(rec.aliases[0].as_ref(), "Terra");
        assert_eq!(rec.evidence.len(), 1);
        assert_eq!(rec.evidence[0].chunk_id.as_ref(), "sec_0001");
        assert_eq!(rec.evidence[0].source_doc_id.as_ref(), "doc-9");
        // Payload blob parses back to a serde_json value.
        let v: serde_json::Value = serde_json::from_slice(rec.payload.as_ref()).unwrap();
        assert_eq!(v["kind"], "entity");

        // Edge adjacency keyed by &str; the relation's participant id.
        let outs = a.edges_by_source.get("entity-1").expect("edges_by_source get");
        assert_eq!(outs.len(), 1);
        // Compact ArchEdge read API (used by the reader's edges_from/to).
        let e0 = &a.edges[0];
        assert_eq!(e0.source.as_ref(), "entity-1");
        assert_eq!(e0.target.as_ref(), "rel-1");
        assert!(matches!(e0.edge_type, ArchivedArchEdgeType::Involves));
        let conf: f32 = e0.confidence.into();
        assert!((conf - 0.9).abs() < 1e-6);
        let rel = &a.atoms[1];
        assert_eq!(rel.participants.len(), 1);
        assert_eq!(rel.participants[0].as_ref(), "entity-1");
        assert_eq!(rel.label.as_ref(), "orbits");
        assert!(a.edges_by_source.get("missing").is_none());
    }
}
