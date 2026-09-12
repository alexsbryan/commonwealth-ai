// SPDX-License-Identifier: AGPL-3.0-or-later
//! The local-corpus vocabulary: the config schema a surface writes, the
//! progress frames it reads, and the results the write-back surface reports.
//!
//! # The generics below are CLOSED as of svt-6 (2026-09-12)
//!
//! `PreScanAnswerView` and `ClusterProgressView` were generic because "the
//! payload a route writes closes over a `sovereign-tools` type with no home
//! at this layer". The types have a home at this layer now — the submodules
//! below — so each view names it, and a client no longer chooses between
//! linking `sovereign-tools` and passing `serde_json::Value` through.
//!
//! What moved, and why it is not `sovereign-tools`' any more: every type here
//! is pure serde over primitives, and it is what the DESKTOP writes when a
//! user drags a folder in, what the DAEMON persists, and what crosses
//! `/internal/corpus/local/*`. Defining them above the wire meant a client
//! that only wanted to spell a source type linked corpus-engine,
//! sovereign-store and sovereign-atos to do it. `sovereign-tools` keeps every
//! behaviour — the manager, the pre-scanner, the clusterer, write-back, the
//! recipe rendering — and re-exports each type at its historical path, so no
//! importer in the monorepo changes (the dd8bb42e6 pattern).

pub mod clusterer;
pub mod config;
pub mod git;
pub mod manager;
pub mod pre_scanner;
pub mod preview;
pub mod progress;
pub mod writeback;

use serde::{Deserialize, Serialize};

// Re-exported flat so `sovereign_contracts::daemon_wire::<Name>` resolves for
// every one of them, the way the rest of this family does.
pub use clusterer::*;
pub use config::*;
pub use git::*;
pub use manager::*;
pub use pre_scanner::*;
pub use preview::*;
pub use progress::*;
pub use writeback::*;

/// What `POST /internal/corpus/local/pre-scan` answers.
///
/// `corpus_id` and `display_name` come from the config AS REGISTERED —
/// the manager keeps an existing id when the path is already registered
/// under one.
///
/// The `Scan` parameter is GONE (svt-6): [`pre_scanner::PreScanResult`] lives
/// at this layer now, so the view names it.
#[derive(Debug, Serialize, Deserialize)]
pub struct PreScanAnswerView {
    pub corpus_id: String,
    pub display_name: String,
    pub result: PreScanResult,
}

/// What `GET …/{corpus}/cluster/progress?after=N` answers.
///
/// `frames` are the manager's own `LocalCorpusProgress` values, verbatim,
/// from the caller's cursor onward — so a client re-emitting them on its
/// own channel puts the same bytes there that the in-process callback
/// used to. The terminal frame is appended by the job itself: `Complete
/// { result: Ingest(zero stats) }` on success (the shape the desktop's
/// `lc_cluster` always emitted — the preview is fetched separately
/// because it is large), or `Error` naming what refused.
///
/// The `Frame` parameter is GONE (svt-6): [`progress::LocalCorpusProgress`]
/// lives at this layer now, so the view names it.
#[derive(Debug, Serialize, Deserialize)]
pub struct ClusterProgressView {
    pub corpus_id: String,
    pub job_id: String,
    pub frames: Vec<LocalCorpusProgress>,
    /// The cursor to send next: the index one past the last frame here.
    pub next: usize,
    /// `true` iff the job has appended its terminal frame.
    pub finished: bool,
}
