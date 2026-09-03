// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atom browse + filter API for the desktop's per-corpus inspector.
//!
//! `list_corpora` (in `reader.rs`) answers "which corpora have an
//! atlas?". This module answers the next question: "within one
//! corpus, show me atoms — filterable by type, searchable by name,
//! paginated." The cache below means that once a user opens a
//! wikipedia-scale atlas, subsequent keystrokes in the search box
//! don't re-deserialise the ~50 MB atoms.json.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, AtomId, AtomType, AtomsFile};
use corpus_engine::enrichment::atlas::{read_atlas_atoms, StableAtomKey};
use corpus_engine::enrichment::pipeline::atlas::EnrichmentDepth;
use serde::{Deserialize, Serialize};

use super::reader::{CurationStatus, FileAtlasReader};
use super::DISPLAY_NAME_TRUNCATION;

/// Server-side filter the desktop's `AtlasCorpusView` posts on every
/// keystroke / tab switch. All fields are independent — an unset
/// field is "match anything". Filtering is case-insensitive on
/// `name_query` and substring (not regex).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AtomFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atom_type: Option<AtomType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_query: Option<String>,
    /// Inclusive lower bound. `None` = no minimum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_salience: Option<f32>,
    /// The author's own nouns — declared subtype names, each matched EXACTLY
    /// against `projection::subtype_of` (ontology-v1 P6). Empty matches
    /// anything; otherwise an atom passes if its subtype is ANY of these.
    ///
    /// Independent of `atom_type` like every other field here: a `role_of`
    /// type lands as a State on a person atom, so requiring the caller to also
    /// pick the right kind would make `ruler` unfindable.
    ///
    /// A LIST rather than one name, because the server must never walk the
    /// declared hierarchy: `coin` and `sceatta` are separate names here, and a
    /// caller wanting the family names both. That keeps one decider for the
    /// hierarchy — the viewer, reading the `specializes` edges the corpus
    /// summary carries (§10.6) — while letting a "13 coins" badge and the 15
    /// rows a click returns be the same question asked once. A single-name
    /// field could not express the family, so the badge and the list
    /// disagreed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtypes: Vec<String>,
}

/// Pagination cursor. Phase 1: simple offset+limit. Future-proof for
/// a real cursor (e.g. opaque token) if disk-side pagination lands;
/// the wire shape can grow a `next_token: Option<String>` field
/// without breaking existing clients.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PageCursor {
    pub offset: usize,
    pub limit: usize,
}

impl PageCursor {
    pub fn first(limit: usize) -> Self {
        Self { offset: 0, limit }
    }
}

impl Default for PageCursor {
    /// 200 atoms is enough to fill a scroll-virtualised viewport
    /// twice over without sending wiki-scale payloads on every
    /// keystroke. Matches the Phase 1 plan.
    fn default() -> Self {
        Self {
            offset: 0,
            limit: 200,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomListPage {
    pub items: Vec<AtomSummary>,
    /// Total atoms that matched the filter across the whole atlas
    /// (not just this page). Drives "1–200 of 14,732" labels.
    pub total_matching: u64,
    /// Offset to pass back in the next request for the following
    /// page, or `None` when this page exhausted the filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
}

/// Compact per-atom record for the browse list. The full atom shape
/// (type-specific fields, full evidence, related edges) lives in
/// Step 4's `AtomDetail`. `AtomSummary` is what fits one row in the
/// virtualised list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomSummary {
    pub atom_id: AtomId,
    pub stable_key: StableAtomKey,
    pub atom_type: AtomType,
    /// The author's own noun for this atom, when it has one — the value a
    /// declared corpus's rows are labelled and filtered by.
    ///
    /// An ENTITY always has one: a declared name (`coin`), or one of the
    /// generic six (`person`, `concept`, …), which is what the kind-based UI
    /// already shows. `None` means the atom genuinely carries no subtype —
    /// an unclassified Relation, Event or State — so a viewer falls back to
    /// `atom_type` rather than rendering a blank chip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    /// Best human-facing label for the row — `canonical_name` /
    /// `label` / `name` depending on type; `content` (truncated) for
    /// Claim and Question which lack a short name.
    pub display_name: String,
    /// `Some` only for Entity (`salience`) and Configuration
    /// (`confidence`). Other atom types don't carry a single scalar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salience: Option<f32>,
    pub enrichment_depth: EnrichmentDepth,
    /// How many chunks of evidence the atom carries. `Entity` always
    /// reports 1 (its `first_appearance` is a single `ChunkRef`).
    pub evidence_chunk_count: u32,
    /// Phase 2 forward-compat — always `Generated` today. Once the
    /// overlay lands the wire shape doesn't change.
    pub curation_status: CurationStatus,
    /// Phase 2 forward-compat — `false` today. UIs can already
    /// branch on this to hide the (empty) edit affordances slot.
    pub overlay_supports: bool,
    /// Unix seconds of the most recent (re)index of this atom's source
    /// document, when known. `Some` means the doc was refreshed *after*
    /// the bulk install — e.g. a wikipedia-newsworthy fetch or a
    /// watched-folder edit — so the backend bubbles it to the top and
    /// the UI can render a "fresh" marker. `None` is baseline
    /// (install-time) content with no recorded recency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

impl FileAtlasReader {
    /// Browse atoms within one corpus.
    ///
    /// First call per atlas pays the atoms.json deserialisation cost
    /// (cached after that). Each subsequent call — filter changes,
    /// search keystrokes — walks the cached `Vec<AtomEnvelope>` in
    /// memory. Cache invalidates when atoms.json's mtime + size
    /// change (matching the v2 summary cache key).
    pub async fn list_atoms(
        &self,
        corpus_id: &str,
        filter: AtomFilter,
        page: PageCursor,
    ) -> Result<AtomListPage, AtomQueryError> {
        let atlas_dir = self
            .atlas_dir(corpus_id)
            .ok_or_else(|| AtomQueryError::UnknownCorpus(corpus_id.to_string()))?;

        let corpus_id_owned = corpus_id.to_string();
        let filter_for_task = filter.clone();
        // Cache hit returns instantly; cache miss pays the
        // serde-deserialise cost on the blocking pool so we don't
        // freeze the runtime for wiki-scale reads.
        let page = tokio::task::spawn_blocking(move || -> Result<AtomListPage, AtomQueryError> {
            let atoms = cached_atoms(&atlas_dir).map_err(AtomQueryError::ReadAtoms)?;
            // Per-doc recency lives one level up from the atlas dir —
            // `<indexes>/<corpus>/_doc_freshness.json`, beside the
            // `atlas/` subdir. Missing sidecar → empty map → insertion
            // order, so a never-reindexed corpus renders unchanged.
            let freshness = atlas_dir
                .parent()
                .map(corpus_engine::freshness::load_doc_freshness)
                .unwrap_or_default();
            Ok(filter_and_page(
                &corpus_id_owned,
                &atoms,
                &filter_for_task,
                page,
                &freshness,
            ))
        })
        .await
        .map_err(|join_err| AtomQueryError::Task(join_err.to_string()))??;

        tracing::debug!(
            corpus_id,
            atom_type = ?filter.atom_type,
            name_query = filter.name_query.as_deref().unwrap_or(""),
            offset = page.next_offset.map(|o| o.saturating_sub(page.items.len())).unwrap_or(0),
            returned = page.items.len(),
            total_matching = page.total_matching,
            "atlas_view:list_atoms",
        );
        Ok(page)
    }
}

/// What can go wrong answering an atom query — browse, detail or subgraph.
/// One type for all three: `atom_detail` declared a byte-identical
/// `AtomDetailError` (same three variants, same three messages) until
/// 2026-08-20. Two names, one concept.
#[derive(Debug, thiserror::Error)]
pub enum AtomQueryError {
    #[error("corpus `{0}` has no atlas")]
    UnknownCorpus(String),
    #[error("read atoms.json: {0}")]
    ReadAtoms(#[source] std::io::Error),
    #[error("background task: {0}")]
    Task(String),
}

// ── Cache ────────────────────────────────────────────────────

/// One atlas's deserialised atoms vec, keyed by atoms.json mtime+size
/// so an external mutation invalidates the cache.
#[derive(Debug)]
struct CachedAtoms {
    mtime_ms: u64,
    size_bytes: u64,
    atoms: Arc<Vec<AtomEnvelope>>,
}

fn cache() -> &'static RwLock<HashMap<PathBuf, CachedAtoms>> {
    static CACHE: OnceLock<RwLock<HashMap<PathBuf, CachedAtoms>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Return the atoms vec for an atlas. Hot path: read-lock + Arc clone.
/// Cold path: re-read atoms.json, write the cache, return. Shared
/// with [`super::atom_detail`] — both browse and detail want the
/// same in-memory copy.
pub(super) fn cached_atoms(atlas_dir: &Path) -> std::io::Result<Arc<Vec<AtomEnvelope>>> {
    let atoms_path = atlas_dir.join("atoms.json");
    let meta = std::fs::metadata(&atoms_path)?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let size_bytes = meta.len();

    // Fast path — read lock, return Arc clone if key matches.
    {
        let read = cache().read().expect("atoms cache rwlock poisoned");
        if let Some(entry) = read.get(atlas_dir) {
            if entry.mtime_ms == mtime_ms && entry.size_bytes == size_bytes {
                return Ok(Arc::clone(&entry.atoms));
            }
        }
    }

    // Slow path — read + parse + insert. Another writer may race;
    // last-writer-wins is fine because all writers produce the same
    // Arc target for the same mtime/size.
    let file: AtomsFile = read_atlas_atoms(atlas_dir)?;
    let atoms = Arc::new(file.atoms);
    let mut write = cache().write().expect("atoms cache rwlock poisoned");
    write.insert(
        atlas_dir.to_path_buf(),
        CachedAtoms {
            mtime_ms,
            size_bytes,
            atoms: Arc::clone(&atoms),
        },
    );
    Ok(atoms)
}

// ── Filter + paginate (pure) ─────────────────────────────────

fn filter_and_page(
    corpus_id: &str,
    atoms: &[AtomEnvelope],
    filter: &AtomFilter,
    page: PageCursor,
    freshness: &HashMap<String, i64>,
) -> AtomListPage {
    let name_needle = filter
        .name_query
        .as_deref()
        .filter(|q| !q.is_empty())
        .map(|q| q.to_lowercase());

    // First pass — count + collect references. Storing references
    // avoids cloning AtomEnvelope just to throw most of them away.
    let mut matches: Vec<&AtomEnvelope> = Vec::new();
    for atom in atoms {
        if let Some(target) = filter.atom_type {
            if atom.atom_type() != target {
                continue;
            }
        }
        if let Some(min) = filter.min_salience {
            match atom.salience() {
                Some(s) if s >= min => {}
                _ => continue,
            }
        }
        if !filter.subtypes.is_empty() {
            let have = corpus_engine::enrichment::atlas::projection::subtype_of(atom);
            if !filter.subtypes.iter().any(|w| *w == have) {
                continue;
            }
        }
        if let Some(needle) = &name_needle {
            if !atom
                .display_name(Some(DISPLAY_NAME_TRUNCATION))
                .to_lowercase()
                .contains(needle.as_str())
            {
                continue;
            }
        }
        matches.push(atom);
    }

    // Fresh-first: documents (re)indexed after the bulk install — a
    // newsworthy fetch, a watched-folder edit — bubble to the top so
    // the most recently-touched knowledge leads the list. The sort is
    // stable, so atoms with equal (or no) freshness keep their original
    // insertion order; a corpus with no recency sidecar is unchanged.
    // Recency is the *only* reordering signal — salience etc. are left
    // to the per-type tiebreak that insertion order already encodes.
    if !freshness.is_empty() {
        matches.sort_by_key(|b| std::cmp::Reverse(atom_freshness(b, freshness)));
    }

    let total_matching = matches.len() as u64;
    let end = page.offset.saturating_add(page.limit).min(matches.len());
    let slice = if page.offset >= matches.len() {
        &[][..]
    } else {
        &matches[page.offset..end]
    };

    let items: Vec<AtomSummary> = slice
        .iter()
        .map(|a| build_summary(corpus_id, a, freshness))
        .collect();

    let next_offset = if end < matches.len() { Some(end) } else { None };

    AtomListPage {
        items,
        total_matching,
        next_offset,
    }
}

fn build_summary(
    corpus_id: &str,
    atom: &AtomEnvelope,
    freshness: &HashMap<String, i64>,
) -> AtomSummary {
    AtomSummary {
        atom_id: atom.id().clone(),
        stable_key: atom.stable_key(corpus_id),
        atom_type: atom.atom_type(),
        subtype: {
            // Empty means "this atom has no subtype", which is not the same as
            // a subtype spelled "" — the row carries `None` so a viewer can
            // fall back to the kind rather than render a blank chip.
            let s = corpus_engine::enrichment::atlas::projection::subtype_of(atom);
            (!s.is_empty()).then_some(s)
        },
        display_name: atom.display_name(Some(DISPLAY_NAME_TRUNCATION)),
        salience: atom.salience(),
        enrichment_depth: atom.enrichment_depth(),
        evidence_chunk_count: atom.evidence().len() as u32,
        curation_status: CurationStatus::Generated,
        overlay_supports: false,
        updated_at: atom_freshness(atom, freshness),
    }
}

// Five 11-arm fan-outs over `AtomEnvelope` lived here — `atom_type_of`,
// `display_name_of` (+ its truncation constant), `scalar_score`,
// `evidence_count` and `atom_source_doc_id` — with byte-identical twins in
// `atom_detail`, under a comment calling the duplication intentional. Every
// one of them is now an accessor on `AtomEnvelope` itself, where the closed
// set of kinds lives, so a new atom kind cannot answer differently in two
// crates. Deleted 2026-08-20 (ARCH §10.6 — one decider, one name).

/// Recency of the atom's source document, or `None` when the doc has
/// no recorded (re)index — i.e. baseline install-time content.
fn atom_freshness(atom: &AtomEnvelope, freshness: &HashMap<String, i64>) -> Option<i64> {
    atom.source_doc_id()
        .and_then(|id| freshness.get(id).copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::enrichment::atlas::atoms::{
        AtomId, AtomsFile, ChunkRef, Claim, Entity, SectionPosition, SectionRange, State,
    };
    use corpus_engine::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EntityType, EpistemicStatus, EventType,
        StateType,
    };
    use tempfile::TempDir;

    fn entity(id: usize, name: &str, salience: f32) -> AtomEnvelope {
        AtomEnvelope::Entity(Entity {
            id: AtomId::entity(id),
            canonical_name: name.into(),
            aliases: vec![],
            entity_type: EntityType::Concept,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "x".into(),
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

    fn claim(id: usize, content: &str) -> AtomEnvelope {
        AtomEnvelope::Claim(Claim {
            attributes: Default::default(),
            subject: None,
            id: AtomId::claim(id),
            content: content.into(),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: vec![
                ChunkRef::new("sec_0001", None),
                ChunkRef::new("sec_0002", None),
            ],
            quotable_excerpt: None,
            attributed_to: None,
            confidence: None,
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

    fn make_atlas() -> (TempDir, FileAtlasReader) {
        let tmp = tempfile::tempdir().unwrap();
        let reader = FileAtlasReader::new(tmp.path().to_path_buf());
        write_atoms(
            &tmp.path().join("wiki").join("atlas"),
            vec![
                entity(1, "Knowledge", 0.9),
                entity(2, "Belief", 0.6),
                entity(3, "Justification", 0.3),
                claim(1, "Knowledge is justified true belief."),
                claim(2, "Belief without justification is not knowledge."),
            ],
        );
        (tmp, reader)
    }

    #[tokio::test]
    async fn list_atoms_unfiltered_returns_all() {
        let (_tmp, reader) = make_atlas();
        let page = reader
            .list_atoms("wiki", AtomFilter::default(), PageCursor::default())
            .await
            .unwrap();
        assert_eq!(page.total_matching, 5);
        assert_eq!(page.items.len(), 5);
        assert!(page.next_offset.is_none());
    }

    #[tokio::test]
    async fn list_atoms_filters_by_type() {
        let (_tmp, reader) = make_atlas();
        let only_entities = reader
            .list_atoms(
                "wiki",
                AtomFilter {
                    atom_type: Some(AtomType::Entity),
                    ..Default::default()
                },
                PageCursor::default(),
            )
            .await
            .unwrap();
        assert_eq!(only_entities.total_matching, 3);
        for item in &only_entities.items {
            assert_eq!(item.atom_type, AtomType::Entity);
        }
    }

    /// The author's noun is findable, and it is not the atom kind.
    ///
    /// `coin` and `sceatta` are both Entities, so a kind filter cannot tell
    /// them apart; `subtype` can. The roll-up is deliberately NOT here — a
    /// caller wanting the family asks for each name.
    ///
    /// Falsifier: drop the `subtype` arm from the filter loop and this returns
    /// all four atoms instead of two.
    #[tokio::test]
    async fn list_atoms_filters_by_declared_subtype() {
        let tmp = tempfile::tempdir().unwrap();
        let reader = FileAtlasReader::new(tmp.path().to_path_buf());
        let typed = |id: usize, name: &str, t: &str| match entity(id, name, 0.5) {
            AtomEnvelope::Entity(mut e) => {
                e.entity_type = EntityType::Other(t.into());
                AtomEnvelope::Entity(e)
            }
            other => other,
        };
        write_atoms(
            &tmp.path().join("wiki").join("atlas"),
            vec![
                typed(1, "Beonna penny", "coin"),
                typed(2, "Offa gold dinar", "coin"),
                typed(3, "Series R sceatta", "sceatta"),
                entity(4, "Aldfrith", 0.5),
            ],
        );

        let coins = reader
            .list_atoms(
                "wiki",
                AtomFilter {
                    subtypes: vec!["coin".into()],
                    ..Default::default()
                },
                PageCursor::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            coins.total_matching, 2,
            "own only — `sceatta` is not a coin here"
        );
        for item in &coins.items {
            assert_eq!(item.atom_type, AtomType::Entity, "same KIND as the sceatta");
            assert_eq!(item.subtype.as_deref(), Some("coin"));
        }

        // An undeclared entity is not subtype-less: it carries its generic
        // kind, which is the same word the kind-based UI already shows. Only
        // an unclassified Relation/Event/State has no subtype at all.
        let all = reader
            .list_atoms("wiki", AtomFilter::default(), PageCursor::default())
            .await
            .unwrap();
        let generic = all
            .items
            .iter()
            .find(|i| i.display_name == "Aldfrith")
            .expect("the generic entity is listed");
        assert_eq!(generic.subtype.as_deref(), Some("concept"));
        let claims_have_none = reader
            .list_atoms(
                "wiki",
                AtomFilter {
                    subtypes: vec!["concept".into()],
                    ..Default::default()
                },
                PageCursor::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            claims_have_none.total_matching, 1,
            "the generic six are filterable too, by the same field"
        );
    }

    #[tokio::test]
    async fn list_atoms_filters_by_name_substring_case_insensitive() {
        let (_tmp, reader) = make_atlas();
        let page = reader
            .list_atoms(
                "wiki",
                AtomFilter {
                    name_query: Some("BELIEF".into()),
                    ..Default::default()
                },
                PageCursor::default(),
            )
            .await
            .unwrap();
        // Matches: entity "Belief", claim 1 ("...true belief..."),
        // claim 2 ("Belief without...").
        assert_eq!(page.total_matching, 3);
    }

    #[tokio::test]
    async fn list_atoms_filters_by_min_salience() {
        let (_tmp, reader) = make_atlas();
        let page = reader
            .list_atoms(
                "wiki",
                AtomFilter {
                    min_salience: Some(0.5),
                    ..Default::default()
                },
                PageCursor::default(),
            )
            .await
            .unwrap();
        // Only Entities have salience; Knowledge (0.9) and Belief
        // (0.6) clear the bar. Justification (0.3) doesn't. Claims
        // have no scalar score and are filtered out.
        assert_eq!(page.total_matching, 2);
        let names: Vec<&str> = page.items.iter().map(|i| i.display_name.as_str()).collect();
        assert!(names.contains(&"Knowledge"));
        assert!(names.contains(&"Belief"));
    }

    #[tokio::test]
    async fn list_atoms_paginates() {
        let (_tmp, reader) = make_atlas();
        let first = reader
            .list_atoms(
                "wiki",
                AtomFilter::default(),
                PageCursor {
                    offset: 0,
                    limit: 2,
                },
            )
            .await
            .unwrap();
        assert_eq!(first.items.len(), 2);
        assert_eq!(first.total_matching, 5);
        assert_eq!(first.next_offset, Some(2));

        let second = reader
            .list_atoms(
                "wiki",
                AtomFilter::default(),
                PageCursor {
                    offset: 2,
                    limit: 2,
                },
            )
            .await
            .unwrap();
        assert_eq!(second.items.len(), 2);
        assert_eq!(second.next_offset, Some(4));

        let third = reader
            .list_atoms(
                "wiki",
                AtomFilter::default(),
                PageCursor {
                    offset: 4,
                    limit: 2,
                },
            )
            .await
            .unwrap();
        assert_eq!(third.items.len(), 1);
        assert!(third.next_offset.is_none());

        let past_end = reader
            .list_atoms(
                "wiki",
                AtomFilter::default(),
                PageCursor {
                    offset: 99,
                    limit: 10,
                },
            )
            .await
            .unwrap();
        assert!(past_end.items.is_empty());
        assert_eq!(past_end.total_matching, 5);
    }

    #[tokio::test]
    async fn list_atoms_combines_type_and_name_filters() {
        let (_tmp, reader) = make_atlas();
        let page = reader
            .list_atoms(
                "wiki",
                AtomFilter {
                    atom_type: Some(AtomType::Claim),
                    name_query: Some("justified".into()),
                    ..Default::default()
                },
                PageCursor::default(),
            )
            .await
            .unwrap();
        // Type AND name both apply: only claim 1
        // ("Knowledge is justified true belief.") contains "justified"
        // among the Claims. Entity "Justification" would match by
        // name but is excluded by the type filter.
        assert_eq!(page.total_matching, 1);
        assert_eq!(page.items[0].atom_type, AtomType::Claim);
    }

    #[tokio::test]
    async fn list_atoms_unknown_corpus_returns_error() {
        let (_tmp, reader) = make_atlas();
        let err = reader
            .list_atoms("nonexistent", AtomFilter::default(), PageCursor::default())
            .await
            .unwrap_err();
        match err {
            AtomQueryError::UnknownCorpus(id) => assert_eq!(id, "nonexistent"),
            other => panic!("expected UnknownCorpus, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_atoms_truncates_long_content_in_display_name() {
        let tmp = tempfile::tempdir().unwrap();
        let reader = FileAtlasReader::new(tmp.path().to_path_buf());
        let long = "x".repeat(200);
        write_atoms(&tmp.path().join("c").join("atlas"), vec![claim(1, &long)]);
        let page = reader
            .list_atoms("c", AtomFilter::default(), PageCursor::default())
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        let name = &page.items[0].display_name;
        // Truncated to DISPLAY_NAME_TRUNCATION chars + ellipsis.
        assert!(name.chars().count() <= DISPLAY_NAME_TRUNCATION + 1);
        assert!(name.ends_with('…'));
    }

    #[tokio::test]
    async fn list_atoms_summary_carries_stable_key_and_phase2_fields() {
        let (_tmp, reader) = make_atlas();
        let page = reader
            .list_atoms(
                "wiki",
                AtomFilter {
                    atom_type: Some(AtomType::Entity),
                    name_query: Some("Knowledge".into()),
                    ..Default::default()
                },
                PageCursor::default(),
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        let s = &page.items[0];
        // Phase-2 fields exist in the wire shape with their Phase-1
        // hardcoded values.
        assert_eq!(s.curation_status, CurationStatus::Generated);
        assert!(!s.overlay_supports);
        // stable_key is a 64-char blake3 hex.
        assert_eq!(s.stable_key.as_str().len(), 64);
        // Entity-specific fields.
        assert_eq!(s.salience, Some(0.9));
        assert_eq!(s.evidence_chunk_count, 1);
    }

    #[tokio::test]
    async fn list_atoms_cache_hit_after_first_call() {
        // This test deliberately corrupts atoms.json after seeding the
        // cache to prove the in-memory copy is the data source on the
        // second call. Each test gets a unique tempdir path, which is
        // the cache key — so this test's entries can't collide with
        // (or be evicted by) tests running in parallel.
        let (tmp, reader) = make_atlas();
        let atlas_dir = tmp.path().join("wiki").join("atlas");
        let first = reader
            .list_atoms("wiki", AtomFilter::default(), PageCursor::default())
            .await
            .unwrap();
        assert_eq!(first.total_matching, 5);
        // Render atoms.json unreadable but keep the cache key
        // (mtime + size) intact — the cache should still serve.
        let atoms_path = atlas_dir.join("atoms.json");
        let original_meta = std::fs::metadata(&atoms_path).unwrap();
        std::fs::write(&atoms_path, vec![0u8; original_meta.len() as usize]).unwrap();
        filetime::set_file_mtime(
            &atoms_path,
            filetime::FileTime::from_system_time(original_meta.modified().unwrap()),
        )
        .unwrap();
        let second = reader
            .list_atoms("wiki", AtomFilter::default(), PageCursor::default())
            .await
            .unwrap();
        assert_eq!(second.total_matching, 5);
        assert_eq!(second.items.len(), 5);
    }

    #[test]
    fn evidence_count_matches_per_type_field() {
        // State has its own evidence shape; pin that we count it.
        let s = AtomEnvelope::State(State {
            id: AtomId::state(1),
            entity_id: AtomId::entity(1),
            label: "Anxious".into(),
            state_type: StateType::Psychological,
            evidence: vec![
                ChunkRef::new("a", None),
                ChunkRef::new("b", None),
                ChunkRef::new("c", None),
            ],
            section_range: SectionRange::point("ch001"),
            confidence: None,
            enrichment_depth: EnrichmentDepth::Extracted,
        });
        assert_eq!(s.evidence().len() as u32, 3);
        // Event also has evidence + section_position; pin the shape.
        let e = AtomEnvelope::Event(corpus_engine::enrichment::atlas::atoms::Event {
            attributes: Default::default(),
            id: AtomId::event(1),
            description: "x".into(),
            event_type: EventType::Action,
            participants: vec![],
            evidence: vec![ChunkRef::new("a", None)],
            section_position: SectionPosition::section("ch001"),
            causal_antecedents: vec![],
            enrichment_depth: EnrichmentDepth::Extracted,
        });
        assert_eq!(e.evidence().len() as u32, 1);
    }

    /// An entity whose `first_appearance` carries a `source_doc_id` —
    /// the join key the freshness sort uses.
    fn entity_doc(id: usize, name: &str, salience: f32, doc: &str) -> AtomEnvelope {
        let AtomEnvelope::Entity(mut e) = entity(id, name, salience) else {
            unreachable!("entity() builds an Entity")
        };
        e.first_appearance = ChunkRef::new("sec_0001", None).with_source_doc(Some(doc.to_string()));
        AtomEnvelope::Entity(e)
    }

    #[tokio::test]
    async fn list_atoms_sorts_fresh_docs_first() {
        let tmp = tempfile::tempdir().unwrap();
        let reader = FileAtlasReader::new(tmp.path().to_path_buf());
        let corpus_dir = tmp.path().join("news");
        // Insertion order: Alpha, Beta, Gamma.
        write_atoms(
            &corpus_dir.join("atlas"),
            vec![
                entity_doc(1, "Alpha", 0.9, "Alpha"),
                entity_doc(2, "Beta", 0.9, "Beta"),
                entity_doc(3, "Gamma", 0.9, "Gamma"),
            ],
        );
        // Freshness sidecar one level up from atlas/. Gamma newest,
        // Beta older, Alpha absent (baseline).
        let mut freshness = HashMap::new();
        freshness.insert("Beta".to_string(), 2_000i64);
        freshness.insert("Gamma".to_string(), 3_000i64);
        std::fs::write(
            corpus_dir.join(corpus_engine::freshness::DOC_FRESHNESS_FILE),
            serde_json::to_vec(&freshness).unwrap(),
        )
        .unwrap();

        let page = reader
            .list_atoms("news", AtomFilter::default(), PageCursor::default())
            .await
            .unwrap();

        let names: Vec<&str> = page.items.iter().map(|i| i.display_name.as_str()).collect();
        // Fresh-first: Gamma (3000) → Beta (2000) → Alpha (baseline, last).
        assert_eq!(names, vec!["Gamma", "Beta", "Alpha"]);
        // updated_at reflects each doc's recency; baseline doc is None.
        assert_eq!(page.items[0].updated_at, Some(3_000));
        assert_eq!(page.items[1].updated_at, Some(2_000));
        assert_eq!(page.items[2].updated_at, None);
    }

    #[tokio::test]
    async fn list_atoms_without_sidecar_keeps_insertion_order() {
        // No `_doc_freshness.json` → empty map → no reordering, no
        // updated_at. Proves the feature is inert for baseline corpora.
        let tmp = tempfile::tempdir().unwrap();
        let reader = FileAtlasReader::new(tmp.path().to_path_buf());
        write_atoms(
            &tmp.path().join("c").join("atlas"),
            vec![
                entity_doc(1, "Alpha", 0.9, "Alpha"),
                entity_doc(2, "Beta", 0.9, "Beta"),
            ],
        );
        let page = reader
            .list_atoms("c", AtomFilter::default(), PageCursor::default())
            .await
            .unwrap();
        let names: Vec<&str> = page.items.iter().map(|i| i.display_name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "Beta"]);
        assert!(page.items.iter().all(|i| i.updated_at.is_none()));
    }
}
