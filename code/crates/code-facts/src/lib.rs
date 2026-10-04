// SPDX-License-Identifier: AGPL-3.0-or-later
//! # code-facts
//!
//! The code-intel package's deterministic tree-sitter fact base
//! (`docs/CODE_TOOLING_BOUNDARY.md` §2, Phase 2). Three modules moved here from
//! `corpus-engine` at domains `REVIEW-build-code-facts`:
//!
//! - [`facts`] — the extraction loops and the [`facts::Facts`] schema. Always
//!   compiled; the tree-sitter walk lives behind `treesitter`, with an empty
//!   fact base as the fallback.
//! - [`facts_check`] — the deterministic dispatch over the fact base and the
//!   SCIP call graph (`treesitter`).
//! - [`facts_store`] — the SQLite per-file home for the fact base (`stores`).
//!
//! ## The reaches, resolved
//!
//! - `crate::types::EmbedFn` (facts_check) and `crate::error::{Error, Result}`
//!   (facts_store) are `corpus-index` items, re-exported by `corpus-engine` at
//!   their historical paths. `corpus-index` is a shared `[[package_leaf]]`, so
//!   the crate names it directly.
//! - `corpus_engine_scip::ScipGraph` / `ScipSymbolRecord` is a code-intel
//!   package member, reached directly as the package's own SCIP store.
//!
//! `corpus-engine` re-exports every module at its historical path
//! (`facts` / `facts_check` / `facts_store`) so no in-monorepo importer had to
//! change; the four consumers that named the engine path repoint here.

pub mod facts;
#[cfg(feature = "treesitter")]
pub mod facts_check;
#[cfg(feature = "stores")]
pub mod facts_store;
