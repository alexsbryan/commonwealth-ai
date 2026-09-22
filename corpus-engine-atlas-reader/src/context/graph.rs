// SPDX-License-Identifier: AGPL-3.0-or-later
//! `AtlasGraph` — the structural half of the atlas: the v2 atom store
//! (resident `atoms.lance` records + the `edges.csr` mmap adjacency), its
//! disk and in-memory loaders, its builder-style attachors, and the
//! CallChain traversal. Split out of `context.rs` (file ceiling); every
//! public item is re-exported at `crate::context`, so historical paths hold.

use super::views::contains_whole_word;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ann_store::AnnSeedTable;
use crate::atoms::AtomType;
use crate::edges::{EdgeProvenance, EdgeType};
use crate::evidence_site::EvidenceSite;
use crate::inventory::AtlasInventory;
use crate::provider::NavigationSource;
use crate::store::LancePreload;
use understanding_atlas::enrichment::ontology::type_index::TypeIndex;
use understanding_vocab::ontology::{NavigationPolicy, OntologyPolicies};

use super::views::{
    AtomView, CallChainNode, CallChainResult, CallDirection, EdgeView, EvidenceRef,
};

/// Sibling to [`AtlasContext`](super::AtlasContext) — the structural graph layer that
/// cosine-only retrieval ignores. The atlas is a typed knowledge
/// graph (see `corpus-engine/ATLAS.md`); cosine/ANN matching over atom
/// embeddings ("bag-of-atoms") finds seeds, but the substantive
/// structure — dialectical tensions, grounding chains, configuration
/// constituents — lives on the edges. [`atlas_navigate_ann`](super::atlas_navigate_ann) walks that
/// graph from seeded atoms to surface the chunk-evidence neighborhood.
///
/// **Storage (ATLAS_STORAGE_V2).** The graph is the v2 store: atoms read
/// resident from `atlas/atoms.lance` (the projected [`AtomRecord`](crate::projection::AtomRecord)s) + the
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
    /// Where this atlas's evidence chunks actually live, and whether a title
    /// filter applies. Replaces the old `article_slug: String`, which carried
    /// the article for a per-article atlas and the CORPUS ID for a self-hosted
    /// one — a conflation that made `sep-freewill` look like a searchable
    /// corpus and cost every SEP answer its atlas grounding. See
    /// [`crate::evidence_site`] for the incident and the reasoning.
    pub site: EvidenceSite,
    /// The v2 store backend: `atoms.lance` atoms read resident (the projected
    /// [`AtomRecord`]s) + the `edges.csr` mmap. The sync query API reads
    /// straight off the resident records + the CSR mmap — no async ripple into
    /// `atlas_navigate`. `Arc` so the graph stays cheaply `Clone` (the daemon
    /// hands out `Arc<AtlasGraph>`; the eval CLI holds `Vec<AtlasGraph>`). See
    /// [`LancePreload`].
    preload: Arc<LancePreload>,
    /// Persistent per-corpus ANN seed table (`atlas/atoms_ann.lance`), when the
    /// corpus has been backfilled (ATLAS_STORAGE_V2 3b). `Some` selects the ANN
    /// seed path in [`atlas_navigate_ann`](super::atlas_navigate_ann). Attached by the long-lived loader
    /// (`AtlasContextManager` / eval runner) via
    /// [`AtlasGraph::with_ann_seed_table`], never the sync open bridge.
    ann: Option<Arc<AnnSeedTable>>,
    /// The ontology this atlas was extracted under, read from
    /// `atlas/ontology.json` by [`Self::load_lance_from_disk`]. This is the
    /// vocabulary carrier for ontology-v1 P5: every reader that needs to know
    /// "what did the author declare" asks the graph it is already holding,
    /// rather than re-opening the atlas dir or growing a second projection
    /// beside `_summary.json`'s [`crate::summary::OntologySummary`].
    ///
    /// **`None` is load-bearing.** [`Self::with_ontology`] drops a policy set
    /// with no declared types, so `Some` means *this corpus declared types* —
    /// the single structural gate (I5) that keeps SEP, Wikipedia and Enron off
    /// every declared-type code path. A caller never re-checks
    /// `has_declarations()`; the `Option` already answered.
    ontology: Option<Arc<OntologyPolicies>>,
    /// The indexes dir this atlas was opened from; `None` for an in-memory
    /// graph, which is a real answer rather than a gap. One consumer — see
    /// [`Self::summary_corpus_dir`].
    index_root: Option<PathBuf>,
    /// `section_id -> chunk ids`, read from this atlas's `chapters.json`
    /// (`ChapterEntry::chunk_ids`) at load.
    ///
    /// This is the EXACT answer to "which chunks is `sec_0011`?", and it is
    /// the answer [`ChunkRequest`](super::ChunkRequest)'s own doc has always promised — "resolved
    /// by direct lookup in the article's chapters.json source, no FTS or
    /// vector search needed". Until 2026-09-09 nothing read it, so the
    /// resolver fell to searching the atom's passage preview and the chunk an
    /// atom NAMED could lose to base retrieval and never reach the reader.
    ///
    /// Empty is a real answer, not a gap: an in-memory graph has no manifest,
    /// and a corpus whose join was never filled by `svrn enrich
    /// backfill-sections` genuinely has no mapping. The resolver reports which
    /// path it took rather than defaulting silently (ARCH §18.3).
    section_rows: Arc<HashMap<String, Vec<u64>>>,
    /// The navigation map this atlas walks under, kept APART from
    /// [`Self::ontology`] because [`Self::with_ontology`] drops a policy set
    /// that declares no types — right for every declared-type code path (I5),
    /// wrong for the rows, which a typeless map (engineering) carries all the
    /// same. Read off `ontology.json` by [`Self::load_lance_from_disk`]
    /// whether or not types are declared, or attached by a loader from the
    /// corpus's pipeline ([`Self::with_pipeline_map`]) when there is no file.
    navigation: Option<NavigationAttachment>,
}

/// How a navigation map reached this graph — the owned form of
/// [`NavigationSource`].
#[derive(Debug, Clone, PartialEq)]
pub enum NavigationAttachment {
    Declared(NavigationPolicy),
    PipelineDefault {
        pipeline: String,
        policy: NavigationPolicy,
    },
}

impl std::fmt::Debug for AtlasGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtlasGraph")
            .field("atlas_corpus_id", &self.atlas_corpus_id)
            .field("site", &self.site)
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
    pub fn load_from_disk(
        atlas_corpus_id: &str,
        atlas_dir: &Path,
        section_rows: HashMap<String, Vec<u64>>,
    ) -> Result<Self, String> {
        if !v2_store_present(atlas_dir) {
            return Err(format!(
                "no v2 atlas store for {atlas_corpus_id} at {} (need atoms.lance + edges.csr); \
                 rebuild with `sovereign atlas migrate-all`",
                atlas_dir.display()
            ));
        }
        let g = Self::load_lance_from_disk(atlas_corpus_id, atlas_dir, section_rows)?;
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
        Self::from_lance_preload_at(atlas_corpus_id, preload, None)
    }

    /// [`Self::from_lance_preload`] with the atlas's DECLARED evidence site,
    /// when one was read off disk. `None` derives it from the atlas id — the
    /// compat path for atlases written before the declaration existed.
    pub fn from_lance_preload_at(
        atlas_corpus_id: &str,
        preload: LancePreload,
        declared_site: Option<EvidenceSite>,
    ) -> Self {
        let site = EvidenceSite::declared_or_derived(atlas_corpus_id, declared_site);
        Self {
            atlas_corpus_id: atlas_corpus_id.to_string(),
            site,
            preload: Arc::new(preload),
            ann: None,
            ontology: None,
            index_root: None,
            section_rows: Arc::new(HashMap::new()),
            navigation: None,
        }
    }

    /// The map this graph walks under, if any — see the field.
    pub fn navigation(&self) -> Option<NavigationSource<'_>> {
        match &self.navigation {
            Some(NavigationAttachment::Declared(p)) => Some(NavigationSource::Declared(p)),
            Some(NavigationAttachment::PipelineDefault { pipeline, policy }) => {
                Some(NavigationSource::PipelineDefault { pipeline, policy })
            }
            None => None,
        }
    }

    /// The rows `ontology.json` carried, types or no types. Set by the disk
    /// loader; `None` leaves the graph mapless.
    pub fn with_declared_navigation(mut self, navigation: Option<NavigationPolicy>) -> Self {
        if let Some(p) = navigation {
            self.navigation = Some(NavigationAttachment::Declared(p));
        }
        self
    }

    /// Attach a built-in pipeline's declared map to a graph that has no map
    /// of its own — the loader's fallback for an atlas whose `ontology.json`
    /// has not been converted in yet. A declared map is never displaced: this
    /// is a no-op when one is present, so a loader cannot override what the
    /// atlas dir itself says.
    pub fn with_pipeline_map(mut self, pipeline: &str, policy: NavigationPolicy) -> Self {
        if self.navigation.is_none() {
            self.navigation = Some(NavigationAttachment::PipelineDefault {
                pipeline: pipeline.to_string(),
                policy,
            });
        }
        self
    }

    /// Set by the disk loader and nobody else: an in-memory caller has no root
    /// and must not invent one.
    pub fn with_index_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.index_root = Some(root.into());
        self
    }

    /// Attach the `section_id -> chunk ids` join this atlas's `chapters.json`
    /// records. Read once at load; see [`Self::section_rows`].
    pub fn with_section_rows(mut self, rows: HashMap<String, Vec<u64>>) -> Self {
        self.section_rows = Arc::new(rows);
        self
    }

    /// The chunk ids this atlas's manifest records for `section_id`, or `&[]`
    /// when the corpus keeps no join. See the field doc for why empty is an
    /// answer rather than a gap.
    pub fn section_rows(&self, section_id: &str) -> &[u64] {
        self.section_rows
            .get(section_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The CHUNK corpus's dir, `<indexes>/<site.chunk_corpus()>` — where the
    /// `raptor` summary source reads (`ground::summaries`). Through
    /// [`EvidenceSite`] because the atlas id is NOT the corpus id for a
    /// per-article atlas: `<indexes>/sep-freewill` holds no table.
    pub fn summary_corpus_dir(&self) -> Option<PathBuf> {
        self.index_root
            .as_ref()
            .map(|r| r.join(self.site.chunk_corpus().as_str()))
    }

    /// Open the v2 store under `atlas_dir` (`atoms.lance` + `edges.csr`) and
    /// build the graph. Sync (bridges the async open via
    /// [`LancePreload::open_blocking`]) so it slots into the daemon's sync load
    /// path; lifecycle-time only (boot / corpus load), never the hot query path.
    pub fn load_lance_from_disk(
        atlas_corpus_id: &str,
        atlas_dir: &Path,
        section_rows: HashMap<String, Vec<u64>>,
    ) -> Result<Self, String> {
        let preload = LancePreload::open_blocking(atlas_dir)
            .map_err(|e| format!("open v2 store for {atlas_corpus_id}: {e}"))?;
        let file = crate::raw::read_atlas_ontology(atlas_dir);
        // The rows are kept whether or not the map declares types; the
        // declared-type gate is `with_ontology`'s alone.
        let navigation = file.as_ref().map(|f| f.policies.navigation.clone());
        let ontology = file.map(|f| f.policies);
        // `<indexes>/<corpus>/atlas` -> `<indexes>`, derived from the path the
        // caller already gave rather than added to the signature: no call site
        // changes and none can pass a root that disagrees with what it opened.
        let root = atlas_dir.parent().and_then(|p| p.parent());
        let g = Self::from_lance_preload(atlas_corpus_id, preload)
            .with_ontology(ontology)
            .with_declared_navigation(navigation)
            .with_section_rows(section_rows);
        Ok(match root {
            Some(r) => g.with_index_root(r),
            None => g,
        })
    }

    /// The display label for this atlas's evidence site — the article for a
    /// per-article atlas, the corpus id for a self-hosted one.
    ///
    /// This is what the old `article_slug` FIELD returned, kept as an accessor
    /// so the six display/grouping sites read unchanged. It is deliberately
    /// NOT a corpus: ask [`EvidenceSite::chunk_corpus`] for that. Conflating
    /// the two is the defect `evidence_site` exists to prevent.
    pub fn article_slug(&self) -> &str {
        self.site.label()
    }

    /// Attach the declared ontology read from `atlas/ontology.json`. Policies
    /// that declare no types are dropped to `None` HERE, once, so no consumer
    /// has to remember the `has_declarations()` check — see the field doc.
    pub fn with_ontology(mut self, policies: Option<OntologyPolicies>) -> Self {
        let declared = policies.filter(|p| p.has_declarations());
        tracing::debug!(
            corpus = %self.atlas_corpus_id,
            declared_types = declared.as_ref().map(|p| p.shape.types.len()).unwrap_or(0),
            "atlas graph: declared vocabulary attached"
        );
        self.ontology = declared.map(Arc::new);
        self
    }

    /// The declared ontology, or `None` for a corpus that declared nothing.
    pub fn ontology(&self) -> Option<&OntologyPolicies> {
        self.ontology.as_deref()
    }

    /// Is `subtype` the declared type `target`, or a `specializes` descendant
    /// of it? `sceatta specializes coin`, so `is_subtype_of("sceatta", "coin")`
    /// is true and an enumeration of `coin` includes the sceattas.
    ///
    /// Always `false` for a corpus that declared nothing, which is what makes
    /// an undeclared corpus's equality compare byte-identical: callers write
    /// `a == b || graph.is_subtype_of(a, b)` and the second term is inert.
    /// The `specializes` walk itself is [`TypeIndex::is_a`] — the ONE place the
    /// chain is walked (§10.6); this is the graph-side accessor for it, not a
    /// second implementation.
    pub fn is_subtype_of(&self, subtype: &str, target: &str) -> bool {
        match self.ontology() {
            Some(p) => TypeIndex::from_policies(p).is_a(subtype, target),
            None => false,
        }
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
    /// retrieval caller checks to pick [`atlas_navigate_ann`](super::atlas_navigate_ann) over the v1
    /// cosine seed in [`atlas_navigate`].
    pub fn has_ann_seed_table(&self) -> bool {
        self.ann.is_some()
    }

    /// The census the store took at open — see [`AtlasInventory`].
    pub fn inventory(&self) -> &AtlasInventory {
        self.preload.inventory()
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
        self.preload.atom(atom_id).map(AtomView::new)
    }

    /// All atoms, in interned local-id order.
    pub fn atoms(&self) -> impl Iterator<Item = AtomView<'_>> + '_ {
        self.preload.atoms().map(AtomView::new)
    }

    /// Atoms of one kind — the typed-enumeration filter. Reads only the
    /// projected type tag per atom (no payload parse), so a full scan of
    /// the 1.67M-atom wikipedia atlas is ~2ms (Phase 0).
    pub fn atoms_of_kind(&self, kind: AtomType) -> impl Iterator<Item = AtomView<'_>> + '_ {
        self.atoms().filter(move |v| v.kind() == kind)
    }

    /// Evidence ChunkRefs for an atom-id, normalised across atom types by
    /// `AtomEnvelope::evidence`, the canonical per-variant accessor the v2
    /// store projection also uses.
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

    /// Multi-hop **call chain** from `seed_atom_id` over the code atlas's
    /// `ScipStructural` (call / use / impl) edges — the "talk to your
    /// architecture" traversal (Inc 5). Bounded, cycle-safe BFS mirroring
    /// `corpus_engine_scip::ScipGraph::blast_radius`: a `visited` set seeded with
    /// the seed, a level-by-level frontier, `max_depth` hops (clamped `1..=5`),
    /// and a per-node `max_fanout` so a hot symbol can't explode the chain.
    /// Atoms come back in call order, each tagged with its depth from the seed.
    ///
    /// `direction` picks the edge orientation: [`CallDirection::Callees`]
    /// ("what does X call / how does X work / trace the flow") walks out-edges;
    /// [`CallDirection::Callers`] ("what calls X / where is X used") walks
    /// in-edges. Only `ScipStructural` edges are followed —
    /// `containment_structural` (Crate→Module→Item) and `cargo_structural`
    /// (dependency) edges carry the same `Involves` type but are not calls, so
    /// they're filtered by provenance. The walk reads the CSR via
    /// `edges_from`/`edges_to`, so it scales the way `atlas_navigate` does and
    /// the Inc-7 chat path reuses this method unchanged.
    pub fn call_chain(
        &self,
        seed_atom_id: &str,
        direction: CallDirection,
        max_depth: usize,
        max_fanout: usize,
    ) -> CallChainResult {
        let max_depth = max_depth.clamp(1, 5);
        let max_fanout = max_fanout.clamp(1, 64);

        let mut result = CallChainResult {
            corpus_id: self.atlas_corpus_id.clone(),
            direction,
            max_depth,
            nodes: Vec::new(),
            truncated: false,
        };

        // Seed must exist; otherwise an empty chain (the caller renders a miss).
        let Some(seed_view) = self.atom(seed_atom_id) else {
            return result;
        };
        result
            .nodes
            .push(self.call_node(&seed_view, 0, None, false));

        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(seed_atom_id.to_string());
        let mut frontier: Vec<String> = vec![seed_atom_id.to_string()];

        for depth in 1..=max_depth {
            if frontier.is_empty() {
                break;
            }
            let mut next: Vec<String> = Vec::new();
            for current in &frontier {
                // Scip neighbors in the chosen direction, deterministically
                // ordered (by atom-id) and fanout-capped — "first N" is stable
                // across runs, mirroring blast_radius's ORDER-BY-then-cap.
                let mut neighbors = self.scip_neighbors(current, direction);
                neighbors.sort_by(|a, b| a.0.cmp(&b.0));
                if neighbors.len() > max_fanout {
                    neighbors.truncate(max_fanout);
                    result.truncated = true;
                }
                for (neighbor_id, _conf) in neighbors {
                    if visited.contains(&neighbor_id) {
                        continue; // cycle / re-convergence — cut here.
                    }
                    let Some(view) = self.atom(&neighbor_id) else {
                        continue; // dangling endpoint (shouldn't happen post-build).
                    };
                    let dyn_dispatch = self.is_reciprocal_scip(current, &neighbor_id, direction);
                    visited.insert(neighbor_id.clone());
                    result.nodes.push(self.call_node(
                        &view,
                        depth,
                        Some(current.clone()),
                        dyn_dispatch,
                    ));
                    next.push(neighbor_id);
                }
            }
            frontier = next;
        }
        // A non-empty frontier at the depth bound means reachable atoms remain
        // beyond `max_depth` — flag the chain as bounded so the brief says so.
        if !frontier.is_empty() {
            result.truncated = true;
        }
        result
    }

    /// Project an [`AtomView`] into a [`CallChainNode`] — the citation handle
    /// (content-hash id), qualified name, subtype, summary, and first-evidence
    /// chunk id.
    fn call_node(
        &self,
        view: &AtomView<'_>,
        depth: usize,
        via: Option<String>,
        via_dyn_dispatch: bool,
    ) -> CallChainNode {
        let chunk_id = view
            .evidence()
            .next()
            .map(|e| e.chunk_id().to_string())
            .unwrap_or_default();
        CallChainNode {
            atom_id: view.id().to_string(),
            name: view.name().to_string(),
            subtype: view.subtype().to_string(),
            description: view.description().to_string(),
            chunk_id,
            depth,
            via,
            via_dyn_dispatch,
        }
    }

    /// Scip (call / use / impl) neighbors of `atom_id` in `direction`, as
    /// `(neighbor_atom_id, confidence)`. Filters to `ScipStructural` provenance
    /// so containment / dependency edges (same `Involves` type) are excluded.
    fn scip_neighbors(&self, atom_id: &str, direction: CallDirection) -> Vec<(String, f32)> {
        let views = match direction {
            CallDirection::Callees => self.edges_from(atom_id),
            CallDirection::Callers => self.edges_to(atom_id),
        };
        views
            .into_iter()
            .filter(|e| e.provenance == EdgeProvenance::ScipStructural)
            .map(|e| {
                let neighbor = match direction {
                    CallDirection::Callees => e.target,
                    CallDirection::Callers => e.source,
                };
                (neighbor.to_string(), e.confidence)
            })
            .collect()
    }

    /// True when a reciprocal `ScipStructural` edge exists between `from` and
    /// `to` (`to` also references `from` in the same direction's sense). Trait
    /// impls emit a Self↔Trait edge pair (see `EdgeProvenance::ScipStructural`),
    /// so a reciprocal pair is the structural marker for a trait / dynamic-
    /// dispatch boundary — the place a call graph beats grep, mirrored from
    /// `trace::render_trace`'s `[dyn-dispatch]` flag. Best-effort: the atlas
    /// dropped SCIP's per-ref `kind` when it built `Involves` edges, so this
    /// retained signal stands in for a literal `dyn` flag.
    fn is_reciprocal_scip(&self, from: &str, to: &str, direction: CallDirection) -> bool {
        self.scip_neighbors(to, direction)
            .iter()
            .any(|(n, _)| n == from)
    }

    /// Resolve a NAMED seed atom-id by code-symbol name. Matches `query` tokens
    /// against each atom's qualified name (e.g. `semver::eval::matches_req`):
    ///   1. the whole qualified name appears in the query → strongest,
    ///   2. else the last `::`-segment appears as a whole-word query token.
    /// Among candidates it prefers (a) non-test paths, (b) the shortest
    /// qualified name (the most public of overloaded names), then (c) atom-id
    /// for a deterministic final tie-break. `None` when nothing matches.
    ///
    /// This is the code-aware analogue of the classifier's `match_entity_target`
    /// — that keys on whitespace tokens and so never matches `::`-qualified
    /// code symbols (`semver::matches` is one whitespace token).
    pub fn resolve_symbol_seed(&self, query: &str) -> Option<String> {
        let q = query.to_lowercase();
        let q_tokens: HashSet<String> = q
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|t| t.len() >= 3)
            .map(|t| t.to_string())
            .collect();

        // Lower key sorts to "best": `(priority, is_test, rank, atom_id)`.
        //  - priority 0 = whole qualified-name mention, 1 = last-segment token.
        //  - non-test (`false`) preferred over test paths.
        //  - rank is priority-aware: for an explicit qualified mention prefer the
        //    LONGEST name (the most specific symbol the user spelled out — so
        //    `m::alpha` beats the module `m` that whole-word-matches inside it);
        //    for a last-segment token match prefer the SHORTEST path (the public
        //    top-level item over a deeply nested / overloaded one).
        //  - atom-id is the deterministic final tie-break.
        let mut best: Option<(u8, bool, usize, String)> = None;
        for view in self.atoms() {
            let name = view.name();
            if name.is_empty() {
                continue;
            }
            let lname = name.to_lowercase();
            let last_seg = lname.rsplit("::").next().unwrap_or(lname.as_str());
            let seg_count = lname.matches("::").count();
            let is_test = lname.split("::").any(|s| s == "test" || s == "tests");

            let (priority, rank) = if contains_whole_word(&q, &lname) {
                (0u8, usize::MAX - lname.len()) // longer name → smaller rank → better
            } else if !last_seg.is_empty() && q_tokens.contains(last_seg) {
                (1u8, seg_count) // shorter path → smaller rank → better
            } else {
                continue;
            };

            let cand = (priority, is_test, rank, view.id().to_string());
            if best.as_ref().map(|b| cand < *b).unwrap_or(true) {
                best = Some(cand);
            }
        }
        best.map(|(_, _, _, id)| id)
    }
}

/// Lance-backend edge adjacency — map the preload's
/// `(src, tgt, type, conf, provenance)` tuples (endpoint strings borrowed from
/// the resident id table) into [`EdgeView`]s. The v2 store carries real
/// per-edge provenance (from the CSR byte), so the code-atlas CallChain can
/// filter `ScipStructural` precisely.
fn lance_edge_views<'a>(
    raw: Vec<(&'a str, &'a str, EdgeType, f32, EdgeProvenance)>,
) -> Vec<EdgeView<'a>> {
    raw.into_iter()
        .map(
            |(source, target, edge_type, confidence, provenance)| EdgeView {
                source,
                target,
                edge_type,
                confidence,
                provenance,
            },
        )
        .collect()
}

/// Is the v2 store (`atoms.lance` + `edges.csr`) present in `atlas_dir`? Both
/// artifacts are required — a half-present store is treated as absent, so
/// `load_from_disk` returns the clean "no v2 store" `Err` instead of reading a
/// torn store.
fn v2_store_present(atlas_dir: &Path) -> bool {
    use crate::store::{ATOMS_LANCE_DIRNAME, EDGES_CSR_FILENAME};
    atlas_dir.join(ATOMS_LANCE_DIRNAME).exists() && atlas_dir.join(EDGES_CSR_FILENAME).exists()
}

/// Join-coverage diagnostic from [`build_persistent_ann_seed_table`] — the
/// ATLAS_STORAGE_V2 3b: open the persistent ANN seed table under `atlas_dir`,
/// if there is one the walk can use.
///
/// The ONE opener (ARCH §10.6), and it is store-class agnostic on purpose: the
/// seed table is a property of the ATLAS DIRECTORY, not of the backend holding
/// the atoms, so an atom-class store and a wiki-class one get their vector
/// seeding by the same path and cannot drift into two answers about whether a
/// corpus can seed at all. [`open_and_attach_ann_seed_table`] is the atom-class
/// caller; [`crate::provider::open_walk_provider`] is the class-agnostic one.
///
/// MUST run on the caller's long-lived async runtime — the held
/// `lancedb::Table` is queried later by [`atlas_navigate_ann`](super::atlas_navigate_ann), so opening it
/// on a throwaway runtime (e.g. the sync `load_from_disk` bridge) would
/// invalidate it; see [`AtlasGraph::with_ann_seed_table`]. `None` means "no
/// vector seeding here"; a present-but-unreadable table is `None` too, but
/// NAMED at warn (ARCH §18.3) rather than passed off as absence — the two are
/// different facts and the log says which one happened.
pub async fn open_ann_seed_table(corpus_id: &str, atlas_dir: &Path) -> Option<Arc<AnnSeedTable>> {
    if !crate::ann_store::ann_table_present(atlas_dir) {
        return None;
    }
    match AnnSeedTable::open_for_atlas(atlas_dir).await {
        Ok(ann) => {
            tracing::info!(
                corpus = corpus_id,
                "atlas-graph: ANN seed table attached (v2 seeding)"
            );
            Some(Arc::new(ann))
        }
        Err(e) => {
            tracing::warn!(
                corpus = corpus_id,
                error = %e,
                "atlas-graph: ANN seed table present but unreadable; using v1 cosine seeding"
            );
            None
        }
    }
}

/// [`open_ann_seed_table`] attached to an atom-class `graph`; the graph
/// unchanged when there is none. The single attach path shared by the daemon's
/// `AtlasContextManager` and the eval's `--atlas-seed ann` verify, so both load
/// the ANN exactly as the daemon does.
pub async fn open_and_attach_ann_seed_table(
    corpus_id: &str,
    atlas_dir: &Path,
    graph: AtlasGraph,
) -> AtlasGraph {
    match open_ann_seed_table(corpus_id, atlas_dir).await {
        Some(ann) => graph.with_ann_seed_table(ann),
        None => graph,
    }
}
