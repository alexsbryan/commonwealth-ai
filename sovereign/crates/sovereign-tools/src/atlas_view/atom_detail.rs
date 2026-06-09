// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-atom inspector data — full atom shape + related edges +
//! cross-corpus bridges, ready for the desktop's `AtomDetail.svelte`
//! to render.
//!
//! This builds on [`atom_browse`](super::atom_browse)'s in-memory
//! atoms cache: looking up an atom is a `Vec::iter().find()` over the
//! cached vec. Edges and cross-corpus edges are read fresh from disk
//! per call — atom detail is a click-driven interaction, not a
//! per-keystroke one, so the simpler "no edges cache" path is fine
//! at Phase 1 scale.

use std::collections::BTreeMap;
use std::path::Path;

use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, AtomId, AtomType, ResolutionStatus};
use corpus_engine::enrichment::atlas::cross_corpus::CrossCorpusEdge;
use corpus_engine::enrichment::atlas::edges::{Edge, EdgeType};
use corpus_engine::enrichment::atlas::{read_atlas_cross_corpus_edges, read_atlas_edges};
use serde::{Deserialize, Serialize};

use super::atom_browse::cached_atoms;
use super::reader::{CurationStatus, FileAtlasReader};
use super::stable_key::{compute_stable_key, StableAtomKey};

/// Full per-atom inspector record. Carries the entire atom envelope
/// (so the desktop renders the type-specific fields directly) plus
/// the one-hop graph context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomDetail {
    pub corpus_id: String,
    pub atom_id: AtomId,
    pub stable_key: StableAtomKey,
    pub atom_type: AtomType,
    /// Best human label for the header, computed the same way as
    /// [`AtomSummary::display_name`](super::AtomSummary::display_name).
    pub display_name: String,
    /// Scalar score where the atom carries one (Entity, Configuration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salience: Option<f32>,
    /// The atom envelope verbatim — tagged on the wire as
    /// `{"atom_type": "Entity", "data": { ... }}`. The desktop
    /// dispatches on `atom_type` to render the type-specific body.
    pub atom: AtomEnvelope,
    /// Per-atom evidence with passage previews lifted from the
    /// atom's evidence vec. Section ids are *not* resolved to
    /// numeric chunk ids here — Step 5 wires the "Open in
    /// ReadingSurface" CTA that does that resolution.
    pub evidence_excerpts: Vec<EvidenceExcerpt>,
    /// One-hop graph neighbours via intra-corpus edges. Sorted by
    /// `edge_type` then `display_name` for stable rendering.
    pub related: Vec<RelatedAtom>,
    /// Cross-corpus bridges. May be empty when the corpus has no
    /// cross-corpus edges file yet.
    pub cross_corpus: Vec<CrossCorpusLink>,
    /// Every atom id this atom references through its type-specific
    /// fields (`Claim.attributed_to`, `State.entity_id`,
    /// `Event.participants`, etc.), resolved to a display label so
    /// the desktop can render clickable atom links instead of opaque
    /// `entity-0042` mono-text. Unresolvable ids (deleted atom,
    /// dangling ref) are simply absent from the map; the frontend
    /// falls back to rendering the raw id as static text.
    pub referenced_atoms: BTreeMap<String, ReferencedAtom>,
    /// Phase 2 forward-compat — extraction run id when provenance
    /// metadata lands. `None` today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extraction_run: Option<String>,
    /// Phase 2 forward-compat — always `Generated` in Phase 1.
    pub curation_status: CurationStatus,
    /// Phase 2 forward-compat — always `false` in Phase 1.
    pub overlay_supports: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceExcerpt {
    /// Section id from the atom's `ChunkRef` (e.g., `"sec_0042"`).
    pub section_id: String,
    /// Numeric chunk id (LanceDB row id), populated by the Tauri
    /// command layer via `index.resolve_sections_to_chunks`. `None`
    /// in two cases: (1) `FileAtlasReader` is called outside the
    /// daemon (e.g., the CLI's `show-atom`), where no corpus index
    /// is open; (2) resolution failed for that specific section
    /// (deleted chunk, mismatched index). When present, the
    /// desktop's evidence row becomes clickable and opens the
    /// ReadingSurface centered on this chunk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<u64>,
    /// Verbatim passage preview from `ChunkRef.passage_preview` when
    /// the extraction populated one. `None` when the atom carries
    /// only the section anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passage_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedAtom {
    pub atom_id: AtomId,
    pub atom_type: AtomType,
    pub display_name: String,
    pub edge_type: EdgeType,
    /// `"source"` or `"target"` — describes the *other* atom's role
    /// from the perspective of the focal atom. `"source"` means the
    /// focal atom is the source of the edge and `atom_id` is its
    /// target. (String rather than `&'static str` so the struct
    /// round-trips through serde for tests; the value is always one
    /// of the two literals.)
    pub role: String,
    pub confidence: f32,
}

/// Display label for an atom referenced by another atom's body
/// (`Claim.attributed_to`, `State.entity_id`, `Configuration.
/// constituent_atoms`, …). Same shape information as
/// [`RelatedAtom`] but without the edge metadata — these references
/// come from atom-internal fields, not from edges.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferencedAtom {
    pub display_name: String,
    pub atom_type: AtomType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossCorpusLink {
    pub peer_corpus_id: String,
    pub peer_atom_id: AtomId,
    /// Canonical name on the peer side. Copied into the edge file
    /// at extraction time so a traversal needn't open the peer
    /// atlas just to label the link.
    pub peer_canonical_name: String,
    pub edge_type: EdgeType,
    /// Detector signal that produced the edge (e.g.,
    /// `"canonical_exact"`, `"alias_exact"`). Lifted from
    /// `MatchTrace.signal` so the desktop can show "matched on
    /// alias" without re-reading the trace.
    pub signal: String,
    pub confidence: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum AtomDetailError {
    #[error("corpus `{0}` has no atlas")]
    UnknownCorpus(String),
    #[error("read atoms.json: {0}")]
    ReadAtoms(#[source] std::io::Error),
    #[error("background task: {0}")]
    Task(String),
}

impl FileAtlasReader {
    /// Build the detail record for one atom. Returns `Ok(None)` when
    /// the atom id isn't present in the corpus's atoms.json.
    pub async fn get_atom_detail(
        &self,
        corpus_id: &str,
        atom_id: &str,
    ) -> Result<Option<AtomDetail>, AtomDetailError> {
        let atlas_dir = self
            .atlas_dir(corpus_id)
            .ok_or_else(|| AtomDetailError::UnknownCorpus(corpus_id.to_string()))?;
        let target = AtomId::from_raw(atom_id);
        let corpus_id_owned = corpus_id.to_string();

        let detail =
            tokio::task::spawn_blocking(move || -> Result<Option<AtomDetail>, AtomDetailError> {
                build_detail(&corpus_id_owned, &atlas_dir, &target)
            })
            .await
            .map_err(|join_err| AtomDetailError::Task(join_err.to_string()))??;

        tracing::debug!(
            corpus_id,
            atom_id,
            found = detail.is_some(),
            related_count = detail.as_ref().map(|d| d.related.len()).unwrap_or(0),
            cross_corpus_count = detail.as_ref().map(|d| d.cross_corpus.len()).unwrap_or(0),
            evidence_count = detail
                .as_ref()
                .map(|d| d.evidence_excerpts.len())
                .unwrap_or(0),
            "atlas_view:get_atom_detail",
        );
        Ok(detail)
    }
}

fn build_detail(
    corpus_id: &str,
    atlas_dir: &Path,
    target: &AtomId,
) -> Result<Option<AtomDetail>, AtomDetailError> {
    let atoms = cached_atoms(atlas_dir).map_err(AtomDetailError::ReadAtoms)?;
    let Some(atom) = atoms.iter().find(|a| a.id() == target) else {
        return Ok(None);
    };

    // Edges + cross-corpus edges — best-effort reads. A missing
    // edges.json is normal on fresh corpora (no extraction yet);
    // an unreadable one degrades the detail view but shouldn't fail
    // the request.
    let edges = read_atlas_edges(atlas_dir)
        .map(|f| f.edges)
        .unwrap_or_else(|e| {
            tracing::warn!(
                atlas_dir = %atlas_dir.display(),
                error = %e,
                "atlas_view:get_atom_detail: edges.json unreadable; rendering without related links",
            );
            Vec::new()
        });
    let cross = read_atlas_cross_corpus_edges(atlas_dir)
        .map(|f| f.edges)
        .unwrap_or_else(|e| {
            tracing::debug!(
                atlas_dir = %atlas_dir.display(),
                error = %e,
                "atlas_view:get_atom_detail: cross_corpus_edges.json absent or unreadable",
            );
            Vec::new()
        });

    let related = build_related(target, &atoms, &edges);
    let cross_corpus = build_cross_corpus(target, &cross);
    let evidence_excerpts = build_evidence(atom);
    let referenced_atoms = build_referenced_atoms(atom, &atoms);

    let detail = AtomDetail {
        corpus_id: corpus_id.to_string(),
        atom_id: target.clone(),
        stable_key: compute_stable_key(corpus_id, atom),
        atom_type: atom_type_of(atom),
        display_name: display_name_of(atom),
        salience: scalar_score(atom),
        atom: atom.clone(),
        evidence_excerpts,
        related,
        cross_corpus,
        referenced_atoms,
        extraction_run: None,
        curation_status: CurationStatus::Generated,
        overlay_supports: false,
    };
    Ok(Some(detail))
}

fn build_related(target: &AtomId, atoms: &[AtomEnvelope], edges: &[Edge]) -> Vec<RelatedAtom> {
    let mut out: Vec<RelatedAtom> = edges
        .iter()
        .filter(|e| e.source == *target || e.target == *target)
        .filter_map(|e| {
            let (other_id, role) = if e.source == *target {
                (&e.target, "target".to_string())
            } else {
                (&e.source, "source".to_string())
            };
            let other = atoms.iter().find(|a| a.id() == other_id)?;
            Some(RelatedAtom {
                atom_id: other_id.clone(),
                atom_type: atom_type_of(other),
                display_name: display_name_of(other),
                edge_type: e.edge_type,
                role,
                confidence: e.confidence,
            })
        })
        .collect();
    // Stable order — type-first, then name. Matches the pattern in
    // the existing `read_get_atom_card`'s related list.
    out.sort_by(|a, b| {
        a.edge_type
            .cmp(&b.edge_type)
            .then_with(|| a.display_name.cmp(&b.display_name))
    });
    out
}

fn build_cross_corpus(target: &AtomId, edges: &[CrossCorpusEdge]) -> Vec<CrossCorpusLink> {
    let mut out: Vec<CrossCorpusLink> = edges
        .iter()
        .filter(|e| e.edge.source == *target || e.edge.target == *target)
        .map(|e| CrossCorpusLink {
            peer_corpus_id: e.peer.corpus_id.clone(),
            peer_atom_id: e.peer.atom_id.clone(),
            peer_canonical_name: e.peer.canonical_name.clone(),
            edge_type: e.edge.edge_type,
            signal: e.trace.signal.clone(),
            confidence: e.edge.confidence,
        })
        .collect();
    out.sort_by(|a, b| {
        a.edge_type
            .cmp(&b.edge_type)
            .then_with(|| a.peer_canonical_name.cmp(&b.peer_canonical_name))
    });
    out
}

/// Collect every atom_id this atom references through its
/// type-specific fields and resolve each to a `ReferencedAtom`
/// label. Drives the desktop's `<AtomLink>` lookup so refs like
/// `attributed_to: "entity-0002"` render as clickable
/// `Entity · David Hume` chips instead of opaque ids.
fn build_referenced_atoms(
    atom: &AtomEnvelope,
    atoms: &[AtomEnvelope],
) -> BTreeMap<String, ReferencedAtom> {
    let mut refs: Vec<&AtomId> = Vec::new();
    match atom {
        AtomEnvelope::Entity(e) => {
            refs.extend(e.participants.iter());
        }
        AtomEnvelope::Event(e) => {
            refs.extend(e.participants.iter());
            refs.extend(e.causal_antecedents.iter());
        }
        AtomEnvelope::State(s) => {
            refs.push(&s.entity_id);
        }
        AtomEnvelope::Relation(r) => {
            refs.extend(r.participants.iter());
        }
        AtomEnvelope::Claim(c) => {
            if let Some(a) = &c.attributed_to {
                refs.push(a);
            }
        }
        AtomEnvelope::Question(q) => {
            refs.extend(q.addressed_by.iter());
            match &q.resolution_status {
                ResolutionStatus::Resolved { claim_id } => refs.push(claim_id),
                ResolutionStatus::Contested { claim_ids } => refs.extend(claim_ids.iter()),
                ResolutionStatus::Open | ResolutionStatus::Dissolved => {}
            }
        }
        AtomEnvelope::Configuration(c) => {
            refs.extend(c.constituent_atoms.iter());
        }
        AtomEnvelope::ArgumentReconstruction(a) => {
            if let Some(p) = &a.proponent {
                refs.push(p);
            }
        }
        AtomEnvelope::Position(p) => {
            if let Some(prop) = &p.proponent_id {
                refs.push(prop);
            }
            refs.extend(p.evidence_ids.iter());
        }
        AtomEnvelope::Opposition(o) => {
            if let Some(l) = &o.left_atom_id {
                refs.push(l);
            }
            if let Some(r) = &o.right_atom_id {
                refs.push(r);
            }
        }
        AtomEnvelope::Asset(a) => {
            if let Some(d) = &a.described_by {
                refs.push(d);
            }
        }
    }

    let mut out: BTreeMap<String, ReferencedAtom> = BTreeMap::new();
    for id in refs {
        let key = id.as_str().to_string();
        if out.contains_key(&key) {
            continue;
        }
        if let Some(target) = atoms.iter().find(|a| a.id() == id) {
            out.insert(
                key,
                ReferencedAtom {
                    display_name: display_name_of(target),
                    atom_type: atom_type_of(target),
                },
            );
        }
        // Unresolved (dangling ref) — leave absent. The frontend
        // renders the raw id as fallback text.
    }
    out
}

fn build_evidence(atom: &AtomEnvelope) -> Vec<EvidenceExcerpt> {
    let chunks: Vec<&corpus_engine::enrichment::atlas::atoms::ChunkRef> = match atom {
        AtomEnvelope::Entity(e) => vec![&e.first_appearance],
        AtomEnvelope::Event(e) => e.evidence.iter().collect(),
        AtomEnvelope::State(a) => a.evidence.iter().collect(),
        AtomEnvelope::Relation(a) => a.evidence.iter().collect(),
        AtomEnvelope::Claim(a) => a.evidence.iter().collect(),
        AtomEnvelope::Question(a) => a.raised_at.iter().collect(),
        AtomEnvelope::Configuration(a) => a.evidence.iter().collect(),
        AtomEnvelope::ArgumentReconstruction(a) => a.evidence.iter().collect(),
        AtomEnvelope::Position(p) => vec![&p.first_appearance],
        AtomEnvelope::Opposition(o) => vec![&o.first_appearance],
        // Asset atoms carry no chunk evidence — the asset IS the
        // evidence. Detail-view UI surfaces sha256/size/parsed_form
        // through a separate panel.
        AtomEnvelope::Asset(_) => Vec::new(),
    };
    chunks
        .into_iter()
        .map(|c| EvidenceExcerpt {
            section_id: c.chunk_id.clone(),
            chunk_id: None,
            passage_preview: c.passage_preview.clone(),
        })
        .collect()
}

// EdgeType doesn't derive Ord — provide a stable order ourselves.
trait EdgeTypeOrd {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering;
}

impl EdgeTypeOrd for EdgeType {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        edge_type_rank(*self).cmp(&edge_type_rank(*other))
    }
}

fn edge_type_rank(t: EdgeType) -> u8 {
    match t {
        EdgeType::Causes => 0,
        EdgeType::Transition => 1,
        EdgeType::Grounds => 2,
        EdgeType::Tension => 3,
        EdgeType::Involves => 4,
        EdgeType::Composes => 5,
        EdgeType::Configures => 6,
        EdgeType::Grounding => 7,
        EdgeType::Framing => 8,
        EdgeType::Provenance => 9,
        EdgeType::EvidenceFor => 10,
        EdgeType::Concedes => 11,
        EdgeType::OpposesIn => 12,
        EdgeType::Attaches => 13,
    }
}

// ── Helpers (duplicate-but-tiny — shared with atom_browse via
//     intentional copy. Keeping a private `atoms_helpers` mod would
//     save a few lines but tangle the call graph.) ───────────────

fn atom_type_of(atom: &AtomEnvelope) -> AtomType {
    match atom {
        AtomEnvelope::Entity(_) => AtomType::Entity,
        AtomEnvelope::Event(_) => AtomType::Event,
        AtomEnvelope::State(_) => AtomType::State,
        AtomEnvelope::Relation(_) => AtomType::Relation,
        AtomEnvelope::Claim(_) => AtomType::Claim,
        AtomEnvelope::Question(_) => AtomType::Question,
        AtomEnvelope::Configuration(_) => AtomType::Configuration,
        AtomEnvelope::ArgumentReconstruction(_) => AtomType::ArgumentReconstruction,
        AtomEnvelope::Position(_) => AtomType::Position,
        AtomEnvelope::Opposition(_) => AtomType::Opposition,
        AtomEnvelope::Asset(_) => AtomType::Asset,
    }
}

fn display_name_of(atom: &AtomEnvelope) -> String {
    const DISPLAY_NAME_TRUNCATION: usize = 120;
    fn truncate(s: &str) -> String {
        if s.chars().count() <= DISPLAY_NAME_TRUNCATION {
            return s.to_string();
        }
        let mut out: String = s.chars().take(DISPLAY_NAME_TRUNCATION).collect();
        out.push('…');
        out
    }
    match atom {
        AtomEnvelope::Entity(a) => a.canonical_name.clone(),
        AtomEnvelope::Event(a) => truncate(&a.description),
        AtomEnvelope::State(a) => a.label.clone(),
        AtomEnvelope::Relation(a) => a.label.clone(),
        AtomEnvelope::Claim(a) => truncate(&a.content),
        AtomEnvelope::Question(a) => truncate(&a.content),
        AtomEnvelope::Configuration(a) => a.label.clone(),
        AtomEnvelope::ArgumentReconstruction(a) => a.name.clone(),
        AtomEnvelope::Position(a) => a.canonical_name.clone(),
        AtomEnvelope::Opposition(a) => a.canonical_label.clone(),
        AtomEnvelope::Asset(a) => {
            if a.original_filename.is_empty() {
                format!(
                    "{} asset {}",
                    a.asset_kind,
                    &a.sha256[..12.min(a.sha256.len())]
                )
            } else {
                a.original_filename.clone()
            }
        }
    }
}

fn scalar_score(atom: &AtomEnvelope) -> Option<f32> {
    match atom {
        AtomEnvelope::Entity(a) => Some(a.salience),
        AtomEnvelope::Configuration(a) => Some(a.confidence),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::enrichment::atlas::atoms::{
        AtomEnvelope, AtomId, AtomsFile, ChunkRef, Claim, Entity,
    };
    use corpus_engine::enrichment::atlas::cross_corpus::{
        CrossCorpusEdge, CrossCorpusEdgesFile, MatchTrace, PeerAtomRef,
    };
    use corpus_engine::enrichment::atlas::edges::{
        Edge, EdgeId, EdgeProvenance, EdgeType, EdgesFile,
    };
    use corpus_engine::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EntityType, EpistemicStatus,
    };
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn entity(id: usize, name: &str, salience: f32) -> AtomEnvelope {
        AtomEnvelope::Entity(Entity {
            id: AtomId::entity(id),
            canonical_name: name.into(),
            aliases: vec![format!("{name}-alias")],
            entity_type: EntityType::Concept,
            first_appearance: ChunkRef::new(
                format!("sec_{id:04}"),
                Some(format!("preview for {name}")),
            ),
            description: format!("{name} description"),
            defining_quote: None,
            salience,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: vec![],
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        })
    }

    fn claim_with_evidence(id: usize, content: &str, chunks: &[&str]) -> AtomEnvelope {
        AtomEnvelope::Claim(Claim {
            id: AtomId::claim(id),
            content: content.into(),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: chunks
                .iter()
                .map(|c| ChunkRef::new(*c, Some(format!("text from {c}"))))
                .collect(),
            quotable_excerpt: Some("a quotable excerpt".into()),
            attributed_to: None,
            confidence: Some(0.8),
            anchor: None,
            enrichment_depth: EnrichmentDepth::Extracted,
            claim_kind: None,
            concession_outcome: None,
            evidence_kind: None,
        })
    }

    fn write_atoms(atlas_dir: &Path, atoms: Vec<AtomEnvelope>) {
        std::fs::create_dir_all(atlas_dir).unwrap();
        let file = AtomsFile::new(atoms);
        std::fs::write(
            atlas_dir.join("atoms.json"),
            serde_json::to_vec_pretty(&file).unwrap(),
        )
        .unwrap();
    }

    fn write_edges(atlas_dir: &Path, edges: Vec<Edge>) {
        std::fs::create_dir_all(atlas_dir).unwrap();
        let file = EdgesFile::new(edges);
        std::fs::write(
            atlas_dir.join("edges.json"),
            serde_json::to_vec_pretty(&file).unwrap(),
        )
        .unwrap();
    }

    fn write_cross_edges(atlas_dir: &Path, edges: Vec<CrossCorpusEdge>) {
        std::fs::create_dir_all(atlas_dir).unwrap();
        let file = CrossCorpusEdgesFile {
            schema_version: CrossCorpusEdgesFile::SCHEMA_VERSION.into(),
            local_corpus_id: "wiki".into(),
            edges,
        };
        std::fs::write(
            atlas_dir.join("cross_corpus_edges.json"),
            serde_json::to_vec_pretty(&file).unwrap(),
        )
        .unwrap();
    }

    fn make_reader_with_atoms(atoms: Vec<AtomEnvelope>) -> (TempDir, FileAtlasReader, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("wiki").join("atlas");
        write_atoms(&atlas_dir, atoms);
        let reader = FileAtlasReader::new(tmp.path().to_path_buf());
        (tmp, reader, atlas_dir)
    }

    #[tokio::test]
    async fn get_atom_detail_returns_entity_with_full_atom_envelope() {
        let (_tmp, reader, _) = make_reader_with_atoms(vec![entity(1, "Knowledge", 0.9)]);
        let detail = reader
            .get_atom_detail("wiki", "entity-0001")
            .await
            .unwrap()
            .expect("entity found");
        assert_eq!(detail.atom_id.as_str(), "entity-0001");
        assert_eq!(detail.atom_type, AtomType::Entity);
        assert_eq!(detail.display_name, "Knowledge");
        assert_eq!(detail.salience, Some(0.9));
        assert_eq!(detail.curation_status, CurationStatus::Generated);
        assert!(!detail.overlay_supports);
        assert_eq!(detail.stable_key.as_str().len(), 64);
        // Atom envelope is carried verbatim — Entity.aliases survives.
        match &detail.atom {
            AtomEnvelope::Entity(e) => {
                assert_eq!(e.aliases, vec!["Knowledge-alias".to_string()]);
                assert_eq!(e.description, "Knowledge description");
            }
            _ => panic!("expected Entity variant"),
        }
    }

    #[tokio::test]
    async fn get_atom_detail_returns_none_for_unknown_atom() {
        let (_tmp, reader, _) = make_reader_with_atoms(vec![entity(1, "Knowledge", 0.9)]);
        let detail = reader.get_atom_detail("wiki", "entity-9999").await.unwrap();
        assert!(detail.is_none());
    }

    #[tokio::test]
    async fn get_atom_detail_errors_for_unknown_corpus() {
        let (_tmp, reader, _) = make_reader_with_atoms(vec![entity(1, "Knowledge", 0.9)]);
        let err = reader
            .get_atom_detail("nonexistent", "entity-0001")
            .await
            .unwrap_err();
        assert!(matches!(err, AtomDetailError::UnknownCorpus(_)));
    }

    #[tokio::test]
    async fn get_atom_detail_lifts_evidence_excerpts_from_atom() {
        let (_tmp, reader, _) = make_reader_with_atoms(vec![claim_with_evidence(
            1,
            "Knowledge is JTB.",
            &["sec_0001", "sec_0042"],
        )]);
        let detail = reader
            .get_atom_detail("wiki", "claim-0001")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.evidence_excerpts.len(), 2);
        assert_eq!(detail.evidence_excerpts[0].section_id, "sec_0001");
        assert_eq!(
            detail.evidence_excerpts[0].passage_preview.as_deref(),
            Some("text from sec_0001"),
        );
        assert_eq!(detail.evidence_excerpts[1].section_id, "sec_0042");
    }

    #[tokio::test]
    async fn get_atom_detail_evidence_for_entity_uses_first_appearance() {
        let (_tmp, reader, _) = make_reader_with_atoms(vec![entity(1, "Knowledge", 0.9)]);
        let detail = reader
            .get_atom_detail("wiki", "entity-0001")
            .await
            .unwrap()
            .unwrap();
        // Entity has a single ChunkRef in `first_appearance`, not a
        // Vec<ChunkRef>. The detail surfaces it as a one-element
        // excerpt list so the UI doesn't need a per-type branch.
        assert_eq!(detail.evidence_excerpts.len(), 1);
        assert_eq!(detail.evidence_excerpts[0].section_id, "sec_0001");
        assert_eq!(
            detail.evidence_excerpts[0].passage_preview.as_deref(),
            Some("preview for Knowledge"),
        );
    }

    #[tokio::test]
    async fn get_atom_detail_collects_one_hop_related_atoms() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("wiki").join("atlas");
        write_atoms(
            &atlas_dir,
            vec![
                entity(1, "Knowledge", 0.9),
                entity(2, "Belief", 0.6),
                entity(3, "Justification", 0.5),
            ],
        );
        // Knowledge → Belief (Grounds), Justification → Knowledge
        // (Involves). Reader picks up both with proper "source" /
        // "target" role from Knowledge's perspective.
        write_edges(
            &atlas_dir,
            vec![
                Edge {
                    id: EdgeId::new(1),
                    edge_type: EdgeType::Grounds,
                    source: AtomId::entity(1),
                    target: AtomId::entity(2),
                    evidence: vec![],
                    trigger_event: None,
                    sub_question: None,
                    confidence: 0.85,
                    provenance: EdgeProvenance::LlmExtraction,
                },
                Edge {
                    id: EdgeId::new(2),
                    edge_type: EdgeType::Involves,
                    source: AtomId::entity(3),
                    target: AtomId::entity(1),
                    evidence: vec![],
                    trigger_event: None,
                    sub_question: None,
                    confidence: 1.0,
                    provenance: EdgeProvenance::Derived,
                },
            ],
        );
        let reader = FileAtlasReader::new(tmp.path().to_path_buf());
        let detail = reader
            .get_atom_detail("wiki", "entity-0001")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.related.len(), 2);
        // Grounds before Involves in our ranking.
        assert_eq!(detail.related[0].edge_type, EdgeType::Grounds);
        assert_eq!(detail.related[0].display_name, "Belief");
        // Knowledge is the source of edge 1, so the other (Belief)
        // is in the "target" role.
        assert_eq!(detail.related[0].role, "target");
        assert_eq!(detail.related[1].edge_type, EdgeType::Involves);
        assert_eq!(detail.related[1].display_name, "Justification");
        // Knowledge is the target of edge 2, so the other
        // (Justification) is the source.
        assert_eq!(detail.related[1].role, "source");
    }

    #[tokio::test]
    async fn get_atom_detail_handles_missing_edges_file() {
        // No edges.json on disk — detail still works, related list
        // is just empty. (Fresh corpora before any edge extraction.)
        let (_tmp, reader, _) = make_reader_with_atoms(vec![entity(1, "Knowledge", 0.9)]);
        let detail = reader
            .get_atom_detail("wiki", "entity-0001")
            .await
            .unwrap()
            .unwrap();
        assert!(detail.related.is_empty());
        assert!(detail.cross_corpus.is_empty());
    }

    #[tokio::test]
    async fn get_atom_detail_resolves_atom_id_references() {
        // A Claim with `attributed_to` pointing at an Entity in the
        // same corpus. `referenced_atoms` must surface a display
        // label so the desktop can render the ref as a clickable
        // AtomLink rather than opaque "entity-0001".
        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("wiki").join("atlas");
        let mut hume = entity(1, "David Hume", 0.95);
        // Sanity: the entity carries a known canonical_name.
        if let AtomEnvelope::Entity(ref e) = hume {
            assert_eq!(e.canonical_name, "David Hume");
        }
        // Lift the underlying atom_id text so the claim points at it.
        let hume_id = hume.id().clone();
        let claim_with_attribution = AtomEnvelope::Claim(Claim {
            id: AtomId::claim(1),
            content: "Custom is the great guide of human life.".into(),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: vec![],
            quotable_excerpt: None,
            attributed_to: Some(hume_id.clone()),
            confidence: None,
            anchor: None,
            enrichment_depth: EnrichmentDepth::Extracted,
            claim_kind: None,
            concession_outcome: None,
            evidence_kind: None,
        });
        // Drop unused Initiative-style fields on the entity so the
        // serde round-trip stays clean.
        if let AtomEnvelope::Entity(ref mut e) = hume {
            e.participants.clear();
        }
        write_atoms(&atlas_dir, vec![hume.clone(), claim_with_attribution]);
        let reader = FileAtlasReader::new(tmp.path().to_path_buf());
        let detail = reader
            .get_atom_detail("wiki", "claim-0001")
            .await
            .unwrap()
            .unwrap();
        let label = detail
            .referenced_atoms
            .get(hume_id.as_str())
            .expect("attributed_to entity resolved");
        assert_eq!(label.display_name, "David Hume");
        assert_eq!(label.atom_type, AtomType::Entity);
    }

    #[tokio::test]
    async fn get_atom_detail_referenced_atoms_omits_dangling_refs() {
        // Claim points at an entity id that doesn't exist in
        // atoms.json (deleted on re-extraction, or never resolved).
        // The frontend falls back to rendering the raw id; the map
        // entry must be absent, not a placeholder.
        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("wiki").join("atlas");
        let dangling_claim = AtomEnvelope::Claim(Claim {
            id: AtomId::claim(1),
            content: "x".into(),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: vec![],
            quotable_excerpt: None,
            attributed_to: Some(AtomId::from_raw("entity-9999".to_string())),
            confidence: None,
            anchor: None,
            enrichment_depth: EnrichmentDepth::Extracted,
            claim_kind: None,
            concession_outcome: None,
            evidence_kind: None,
        });
        write_atoms(&atlas_dir, vec![dangling_claim]);
        let reader = FileAtlasReader::new(tmp.path().to_path_buf());
        let detail = reader
            .get_atom_detail("wiki", "claim-0001")
            .await
            .unwrap()
            .unwrap();
        assert!(detail.referenced_atoms.is_empty());
    }

    #[tokio::test]
    async fn get_atom_detail_collects_cross_corpus_links() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("wiki").join("atlas");
        write_atoms(&atlas_dir, vec![entity(1, "Knowledge", 0.9)]);
        write_cross_edges(
            &atlas_dir,
            vec![CrossCorpusEdge {
                edge: Edge {
                    id: EdgeId::new(1),
                    edge_type: EdgeType::Grounding,
                    source: AtomId::entity(1),
                    target: AtomId::from_raw("entity-0007".to_string()),
                    evidence: vec![],
                    trigger_event: None,
                    sub_question: None,
                    confidence: 0.95,
                    provenance: EdgeProvenance::Derived,
                },
                peer: PeerAtomRef {
                    corpus_id: "sep-epistemology".into(),
                    atom_id: AtomId::from_raw("entity-0007".to_string()),
                    canonical_name: "Knowledge (SEP)".into(),
                },
                trace: MatchTrace {
                    detector: "grounding".into(),
                    signal: "canonical_exact".into(),
                    local_form: "knowledge".into(),
                    peer_form: "knowledge".into(),
                    confidence: 1.0,
                    rejected_alternatives: vec![],
                },
            }],
        );
        let reader = FileAtlasReader::new(tmp.path().to_path_buf());
        let detail = reader
            .get_atom_detail("wiki", "entity-0001")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.cross_corpus.len(), 1);
        assert_eq!(detail.cross_corpus[0].peer_corpus_id, "sep-epistemology");
        assert_eq!(
            detail.cross_corpus[0].peer_canonical_name,
            "Knowledge (SEP)"
        );
        assert_eq!(detail.cross_corpus[0].edge_type, EdgeType::Grounding);
        assert_eq!(detail.cross_corpus[0].signal, "canonical_exact");
    }
}
