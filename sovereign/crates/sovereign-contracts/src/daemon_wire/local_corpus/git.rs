// SPDX-License-Identifier: AGPL-3.0-or-later
//! The git status a vault's write-back surface reports.
//!
//! Moved down from `sovereign_tools::local_corpus::git` at svt-6
//! (2026-09-12) and re-exported there at the historical path. Pure serde over
//! primitives: a client that only wants to SPELL one of these had to link
//! `sovereign-tools` — and through it corpus-engine, sovereign-store,
//! sovereign-atos and five more. See this module's parent for the full note.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStatus {
    pub current_branch: String,
    pub has_uncommitted_changes: bool,
}
