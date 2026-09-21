// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ATOS MCP tool vocabulary.
//!
//! Moved here from `sovereign-code` on 2026-09-21. The crate doc above names
//! "an MCP tool vocabulary" as one of this library's intended transports, and
//! these six modules are it: they read the ATOS stores
//! (`corpus_engine_atos::FeatureStore`, `corpus_engine_notes::ProjectDocsStore`)
//! and this crate's own `approval` predicate, and they named NOTHING in
//! `sovereign-code` — no `use crate::`, no `use super::` outside their test
//! modules — which is why the move is a file move and not a refactor.
//!
//! WHY THEY LEFT `sovereign-code`. `svrn code` is one of the five programs
//! (docs/FIVE_PROGRAMS.md §2) and its package must stay liftable against the
//! shared leaves alone; these six were its only reason to name `sovereign-atos`
//! and `corpus-engine-atos`, both of which §5 lists under "deleted, not
//! migrated". Hosting the tools in the crate they wrap means that when ATOS
//! goes, its tool surface goes with it, instead of leaving six dangling
//! modules behind a dead feature flag in another program.
//!
//! They carry no feature gate here. In `sovereign-code` they were gated on
//! `all(feature = "treesitter", feature = "atos")`; neither applies — nothing
//! in these modules touches tree-sitter, and inside this crate the ATOS stores
//! are unconditional dependencies.

pub mod archive_feature;
pub mod design_signals_extract;
pub mod drift;
pub mod project_context;
pub mod provision_feature;
pub mod record_atos_event;

pub use archive_feature::ArchiveFeatureTool;
pub use design_signals_extract::DesignSignalsExtractTool;
pub use drift::DriftTool;
pub use project_context::ProjectContextTool;
pub use provision_feature::ProvisionFeatureTool;
pub use record_atos_event::RecordAtosEventTool;
