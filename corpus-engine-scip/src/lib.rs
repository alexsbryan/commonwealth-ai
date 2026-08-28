// SPDX-License-Identifier: AGPL-3.0-or-later
//! # corpus-engine-scip
//!
//! SCIP call graph (`scip_graph`) + language-agnostic exporter
//! dispatch (`scip_export`) + protobuf decoder (`scip_proto`).
//!
//! Carved out of `corpus-engine` (2026-05-23) so consumers of scip
//! don't drag the rest of corpus-engine's 41 modules into their
//! rebuild closure. See the crate-level Cargo.toml header for the
//! blast-radius rationale.
//!
//! ## Public surface
//!
//! - [`converge`] / [`roles`] / [`shape`] — three duplication feeds over the
//!   same graph, along three axes that do not overlap: duplicated NAME,
//!   duplicated ROLE, duplicated SHAPE.
//! - [`ScipGraph`] / [`SymbolRow`] / [`BlastEntry`] / [`BlastRadiusResult`] /
//!   [`ScipGraphStats`] / [`OpenError`] / [`RebuildLock`] / [`SCHEMA_VERSION`] /
//!   [`ScipSymbolRecord`] / [`ScipRefRecord`] — the call graph store.
//! - [`scip_export::export_all`] / [`scip_export::check_exporters`] /
//!   [`scip_export::find_cargo_workspace_roots`] /
//!   [`scip_export::ExportSummary`] / [`scip_export::ScipProgress`] —
//!   per-language exporter dispatch.
//! - [`tool_path::resolve`] / [`tool_path::augmented_path_env`] — the ONE
//!   decider for "where is this tool?", so a daemon with a minimal
//!   service PATH and an operator's shell reach the same answer.
//! - [`Error`] / [`Result`] — narrow local error type (`Io` + `Database`).
//!
//! ## Error type
//!
//! `Error` here is intentionally **narrower** than `corpus_engine::Error`.
//! scip only constructs `Io` and `Database` variants. Callers in the
//! sovereign workspace either `map_err` to their own error (the common
//! pattern in `sovereign-tools`) or convert via the
//! `From<corpus_engine_scip::Error>` impl in `corpus-engine` (for
//! corpus-engine's own internal users — `update::watch::CodeWatcher`,
//! `enrichment::atlas::strategies::code_walk`).

pub mod arch_metrics;
pub mod capability_map;
/// Duplicated concept IDENTITY over the graph — the half `dry_report`
/// (duplicated behaviour) structurally cannot see. See the module docs.
pub mod converge;
/// Derive a symbol's kind + dispatch from its SCIP descriptor — the fix for
/// the unusable `symbols.kind` and never-written `refs.ref_kind` columns.
pub mod descriptor;
pub mod error;
/// Duplicated concept ROLE over the graph — the third feed, seeing what
/// neither a name census nor a behaviour report can. See the module docs.
pub mod roles;
pub mod scip_export;
pub mod scip_graph;
mod scip_proto;
/// Duplicated concept SHAPE over the graph — the renamed fork neither a name
/// census nor a role census can see. See the module docs.
pub mod shape;
// Service-PATH-independent tool resolution. This module existed as an
// UNREFERENCED file from 2026-08-03 until it was declared here on
// 2026-08-07 — it compiled into nothing, its tests never ran, and the
// PATH bug it was written to fix stayed live the whole time while a
// note recorded the fix as shipped. Declared, wired into
// `scip_export`, and load-bearing now.
pub mod tool_path;
pub mod trace;

pub use arch_metrics::{
    compute as compute_arch_metrics, type_spreads, ArchMetrics, ArchOptions, DeclaredDeps,
    TypeSpread,
};
pub use capability_map::{
    build as build_capability_map, Capability, CapabilityMap, EntryPointProvider, MapOptions,
    ProviderKind,
};
pub use converge::{
    census, crate_dag, cross_crate_reached, dossier, duplicate_count, type_defs, Census, CensusRow,
    Dossier, OwnerCandidate, SourceScope, TypeDef,
};
pub use descriptor::{
    descriptor_kind, descriptor_of, dispatch_hint, field_owner_and_name, DescriptorKind,
    DispatchHint,
};
pub use error::{Error, Result};
pub use roles::{
    head_noun, reach_index, render_roles, roles, type_fields, Family, RoleBest, RoleCensus,
    RoleRow, ADOPTION_REACH, FAMILIES,
};
pub use scip_graph::{
    BlastEntry, BlastRadiusResult, LiveExport, OpenError, RebuildLock, ScipGraph, ScipGraphStats,
    ScipRefRecord, ScipSymbolRecord, SymbolRow, REBUILD_COALESCED, SCHEMA_VERSION,
};
pub use shape::{
    field_signatures, render_shape, shape_census, FieldKey, ShapeCensus, ShapeGroup, ShapeMatch,
    ShapeOptions, ShapeSide,
};
pub use trace::{build_symbol_trace, render_trace, CallSite, SymbolTrace};
