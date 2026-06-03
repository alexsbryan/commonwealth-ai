//! # corpus-engine-atos
//!
//! ATOS state — the feature/milestone/plan/design-signal layer that
//! sovereign-atos and the agent tooling read and write while features
//! move through their lifecycle.
//!
//! Carved out of `corpus-engine` (2026-05-23, step 2 of the
//! decomposition plan). The three modules:
//!
//! - [`features`] — `FeatureStore` for the feature/milestone/atos-run
//!   tables. The biggest of the three (1551 LOC pre-carve-out, the
//!   highest §3.1 violation in corpus-engine).
//! - [`plan_items`] — `IMPLEMENTATION_PLAN.md` index + checkbox state.
//! - [`design_signals`] — `DESIGN.md` structural parser
//!   (`pulldown-cmark`-based extraction of sections, gaps, etc.).
//!
//! ## Public surface
//!
//! Re-exported at the crate root for convenience (matches what was
//! at `corpus_engine::*` pre-carve-out):
//!
//! - [`FeatureStore`], [`FeatureRow`], [`FeatureState`], [`MilestoneRow`],
//!   [`AtosRunRow`], [`AtosToolEvent`]
//! - [`Error`], [`Result`]
//!
//! ## Error type
//!
//! Narrow local `Error` (Database + Io). Mirrors the scip carve-out
//! pattern. `From<corpus_engine_atos::Error> for corpus_engine::Error`
//! lives in `corpus-engine/src/error.rs` for downstream `?`-bubbling.

pub mod design_signals;
pub mod error;
pub mod features;
pub mod plan_items;

pub use error::{Error, Result};
pub use features::{
    AtosRunRow, AtosToolEvent, FeatureRow, FeatureState, FeatureStore, MilestoneRow,
};
