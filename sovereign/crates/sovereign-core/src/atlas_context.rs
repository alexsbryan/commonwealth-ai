// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atlas-grounded retrieval primitives shared between the eval CLI
//! and the runtime chat path.
//!
//! The atlas is a typed knowledge graph computed offline (see
//! `corpus-engine/ATLAS.md`). At query time, retrieval can fuse atlas
//! Entity matches into the chunk hit set as virtual `ScoredChunk`s:
//! cosine the question embedding against pre-embedded Entity
//! descriptions, take top-K, surface them as additional candidates.
//! This module owns the data types + math; the eval CLI provides one
//! loader (against `ChatSession::inference`) and the daemon provides
//! another (`sovereign-tools::atlas_context_manager`) that loads at
//! daemon boot and reuses across queries.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use corpus_engine::enrichment::atlas::projection::{ArchChunkRef, AtomRecord};
// Re-exported so retrieval consumers (`runtime/retrieval.rs`) can name the
// atom-kind discriminant the typed-enumeration filter selects on.
pub use corpus_engine::enrichment::atlas::projection::AtomKindTag;
use corpus_engine::enrichment::atlas::ann_store::AnnSeedTable;
use corpus_engine::enrichment::atlas::store::LancePreload;
use corpus_engine::enrichment::atlas::{AtomEnvelope, EdgeType};
use corpus_engine::enrichment::pipeline::atlas::EpistemicStatus;
use corpus_engine::ScoredChunk;

/// One pre-embedded atlas atom available to retrieval as a virtual
/// chunk. Built by a loader, immutable after that.
#[derive(Debug, Clone)]
pub struct AtlasEntry {
    /// The backing atom's id (`entity-<hash>`). First-class since
    /// ATLAS_STORAGE_V2 Phase B: seeding reads it directly instead of
    /// reverse-resolving from `embed_text`, so `resolve_atom_id_from_entry` is
    /// gone. Empty only for entries with no backing atom (the non-default,
    /// eval-only `include_tensions` edge virtual-chunks).
    pub atom_id: String,
    pub canonical_name: String,
    pub embed_text: String,
    pub embedding: Vec<f32>,
}

/// Pre-embedded atlas entity bag for one corpus. Carries the
/// `top_k` the loader was constructed with so the per-query call
/// site doesn't need to re-pick a value.
#[derive(Debug, Clone)]
pub struct AtlasContext {
    pub atlas_corpus_id: String,
    pub entries: Vec<AtlasEntry>,
    pub top_k: usize,
}

/// Sibling to [`AtlasContext`] — the structural graph layer that
/// cosine-only retrieval ignores. The atlas is a typed knowledge
/// graph (see `corpus-engine/ATLAS.md`); cosine/ANN matching over atom
/// embeddings ("bag-of-atoms") finds seeds, but the substantive
/// structure — dialectical tensions, grounding chains, configuration
/// constituents — lives on the edges. [`atlas_navigate_ann`] walks that
/// graph from seeded atoms to surface the chunk-evidence neighborhood.
///
/// **Storage (ATLAS_STORAGE_V2).** The graph is the v2 store: atoms read
/// resident from `atlas/atoms.lance` (the projected [`AtomRecord`]s) + the
/// `atlas/edges.csr` mmap adjacency. Opening is async (reads the atoms table
/// once, off the hot path); every accessor is then a sync slice / mmap read, so
/// `atlas_navigate`'s BFS inner loop stays sync. Consumers read through
/// [`AtomView`] (`&str` over the projected fields; `atom_envelope()` re-parses
/// the JSON payload blob only for the rare deep-field access) and the `atom` /
/// `atoms` / `atoms_of_kind` / `atom_evidence` / `edges_from` / `edges_to` /
/// `edge_degree` methods. See [`LancePreload`].
#[derive(Clone)]
pub struct AtlasGraph {
    pub atlas_corpus_id: String,
    /// Article slug after stripping the leading prefix used by the
    /// extraction pipeline (e.g. `sep-` for SEP atlases). Used to
    /// filter FTS lookups during chunk fetch — the right SEP corpus
    /// chunk has `title == article_slug`.
    pub article_slug: String,
    /// The v2 store backend: `atoms.lance` atoms read resident (the projected
    /// [`AtomRecord`]s) + the `edges.csr` mmap. The sync query API reads
    /// straight off the resident records + the CSR mmap — no async ripple into
    /// `atlas_navigate`. `Arc` so the graph stays cheaply `Clone` (the daemon
    /// hands out `Arc<AtlasGraph>`; the eval CLI holds `Vec<AtlasGraph>`). See
    /// [`LancePreload`].
    preload: Arc<LancePreload>,
    /// Persistent per-corpus ANN seed table (`atlas/atoms_ann.lance`), when the
    /// corpus has been backfilled (ATLAS_STORAGE_V2 3b). `Some` selects the ANN
    /// seed path in [`atlas_navigate_ann`]. Attached by the long-lived loader
    /// (`AtlasContextManager` / eval runner) via
    /// [`AtlasGraph::with_ann_seed_table`], never the sync open bridge.
    ann: Option<Arc<AnnSeedTable>>,
}

impl std::fmt::Debug for AtlasGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtlasGraph")
            .field("atlas_corpus_id", &self.atlas_corpus_id)
            .field("article_slug", &self.article_slug)
            .field("atoms", &self.atom_count())
            .field("edges", &self.edge_count())
            .finish()
    }
}

impl AtlasGraph {
    /// Load the structural graph for a corpus from the v2 store
    /// (`atlas/atoms.lance` + `atlas/edges.csr`). The single canonical loader
    /// used by both the eval CLI (per-process load against `paths::index_root`)
    /// and the daemon (`AtlasContextManager` boot).
    ///
    /// **No fallback (ATLAS_STORAGE_V2).** The v1 rkyv archive + the
    /// `atoms.json` convert-on-load path were retired once every corpus was
    /// migrated to the v2 store. A corpus without a v2 store returns `Err`
    /// (`atoms.json` stays the canonical export + the `sovereign atlas
    /// migrate-all` rebuild source). Wikipedia carries no v2 atom store — it
    /// uses the columnar `WikipediaGraph` for its structural neighbors — so it
    /// returns `Err` here, and the caller (`AtlasContextManager::graph`) treats
    /// that as "no atom graph for this corpus" and skips it.
    ///
    /// `atlas_corpus_id` controls the article-slug derivation (currently strips
    /// a `sep-` prefix). Pass the source-side corpus id even when the on-disk
    /// dir uses a different layout.
    pub fn load_from_disk(atlas_corpus_id: &str, atlas_dir: &Path) -> Result<Self, String> {
        if !v2_store_present(atlas_dir) {
            return Err(format!(
                "no v2 atlas store for {atlas_corpus_id} at {} (need atoms.lance + edges.csr); \
                 rebuild with `sovereign atlas migrate-all`",
                atlas_dir.display()
            ));
        }
        let g = Self::load_lance_from_disk(atlas_corpus_id, atlas_dir)?;
        tracing::info!(
            corpus = atlas_corpus_id,
            backend = "lance",
            atoms = g.atom_count(),
            "atlas loaded via v2 store"
        );
        Ok(g)
    }

    /// Build a graph from an already-opened [`LancePreload`] — the v2 store
    /// (atoms resident from `atoms.lance`, edges from the `edges.csr` mmap).
    pub fn from_lance_preload(atlas_corpus_id: &str, preload: LancePreload) -> Self {
        let article_slug = derive_article_slug(atlas_corpus_id);
        Self {
            atlas_corpus_id: atlas_corpus_id.to_string(),
            article_slug,
            preload: Arc::new(preload),
            ann: None,
        }
    }

    /// Open the v2 store under `atlas_dir` (`atoms.lance` + `edges.csr`) and
    /// build the graph. Sync (bridges the async open via
    /// [`LancePreload::open_blocking`]) so it slots into the daemon's sync load
    /// path; lifecycle-time only (boot / corpus load), never the hot query path.
    pub fn load_lance_from_disk(atlas_corpus_id: &str, atlas_dir: &Path) -> Result<Self, String> {
        let preload = LancePreload::open_blocking(atlas_dir)
            .map_err(|e| format!("open v2 store for {atlas_corpus_id}: {e}"))?;
        Ok(Self::from_lance_preload(atlas_corpus_id, preload))
    }

    /// Attach a persistent ANN seed table (`atlas/atoms_ann.lance`) — the
    /// ATLAS_STORAGE_V2 3b seed source. The table MUST be opened on the
    /// caller's long-lived runtime (the daemon's `AtlasContextManager` or the
    /// eval runner), never the sync open bridge: that bridge runs the open on a
    /// transient runtime it then drops, which would invalidate the held
    /// `lancedb::Table`. Builder-style so loaders can do
    /// `AtlasGraph::load_from_disk(..)?.with_ann_seed_table(ann)`.
    pub fn with_ann_seed_table(mut self, ann: Arc<AnnSeedTable>) -> Self {
        self.ann = Some(ann);
        self
    }

    /// The attached ANN seed table, or `None` for an un-backfilled corpus
    /// (whose seeding falls back to the v1 cosine-over-the-bag path).
    pub fn ann_seed_table(&self) -> Option<&Arc<AnnSeedTable>> {
        self.ann.as_ref()
    }

    /// Whether this graph has an ANN seed table — the per-corpus gate the
    /// retrieval caller checks to pick [`atlas_navigate_ann`] over the v1
    /// cosine seed in [`atlas_navigate`].
    pub fn has_ann_seed_table(&self) -> bool {
        self.ann.is_some()
    }

    /// Number of atoms in the graph.
    pub fn atom_count(&self) -> usize {
        self.preload.atom_count()
    }

    /// Number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.preload.edge_count()
    }

    /// Point lookup by atom-id. `None` if absent.
    pub fn atom(&self, atom_id: &str) -> Option<AtomView<'_>> {
        self.preload.atom(atom_id).map(AtomView)
    }

    /// All atoms, in interned local-id order.
    pub fn atoms(&self) -> impl Iterator<Item = AtomView<'_>> + '_ {
        self.preload.atoms().map(AtomView)
    }

    /// Atoms of one kind — the typed-enumeration filter. Reads only the
    /// projected type tag per atom (no payload parse), so a full scan of
    /// the 1.67M-atom wikipedia atlas is ~2ms (Phase 0).
    pub fn atoms_of_kind(&self, kind: AtomKindTag) -> impl Iterator<Item = AtomView<'_>> + '_ {
        self.atoms().filter(move |v| v.kind() == kind)
    }

    /// Evidence ChunkRefs for an atom-id, normalised across atom types
    /// (the archive builder mirrored the per-variant `evidence_refs`).
    pub fn atom_evidence(&self, atom_id: &str) -> Vec<EvidenceRef<'_>> {
        match self.atom(atom_id) {
            Some(v) => v.evidence().collect(),
            None => Vec::new(),
        }
    }

    /// In + out edge count for an atom — the prominence "degree" signal.
    /// Counts adjacency-list lengths without parsing any edge payload.
    pub fn edge_degree(&self, atom_id: &str) -> usize {
        self.preload.edge_degree(atom_id)
    }

    /// Edges originating at `atom_id` — [`EdgeView`]s over the `edges.csr` mmap
    /// adjacency (no JSON parse), bounded by the BFS frontier in
    /// `atlas_navigate`.
    pub fn edges_from(&self, atom_id: &str) -> Vec<EdgeView<'_>> {
        lance_edge_views(self.preload.out_edges(atom_id))
    }

    /// Edges arriving at `atom_id`.
    pub fn edges_to(&self, atom_id: &str) -> Vec<EdgeView<'_>> {
        lance_edge_views(self.preload.in_edges(atom_id))
    }
}

/// Edge adjacency — map the preload's `(src, tgt, type, conf)`
/// tuples (endpoint strings borrowed from the resident id table) into
/// [`EdgeView`]s.
fn lance_edge_views<'a>(raw: Vec<(&'a str, &'a str, EdgeType, f32)>) -> Vec<EdgeView<'a>> {
    raw.into_iter()
        .map(|(source, target, edge_type, confidence)| EdgeView {
            source,
            target,
            edge_type,
            confidence,
        })
        .collect()
}

/// Borrowing view over one CSR edge — the four fields the
/// navigate path reads. `source`/`target` are zero-copy atom-id `&str`s.
pub struct EdgeView<'a> {
    pub source: &'a str,
    pub target: &'a str,
    pub edge_type: EdgeType,
    pub confidence: f32,
}

fn derive_article_slug(atlas_corpus_id: &str) -> String {
    atlas_corpus_id
        .strip_prefix("sep-")
        .unwrap_or(atlas_corpus_id)
        .to_string()
}

/// Is the v2 store (`atoms.lance` + `edges.csr`) present in `atlas_dir`? Both
/// artifacts are required — a half-present store is treated as absent, so
/// `load_from_disk` returns the clean "no v2 store" `Err` instead of reading a
/// torn store.
fn v2_store_present(atlas_dir: &Path) -> bool {
    use corpus_engine::enrichment::atlas::store::{ATOMS_LANCE_DIRNAME, EDGES_CSR_FILENAME};
    atlas_dir.join(ATOMS_LANCE_DIRNAME).exists() && atlas_dir.join(EDGES_CSR_FILENAME).exists()
}

/// Borrowing view over one atom — the v2 resident projected record
/// (`&AtomRecord`, an `atoms.lance` row re-projected at open). Field borrows are
/// tied to the graph's data (`'a`), not to the (often temporary) view, so a
/// caller can collect [`EvidenceRef`]s out of a transient `AtomView`.
/// [`atom_envelope`](Self::atom_envelope) re-parses the full `AtomEnvelope` from
/// the JSON payload for the rare deep-field read.
pub struct AtomView<'a>(&'a AtomRecord);

impl<'a> AtomView<'a> {
    pub fn id(&self) -> &'a str {
        self.0.id.as_str()
    }
    pub fn kind(&self) -> AtomKindTag {
        self.0.kind
    }
    /// `Entity.canonical_name` (else `""`).
    pub fn name(&self) -> &'a str {
        self.0.name.as_str()
    }
    /// `Relation.label` (else `""`).
    pub fn label(&self) -> &'a str {
        self.0.label.as_str()
    }
    /// `Claim.content` (else `""`).
    pub fn content(&self) -> &'a str {
        self.0.content.as_str()
    }
    /// `Entity.entity_type` string repr (else `""`).
    pub fn subtype(&self) -> &'a str {
        self.0.subtype.as_str()
    }
    /// `Entity.description` (else `""`).
    pub fn description(&self) -> &'a str {
        self.0.description.as_str()
    }
    /// `Claim.quotable_excerpt` (else `""`).
    pub fn excerpt(&self) -> &'a str {
        self.0.excerpt.as_str()
    }
    /// `Claim.confidence` (0.5 default; 0.0 for non-claims).
    pub fn confidence(&self) -> f32 {
        self.0.confidence
    }
    /// `Entity.salience` (0.0 for non-entities).
    pub fn salience(&self) -> f32 {
        self.0.salience
    }
    pub fn alias_count(&self) -> usize {
        self.0.aliases.len()
    }
    pub fn aliases(&self) -> impl Iterator<Item = &'a str> + 'a {
        self.0.aliases.iter().map(|s| s.as_str())
    }
    /// `Relation.participants` atom-ids.
    pub fn participants(&self) -> impl Iterator<Item = &'a str> + 'a {
        self.0.participants.iter().map(|s| s.as_str())
    }
    pub fn evidence(&self) -> impl Iterator<Item = EvidenceRef<'a>> + 'a {
        self.0.evidence.iter().map(EvidenceRef)
    }
    /// Re-parse the full `AtomEnvelope` from the JSON payload blob.
    /// `None` only for the empty-payload edge case or a parse failure.
    pub fn atom_envelope(&self) -> Option<AtomEnvelope> {
        if self.0.payload.is_empty() {
            return None;
        }
        serde_json::from_slice(self.0.payload.as_slice()).ok()
    }
}

/// Borrowing view over one evidence ref (the v2 resident `&ArchChunkRef`). The
/// `Option` fields of the source `ChunkRef` were collapsed to `""` at projection.
pub struct EvidenceRef<'a>(&'a ArchChunkRef);

impl<'a> EvidenceRef<'a> {
    pub fn chunk_id(&self) -> &'a str {
        self.0.chunk_id.as_str()
    }
    pub fn passage_preview(&self) -> &'a str {
        self.0.passage_preview.as_str()
    }
    pub fn source_doc_id(&self) -> &'a str {
        self.0.source_doc_id.as_str()
    }
}

/// One step's worth of source-chunk targeting from atlas navigation.
/// Each request says "atlas thinks the source-corpus section
/// identified by `chunk_id` (in the per-article extraction corpus)
/// is highly relevant to the question". Resolved by direct lookup
/// in the article's chapters.json source — no FTS or vector search
/// needed. The `passage_preview` is a fallback for paragraph-level
/// targeting within the larger section.
#[derive(Debug, Clone)]
pub struct ChunkRequest {
    /// The corpus this atom (and therefore its source chunk) belongs to
    /// — the `atlas_corpus_id` of the graph that produced it. Lets the
    /// fetch scope its search to the one corpus the chunk lives in,
    /// instead of FTS-scanning every enabled corpus per request (a
    /// 1.9M-chunk wikipedia index would otherwise be searched once per
    /// atom). The chunk lives here because the atlas was extracted from
    /// this corpus, so scoping selects the same chunk the cross-corpus
    /// title filter would — and avoids pulling a same-titled article
    /// from the wrong corpus.
    pub corpus_id: String,
    pub article_slug: String,
    /// The atom-evidence section id (e.g. `sec_0001`) in the
    /// per-article extraction corpus. Direct key into chapters.json.
    pub chunk_id: String,
    /// Snippet of the source passage the atom was extracted from.
    /// Used to home in on the specific paragraph within the
    /// (10-paragraph-wide) section.
    pub passage_preview: String,
    /// Aggregate score: sum across all atoms in the navigation
    /// neighborhood that ground this passage, weighted by cosine
    /// match × graph-distance decay × edge-type weight. Chunks that
    /// ground multiple high-relevance atoms float to the top.
    pub score: f32,
    /// Diagnostic — which atoms motivated this fetch and via which
    /// edge types. Surfaces "this chunk is here because of the
    /// Tension between Knowledge Argument and Ability Hypothesis."
    pub motivating_atoms: Vec<String>,
    /// Verbatim ≤200-char excerpts harvested from the motivating
    /// atoms' `defining_quote` / `quotable_excerpt` fields. Each
    /// string is already formatted ("Defining X: …" or "[Y]: …")
    /// for direct injection into the fetched chunk's content. The
    /// caller (apply_atlas_grounding) prepends these to the chunk
    /// so the article's exact words for a defined concept or an
    /// attributed claim sit visibly at the head of the passage —
    /// addresses the essay-judge's "wants direct primary text"
    /// finding from the 2026-05-06 calibration audit.
    pub verbatim_excerpts: Vec<String>,
}

/// Per-edge-type relevance weights for graph BFS. Tunable; a value
/// of 0 disables walking that edge type. Defaults reflect what each
/// edge type contributes to question-answering retrieval:
///   - Tension → highest (only edge that supplies dialectical
///     breadth — opposing claim pairs surface counter-positions)
///   - Grounds → high (argument-depth: claims supported by other
///     claims walk us into the reasoning chain)
///   - Configures/Composes → medium (configuration's constituent
///     atoms identify the article's interpretive frame)
///   - Involves → medium (entity-event participation)
///   - Causes/Transition → low (state/event chains)
pub fn edge_weight(edge_type: EdgeType) -> f32 {
    match edge_type {
        EdgeType::Tension => 1.0,
        EdgeType::Grounds => 0.8,
        EdgeType::Configures => 0.6,
        EdgeType::Composes => 0.6,
        EdgeType::Involves => 0.5,
        EdgeType::Causes => 0.3,
        EdgeType::Transition => 0.3,
        // Cross-corpus edges aren't relevant for intra-article
        // navigation; they're surfaced via dedicated cross-corpus
        // retrieval paths.
        EdgeType::Grounding | EdgeType::Framing | EdgeType::Provenance => 0.0,
        // Gap-B typed-extension edges. EvidenceFor lands at Grounds
        // weight because the semantics overlap (evidence supports a
        // claim/position the same way Grounds links one claim to
        // its evidential basis). Concedes mirrors Tension (a
        // concession addresses a counter-position the same way a
        // Tension edge captures dialectical disagreement). OpposesIn
        // walks from an Opposition atom out to its two sides — the
        // graph traversal benefit lives mainly downstream of the
        // Opposition atom itself, so the edge weight is medium.
        EdgeType::EvidenceFor => 0.8,
        EdgeType::Concedes => 1.0,
        EdgeType::OpposesIn => 0.6,
        // Attaches connects a carrier doc to a described asset.
        // Intra-article navigation rarely benefits from this edge —
        // surfacing the asset is downstream UX (atom detail panel),
        // not retrieval. Zero weight here keeps the navigator
        // focused on argumentative structure.
        EdgeType::Attaches => 0.0,
    }
}

/// Whole-word case-insensitive substring check. Returns true iff
/// `needle` appears in `haystack` bounded by non-alphanumeric chars
/// on both sides (or string boundaries). Used by name-match seeding
/// in [`atlas_navigate`] to avoid false positives like "form" inside
/// "informed". Both args MUST already be lowercase.
pub fn contains_whole_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    let mut start = 0;
    while let Some(off) = haystack[start..].find(needle) {
        let abs = start + off;
        let end = abs + needle.len();
        let left_ok = abs == 0
            || !haystack[..abs]
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric());
        let right_ok = end == haystack.len()
            || !haystack[end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric());
        if left_ok && right_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

/// Pull the verbatim excerpt off an atom — `defining_quote` from a
/// concept Entity, `quotable_excerpt` from a Claim — and format it
/// for direct injection into a chunk's content. Returns `None` for
/// atoms that don't carry a quote field or whose quote is empty.
///
/// Format pins the source so the judge can attribute (mirrors the
/// essay-judge calibration's "named with substantive content"
/// rubric without demanding pre-assembled reconstruction). Single-
/// line, prefixed; the chunk-annotation site joins these with
/// newlines and prepends them to the chunk content.
/// Floor under which a verbatim excerpt is treated as a fragment
/// the model truncated rather than a real ≤200-char sentence.
/// Empirical: under the condensed prompt, 80%+ of populated quotes
/// land 100-220c; the rest cluster under 50c (mid-word cuts the
/// constraint sampler couldn't fully prevent). 60c is the
/// inflection — long enough to carry a clause that adds judge-
/// visible signal, short enough not to drop legitimate short
/// definitional sentences ("X is Y").
const MIN_VERBATIM_EXCERPT_CHARS: usize = 60;

pub fn atom_verbatim_excerpt(graph: &AtlasGraph, atom_id: &str) -> Option<String> {
    // Deep-field read over the bounded navigation neighborhood (not a hot
    // scan path) — parse the full atom from its JSON payload blob.
    let atom = graph.atom(atom_id)?.atom_envelope()?;
    match &atom {
        AtomEnvelope::ArgumentReconstruction(a) => {
            // Pre-format the reconstruction as P1/.../C/Objections.
            // Targets the essay-judge "argument_depth" axis, which
            // under-credits chunks that contain the argument's
            // pieces scattered across paragraphs without an explicit
            // reconstruction. Article-voice attribution.
            if a.premises.is_empty() && a.conclusion.trim().is_empty() {
                return None;
            }
            let mut s = String::with_capacity(256);
            s.push_str(&format!("Argument: {}", a.name));
            // Resolve proponent to canonical name when possible.
            if let Some(prop_id) = a.proponent.as_ref() {
                if let Some(prop) = graph.atom(prop_id.as_str()) {
                    if prop.kind() == AtomKindTag::Entity {
                        s.push_str(&format!(" ({})", prop.name()));
                    }
                }
            }
            s.push_str(&format!(" [from {}]", graph.article_slug));
            s.push('\n');
            for (i, p) in a.premises.iter().enumerate() {
                s.push_str(&format!("  P{}. {}\n", i + 1, p.trim()));
            }
            if !a.conclusion.trim().is_empty() {
                s.push_str(&format!("  C. {}\n", a.conclusion.trim()));
            }
            if !a.objections.is_empty() {
                // Render each objection on its own line with prose
                // content when available — the dialectical_breadth
                // axis credits expounded objections, not bare names.
                // Falls back to bare-name rendering for legacy atoms
                // whose objections were extracted as Vec<String>.
                s.push_str("  Objections:\n");
                for o in a.objections.iter() {
                    let name = o.name.trim();
                    let content = o.content.trim();
                    if content.is_empty() {
                        s.push_str(&format!("    - {}\n", name));
                    } else {
                        s.push_str(&format!("    - {}: {}\n", name, content));
                    }
                }
            }
            Some(s)
        }
        AtomEnvelope::Entity(e) => {
            let q = e.defining_quote.as_deref()?.trim();
            if q.chars().count() < MIN_VERBATIM_EXCERPT_CHARS {
                return None;
            }
            // "Defining $name: $sentence" — keeps the term anchored.
            Some(format!(
                "Defining {} ({}): \"{}\"",
                e.canonical_name, graph.article_slug, q
            ))
        }
        AtomEnvelope::Claim(c) => {
            let q = c.quotable_excerpt.as_deref()?.trim();
            if q.chars().count() < MIN_VERBATIM_EXCERPT_CHARS {
                return None;
            }
            // Resolve attribution to a canonical name when possible.
            // The Claim atom holds an AtomId — look it up in the
            // graph for the human-readable label. Fallback: bare id.
            let attribution = c.attributed_to.as_ref().and_then(|aid| {
                graph
                    .atom(aid.as_str())
                    .filter(|a| a.kind() == AtomKindTag::Entity)
                    .map(|a| a.name().to_string())
            });
            // Tag contested-status claims so the essay-judge sees them
            // as counter-position content rather than mainline support.
            // SEP articles routinely encode disputed claims with
            // epistemic_status=contested; without flagging, the
            // surfaced quote reads as part of the position the question
            // asks about, when really it's a rival voice. This flips
            // the dialectical_breadth axis from "names objections" (1)
            // to "expounds counter-position" (2) without changing
            // chunk content.
            let contested_tag = if matches!(c.epistemic_status, EpistemicStatus::Contested) {
                " — contested"
            } else {
                ""
            };
            match attribution {
                Some(name) => Some(format!(
                    "[{} ({}){}]: \"{}\"",
                    name, graph.article_slug, contested_tag, q
                )),
                None => Some(format!(
                    "[{}{}]: \"{}\"",
                    graph.article_slug, contested_tag, q
                )),
            }
        }
        _ => None,
    }
}

/// ANN-seeded atlas navigation (ATLAS_STORAGE_V2). Walk the typed graph from
/// seeded atoms, expand 1-2 hops across weighted edges, and aggregate the
/// evidence chunks by score density into [`ChunkRequest`]s — atlas's curated
/// answer to "which source chunks should the retriever fetch for this question?".
///
/// # Seeds
/// - **Vector seed (1a):** each graph's persistent ANN table
///   ([`AtlasGraph::ann_seed_table`]) returns the nearest atom-ids DIRECTLY (the
///   embed→atom-id join ran once at backfill, never per query), re-scored with the
///   canonical [`cosine`] so the BFS sees stable weights. One global top-`max_seeds`
///   pool across all graphs.
/// - **Name-match seed (1b):** every bag atom whose `canonical_name` (or trailing
///   token, or `[Argument: …]` name) appears literally in the question is
///   force-seeded — catches compound questions a single embedding can't rank. The
///   `atom_id` is read straight off the [`AtlasEntry`] (first-class since Phase B),
///   so neither seed path reverse-resolves from `embed_text`.
///
/// Async only because the ANN query awaits; the BFS inner loop stays sync
/// (resident atoms + `edges.csr` mmap) — the "hot BFS stays sync" invariant.
pub async fn atlas_navigate_ann(
    query_text: &str,
    query_embedding: &[f32],
    atlases: &[&AtlasContext],
    graphs: &[&AtlasGraph],
    max_seeds: usize,
    max_hops: usize,
) -> Vec<ChunkRequest> {
    if query_embedding.is_empty() || atlases.is_empty() {
        return Vec::new();
    }
    let graph_by_id: HashMap<&str, &AtlasGraph> = graphs
        .iter()
        .map(|g| (g.atlas_corpus_id.as_str(), *g))
        .collect();

    // 1a. Vector seeds — each graph's ANN table returns the nearest atom-ids
    // directly (no resolve), re-scored with cosine. One global top-`max_seeds`
    // pool. A graph without a table contributes name-match seeds only (below).
    let mut scored: Vec<(f32, String, String)> = Vec::new(); // (score, corpus_id, atom_id)
    for graph in graphs {
        let Some(ann) = graph.ann_seed_table() else {
            continue;
        };
        match ann.nearest_with_vectors(query_embedding, max_seeds).await {
            Ok(hits) => {
                for (atom_id, vector) in hits {
                    let score = cosine(query_embedding, &vector);
                    scored.push((score, graph.atlas_corpus_id.clone(), atom_id));
                }
            }
            Err(e) => tracing::warn!(
                corpus = %graph.atlas_corpus_id,
                "atlas_navigate_ann: ANN nearest failed ({e}); corpus contributes name-match seeds only"
            ),
        }
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(max_seeds);
    let primary_seeds: Vec<(String, String, f32)> =
        scored.into_iter().map(|(s, cid, aid)| (cid, aid, s)).collect();

    // 1b. Name-match seeds — `atom_id` read straight off the bag entry (no
    // resolve). Force-seeds every atom literally named in the question, so a
    // compound question a single embedding can't rank still reaches its atoms.
    let q_lower = query_text.to_lowercase();
    let mut name_seeds: Vec<(String, String, f32)> = Vec::new();
    for ctx in atlases {
        if !graph_by_id.contains_key(ctx.atlas_corpus_id.as_str()) {
            continue;
        }
        for entry in &ctx.entries {
            if entry.atom_id.is_empty() {
                continue;
            }
            let name = entry.canonical_name.trim();
            if name.len() < 4 {
                continue;
            }
            let name_lower = name.to_lowercase();
            let mut hit = contains_whole_word(&q_lower, &name_lower);
            if !hit {
                if let Some(last) = name_lower.split_whitespace().last() {
                    if last.len() >= 4 && last != name_lower {
                        hit = contains_whole_word(&q_lower, last);
                    }
                }
            }
            if !hit {
                if let Some(rest) = entry.embed_text.strip_prefix("[Argument: ") {
                    if let Some(end) = rest.find(']') {
                        let arg_name = rest[..end].trim().to_lowercase();
                        if arg_name.len() >= 4 {
                            let toks: Vec<&str> = arg_name.split_whitespace().collect();
                            for w in toks.windows(2) {
                                let phrase = format!("{} {}", w[0], w[1]);
                                if phrase.len() >= 6 && q_lower.contains(&phrase) {
                                    hit = true;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            if !hit {
                continue;
            }
            let s = cosine(query_embedding, &entry.embedding).max(0.6);
            name_seeds.push((ctx.atlas_corpus_id.clone(), entry.atom_id.clone(), s));
        }
    }

    // Merge vector + name seeds, dedup by (corpus_id, atom_id), keep the max
    // score. Name additions are an intentional broadening beyond max_seeds.
    let mut merged: HashMap<(String, String), f32> = HashMap::new();
    for (cid, aid, s) in primary_seeds.into_iter().chain(name_seeds.into_iter()) {
        merged
            .entry((cid, aid))
            .and_modify(|e| {
                if s > *e {
                    *e = s;
                }
            })
            .or_insert(s);
    }
    let seeds: Vec<(String, String, f32)> = merged
        .into_iter()
        .filter(|((cid, _), _)| graph_by_id.contains_key(cid.as_str()))
        .map(|((cid, aid), s)| (cid, aid, s))
        .collect();

    // 2. BFS expand from each seed — identical logic to atlas_navigate.
    let mut neighborhood: HashMap<(String, String), f32> = HashMap::new();
    for (atlas_id, atom_id, seed_score) in &seeds {
        let Some(graph) = graph_by_id.get(atlas_id.as_str()) else {
            continue;
        };
        let key = (atlas_id.clone(), atom_id.clone());
        let entry = neighborhood.entry(key).or_insert(0.0);
        *entry = entry.max(*seed_score);

        let mut frontier: Vec<(String, f32)> = vec![(atom_id.clone(), *seed_score)];
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(atom_id.clone());
        let decay = 0.6_f32;

        for hop in 1..=max_hops {
            let hop_decay = decay.powi(hop as i32);
            let mut next_frontier: Vec<(String, f32)> = Vec::new();
            for (current_id, current_score) in &frontier {
                let mut consider = |neighbor_id: &str, edge_type: EdgeType, conf: f32| {
                    if visited.contains(neighbor_id) {
                        return;
                    }
                    let w = edge_weight(edge_type);
                    if w <= 0.0 {
                        return;
                    }
                    let neighbor_score = current_score * w * conf * hop_decay;
                    if neighbor_score < 0.05 {
                        return;
                    }
                    let key = (atlas_id.clone(), neighbor_id.to_string());
                    let entry = neighborhood.entry(key).or_insert(0.0);
                    if neighbor_score > *entry {
                        *entry = neighbor_score;
                    }
                    visited.insert(neighbor_id.to_string());
                    next_frontier.push((neighbor_id.to_string(), neighbor_score));
                };
                for edge in graph.edges_from(current_id) {
                    consider(edge.target, edge.edge_type, edge.confidence);
                }
                for edge in graph.edges_to(current_id) {
                    consider(edge.source, edge.edge_type, edge.confidence);
                }
            }
            if next_frontier.is_empty() {
                break;
            }
            frontier = next_frontier;
        }
    }

    // 3. Emit ChunkRequests — identical logic to atlas_navigate.
    let mut chunk_scores: HashMap<
        (String, String),
        (f32, String, Vec<String>, Vec<String>, String),
    > = HashMap::new();
    for ((atlas_id, atom_id), atom_weight) in &neighborhood {
        let Some(graph) = graph_by_id.get(atlas_id.as_str()) else {
            continue;
        };
        let evidence = graph.atom_evidence(atom_id);
        let verbatim = atom_verbatim_excerpt(graph, atom_id);
        for ev in evidence {
            let chunk_id = ev.chunk_id().trim();
            if chunk_id.is_empty() {
                continue;
            }
            let preview = ev.passage_preview().trim();
            let key = (graph.article_slug.clone(), chunk_id.to_string());
            let entry = chunk_scores.entry(key).or_insert((
                0.0,
                preview.to_string(),
                Vec::new(),
                Vec::new(),
                graph.atlas_corpus_id.clone(),
            ));
            entry.0 += atom_weight;
            if preview.len() > entry.1.len() {
                entry.1 = preview.to_string();
            }
            entry.2.push(atom_id.clone());
            if let Some(line) = verbatim.as_ref() {
                if !entry.3.iter().any(|existing| existing == line) {
                    entry.3.push(line.clone());
                }
            }
        }
    }

    let mut requests: Vec<ChunkRequest> = chunk_scores
        .into_iter()
        .map(
            |((article_slug, chunk_id), (score, preview, motivating, verbatim, corpus_id))| {
                ChunkRequest {
                    corpus_id,
                    article_slug,
                    chunk_id,
                    passage_preview: preview,
                    score,
                    motivating_atoms: motivating,
                    verbatim_excerpts: verbatim,
                }
            },
        )
        .collect();
    requests.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    requests
}

/// Max chars of rendered atom text fed to the embedder — the cap
/// [`render_atom_entry`] truncates to. The loaders share the renderer, so an
/// entry's `embed_text` is stable across the build (embed) and read (bag) paths.
const ATLAS_ENTRY_CHAR_LIMIT: usize = 3000;

/// Join-coverage diagnostic from [`build_persistent_ann_seed_table`] — the
/// glassbox number the 3b go/no-go watches: `resolved` of `total` bag entries
/// became ANN rows (the rest had no embedding or didn't resolve to an atom-id,
/// which the v1 cosine path also drops, so the seedable set matches).
#[derive(Debug, Clone, Copy)]
pub struct AnnBuildStats {
    pub resolved: usize,
    pub total: usize,
}

/// ATLAS_STORAGE_V2 backfill: write the persistent per-corpus ANN seed table
/// (`<atlas_dir>/atoms_ann.lance`) from an already-embedded [`AtlasContext`].
/// Each atom-bearing entry contributes `(atom_id, embedding)` — `atom_id` is
/// first-class on the entry (Phase B), so this is a pure transform with no
/// reverse-resolve. Idempotent: a stale table dir is removed first.
/// Lifecycle-time only (the CLI backfill / migrate-all / enrich completion),
/// never the hot query path.
pub async fn build_persistent_ann_seed_table(
    atlas_dir: &Path,
    ctx: &AtlasContext,
) -> Result<AnnBuildStats, String> {
    let mut rows: Vec<(String, Vec<f32>)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let total = ctx.entries.len();
    for entry in &ctx.entries {
        // Entries with no backing atom (eval-only Tension virtual chunks) or no
        // embedding can't seed the ANN table; the read-path bag drops them too.
        if entry.atom_id.is_empty() || entry.embedding.is_empty() {
            continue;
        }
        // First-seen wins (deterministic); duplicates are the same atom.
        if !seen.insert(entry.atom_id.clone()) {
            continue;
        }
        rows.push((entry.atom_id.clone(), entry.embedding.clone()));
    }
    let resolved = rows.len();
    if resolved == 0 {
        return Err(format!(
            "no atom-bearing entries for {} (0/{total}) — nothing to index",
            ctx.atlas_corpus_id
        ));
    }
    let dir = corpus_engine::enrichment::atlas::ann_store::ann_table_dir(atlas_dir);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("remove stale ANN table {}: {e}", dir.display()))?;
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("create ANN table dir: {e}"))?;
    AnnSeedTable::build(&dir, &rows).await?;
    Ok(AnnBuildStats { resolved, total })
}

/// ATLAS_STORAGE_V2 3b: open the persistent ANN seed table under `atlas_dir` and
/// attach it to `graph`. MUST run on the caller's long-lived async runtime — the
/// held `lancedb::Table` is queried later by [`atlas_navigate_ann`], so opening
/// it on a throwaway runtime (e.g. the sync `load_from_disk` bridge) would
/// invalidate it; see [`AtlasGraph::with_ann_seed_table`]. No-op when no table
/// is present; a present-but-unreadable table is non-fatal (the graph keeps its
/// v1 cosine seed path). The single attach path shared by the daemon's
/// `AtlasContextManager` and the eval's `--atlas-seed ann` verify, so both load
/// the ANN exactly as the daemon does.
pub async fn open_and_attach_ann_seed_table(
    corpus_id: &str,
    atlas_dir: &Path,
    graph: AtlasGraph,
) -> AtlasGraph {
    if !corpus_engine::enrichment::atlas::ann_store::ann_table_present(atlas_dir) {
        return graph;
    }
    match AnnSeedTable::open_for_atlas(atlas_dir).await {
        Ok(ann) => {
            tracing::info!(
                corpus = corpus_id,
                "atlas-graph: ANN seed table attached (v2 seeding)"
            );
            graph.with_ann_seed_table(Arc::new(ann))
        }
        Err(e) => {
            tracing::warn!(
                corpus = corpus_id,
                error = %e,
                "atlas-graph: ANN seed table present but unreadable; using v1 cosine seeding"
            );
            graph
        }
    }
}

/// Render one atom into its `(canonical_name, embed_text)` bag pair — the SINGLE
/// source of the atlas embed-text shape, shared by the build-time embedder
/// (eval / backfill, over `atoms.json`) and the read-time bag builder
/// ([`build_atlas_context_from_ann`], over the resident store). `canonical_name`
/// is the Entity's name (so rigid source-matching credits it) or the
/// `article_slug` for the article-scoped kinds (Claim / Configuration /
/// ArgumentReconstruction). `None` for atom kinds that never enter the bag. Both
/// paths sharing this guarantees the embedding written to the ANN table
/// corresponds to the bag's re-rendered `embed_text`. `pub` so the eval / backfill
/// loaders reuse it rather than forking the rendering.
pub fn render_atom_entry(atom: &AtomEnvelope, article_slug: &str) -> Option<(String, String)> {
    match atom {
        AtomEnvelope::Entity(e) => {
            let mut text = String::new();
            text.push_str(&e.canonical_name);
            text.push('\n');
            if !e.aliases.is_empty() {
                text.push_str(&e.aliases.join(", "));
                text.push('\n');
            }
            text.push_str(&e.description);
            if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
            }
            Some((e.canonical_name.clone(), text))
        }
        AtomEnvelope::Claim(c) => {
            let act = serde_json::to_string(&c.discourse_act)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            let status = serde_json::to_string(&c.epistemic_status)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            let mut text = format!("[Claim: {act}, {status}] {content}", content = c.content);
            if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
            }
            Some((article_slug.to_string(), text))
        }
        AtomEnvelope::Configuration(cfg) => {
            let mut text = format!("[Configuration: {}] {}", cfg.label, cfg.description);
            if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
            }
            Some((article_slug.to_string(), text))
        }
        AtomEnvelope::ArgumentReconstruction(a) => {
            let mut text = String::with_capacity(256);
            text.push_str("[Argument: ");
            text.push_str(&a.name);
            text.push_str("] ");
            for p in &a.premises {
                text.push_str(p);
                text.push(' ');
            }
            text.push_str(&a.conclusion);
            for o in &a.objections {
                if !o.content.trim().is_empty() {
                    text.push(' ');
                    text.push_str(o.content.trim());
                } else if !o.name.trim().is_empty() {
                    text.push(' ');
                    text.push_str(o.name.trim());
                }
            }
            if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
            }
            Some((article_slug.to_string(), text))
        }
        _ => None,
    }
}

/// Build the query-time embedding bag from a corpus's persistent ANN seed table
/// joined to its resident atoms — the ATLAS_STORAGE_V2 Phase B read path. Atom
/// embeddings live ONLY in `atoms_ann.lance` (written once at enrich / backfill);
/// the bag is derived here at load with no re-embed and no `atoms.embeddings.bin`
/// sidecar. Each ANN row's `(atom_id, embedding)` joins to the resident atom for
/// its rendered `(canonical_name, embed_text)` via [`render_atom_entry`], so the
/// bag's text matches the text the embedding represents. Requires `graph` to
/// carry an ANN table (attached by [`open_and_attach_ann_seed_table`]); a corpus
/// with no table yields no bag (it then contributes only name-match seeds).
pub async fn build_atlas_context_from_ann(
    atlas_corpus_id: &str,
    graph: &AtlasGraph,
    top_k: usize,
) -> Result<AtlasContext, String> {
    let Some(ann) = graph.ann_seed_table() else {
        return Err(format!(
            "no ANN seed table for {atlas_corpus_id}; backfill with `sovereign atlas backfill-ann`"
        ));
    };
    let rows = ann.all_rows().await?;
    let mut entries: Vec<AtlasEntry> = Vec::with_capacity(rows.len());
    for (atom_id, embedding) in rows {
        // Join the ANN row back to its resident atom for the rendered text. An
        // atom referenced by the table but absent from the store (a torn build)
        // is skipped rather than fatal.
        let Some(envelope) = graph.atom(&atom_id).and_then(|v| v.atom_envelope()) else {
            continue;
        };
        let Some((canonical_name, embed_text)) = render_atom_entry(&envelope, &graph.article_slug)
        else {
            continue;
        };
        entries.push(AtlasEntry {
            atom_id,
            canonical_name,
            embed_text,
            embedding,
        });
    }
    Ok(AtlasContext {
        atlas_corpus_id: atlas_corpus_id.to_string(),
        entries,
        top_k,
    })
}

/// Cosine similarity. Returns 0 on zero-length vectors or
/// dimension mismatch — both are signs of a misconfigured loader,
/// and silently degrading to zero score keeps retrieval going
/// rather than poisoning a query.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = (na.sqrt() * nb.sqrt()).max(1e-9);
    dot / denom
}

/// Score every entry by cosine sim to `query_embedding`, take the
/// top-K from `ctx`, return as virtual `ScoredChunk`s. Each chunk's
/// `corpus_id` is `atlas:<corpus_id>` so downstream provenance keeps
/// the origin obvious — the per-question report distinguishes
/// "wikipedia chunk" from "atlas-derived virtual chunk."
///
/// Phase C4 — every chunk also carries provenance metadata so eval
/// `--inspect` and the desktop's hit attribution can surface where
/// each result actually came from:
///
///   - `metadata["source"] = "atlas"` — discriminator for atlas vs
///     chunk vs mesh-peer hits.
///   - `metadata["atlas_corpus"] = <corpus_id>` — the underlying
///     corpus the atlas was built over.
///   - `metadata["atlas_tier"] = "tier-2"` — for now we only carry
///     extracted entries (see `AtlasContextFilter::default`); a
///     future per-entry tier would land here when the loader
///     surfaces mixed depths.
pub fn atlas_top_k_as_chunks(query_embedding: &[f32], ctx: &AtlasContext) -> Vec<ScoredChunk> {
    atlas_top_k_across(query_embedding, std::slice::from_ref(&ctx), ctx.top_k)
}

/// Multi-atlas variant: pool every entry across `ctxs`, score them
/// together, and return the global top-`k_total`. Each chunk carries
/// the metadata of the atlas it actually came from — so a virtual
/// chunk surfaced from `sep-consciousness` keeps `atlas:sep-consciousness`
/// as its corpus_id even when several atlases were considered.
///
/// Why a global top-K rather than per-atlas K then truncate: when
/// retrieval pools several per-article SEP atlases, the right 3
/// answers may all live in the topically-aligned atlas — a per-atlas
/// fairness budget would dilute that with noisy off-topic surfaces
/// from the other articles. The cosine score is the right
/// arbitrator.
pub fn atlas_top_k_across(
    query_embedding: &[f32],
    ctxs: &[&AtlasContext],
    k_total: usize,
) -> Vec<ScoredChunk> {
    if k_total == 0 {
        return Vec::new();
    }
    let mut scored: Vec<(f32, &AtlasContext, &AtlasEntry)> = Vec::new();
    for ctx in ctxs {
        for entry in &ctx.entries {
            let s = cosine(query_embedding, &entry.embedding);
            scored.push((s, ctx, entry));
        }
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k_total);
    scored
        .into_iter()
        .map(|(score, ctx, e)| {
            let mut metadata = HashMap::new();
            metadata.insert("source".to_string(), "atlas".to_string());
            metadata.insert("atlas_corpus".to_string(), ctx.atlas_corpus_id.clone());
            metadata.insert("atlas_tier".to_string(), "tier-2".to_string());
            ScoredChunk {
                content: e.embed_text.clone(),
                title: Some(e.canonical_name.clone()),
                url: None,
                corpus_id: format!("atlas:{}", ctx.atlas_corpus_id),
                score,
                metadata,
                chunk_id: None,
                source_doc_id: None,
                vector_distance: None,
            }
        })
        .collect()
}

/// Source of `AtlasContext`s, looked up at query time. The runtime
/// holds an `Option<Arc<dyn AtlasContextProvider>>` and consults it
/// inside the chunk-retrieval path; the daemon's
/// `AtlasContextManager` is the production implementation, while
/// the eval CLI builds one inline from `ChatSession`.
pub trait AtlasContextProvider: Send + Sync {
    /// Look up a pre-loaded context by its atlas corpus id. Returns
    /// `None` when no atlas has been loaded for that id (e.g. the
    /// corpus has no `atlas/` dir, or daemon boot is still warming).
    fn get(&self, atlas_corpus_id: &str) -> Option<Arc<AtlasContext>>;

    /// All atlas corpus ids currently loaded. Used by the runtime
    /// to fuse atlas grounding for every installed corpus that has
    /// one — the caller doesn't need to know which corpora have
    /// atlases ahead of time.
    fn loaded_corpus_ids(&self) -> Vec<String>;

    /// Record that `canonical_name` from `atlas_corpus_id` matched a
    /// query (i.e. it landed in the top-K returned by
    /// [`atlas_top_k_as_chunks`]). Persisted as a per-corpus bump
    /// map and consumed by the next triage rebuild as a centrality
    /// addition — articles users actually ask about move up the
    /// Tier-2 enrichment queue. Default: no-op (eval CLI doesn't
    /// need adaptive triage).
    fn record_match(&self, _atlas_corpus_id: &str, _canonical_name: &str) {}

    /// Look up the structural graph layer for an atlas — atom-by-id,
    /// edge adjacency. Used by [`atlas_navigate`] to walk the typed
    /// knowledge graph beyond bag-of-atoms cosine matching. Default
    /// `None` for providers that haven't loaded the graph layer yet
    /// (back-compat with the entity-only embedding cache); they fall
    /// back to [`atlas_top_k_as_chunks`].
    fn graph(&self, _atlas_corpus_id: &str) -> Option<Arc<AtlasGraph>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, embed: Vec<f32>) -> AtlasEntry {
        AtlasEntry {
            atom_id: format!("entity-{name}"),
            canonical_name: name.to_string(),
            embed_text: format!("{name} desc"),
            embedding: embed,
        }
    }

    #[test]
    fn cosine_matches_identical_vector_at_one() {
        let v = vec![1.0, 2.0, 3.0];
        let s = cosine(&v, &v);
        assert!((s - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_zero_on_dim_mismatch() {
        assert_eq!(cosine(&[1.0, 2.0], &[1.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
    }

    #[test]
    fn top_k_returns_highest_cosine_first() {
        let ctx = AtlasContext {
            atlas_corpus_id: "test".into(),
            entries: vec![
                entry("Far", vec![-1.0, -1.0]),
                entry("Near", vec![1.0, 1.0]),
                entry("Mid", vec![1.0, 0.0]),
            ],
            top_k: 2,
        };
        let q = vec![1.0, 1.0];
        let chunks = atlas_top_k_as_chunks(&q, &ctx);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].title.as_deref(), Some("Near"));
        assert_eq!(chunks[0].corpus_id, "atlas:test");
    }

    /// Phase C4: every atlas chunk carries provenance metadata so
    /// downstream consumers can distinguish atlas vs chunk vs mesh
    /// hits without sniffing the corpus_id prefix.
    #[test]
    fn atlas_chunks_carry_provenance_metadata() {
        let ctx = AtlasContext {
            atlas_corpus_id: "wikipedia".into(),
            entries: vec![entry("Earth", vec![1.0, 0.0])],
            top_k: 1,
        };
        let chunks = atlas_top_k_as_chunks(&[1.0, 0.0], &ctx);
        let m = &chunks[0].metadata;
        assert_eq!(m.get("source").map(|s| s.as_str()), Some("atlas"));
        assert_eq!(m.get("atlas_corpus").map(|s| s.as_str()), Some("wikipedia"));
        assert_eq!(m.get("atlas_tier").map(|s| s.as_str()), Some("tier-2"));
    }
}

#[cfg(test)]
mod store_io_tests {
    //! L5 — the v2 store read path end to end: projection fidelity through
    //! [`AtomView`] and the `atoms.lance` + `edges.csr` load.
    use super::*;
    use corpus_engine::enrichment::atlas::atoms::AtomId;
    use corpus_engine::enrichment::atlas::store;
    use corpus_engine::enrichment::atlas::{
        AtomEnvelope, ChunkRef, Edge, EdgeId, EdgeProvenance, EdgeType, Entity,
    };
    use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

    fn sample_entity(n: usize, name: &str, salience: f32) -> Entity {
        Entity {
            id: AtomId::entity(n),
            canonical_name: name.into(),
            aliases: vec![format!("{name}-alias")],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new(format!("sec_{n:04}"), Some("preview text".into())),
            description: format!("desc of {name}"),
            defining_quote: None,
            salience,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        }
    }

    fn sample_edge(n: usize, source: AtomId, target: AtomId) -> Edge {
        Edge {
            id: EdgeId::new(n),
            edge_type: EdgeType::Involves,
            source,
            target,
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        }
    }

    /// The v2 store, read back through the public `AtlasGraph` API: projected
    /// fields via `AtomView`, edge adjacency + degree, typed enumeration, and
    /// the deep `atom_envelope` parse.
    #[test]
    fn v2_store_projects_fields_and_edges() {
        let atoms = vec![
            AtomEnvelope::Entity(sample_entity(1, "Alice", 0.9)),
            AtomEnvelope::Entity(sample_entity(2, "Bob", 0.4)),
        ];
        let id1 = atoms[0].id().as_str().to_string();
        let id2 = atoms[1].id().as_str().to_string();
        let edge = sample_edge(1, AtomId::entity(1), AtomId::entity(2));

        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path();
        store::write_store_blocking(atlas_dir, "c1", &atoms, std::slice::from_ref(&edge)).unwrap();
        let graph = AtlasGraph::load_lance_from_disk("c1", atlas_dir).unwrap();

        assert_eq!(graph.atom_count(), 2);
        assert_eq!(graph.edge_count(), 1);

        let a = graph.atom(&id1).expect("lookup id1");
        assert_eq!(a.kind(), AtomKindTag::Entity);
        assert_eq!(a.name(), "Alice");
        assert_eq!(a.subtype(), EntityType::Person.as_str_repr());
        assert_eq!(a.description(), "desc of Alice");
        assert!((a.salience() - 0.9).abs() < 1e-6);
        assert_eq!(a.alias_count(), 1);
        let ev: Vec<_> = a.evidence().collect();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].chunk_id(), "sec_0001");
        assert_eq!(ev[0].passage_preview(), "preview text");

        // Typed enumeration touches only the projected kind tag.
        assert_eq!(graph.atoms_of_kind(AtomKindTag::Entity).count(), 2);
        assert_eq!(graph.atoms_of_kind(AtomKindTag::Claim).count(), 0);

        // Edge adjacency + degree.
        assert_eq!(graph.edge_degree(&id1), 1);
        assert_eq!(graph.edge_degree(&id2), 1);
        let from1 = graph.edges_from(&id1);
        assert_eq!(from1.len(), 1);
        assert_eq!(from1[0].target, id2.as_str());
        assert_eq!(graph.edges_to(&id2).len(), 1);
        assert_eq!(graph.edges_from(&id2).len(), 0);

        // Deep parse round-trips the full atom from its payload blob.
        match a.atom_envelope().expect("payload parses") {
            AtomEnvelope::Entity(e) => assert_eq!(e.canonical_name, "Alice"),
            _ => panic!("expected entity payload"),
        }

        assert!(graph.atom("no-such-id").is_none());
    }

    /// `load_from_disk` loads the v2 store when present and errors (no
    /// fallback) when absent — the ATLAS_STORAGE_V2 "no v2 store ⇒ Err"
    /// invariant that lets wikipedia (no atom store) be skipped by the caller
    /// rather than stranded.
    #[test]
    fn load_from_disk_requires_a_v2_store() {
        use corpus_engine::enrichment::atlas::ATLAS_DIRNAME;

        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("c1").join(ATLAS_DIRNAME);
        std::fs::create_dir_all(&atlas_dir).unwrap();
        let id1 = AtomId::entity(1).as_str().to_string();

        // No v2 store yet → Err, never a panic or a silent empty graph.
        assert!(AtlasGraph::load_from_disk("c1", &atlas_dir).is_err());

        // Write the v2 store → load_from_disk serves it.
        let atoms = vec![
            AtomEnvelope::Entity(sample_entity(1, "Alice", 0.9)),
            AtomEnvelope::Entity(sample_entity(2, "Bob", 0.4)),
        ];
        let edge = sample_edge(1, AtomId::entity(1), AtomId::entity(2));
        store::write_store_blocking(&atlas_dir, "c1", &atoms, std::slice::from_ref(&edge)).unwrap();
        let g = AtlasGraph::load_from_disk("c1", &atlas_dir).unwrap();
        assert_eq!(g.atom_count(), 2);
        assert_eq!(g.atom(&id1).unwrap().name(), "Alice");

        // Remove the store → Err again (the no-fallback invariant).
        std::fs::remove_dir_all(atlas_dir.join(store::ATOMS_LANCE_DIRNAME)).unwrap();
        assert!(AtlasGraph::load_from_disk("c1", &atlas_dir).is_err());
    }
}
