// SPDX-License-Identifier: AGPL-3.0-or-later
//! The code program's one freshness path (phase-b pb-code-freshness): the
//! `Reindexer` over the merged graph the tools read, with the commit
//! harvester, resumed from the project registry. `svrn code mcp` runs it;
//! the svrn daemon runs the same construction through its sovereign-code
//! edge until pb-code-daemon-exit. It replaced the code server's 30 s
//! `scip_graph.db` mtime poll.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine_notes::NoteStore;
use corpus_engine_watchers::reindexer::{MergedPrimer, Reindexer};
use sovereign_contracts::watcher_projects::Registry;

use crate::LazyScipGraph;

/// Build the Reindexer over `merged`, writing into the handle the tools read
/// so every overlay and rebuild is live to `symbols`, and re-register every
/// project in the registry. A deferred graph is loaded before the first
/// write into it (`MergedPrimer`). Returns the registry it resumed from.
/// A missing or unreadable registry starts empty, by name in the log.
pub async fn start_reindexer(
    indexes_dir: PathBuf,
    merged: &LazyScipGraph,
    notes: Arc<NoteStore>,
) -> (Arc<Reindexer>, Registry) {
    let mut reindexer = Reindexer::new(indexes_dir, merged.handle());
    // Phase 7.1: the git-HEAD poll harvests non-noisy commits into
    // `source='committed'` notes. Before any share (Arc::get_mut).
    Reindexer::with_commit_harvester(&mut reindexer, notes);
    if !merged.is_loaded() {
        let lazy = merged.clone();
        let primer: MergedPrimer = Arc::new(move || {
            let lazy = lazy.clone();
            Box::pin(async move { lazy.ensure_loaded().await })
        });
        Reindexer::with_merged_primer(&mut reindexer, primer);
    }

    let registry = Registry::load().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "could not load project registry; starting empty");
        Registry::default()
    });
    for entry in registry.entries() {
        reindexer.register(entry.clone()).await;
        tracing::info!(corpus = %entry.corpus_id, "resumed registered project");
    }
    (reindexer, registry)
}
