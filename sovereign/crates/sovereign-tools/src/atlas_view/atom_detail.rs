// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-atom inspector data — full atom shape + related edges +
//! cross-corpus bridges, ready for the desktop's `AtomDetail.svelte`
//! to render.
//!
//! This builds on [`atom_browse`](super::atom_browse)'s in-memory
//! atoms cache: looking up an atom is a `Vec::iter().find()` over the
//! cached vec. Edges and cross-corpus edges are likewise cached
//! process-globally by file mtime+size (see [`cached_edges`]).
//!
//! The original design read edges.json fresh per click on the
//! assumption that atom detail is click-driven, not per-keystroke, so
//! the parse cost didn't matter. That assumption broke on the Wikipedia
//! atlas, whose edges.json is 1.3 GB: re-deserialising it on every
//! article click cost ~a minute EACH time. The edges cache makes the
//! first click on a corpus pay the parse once and every later click a
//! read-lock + Arc clone, mirroring the atoms cache.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Instant;

use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, AtomId, AtomType};
use corpus_engine::enrichment::atlas::cross_corpus::CrossCorpusEdge;
use corpus_engine::enrichment::atlas::edges::{Edge, EdgeType};
use corpus_engine::enrichment::atlas::{
    read_atlas_cross_corpus_edges, read_atlas_edges, StableAtomKey,
};
use serde::{Deserialize, Serialize};

use super::atom_browse::{cached_atoms, AtomQueryError};
use super::reader::{CurationStatus, FileAtlasReader};
use super::DISPLAY_NAME_TRUNCATION;

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
    /// Serialised as `canonical_name` — the wire name the other two
    /// producers of this shape (`sovereign_mesh::RelatedAtom` and the
    /// desktop's `RelatedAtomDto`) already emit, and the one the TS
    /// `RelatedAtom` interface declares. Emitting `display_name` here
    /// made the atlas Explore path the odd one out: the related-name
    /// chip read `r.canonical_name`, got `undefined`, and rendered a
    /// row of blank buttons. The Rust-side field keeps its name
    /// because that is what a related atom IS to this module — the
    /// rename is about converging ONE wire key (ARCH §10.6), not
    /// about renaming the concept. Do NOT extend this to
    /// `AtomDetail::display_name`: that is the FOCAL atom's name and
    /// its wire key is correct.
    #[serde(rename = "canonical_name")]
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

impl FileAtlasReader {
    /// Build the detail record for one atom. Returns `Ok(None)` when
    /// the atom id isn't present in the corpus's atoms.json.
    pub async fn get_atom_detail(
        &self,
        corpus_id: &str,
        atom_id: &str,
    ) -> Result<Option<AtomDetail>, AtomQueryError> {
        let atlas_dir = self
            .atlas_dir(corpus_id)
            .ok_or_else(|| AtomQueryError::UnknownCorpus(corpus_id.to_string()))?;
        let target = AtomId::from_raw(atom_id);
        let corpus_id_owned = corpus_id.to_string();

        let started = Instant::now();
        let detail =
            tokio::task::spawn_blocking(move || -> Result<Option<AtomDetail>, AtomQueryError> {
                build_detail(&corpus_id_owned, &atlas_dir, &target)
            })
            .await
            .map_err(|join_err| AtomQueryError::Task(join_err.to_string()))??;

        tracing::debug!(
            corpus_id,
            atom_id,
            elapsed_ms = started.elapsed().as_millis() as u64,
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

// ── Edge caches ──────────────────────────────────────────────
//
// Mirrors `atom_browse::cached_atoms`. Keyed by the edge file's
// mtime+size so an external atlas rebuild invalidates the entry. The
// first atom-detail click on a corpus pays the deserialisation (1.3 GB
// for Wikipedia); every later click is a read-lock + Arc clone.

struct CachedEdges {
    mtime_ms: u64,
    size_bytes: u64,
    edges: Arc<Vec<Edge>>,
}

fn edges_cache() -> &'static RwLock<HashMap<PathBuf, CachedEdges>> {
    static CACHE: OnceLock<RwLock<HashMap<PathBuf, CachedEdges>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

struct CachedCrossEdges {
    mtime_ms: u64,
    size_bytes: u64,
    edges: Arc<Vec<CrossCorpusEdge>>,
}

fn cross_edges_cache() -> &'static RwLock<HashMap<PathBuf, CachedCrossEdges>> {
    static CACHE: OnceLock<RwLock<HashMap<PathBuf, CachedCrossEdges>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn file_mtime_size(meta: &std::fs::Metadata) -> (u64, u64) {
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    (mtime_ms, meta.len())
}

/// Intra-corpus edges for an atlas, cached by edges.json mtime+size.
/// A missing edges.json is a normal fresh-corpus state — the
/// `io::Error` propagates and the caller renders without related links.
fn cached_edges(atlas_dir: &Path) -> std::io::Result<Arc<Vec<Edge>>> {
    let path = atlas_dir.join("edges.json");
    let (mtime_ms, size_bytes) = file_mtime_size(&std::fs::metadata(&path)?);
    {
        let read = edges_cache().read().expect("edges cache rwlock poisoned");
        if let Some(entry) = read.get(atlas_dir) {
            if entry.mtime_ms == mtime_ms && entry.size_bytes == size_bytes {
                return Ok(Arc::clone(&entry.edges));
            }
        }
    }
    // Cold path — the expensive full parse. Log it: this is the ~1-min
    // Wikipedia stall, and the log makes "why did the first click hang?"
    // self-answering (and any regression re-parsing per click visible).
    let started = Instant::now();
    let file = read_atlas_edges(atlas_dir)?;
    let count = file.edges.len();
    let edges = Arc::new(file.edges);
    edges_cache()
        .write()
        .expect("edges cache rwlock poisoned")
        .insert(
            atlas_dir.to_path_buf(),
            CachedEdges {
                mtime_ms,
                size_bytes,
                edges: Arc::clone(&edges),
            },
        );
    tracing::info!(
        target: "atlas_view",
        atlas_dir = %atlas_dir.display(),
        edges = count,
        bytes = size_bytes,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "atlas_view: parsed + cached edges.json (cold path; subsequent atom clicks reuse this)",
    );
    Ok(edges)
}

/// Cross-corpus edges for an atlas, cached like [`cached_edges`].
/// Usually absent (returns `Err`, treated as "no bridges"), but a
/// large cross_corpus_edges.json would hit the same per-click cost.
fn cached_cross_corpus_edges(atlas_dir: &Path) -> std::io::Result<Arc<Vec<CrossCorpusEdge>>> {
    let path = atlas_dir.join("cross_corpus_edges.json");
    let (mtime_ms, size_bytes) = file_mtime_size(&std::fs::metadata(&path)?);
    {
        let read = cross_edges_cache()
            .read()
            .expect("cross edges cache rwlock poisoned");
        if let Some(entry) = read.get(atlas_dir) {
            if entry.mtime_ms == mtime_ms && entry.size_bytes == size_bytes {
                return Ok(Arc::clone(&entry.edges));
            }
        }
    }
    let file = read_atlas_cross_corpus_edges(atlas_dir)?;
    let edges = Arc::new(file.edges);
    cross_edges_cache()
        .write()
        .expect("cross edges cache rwlock poisoned")
        .insert(
            atlas_dir.to_path_buf(),
            CachedCrossEdges {
                mtime_ms,
                size_bytes,
                edges: Arc::clone(&edges),
            },
        );
    Ok(edges)
}

fn build_detail(
    corpus_id: &str,
    atlas_dir: &Path,
    target: &AtomId,
) -> Result<Option<AtomDetail>, AtomQueryError> {
    let atoms = cached_atoms(atlas_dir).map_err(AtomQueryError::ReadAtoms)?;
    // Index atoms by id ONCE (one O(n) pass) so the target lookup and the
    // per-neighbour / per-reference lookups below are O(1) instead of a
    // full Vec scan each. Wikipedia has 1.69M atoms and hub entities carry
    // 1000+ edges, so the old `atoms.iter().find()` per neighbour was
    // O(neighbours × atoms) — ~20s for a hub node even with edges cached.
    // Keyed by the id's string form to avoid requiring Hash on AtomId.
    let by_id: HashMap<&str, &AtomEnvelope> = atoms.iter().map(|a| (a.id().as_str(), a)).collect();
    let Some(atom) = by_id.get(target.as_str()).copied() else {
        return Ok(None);
    };

    // Edges + cross-corpus edges — best-effort reads. A missing
    // edges.json is normal on fresh corpora (no extraction yet);
    // an unreadable one degrades the detail view but shouldn't fail
    // the request.
    let edges = cached_edges(atlas_dir).unwrap_or_else(|e| {
        tracing::warn!(
            atlas_dir = %atlas_dir.display(),
            error = %e,
            "atlas_view:get_atom_detail: edges.json unreadable; rendering without related links",
        );
        Arc::new(Vec::new())
    });
    let cross = cached_cross_corpus_edges(atlas_dir).unwrap_or_else(|e| {
        tracing::debug!(
            atlas_dir = %atlas_dir.display(),
            error = %e,
            "atlas_view:get_atom_detail: cross_corpus_edges.json absent or unreadable",
        );
        Arc::new(Vec::new())
    });

    let related = build_related(target, &by_id, &edges);
    let cross_corpus = build_cross_corpus(target, &cross);
    let evidence_excerpts = build_evidence(atom);
    let referenced_atoms = build_referenced_atoms(atom, &by_id);

    let detail = AtomDetail {
        corpus_id: corpus_id.to_string(),
        atom_id: target.clone(),
        stable_key: atom.stable_key(corpus_id),
        atom_type: atom.atom_type(),
        display_name: atom.display_name(Some(DISPLAY_NAME_TRUNCATION)),
        salience: atom.salience(),
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

fn build_related(
    target: &AtomId,
    by_id: &HashMap<&str, &AtomEnvelope>,
    edges: &[Edge],
) -> Vec<RelatedAtom> {
    let mut out: Vec<RelatedAtom> = edges
        .iter()
        .filter(|e| e.source == *target || e.target == *target)
        .filter_map(|e| {
            let (other_id, role) = if e.source == *target {
                (&e.target, "target".to_string())
            } else {
                (&e.source, "source".to_string())
            };
            let other = by_id.get(other_id.as_str()).copied()?;
            Some(RelatedAtom {
                atom_id: other_id.clone(),
                atom_type: other.atom_type(),
                display_name: other.display_name(Some(DISPLAY_NAME_TRUNCATION)),
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

/// Resolve the atoms this one references to display labels. Drives the
/// desktop's `<AtomLink>` lookup so refs like `attributed_to: "entity-0002"`
/// render as clickable `Entity · David Hume` chips instead of opaque ids.
///
/// WHICH ids an atom references is the atom's own shape, so it comes from
/// [`AtomEnvelope::referenced_atom_ids`]; an eleven-arm copy of that fan-out
/// reading each variant's private fields lived here until 2026-08-20. What is
/// left is the part this view actually owns: turning ids into labels, and
/// deciding that a dangling ref renders as its raw id rather than failing.
fn build_referenced_atoms(
    atom: &AtomEnvelope,
    by_id: &HashMap<&str, &AtomEnvelope>,
) -> BTreeMap<String, ReferencedAtom> {
    let mut out: BTreeMap<String, ReferencedAtom> = BTreeMap::new();
    for id in atom.referenced_atom_ids() {
        let key = id.as_str().to_string();
        if out.contains_key(&key) {
            continue;
        }
        if let Some(target) = by_id.get(id.as_str()).copied() {
            out.insert(
                key,
                ReferencedAtom {
                    display_name: target.display_name(Some(DISPLAY_NAME_TRUNCATION)),
                    atom_type: target.atom_type(),
                },
            );
        }
        // Unresolved (dangling ref) — leave absent. The frontend renders the
        // raw id as fallback text.
    }
    out
}

fn build_evidence(atom: &AtomEnvelope) -> Vec<EvidenceExcerpt> {
    // `AtomEnvelope::evidence()` is the canonical per-variant accessor and its
    // doc comment forbids re-matching the variants "so a new atom kind can't
    // silently escape evidence checks". This function carried a byte-identical
    // copy of that match until 2026-08-20.
    let chunks = atom.evidence();
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

// The three helpers that lived here — `atom_type_of`, `display_name_of` and
// `scalar_score` — were byte-identical copies of `atom_browse`'s, kept under a
// comment that called the duplication intentional. They are accessors on
// `AtomEnvelope` now; see the note at the same place in `atom_browse`.

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
        assert!(matches!(err, AtomQueryError::UnknownCorpus(_)));
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

    /// The related-atom NAME travels as `canonical_name` on the wire.
    ///
    /// Every other test in this module asserts the Rust field
    /// (`related[0].display_name`), which a `#[serde(rename)]` does not
    /// touch — so the whole suite stayed green while the desktop's
    /// Explore tab rendered a column of nameless buttons. Three
    /// producers emit this shape (`sovereign_mesh::RelatedAtom`, the
    /// desktop's `RelatedAtomDto`, and this one) and the TS
    /// `RelatedAtom` interface reads `canonical_name` from all three;
    /// this module was the one that disagreed. The assertion is on
    /// serialised JSON because the wire key IS the contract here.
    #[tokio::test]
    async fn related_atom_serialises_its_name_as_canonical_name() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("wiki").join("atlas");
        write_atoms(
            &atlas_dir,
            vec![entity(1, "Knowledge", 0.9), entity(2, "Belief", 0.6)],
        );
        write_edges(
            &atlas_dir,
            vec![Edge {
                id: EdgeId::new(1),
                edge_type: EdgeType::Grounds,
                source: AtomId::entity(1),
                target: AtomId::entity(2),
                evidence: vec![],
                trigger_event: None,
                sub_question: None,
                confidence: 0.85,
                provenance: EdgeProvenance::LlmExtraction,
            }],
        );
        let reader = FileAtlasReader::new(tmp.path().to_path_buf());
        let detail = reader
            .get_atom_detail("wiki", "entity-0001")
            .await
            .unwrap()
            .unwrap();

        let json = serde_json::to_value(&detail).unwrap();
        let related = &json["related"][0];
        assert_eq!(
            related["canonical_name"], "Belief",
            "the desktop reads r.canonical_name; anything else renders blank",
        );
        assert!(
            related.get("display_name").is_none(),
            "two keys for one name is how the frontend ends up reading the wrong one",
        );

        // The FOCAL atom keeps `display_name` — it is a different
        // field with a different consumer, and the rename above must
        // not spread to it.
        assert_eq!(json["display_name"], "Knowledge");
        assert!(json.get("canonical_name").is_none());
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
