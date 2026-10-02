// SPDX-License-Identifier: AGPL-3.0-or-later
//! The local activity ledger's records: what this daemon did, and the summary
//! they aggregate into.
//!
//! Moved here from `commonwealth_core::activity` by pb-mesh-exit-core
//! (FIVE_PROGRAMS §12 3a rung 2): the events are served through cw-rails'
//! ledger doors and svrn writes them, so two programs speak them. The
//! aggregation (`commonwealth_core::activity::aggregate_activity`) stays
//! there; that module re-exports these records.

use serde::{Deserialize, Serialize};

use kernel_types::NodeId;

/// One discrete unit of local daemon work. Append-only; the `node_id`
/// is always *this* node (activity is never about a peer's work — a
/// peer's work for us is `contributions`, and our work is local).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActivityEvent {
    /// Origin: always the local node. Carried for symmetry with
    /// `LedgerEvent` and so the on-disk record is self-describing.
    pub node_id: NodeId,
    /// Unix seconds when the work completed.
    pub timestamp: u64,
    pub kind: ActivityEventKind,
}

/// Who a served unit of work was for. Embeddings and inference served
/// over the daemon's HTTP surface can be driven either by a mesh peer
/// (a node with no embed model of its own) or by a local OpenAI-API
/// client (a CLI tool, an editor plugin). The split matters in the UI:
/// "served to the mesh" vs "served to you, locally."
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "actor", rename_all = "snake_case")]
pub enum ServedFor {
    /// A local OpenAI-API client on this machine (no `X-Node-Id`).
    Local,
    /// A mesh peer, identified by node id.
    Peer { node_id: NodeId },
}

impl ServedFor {
    pub fn is_peer(&self) -> bool {
        matches!(self, ServedFor::Peer { .. })
    }
}

/// The closed set of local-activity dimensions. Per ARCH_PRINCIPLES
/// §2.1 each variant is a genuinely distinct kind of work; do not
/// overload one by stuffing a new payload into it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ActivityEventKind {
    /// The daemon served an inference completion to a *local* API
    /// client (requester had no `X-Node-Id`). Peer-served inference
    /// is the contribution ledger's `InferenceServed`; this is the
    /// local counterpart the contribution gate drops.
    LocalInferenceServed {
        model_id: String,
        prompt_tokens: u64,
        completion_tokens: u64,
        wall_seconds: f64,
    },
    /// The daemon served embeddings over `/v1/embeddings`. Recorded
    /// for both peer and local callers (`served_for`). This is the
    /// dimension that was previously invisible: a peer with no embed
    /// model drives this and nothing recorded it.
    EmbeddingsServed {
        served_for: ServedFor,
        /// Number of input texts embedded in the request.
        n_texts: u64,
        /// Approximate input tokens (the embeddings handler's
        /// `Usage.prompt_tokens`).
        tokens: u64,
    },
    /// The daemon answered a knowledge query for a *local* API client.
    /// Peer-served knowledge is the contribution ledger's
    /// `KnowledgeQueryServed`; this is the local counterpart.
    LocalKnowledgeServed {
        corpus_id: String,
        chunks_returned: u32,
    },
    /// A corpus ingest completed on this machine — `chunks` chunks
    /// were extracted, embedded, and indexed. This is the headline
    /// "your Obsidian import did real work" signal.
    ChunksIngested {
        corpus_id: String,
        chunks: u64,
        duration_secs: u64,
    },
    /// A corpus enrichment pass completed (atlas / RAPTOR / atom
    /// extraction). Heavy local inference work, distinct from the
    /// raw ingest embed pass above.
    CorpusEnriched {
        corpus_id: String,
        /// Atoms / nodes produced, when the pipeline reports it.
        atoms: u64,
        duration_secs: u64,
    },
    /// A wikipedia-newsworthy freshness tick fetched articles.
    NewsworthyFetched {
        articles: u64,
        portal_ingested: bool,
    },
}

/// A served-work tally split by who it was for (mesh peer vs local
/// client). Three orthogonal counts so the UI can say "N requests, M
/// texts, K tokens" without inferring one from another.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ServedTally {
    pub local_requests: u64,
    pub peer_requests: u64,
    /// Unit count (tokens for inference, texts for embeddings).
    pub local_units: u64,
    pub peer_units: u64,
}

/// Per-corpus local ingest + enrich activity within the window.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CorpusActivity {
    pub corpus_id: String,
    pub chunks_ingested: u64,
    pub ingest_runs: u64,
    pub ingest_seconds: u64,
    pub enrich_runs: u64,
    pub enrich_atoms: u64,
    pub enrich_seconds: u64,
}

/// The single self-view rollup of local activity over a window. This
/// is the activity counterpart to `NodeContributions`, but folded to
/// one node (always this one) and organised by dimension rather than
/// by peer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ActivitySummary {
    pub window_days: u32,
    // Inference the daemon served to local API clients.
    pub local_inference_requests: u64,
    pub local_tokens_generated: u64,
    pub local_inference_wall_seconds: f64,
    // Embeddings served over /v1/embeddings (peer + local).
    pub embeddings: ServedTally,
    // Knowledge served to local API clients.
    pub local_knowledge_queries: u64,
    pub local_chunks_served: u64,
    // Per-corpus ingest + enrichment work done on this machine.
    pub corpora: Vec<CorpusActivity>,
    pub total_chunks_ingested: u64,
    // Newsworthy freshness fetches.
    pub newsworthy_fetches: u64,
    pub newsworthy_articles: u64,
}

/// Default activity window. Shorter than the contribution ledger's 30
/// days — "what has my daemon been up to lately" is a more
/// immediate question than peer-fairness accounting, and a 7-day
/// window keeps the totals legible.
pub const DEFAULT_ACTIVITY_WINDOW_DAYS: u32 = 7;
