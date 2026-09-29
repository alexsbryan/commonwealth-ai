// SPDX-License-Identifier: AGPL-3.0-or-later
//! The attached-document probe's request and evidence
//! ([`super::ProbeMode::Attached`]; pb-bench-dials-docs).
//!
//! svrn resolves, ingests or reuses a document asset, meters the build, and
//! answers each question through a minted `DocumentSession` turn. The
//! evidence is what a judge reads: the answer and its message metadata, the
//! turn's narration, the asset's chunks and the build's ledger. No bank, no
//! expectation, no verdict (principle 12).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::ResourceReport;
use crate::types::{DocumentAsset, DocumentChunk, NarrationEvent};

/// What the attached probe builds, and how.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachedProbe {
    /// The asset to answer from.
    pub source: AttachedSource,
    /// Model id that serves the enrichment pipeline (ingest, skeleton,
    /// RAPTOR); `None` = the session's chat model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrich_model: Option<String>,
    /// Force the LLM window pass for the T2 entity extraction even when
    /// serve holds a GLiNER model.
    #[serde(default)]
    pub no_gliner: bool,
    /// On a reused asset: rebuild its skeleton before the questions.
    #[serde(default)]
    pub rebuild_skeleton: bool,
    /// On a reused asset: rebuild its RAPTOR atlas + motif index before
    /// the questions.
    #[serde(default)]
    pub rebuild_raptor: bool,
    /// Warm the sealed corpus's enrichment atlas into the session before
    /// any turn.
    #[serde(default)]
    pub warm_atlas: bool,
    /// Who asked, for serve's NER dial and the log lines
    /// (`bench book-report`, `bench chaos`).
    pub lane: String,
}

/// Where the asset comes from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttachedSource {
    /// Ingest this file as a new asset.
    Ingest {
        /// The document on disk.
        path: PathBuf,
    },
    /// Answer from an asset already in the store.
    Reuse {
        /// The asset's id.
        asset_id: String,
    },
    /// List the store's assets and answer nothing.
    List,
}

/// Everything an attached probe run observed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachedEvidence {
    /// [`AttachedSource::List`]: every asset in the store. Empty otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assets: Vec<DocumentAsset>,
    /// The asset the questions were answered from. `None` = ingest failed
    /// (the last transition says why), or nothing was asked.
    pub asset: Option<DocumentAsset>,
    /// The session's chat model — the one that answered.
    pub chat_model: String,
    /// The model that served the enrichment pipeline.
    pub enrich_model: String,
    /// Wall time of the ingest (0 on a reused asset).
    pub attach_ms: u64,
    /// The build's transition log, in order.
    pub transitions: Vec<StateTransition>,
    /// `ready`, `failed` or `reused`.
    pub terminal_phase: String,
    /// The asset's chunks, with their embeddings: the judging evidence.
    pub chunks: Vec<DocumentChunk>,
    /// One turn per question, in request order.
    pub rows: Vec<AttachedTurn>,
    /// The metered build: what the enrichment provider was asked, per phase
    /// and per call.
    pub resources: ResourceReport,
}

/// One question answered through a minted `DocumentSession`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachedTurn {
    /// [`super::ProbeQuestion::id`].
    pub id: String,
    /// `Some(why)` when the turn failed or panicked; `answer` is then empty
    /// and makes no claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The assistant message's content, as the runtime returned it.
    pub answer: String,
    /// The assistant message's metadata (`retrieved_chunks`, …).
    pub metadata: Option<serde_json::Value>,
    /// The runtime's narration for the turn.
    pub narration: Vec<NarrationEvent>,
    /// Wall time of the turn.
    pub latency_ms: u64,
    /// The question's embedding under the session's embed model, which
    /// embedded the chunks; empty when the embed failed.
    pub question_embedding: Vec<f32>,
}

/// One recorded state transition. The bench renders these as a timeline
/// in the report so the team can see "ingest got to PartiallyReady at
/// 4.2s, BuildingSkeleton at 18s, Ready at 47s" at a glance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTransition {
    /// Milliseconds since `attach_at`.
    pub ms_since_attach: u64,
    /// One of: started, indexing, chunk_indexed, partially_ready,
    /// skeleton_building, skeleton_chunk_processed, ready, failed.
    pub phase: String,
    /// Free-form per-phase detail (chunk counts, durations, etc.).
    pub detail: serde_json::Value,
}
