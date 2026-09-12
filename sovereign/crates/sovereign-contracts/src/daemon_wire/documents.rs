// SPDX-License-Identifier: AGPL-3.0-or-later
//! The answers of the three document-asset JOB routes added 2026-09-11 —
//! `POST /v1/documents` + `GET /v1/documents/{id}/progress`, `POST
//! /v1/documents/legacy`, and `POST /v1/documents/{id}/ask` + `GET
//! /v1/documents/{id}/ask/{job_id}` — defined here so a client parses them
//! without linking `sovereign-mesh`, and re-exported by
//! `sovereign_mesh::documents_http`, which serialises them (one
//! definition, ARCH principle 8). Frames are `serde_json::Value` because
//! the manager's `IngestProgress` / `OperationProgress` are
//! `sovereign-tools` types that serialise only; a client re-emits them
//! verbatim and never needs to name them.

use serde::{Deserialize, Serialize};

use crate::types::{DocumentAssetOperation, Message};

/// What `GET /v1/documents/{id}/progress?after=N` answers.
///
/// `frames` are `sovereign_tools::document_asset::IngestProgress` values
/// serialised by the manager (`{"type": …}`), each with `asset_id`
/// stamped on by the route, from the caller's cursor onward. `finished`
/// flips when the job appended its terminal frame — `Ready`, or `Failed`
/// naming the reason.
#[derive(Debug, Serialize, Deserialize)]
pub struct DocumentIngestProgress {
    pub asset_id: String,
    pub frames: Vec<serde_json::Value>,
    /// The cursor to send next: one past the last frame here.
    pub next: usize,
    pub finished: bool,
}

/// `POST /v1/documents/legacy`'s answer. `source` is the file NAME — the
/// shape the desktop command always returned and the attachment chip
/// renders. (The legacy listing's `source` key is the full path the
/// chunks were stored under; its `filename` is this value.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestLegacyResponse {
    pub source: String,
    pub chunks_created: usize,
}

/// `POST /v1/documents/{id}/ask`'s 202 body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskJobAck {
    pub asset_id: String,
    pub job_id: String,
    pub progress_route: String,
}

/// How an ask job ended. Exactly one is set when `finished`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AskOutcome {
    /// The document answered. `message` is the PERSISTED assistant
    /// message, metadata and all — what a reload renders from, returned
    /// so the live bubble renders identically (the desktop's reason for
    /// carrying `metadata` verbatim).
    Answered {
        operation: DocumentAssetOperation,
        message: Message,
        sources: Vec<String>,
    },
    /// The document did not answer and the question is an ordinary
    /// turn: off-topic by the router's decision, or a `Rag` route that
    /// retrieved nothing. The user message is already persisted (tagged
    /// with the asset), so the client runs the turn and nothing else.
    FellThrough {
        operation: DocumentAssetOperation,
        reason: String,
    },
    Failed {
        error: String,
    },
}

/// What `GET /v1/documents/{id}/ask/{job_id}?after=N` answers.
///
/// `frames` are `sovereign_tools::document_asset::OperationProgress`
/// values (`{"type": …}`) from the caller's cursor on — the desktop's
/// `document:operation` events, verbatim.
#[derive(Debug, Serialize, Deserialize)]
pub struct AskProgress {
    pub asset_id: String,
    pub job_id: String,
    pub frames: Vec<serde_json::Value>,
    pub next: usize,
    pub finished: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<AskOutcome>,
}
