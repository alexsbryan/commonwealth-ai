//! Process-global handles for the watched-folder reconciliation
//! subsystem.
//!
//! The HTTP routes (`/internal/corpus/watch/*`) and the CLI commands
//! (`sovereign corpus watch …`) both need to reach the same
//! `LocalCorpusManager` and `WatchedFolderRegistry` the daemon spawned
//! at startup. Rather than thread these through every router function
//! signature, we park them on a process-global `OnceLock` here.
//!
//! This is the same pattern `auto_ingest::ACTIVE_INGESTS` uses for
//! the in-flight ingest task list — a small, intentional
//! "registry-as-singleton" surface that the HTTP layer reaches into.
//!
//! Lifetime: `install` is called exactly once during daemon startup
//! (`daemon_cmd.rs::run_daemon`). Subsequent calls are silently
//! ignored because the underlying `OnceLock::set` returns `Err`. The
//! handles live for the daemon's lifetime; there is no `uninstall`.

use std::sync::{Arc, OnceLock};

use sovereign_tools::local_corpus::watched::registry::WatchedFolderRegistry;
use sovereign_tools::local_corpus::watched::scheduler::ScheduleCancel;
use sovereign_tools::local_corpus::LocalCorpusManager;

static MANAGER: OnceLock<Arc<LocalCorpusManager>> = OnceLock::new();
static REGISTRY: OnceLock<Arc<WatchedFolderRegistry>> = OnceLock::new();
static CANCEL: OnceLock<ScheduleCancel> = OnceLock::new();

/// Install the manager + registry. Called once from
/// `daemon_cmd.rs::run_daemon`. Subsequent calls are no-ops.
pub fn install(manager: Arc<LocalCorpusManager>, registry: Arc<WatchedFolderRegistry>) {
    let _ = MANAGER.set(manager);
    let _ = REGISTRY.set(registry);
}

/// Park the scheduler's cancellation sender so it outlives the
/// daemon-startup scope. Currently the JoinHandle abort on Drop
/// handles graceful shutdown adequately; this exists so a future
/// `manager.shutdown` HTTP route can signal exit cleanly without
/// racing with the daemon's own cleanup.
pub fn set_cancel(c: ScheduleCancel) {
    let _ = CANCEL.set(c);
}

/// Borrow the manager. Returns `None` before `install` runs (which
/// happens during daemon startup) — HTTP handlers that fire before
/// then return 503 to the caller. Should never happen in practice
/// because the listener binds after install.
pub fn manager() -> Option<Arc<LocalCorpusManager>> {
    MANAGER.get().cloned()
}

/// Borrow the registry. Same `None`-during-startup contract as
/// `manager`.
pub fn registry() -> Option<Arc<WatchedFolderRegistry>> {
    REGISTRY.get().cloned()
}

/// Trigger graceful shutdown of the scheduler loop. Idempotent; no-op
/// if `set_cancel` was never called.
pub fn cancel() {
    if let Some(c) = CANCEL.get() {
        c.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The OnceLock state is process-global, so cross-test mutation is
    // observable. These tests only check the read-side contract; the
    // write side is exercised by the daemon integration test
    // (watched_folder_e2e.rs).

    #[test]
    fn manager_is_none_before_install() {
        // We can't actually clear the OnceLock between tests, so this
        // assertion only holds in a fresh process. The CI test runner
        // creates one process per test binary, so this is fine.
        if MANAGER.get().is_none() {
            assert!(manager().is_none());
        }
    }
}
