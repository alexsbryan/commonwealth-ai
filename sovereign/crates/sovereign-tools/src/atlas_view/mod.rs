// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atlas inspection surface for sovereign-desktop and the CLI.
//!
//! The atlas pipeline (corpus-engine) writes typed atom + edge files
//! under `<index_dir>/<corpus>/atlas/`. Inference paths
//! (`AtlasContextManager`) read them at runtime. This module is the
//! third reader: a *human-facing* view that lists corpora, browses
//! atoms, and inspects single atoms with their evidence.
//!
//! ## Phase 1 (this module today)
//!
//! - [`FileAtlasReader`] — reads `atoms.json` / `edges.json` directly
//!   from the indexes dir. No daemon required, no cache: each call
//!   re-reads the JSON. Fine for inspection rates.
//! - [`list_corpora`](FileAtlasReader::list_corpora) — enumerates
//!   corpora that have an atlas, returns per-type atom counts.
//!
//! ## Phase 2 (deferred — see todo note "Atlas inspector Phase 2")
//!
//! Curation overlay. An `atlas/overlay.sqlite` keyed by
//! [`StableAtomKey`](corpus_engine::enrichment::atlas::StableAtomKey) —
//! corpus-engine's, since it is the atom's identity and not this view's —
//! will store user edits and approval state.
//! `FileAtlasReader` will grow an overlay-merging branch (no new
//! trait — one struct, one concern). Forward-compat fields
//! ([`CurationStatus`], `overlay_supports`) are plumbed through Phase
//! 1 DTOs so the UI gate flips on without a schema migration.

/// Max characters of PROSE shown in a row label before an ellipsis. The
/// atlas view's presentation policy, decided once here rather than in each
/// submodule — `atom_browse` and `atom_detail` each carried their own copy of
/// this constant (both 120) until 2026-08-20. Which atom kinds are prose is
/// `AtomEnvelope::display_name`'s business, not this module's.
pub(crate) const DISPLAY_NAME_TRUNCATION: usize = 120;

pub mod atom_browse;
pub mod atom_detail;
pub mod conv;
pub mod reader;
pub mod subgraph;

pub use atom_browse::{AtomFilter, AtomListPage, AtomQueryError, AtomSummary, PageCursor};
pub use atom_detail::{AtomDetail, CrossCorpusLink, EvidenceExcerpt, ReferencedAtom, RelatedAtom};
pub use conv::{
    ConvCorpusSummary, ConvDetailView, ConvEntityChip, ConvListPage, ConvRaptorNodeView,
    ConvSummary, SummaryCorrectionView,
};
pub use reader::{
    AtlasBuildReport, AtlasCorpusSummary, AtlasMemberSummary, AtlasViewError, CurationStatus,
    DeclaredTypeRow, FileAtlasReader,
};
pub use subgraph::{AtlasEdge, AtlasNode, AtlasSubgraph, SubgraphCensus, DEFAULT_MAX_NODES};
