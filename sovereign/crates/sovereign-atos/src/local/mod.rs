// SPDX-License-Identifier: AGPL-3.0-or-later
//! Default [`crate::AtosOrchestrator`] implementation.
//!
//! Split per ARCH_PRINCIPLES.md §3 (soft ceiling on per-file concern
//! count) into two children:
//!
//! - [`orchestrator`] — `LocalAtosOrchestrator`: struct, trait impl,
//!   inherent CLI-facing methods, end-to-end tests against real
//!   SQLite tempdirs.
//! - [`helpers`] — pure text + notes helpers used by the orchestrator:
//!   charter heading parsing, stop-condition marker extraction,
//!   preamble assembly. Deterministic, covered by unit tests.
//!
//! External callers consume this module through the re-exports below;
//! the submodule boundary is an internal cleanup, not a public API
//! change.
//!
//! Reference: [`super::AtosOrchestrator`] for the trait contract.

mod helpers;
mod orchestrator;

pub use helpers::{extract_milestone_stop_condition, feature_dir};
pub use orchestrator::LocalAtosOrchestrator;
