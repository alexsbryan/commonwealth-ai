// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `code-intel` tool bundle — `svrn code`'s own composition surface.
//!
//! It lived in `sovereign-tools/src/bundles.rs` until 2026-09-21. A bundle
//! that registers only this crate's tools, and whose privilege argument is
//! about this crate's handle, belongs to the program it composes: leaving it
//! behind was the last reason a svrn-side crate had to name `sovereign-code`
//! (FIVE_PROGRAMS §4 rule 6).

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
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
    inference: Option<Arc<dyn InferenceProvider>>,
    scip_graph: crate::LazyScipGraph,
    project_root: Option<PathBuf>,
    peer_work: Option<Arc<dyn crate::PeerWork>>,
}

#[cfg(feature = "treesitter")]
impl CodeIntelTools {
    /// Build the family over a graph handle the host owns. With no model,
    /// `code_search` answers from full text alone.
    pub fn new(
        corpus_engine: Arc<dyn corpus_index::source::IndexSource>,
        scip_graph: impl Into<crate::LazyScipGraph>,
    ) -> Self {
        Self {
            corpus_engine,
            inference: None,
            scip_graph: scip_graph.into(),
            project_root: None,
            peer_work: None,
        }
    }

    /// Rank `code_search` with a model.
    pub fn with_inference(mut self, inference: Arc<dyn InferenceProvider>) -> Self {
        self.inference = Some(inference);
        self
    }

    /// The project `blast` resolves paths against.
    pub fn with_project_root(mut self, root: PathBuf) -> Self {
        self.project_root = Some(root);
        self
    }

    /// Peers' claims, which `blast` reports as concurrent work.
    pub fn with_peer_work(mut self, peer_work: Arc<dyn crate::PeerWork>) -> Self {
        self.peer_work = Some(peer_work);
        self
    }
}

#[cfg(feature = "treesitter")]
#[async_trait]
impl ToolBundle for CodeIntelTools {
    fn name(&self) -> &'static str {
        "code-intel"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        let health = Arc::new(crate::IndexHealthChecker::new(self.scip_graph.clone()));
        let mut search = crate::CodeSearchTool::new(Arc::clone(&self.corpus_engine));
        match &self.inference {
            Some(inference) => search = search.with_inference(Arc::clone(inference)),
            None => tracing::debug!(bundle = "code-intel", "no model: code_search is full text"),
        }
        let mut blast = crate::BlastRadiusTool::new(self.scip_graph.clone())
            .with_health_checker(Arc::clone(&health));
        if let Some(root) = &self.project_root {
            blast = blast.with_project_root(root.clone());
        }
        if let Some(peer_work) = &self.peer_work {
            blast = blast.with_atlas(Arc::clone(peer_work));
        }
        BundleReport::new(self.name())
            .record(
                reg.register_reporting(Box::new(
                    crate::SymbolLookupTool::new(
                        Arc::clone(&self.corpus_engine),
                        self.scip_graph.clone(),
                    )
                    .with_health_checker(Arc::clone(&health))
                    .declared(),
                )),
            )
            .record(reg.register_reporting(Box::new(search.declared())))
            .record(reg.register_reporting(Box::new(
                crate::RecentChangesTool::new(Arc::clone(&self.corpus_engine)).declared(),
            )))
            .record(
                reg.register_reporting(Box::new(
                    crate::FindCalleesTool::new(
                        Arc::clone(&self.corpus_engine),
                        self.scip_graph.clone(),
                    )
                    .with_health_checker(Arc::clone(&health))
                    .declared(),
                )),
            )
            .record(
                reg.register_reporting(Box::new(
                    crate::FindCallersTool::new(
                        Arc::clone(&self.corpus_engine),
                        self.scip_graph.clone(),
                    )
                    .with_health_checker(Arc::clone(&health))
                    .declared(),
                )),
            )
            .record(reg.register_reporting(Box::new(crate::CapabilityMapTool::new().declared())))
            .record(reg.register_reporting(Box::new(blast.declared())))
    }
}

/// Working notes — persist across sessions, used for session attribution.
///
/// Takes an ALREADY-OPEN store: one writer per data root (TOPOLOGY phase 1),
/// so a bundle never opens a database.
#[cfg(feature = "treesitter")]
pub struct NotesTools {
    notes: Arc<corpus_engine_notes::NoteStore>,
    inference: Option<Arc<dyn InferenceProvider>>,
    workspace_root: Option<PathBuf>,
}

#[cfg(feature = "treesitter")]
impl NotesTools {
    /// Build the family over a note store the host opened. With no model,
    /// `read_note_digest` answers in its header-only fallback, which names
    /// the degradation.
    pub fn new(notes: Arc<corpus_engine_notes::NoteStore>) -> Self {
        Self {
            notes,
            inference: None,
            workspace_root: None,
        }
    }

    /// Summarise `read_note_digest` with a model.
    pub fn with_inference(mut self, inference: Arc<dyn InferenceProvider>) -> Self {
        self.inference = Some(inference);
        self
    }

    /// The repo `session_state` stamps a frame's head and branch from;
    /// without it the frame is written unstamped.
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }
}

#[cfg(feature = "treesitter")]
#[async_trait]
impl ToolBundle for NotesTools {
    fn name(&self) -> &'static str {
        "notes"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        let mut digest = crate::ReadNoteDigestTool::new(Arc::clone(&self.notes));
        match &self.inference {
            Some(inference) => digest = digest.with_inference(Arc::clone(inference)),
            None => tracing::debug!(
                bundle = "notes",
                "no model: read_note_digest is header-only"
            ),
        }
        let mut session_state = crate::SessionStateTool::new();
        match &self.workspace_root {
            Some(root) => session_state = session_state.with_workspace_root(root.clone()),
            None => tracing::debug!(
                bundle = "notes",
                "no workspace: session frames go unstamped"
            ),
        }
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
            .record(reg.register_reporting(Box::new(
                crate::ReadNoteByIdTool::new(Arc::clone(&self.notes)).declared(),
            )))
            .record(reg.register_reporting(Box::new(
                crate::PromoteNoteTool::new(Arc::clone(&self.notes)).declared(),
            )))
            .record(reg.register_reporting(Box::new(digest.declared())))
            .record(reg.register_reporting(Box::new(
                crate::WriteRedteamFindingTool::new(Arc::clone(&self.notes)).declared(),
            )))
            .record(reg.register_reporting(Box::new(
                crate::SessionReflectionTool::new(Arc::clone(&self.notes)).declared(),
            )))
            .record(reg.register_reporting(Box::new(
                crate::RetireNoteTool::new(Arc::clone(&self.notes)).declared(),
            )))
            .record(reg.register_reporting(Box::new(session_state.declared())))
    }
}

/// The test and lint watchers' results. Takes ALREADY-OPEN stores, like
/// [`NotesTools`]; `run_tests` registers only with a test watcher to run.
#[cfg(feature = "treesitter")]
pub struct WatcherTools {
    test_store: Arc<corpus_engine_watchers::TestResultStore>,
    lint_store: Arc<corpus_engine_watchers::LintResultStore>,
    watcher_active: Arc<AtomicBool>,
    workspace_root: PathBuf,
    test_watcher: Option<Arc<corpus_engine_watchers::TestWatcher>>,
    test_scope: Option<String>,
    lint_scope: Option<String>,
}

#[cfg(feature = "treesitter")]
impl WatcherTools {
    /// `watcher_active` is the flag the host sets once its FS watcher runs.
    pub fn new(
        test_store: Arc<corpus_engine_watchers::TestResultStore>,
        lint_store: Arc<corpus_engine_watchers::LintResultStore>,
        watcher_active: Arc<AtomicBool>,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            test_store,
            lint_store,
            watcher_active,
            workspace_root,
            test_watcher: None,
            test_scope: None,
            lint_scope: None,
        }
    }

    /// The test watcher `run_tests` drives.
    pub fn with_test_watcher(mut self, watcher: Arc<corpus_engine_watchers::TestWatcher>) -> Self {
        self.test_watcher = Some(watcher);
        self
    }

    /// The commands each watcher runs, shown so an agent can confirm the
    /// watcher covers what it edited.
    pub fn with_scopes(mut self, test: Option<String>, lint: Option<String>) -> Self {
        self.test_scope = test;
        self.lint_scope = lint;
        self
    }
}

#[cfg(feature = "treesitter")]
#[async_trait]
impl ToolBundle for WatcherTools {
    fn name(&self) -> &'static str {
        "watchers"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        let mut test_status = crate::TestStatusTool::new(Arc::clone(&self.test_store))
            .with_watcher_active(Arc::clone(&self.watcher_active));
        if let Some(scope) = &self.test_scope {
            test_status = test_status.with_watched_scope(scope.clone());
        }
        let mut lint_status = crate::LintStatusTool::new(Arc::clone(&self.lint_store))
            .with_watcher_active(Arc::clone(&self.watcher_active))
            .with_workspace_root(self.workspace_root.clone());
        if let Some(scope) = &self.lint_scope {
            lint_status = lint_status.with_watched_scope(scope.clone());
        }
        let mut build = crate::BuildTool::new(Arc::clone(&self.lint_store))
            .with_watcher_active(Arc::clone(&self.watcher_active));
        if let Some(scope) = &self.lint_scope {
            build = build.with_watched_scope(scope.clone());
        }
        let mut report = BundleReport::new(self.name())
            .record(reg.register_reporting(Box::new(test_status.declared())))
            .record(reg.register_reporting(Box::new(
                crate::GetRunOutputTool::new(Arc::clone(&self.test_store)).declared(),
            )))
            .record(reg.register_reporting(Box::new(lint_status.declared())))
            .record(reg.register_reporting(Box::new(
                crate::GetLintOutputTool::new(Arc::clone(&self.lint_store)).declared(),
            )))
            .record(reg.register_reporting(Box::new(build.declared())));
        report = match &self.test_watcher {
            Some(watcher) => report.record(reg.register_reporting(Box::new(
                crate::RunTestsTool::new(Arc::clone(watcher)).declared(),
            ))),
            None => report.withheld("run_tests", "no [test_runner] configured"),
        };
        report
    }
}

/// The architecture and drift reports a project's quality gates write.
#[cfg(feature = "treesitter")]
pub struct ArchTools {
    workspace_root: PathBuf,
}

#[cfg(feature = "treesitter")]
impl ArchTools {
    /// Over the reports under `workspace_root`.
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[cfg(feature = "treesitter")]
#[async_trait]
impl ToolBundle for ArchTools {
    fn name(&self) -> &'static str {
        "arch"
    }

    async fn register_into(&self, reg: &mut ToolRegistry) -> BundleReport {
        BundleReport::new(self.name())
            .record(reg.register_reporting(Box::new(crate::ArchReportTool::new().declared())))
            .record(reg.register_reporting(Box::new(crate::ArchPostureTool::new().declared())))
            .record(
                reg.register_reporting(Box::new(
                    crate::DriftPostureTool::new()
                        .with_workspace_root(self.workspace_root.clone())
                        .declared(),
                )),
            )
            .record(reg.register_reporting(Box::new(crate::DriftFindingsTool::new().declared())))
    }
}
