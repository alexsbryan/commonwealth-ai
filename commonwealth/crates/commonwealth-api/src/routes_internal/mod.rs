//! Internal API surface (mTLS, port 9742). Façade module — handlers
//! live in endpoint-scoped submodules; this file re-exports them so
//! `server.rs` and the integration tests keep their existing import
//! paths.
//!
//! The four shared wire types kept here (`IngestPartitionRequest`,
//! `IngestPartitionResponse`, `ErrorBody`, plus the submodule
//! re-exports) cross between siblings: `corpus_collaborate` POSTs an
//! `IngestPartitionRequest` to a peer's `corpus_queue::corpus_ingest_partition`
//! handler. Pulling either end into one submodule would force the
//! other to import across siblings.

use serde::{Deserialize, Serialize};

use commonwealth_inference::oicp::EmbedModelInfo;

mod corpus_collaborate;
mod corpus_ingest;
mod corpus_queue;
mod corpus_sync;
mod gossip;
mod knowledge;
mod mesh_admin;

pub use corpus_collaborate::{corpus_collaborate, CollaborateRequest};
pub use corpus_ingest::{
    corpus_cancel, corpus_canonical_stream, corpus_expand, corpus_install, corpus_pause,
    corpus_progress, corpus_status, spawn_corpus_expand, spawn_corpus_install,
    spawn_corpus_install_with_parameters, CancelRequest, CancelResponse, CorpusStatusEntry,
    CorpusStatusResponse, ExpandRequest, InstallRequest, InstallResponse, PauseResponse,
    ProgressSnapshotResponse,
};
pub use corpus_queue::{
    corpus_complete_unit, corpus_heartbeat, corpus_ingest_partition, corpus_next_unit,
    CompleteUnitRequest, CompleteUnitResponse, HeartbeatRequest, HeartbeatResponseBody,
    NextUnitRequest, NextUnitResponse,
};
pub use corpus_sync::{index_serve, index_transfer, model_transfer};
pub use gossip::{
    gossip, scheduling_intent, scheduling_plan, GossipRejection, GossipRequest, GossipResponse,
    SchedulingIntent, SchedulingIntentResponse,
};
pub use knowledge::{knowledge_search, latency_probe};
pub use mesh_admin::{
    foreground_state, ingest_budget_get, ingest_budget_set, join, mesh_quiesce_get,
    mesh_quiesce_set, models_inventory, models_load, models_unload, node_activity,
    ForegroundStateResponse, IngestBudgetState, InventoryEntry, InventoryResponse, JoinRejection,
    JoinRequest, JoinResponse, LoadModelRequest, LoadModelResponse, MeshQuiesceState, MeshWire,
    NodeActivityPayload, SetIngestBudgetRequest, SetMeshQuiesceRequest, UnloadModelRequest,
    UnloadModelResponse,
};

// Queue helpers re-exported intra-module so `corpus_collaborate` can keep
// reaching for `super::find_local_handoff_for_corpus` /
// `super::spawn_queue_merge` without knowing they actually live in
// `corpus_queue`. The collaborate coordinator drives merges after the
// last unit completes; that path crosses sibling boundaries naturally.
pub(super) use corpus_queue::{find_local_handoff_for_corpus, spawn_queue_merge};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestPartitionRequest {
    pub handoff_id: commonwealth_core::ids::HandoffId,
    pub corpus_id: String,
    pub recipe_id: String,
    pub file_indices: Vec<usize>,
    /// Article range for JSONL corpora (e.g. Wikipedia). Mutually exclusive
    /// with `file_indices` — exactly one should be non-empty/non-None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub article_range: Option<(u64, u64)>,
    pub embed_model: EmbedModelInfo,
}

#[derive(Debug, Serialize)]
pub struct IngestPartitionResponse {
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: String,
}
