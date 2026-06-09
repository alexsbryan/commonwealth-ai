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
//! - [`StableAtomKey`] — content-derived hash that survives
//!   re-extraction's `AtomId` renumbering (see decision note "Atlas
//!   inspector: stable_key by content hash").
//! - [`FileAtlasReader`] — reads `atoms.json` / `edges.json` directly
//!   from the indexes dir. No daemon required, no cache: each call
//!   re-reads the JSON. Fine for inspection rates.
//! - [`list_corpora`](FileAtlasReader::list_corpora) — enumerates
//!   corpora that have an atlas, returns per-type atom counts.
//!
//! ## Phase 2 (deferred — see todo note "Atlas inspector Phase 2")
//!
//! Curation overlay. An `atlas/overlay.sqlite` keyed by
//! [`StableAtomKey`] will store user edits and approval state.
//! `FileAtlasReader` will grow an overlay-merging branch (no new
//! trait — one struct, one concern). Forward-compat fields
//! ([`CurationStatus`], `overlay_supports`) are plumbed through Phase
//! 1 DTOs so the UI gate flips on without a schema migration.

pub mod atom_browse;
pub mod atom_detail;
pub mod conv;
pub mod reader;
pub mod stable_key;
pub mod subgraph;

pub use atom_browse::{AtomBrowseError, AtomFilter, AtomListPage, AtomSummary, PageCursor};
pub use atom_detail::{
    AtomDetail, AtomDetailError, CrossCorpusLink, EvidenceExcerpt, ReferencedAtom, RelatedAtom,
};
pub use conv::{
    ConvCorpusSummary, ConvDetailView, ConvEntityChip, ConvListPage, ConvRaptorNodeView,
    ConvSummary,
};
pub use reader::{AtlasCorpusSummary, AtlasViewError, CurationStatus, FileAtlasReader};
pub use stable_key::{compute_stable_key, StableAtomKey};
pub use subgraph::{AtlasEdge, AtlasNode, AtlasSubgraph, SubgraphCensus, DEFAULT_MAX_NODES};
