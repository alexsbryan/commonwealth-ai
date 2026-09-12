// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client's read of the two local-corpus routes added 2026-09-11 —
//! `POST /internal/corpus/local/pre-scan` and
//! `GET /internal/corpus/local/{corpus}/cluster/progress` — in the shape
//! `ingest.rs` set: the payload a route writes closes over a
//! `sovereign-tools` type with no home at this layer, so the view is
//! generic over it. A client that links `sovereign-tools` names the type
//! (the desktop); one that does not passes `serde_json::Value` through.
//! `sovereign_mesh::lc_http` aliases each view at the concrete type and
//! serialises it, so the bytes are one definition's (ARCH principle 8).

use serde::{Deserialize, Serialize};

/// What `POST /internal/corpus/local/pre-scan` answers.
///
/// `corpus_id` and `display_name` come from the config AS REGISTERED —
/// the manager keeps an existing id when the path is already registered
/// under one. `Scan` is `sovereign_tools::local_corpus::pre_scanner::
/// PreScanResult`.
#[derive(Debug, Serialize, Deserialize)]
pub struct PreScanAnswerView<Scan = serde_json::Value> {
    pub corpus_id: String,
    pub display_name: String,
    pub result: Scan,
}

/// What `GET …/{corpus}/cluster/progress?after=N` answers.
///
/// `frames` are the manager's own `LocalCorpusProgress` values, verbatim,
/// from the caller's cursor onward — so a client re-emitting them on its
/// own channel puts the same bytes there that the in-process callback
/// used to. The terminal frame is appended by the job itself: `Complete
/// { result: Ingest(zero stats) }` on success (the shape the desktop's
/// `lc_cluster` always emitted — the preview is fetched separately
/// because it is large), or `Error` naming what refused. `Frame` is
/// `sovereign_tools::local_corpus::progress::LocalCorpusProgress`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ClusterProgressView<Frame = serde_json::Value> {
    pub corpus_id: String,
    pub job_id: String,
    pub frames: Vec<Frame>,
    /// The cursor to send next: the index one past the last frame here.
    pub next: usize,
    /// `true` iff the job has appended its terminal frame.
    pub finished: bool,
}
