// SPDX-License-Identifier: AGPL-3.0-or-later
//! The living-trigger seam: a watched folder running a workflow on its own.
//!
//! The `Worker` knows nothing about workflows — it lives in `sovereign-tools`,
//! while the workflow engine and its assembly live in `sovereign-workflow` /
//! `sovereign-workflow-host`, which depend ON `sovereign-tools`, not the other way
//! round. To run a workflow when a sweep produces changes WITHOUT inverting that
//! dependency, the worker calls through this trait. The daemon installs a concrete
//! `DaemonWorkflowRuntime` (daemon-side glue in `sovereign-cli-daemon`, which
//! composes this trait with the `sovereign-workflow-host` runner) that resolves the
//! folder's `run_on_changes` workflow and runs it; tests and the desktop install
//! nothing (`None`), so the seam is inert there.
//!
//! `dispatch` is fire-and-forget — the implementation spawns + debounces, so it
//! returns immediately and never blocks the sweep or holds the per-corpus lock.

use async_trait::async_trait;

use super::diff::WatchedDiff;
use crate::local_corpus::config::{LocalCorpusConfig, WatchedFolderConfig};

/// Runs the workflow attached to a watched folder when a sweep changes its files.
/// Implemented in `sovereign-workflow-host` (the daemon side); `None` everywhere
/// else.
#[async_trait]
pub trait WorkflowTriggerRuntime: Send + Sync {
    /// A sweep on `corpus_id` applied a non-empty `diff` and the folder has a
    /// `run_on_changes` workflow attached. Run it over the changed files.
    ///
    /// Fire-and-forget: the implementation spawns + debounces and returns promptly,
    /// so the caller (the sweep loop, holding the per-corpus lock) is never blocked.
    async fn dispatch(
        &self,
        corpus_id: &str,
        config: &LocalCorpusConfig,
        watched_cfg: &WatchedFolderConfig,
        diff: &WatchedDiff,
    );
}
