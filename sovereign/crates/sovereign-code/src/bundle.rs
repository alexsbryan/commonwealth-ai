// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `code-intel` tool bundle — `svrn code`'s own composition surface.
//!
//! It lived in `sovereign-tools/src/bundles.rs` until 2026-09-21. A bundle
//! that registers only this crate's tools, and whose privilege argument is
//! about this crate's handle, belongs to the program it composes: leaving it
//! behind was the last reason a svrn-side crate had to name `sovereign-code`
//! (FIVE_PROGRAMS §4 rule 6).

use std::sync::Arc;

use async_trait::async_trait;
use sovereign_contracts::registry::ToolRegistry;
use sovereign_contracts::tool_bundle::{BundleReport, ToolBundle};
use sovereign_contracts::traits::InferenceProvider;

// `NotesTools` came across with it: corpus-engine-notes is a code-program
// crate and all three tools it registers are this crate's.

/// Code intelligence over a SCIP graph and a code corpus.
///
/// Behind `treesitter`, like the tools it registers: a build without it has
/// no code-intel surface to compose.
///
/// **The privilege is the handle.** This bundle cannot be constructed without
/// a [`ScipGraphHandle`](crate::ScipGraphHandle) and a corpus engine, so a
/// host may only offer code intel over an index it actually owns. That is why
/// "should the shared registry carry code intel on a multi-tenant hub?" is not
/// a policy question: a tenant-scoped host has no other tenant's handle to
/// compose from.
#[cfg(feature = "treesitter")]
pub struct CodeIntelTools {
    corpus_engine: Arc<dyn corpus_index::source::IndexSource>,
    inference: Arc<dyn InferenceProvider>,
    scip_graph: crate::ScipGraphHandle,
}

#[cfg(feature = "treesitter")]
impl CodeIntelTools {
    /// Build the family over a graph handle the host owns.
    pub fn new(
        corpus_engine: Arc<dyn corpus_index::source::IndexSource>,
        inference: Arc<dyn InferenceProvider>,
        scip_graph: crate::ScipGraphHandle,
    ) -> Self {
        Self {
            corpus_engine,
            inference,
            scip_graph,
        }
    }
}

#[cfg(feature = "treesitter")]
#[async_trait]
impl ToolBundle for CodeIntelTools {
    fn name(&self) -> &'static str {
        "code-intel"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        let health = Arc::new(crate::IndexHealthChecker::new(Arc::clone(&self.scip_graph)));
        BundleReport::new(self.name())
            .record(
                reg.register_reporting(Box::new(
                    crate::SymbolLookupTool::new(
                        Arc::clone(&self.corpus_engine),
                        Arc::clone(&self.scip_graph),
                    )
                    .with_health_checker(Arc::clone(&health))
                    .declared(),
                )),
            )
            .record(
                reg.register_reporting(Box::new(
                    crate::CodeSearchTool::new(Arc::clone(&self.corpus_engine))
                        .with_inference(Arc::clone(&self.inference))
                        .declared(),
                )),
            )
            .record(reg.register_reporting(Box::new(
                crate::RecentChangesTool::new(Arc::clone(&self.corpus_engine)).declared(),
            )))
            .record(
                reg.register_reporting(Box::new(
                    crate::FindCalleesTool::new(
                        Arc::clone(&self.corpus_engine),
                        Arc::clone(&self.scip_graph),
                    )
                    .with_health_checker(Arc::clone(&health))
                    .declared(),
                )),
            )
            .record(
                reg.register_reporting(Box::new(
                    crate::FindCallersTool::new(
                        Arc::clone(&self.corpus_engine),
                        Arc::clone(&self.scip_graph),
                    )
                    .with_health_checker(Arc::clone(&health))
                    .declared(),
                )),
            )
            .record(reg.register_reporting(Box::new(crate::CapabilityMapTool::new().declared())))
    }
}

/// Working notes — persist across sessions, used for session attribution.
///
/// Takes an ALREADY-OPEN store: one writer per data root (TOPOLOGY phase 1),
/// so a bundle never opens a database.
#[cfg(feature = "treesitter")]
pub struct NotesTools {
    notes: Arc<corpus_engine_notes::NoteStore>,
}

#[cfg(feature = "treesitter")]
impl NotesTools {
    /// Build the family over a note store the host opened.
    pub fn new(notes: Arc<corpus_engine_notes::NoteStore>) -> Self {
        Self { notes }
    }
}

#[cfg(feature = "treesitter")]
#[async_trait]
impl ToolBundle for NotesTools {
    fn name(&self) -> &'static str {
        "notes"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        BundleReport::new(self.name())
            .record(reg.register_reporting(Box::new(
                crate::WriteNoteTool::new(Arc::clone(&self.notes)).declared(),
            )))
            .record(reg.register_reporting(Box::new(
                crate::ReadNotesTool::new(Arc::clone(&self.notes)).declared(),
            )))
            .record(reg.register_reporting(Box::new(
                crate::DeleteNoteTool::new(Arc::clone(&self.notes)).declared(),
            )))
    }
}
