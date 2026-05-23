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
//! - [`ScipGraph`] / [`SymbolRow`] / [`BlastEntry`] / [`BlastRadiusResult`] /
//!   [`ScipGraphStats`] / [`OpenError`] / [`RebuildLock`] / [`SCHEMA_VERSION`] /
//!   [`ScipSymbolRecord`] / [`ScipRefRecord`] — the call graph store.
//! - [`scip_export::export_all`] / [`scip_export::check_exporters`] /
//!   [`scip_export::find_cargo_workspace_roots`] /
//!   [`scip_export::ExportSummary`] / [`scip_export::ScipProgress`] —
//!   per-language exporter dispatch.
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

pub mod error;
pub mod scip_graph;
pub mod scip_export;
mod scip_proto;

pub use error::{Error, Result};
pub use scip_graph::{
    BlastEntry, BlastRadiusResult, OpenError, RebuildLock, ScipGraph, ScipGraphStats,
    ScipRefRecord, ScipSymbolRecord, SymbolRow, SCHEMA_VERSION,
};
