// SPDX-License-Identifier: AGPL-3.0-or-later
//! What enriches a turn — as a VALUE the caller builds and passes down, not
//! seven fields a stage reaches back into the [`Runtime`] to read.
//!
//! ## The defect this closes
//!
//! `quality/TOPOLOGY.md` §3.5 measured **35 reach-throughs** of the form
//! `self.<enrichment_field>` scattered across `runtime/retrieval/*`,
//! `evidence_loop`, `streaming.rs` and `turn.rs`, where a stage closes over
//! `&self` and pulls what it wants out of the Runtime. Those are what make the
//! Runtime fat, and **grouping them into a sub-struct does nothing about it**:
//! `self.gliner` merely becomes `self.enrichment.gliner`, the same coupling
//! down a longer path. The bar is therefore a count, not a shape — when it is
//! zero the Runtime is core-only *because nothing else can reach it*, and a
//! turn becomes testable without building a Runtime at all.
//!
//! ## This is not a new invention (ARCH §19)
//!
//! It already existed once and was never applied consistently:
//! `PprLane { graph, engine, rerank_fn, gliner }` (`retrieval/query_expansion.rs`)
//! already bundled the providers one stage needs and handed them down. This is
//! that type, generalised to every stage, and `PprLane` is now built FROM it.
//!
//! ## Snapshot semantics — the correctness win, not just the tidiness one
//!
//! [`Runtime::lane`] resolves `meta_atlas` out of its `RwLock` **once**. The
//! desktop attaches that ~900MB index in the background after the app is
//! already interactive ([`Runtime::install_meta_atlas`]), so a turn that read
//! the lock per stage could answer its first half against no index and its
//! second half against one. A lane is a consistent cut of what this turn may
//! enrich with, for its whole length.
//!
//! ## Why here and not `sovereign-contracts`
//!
//! Note `d91de4b1` places the *wire* turn shapes (`Scope`, `Capabilities`, and
//! the client's enrichment SELECTION) in `sovereign-contracts`, and that
//! stands. This type is the other half: the RESOLVED handles the daemon binds
//! for the turn — `Arc<dyn EntityExtractor>`, `RerankFn`, `Arc<dyn
//! ConvTieredReader>`. Those traits are owned by `sovereign-core` and
//! `corpus-engine`, both of which sit ABOVE `sovereign-contracts`, so the DTO
//! crate cannot name them. A request selects a lane; the daemon resolves it.

use std::sync::Arc;

use crate::runtime::Runtime;

/// The cross-encoder rerank pass, as one value with one notion of "on".
///
/// Both halves must agree, and before this type existed the conjunction was
/// re-derived at the call site (`self.rerank_config.enabled &&
/// self.rerank_fn.is_some()`) — one threshold, two deciders, which is the
/// §10.6 smell. [`Rerank::active`] is now the only place that question is
/// answered.
#[derive(Clone)]
pub struct Rerank {
    /// The cross-encoder itself. `None` = none is wired, and every
    /// `search_with_rerank` degrades to plain fusion rather than failing.
    pub f: Option<corpus_engine::RerankFn>,
    /// Overfetch size, threshold, blend weight. Always present: `enabled =
    /// false` makes the pass a no-op regardless of `f`.
    pub config: corpus_engine::RerankConfig,
}

impl Rerank {
    /// The ONE definition of "this turn reranks". A wired model with
    /// `enabled = false` does not; `enabled` with no model cannot.
    pub fn active(&self) -> bool {
        self.config.enabled && self.f.is_some()
    }

    /// The cross-encoder, borrowed for the call sites that pass it straight
    /// through to `CorpusIndex::search_with_rerank`.
    pub fn f(&self) -> Option<&corpus_engine::RerankFn> {
        self.f.as_ref()
    }
}

impl Default for Rerank {
    fn default() -> Self {
        Self {
            f: None,
            config: corpus_engine::RerankConfig::default(),
        }
    }
}

/// Everything that enriches a turn, resolved once and passed to the stages
/// that use it.
///
/// Every member is optional and every absence is the same fact — *this
/// deployment has no such provider wired* — which is why a bare `Option` is
/// honest here and is NOT the §18.3 defect it would be on a serving surface:
/// there is no second reason a lane member could be missing, and no caller
/// derives a user-visible refusal from one. A stage with `None` runs the
/// pre-enrichment path, which is the behaviour every one of these fields
/// documented individually before they were gathered.
#[derive(Clone, Default)]
pub struct Lane {
    /// Pre-embedded atlas Entity contexts fused into chunk retrieval as
    /// virtual `ScoredChunk`s.
    pub atlas_context: Option<Arc<dyn crate::atlas_context::AtlasContextProvider>>,
    /// Structural link graph for a corpus that exposes one (today:
    /// Wikipedia) — one-hop neighbour expansion and `(contested)` markers.
    pub wikipedia_graph: Option<Arc<dyn corpus_engine::WikipediaGraphApi>>,
    /// Cross-corpus meta-atlas index. Snapshotted out of the Runtime's
    /// `RwLock` at lane-build time — see the module docs on why a per-stage
    /// read is a bug and not merely a cost.
    pub meta_atlas: Option<Arc<corpus_engine::meta_atlas::MetaAtlasIndex>>,
    /// Cross-corpus bridge edges (typed topic-to-topic alignment).
    pub bridge: Option<Arc<corpus_engine::meta_atlas::BridgeIndex>>,
    /// The cross-encoder pass.
    pub rerank: Rerank,
    /// Entity extractor for entity-aware history retrieval and hybrid
    /// cosine/jaccard scoring.
    pub gliner: Option<Arc<dyn crate::traits::EntityExtractor>>,
    /// Read-side handle for conversation tiered retrieval (`conv_skeletons` /
    /// `conv_raptor_nodes` / `conv_motifs`).
    pub conv_tiered: Option<Arc<dyn crate::conv_tiered::ConvTieredReader>>,
}

impl Lane {
    /// A lane that enriches nothing — every stage takes its baseline path.
    /// The honest value for a caller that has no providers, and what makes a
    /// stage testable without constructing a `Runtime`.
    pub fn none() -> Self {
        Self::default()
    }
}

/// What the PROCESS holds; [`Lane`] is the per-turn snapshot of it.
///
/// One field on the `Runtime` in place of the seven it carried, and — this is
/// the part that matters — a **required constructor argument**. Phase 4b's
/// measurement (note banked 2026-08-25) ran the Phase 2 pair-independence pass
/// over all 19 `Runtime` builders across the three live `Runtime::new` sites
/// and found no variant structure at all: the raggedness was OMISSION, not
/// topology, and the code says so in its own comments — `with_rerank` carries
/// *"Until 2026-08-03 the ONLY surface that installed one was the `svrn chat`
/// CLI, so the hub server and the desktop shipped baseline fusion ordering
/// WHILE THE LEDGER RECORDED THE CAPABILITY AS AVAILABLE."*
///
/// A `with_*` builder cannot prevent that and never could: forgetting to call
/// one is indistinguishable, from inside the Runtime, from a host that has no
/// such provider. So the eight enrichment builders are deleted and this value
/// is passed to [`Runtime::new`]. A host now names its providers or names
/// [`LaneSources::none`] — and the difference is in the diff, not in a
/// capability ledger that disagrees with the binary (§4, totality over
/// assembly).
#[derive(Clone, Default)]
pub struct LaneSources {
    pub atlas_context: Option<Arc<dyn crate::atlas_context::AtlasContextProvider>>,
    pub wikipedia_graph: Option<Arc<dyn corpus_engine::WikipediaGraphApi>>,
    /// A CELL, not a value — the one member that can arrive after
    /// construction. `canonical_atoms.json` is ~900MB and parsing it was the
    /// bulk of the desktop splash's `BuildingRuntime` phase, so the desktop
    /// constructs empty, goes interactive, and fills this from a background
    /// warm via [`Runtime::install_meta_atlas`]. `ArcSwapOption` rather than
    /// `RwLock<Option<_>>` because every turn reads it and only one writer
    /// ever fires.
    pub meta_atlas: Arc<arc_swap::ArcSwapOption<corpus_engine::meta_atlas::MetaAtlasIndex>>,
    pub bridge: Option<Arc<corpus_engine::meta_atlas::BridgeIndex>>,
    pub rerank: Rerank,
    pub gliner: Option<Arc<dyn crate::traits::EntityExtractor>>,
    pub conv_tiered: Option<Arc<dyn crate::conv_tiered::ConvTieredReader>>,
}

impl LaneSources {
    /// A process that wires no enrichment at all. Tests and one-shot tools
    /// say this explicitly rather than reaching a degraded runtime by
    /// omission.
    pub fn none() -> Self {
        Self::default()
    }

    /// The turn's consistent cut of what may enrich it.
    ///
    /// Taken ONCE per turn, which is a correctness property and not only a
    /// cost one: `meta_atlas` can be filled by the desktop's background warm
    /// at any moment, so a pipeline that re-read the cell per stage could
    /// score the first half of one pool against no index and the second half
    /// against one.
    pub fn snapshot(&self) -> Lane {
        Lane {
            atlas_context: self.atlas_context.clone(),
            wikipedia_graph: self.wikipedia_graph.clone(),
            meta_atlas: self.meta_atlas.load_full(),
            bridge: self.bridge.clone(),
            rerank: self.rerank.clone(),
            gliner: self.gliner.clone(),
            conv_tiered: self.conv_tiered.clone(),
        }
    }
}

impl Runtime {
    /// This turn's [`Lane`] — the only way a stage obtains its providers.
    ///
    /// The field is `lane_sources` and the method is `lane()` on purpose: what
    /// the process holds and what a turn gets are different values, and the
    /// second is a snapshot of the first.
    pub fn lane(&self) -> Lane {
        self.lane_sources.snapshot()
    }
}
