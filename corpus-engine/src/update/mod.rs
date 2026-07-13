// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod delta;
pub mod newsworthy_event_stream;
pub mod newsworthy_watcher;

#[cfg(feature = "treesitter")]
pub mod watch;

// watcher_coordinator + lint/test/project-index watchers moved to
// `corpus-engine-watchers` (R4 Step 1, DECOMPOSITION.md). Only the
// SCIP `watch::CodeWatcher` (genuinely tree-sitter-coupled) stays.
