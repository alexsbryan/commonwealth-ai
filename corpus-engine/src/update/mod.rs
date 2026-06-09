// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod delta;
pub mod newsworthy_event_stream;
pub mod newsworthy_watcher;

#[cfg(feature = "treesitter")]
pub mod watch;

// `watcher_coordinator` only needs `notify` (lives in `stores`).
// The watcher *implementations* (test/lint/project_index) still
// require treesitter, but the BackgroundWatcher trait + coordinator
// types are available to anyone with `stores` so external observers
// (work-atlas) can implement the trait without dragging tree-sitter.
#[cfg(feature = "stores")]
pub mod watcher_coordinator;

#[cfg(feature = "treesitter")]
pub mod test_watcher;

#[cfg(feature = "treesitter")]
pub mod lint_watcher;

#[cfg(feature = "treesitter")]
pub mod project_index_watcher;
