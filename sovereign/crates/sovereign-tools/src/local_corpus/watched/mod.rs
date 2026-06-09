// SPDX-License-Identifier: AGPL-3.0-or-later
//! Watched-folder reconciliation — third local-corpus source type.
//!
//! Sits between `DropFolder` (one-shot ingest) and `ObsidianVault`
//! (one-shot ingest with writeback) by adding a polling reconciliation
//! worker that walks the folder every couple of minutes and applies a
//! diff through `corpus_engine::update::CorpusUpdater`.
//!
//! Architecture:
//!
//! ```text
//! WatchedFolderRegistry
//!   ├── Scheduler        — per-corpus cadence + per-corpus lock
//!   └── Worker::run_once — walk → diff → guard → apply → tombstones
//!         ├── walker      — wraps PreScanner + (mtime,size) fast-path
//!         ├── diff        — pure compute_diff(prior, snapshot)
//!         ├── threshold   — pure DeletionGuard::evaluate
//!         ├── apply       — bridge to CorpusUpdater::apply_update
//!         └── soft_delete_gc — tombstone record/expire/cap-eviction
//! ```
//!
//! Privacy invariant (ARCH §7): every watched-folder corpus is `scope =
//! "local"` with `mesh_sharing = false` — hardcoded in the recipe
//! builder, pinned by a test in `config::tests`.
//!
//! The full implementation plan lives at
//! `~/.claude/plans/let-s-build-out-this-noble-ladybug.md`.

pub mod apply;
pub mod diff;
pub mod enrich;
pub mod events;
pub mod registry;
pub mod scheduler;
pub mod soft_delete_gc;
pub mod state;
pub mod status;
pub mod threshold;
pub mod walker;
pub mod worker;

pub use events::{EventSink, WatchedFolderEvent};
pub use registry::WatchedFolderRegistry;
pub use scheduler::Scheduler;
pub use state::{Tombstone, WatchedFolderState};
pub use status::{DiffSummary, SweepPhase, TrippedRule, WatchedFolderStatus};
pub use threshold::{DeletionGuard, GuardDecision};
pub use worker::{SkipReason, Worker, WorkerOutcome};
