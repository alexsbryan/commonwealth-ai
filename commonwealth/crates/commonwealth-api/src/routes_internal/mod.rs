// SPDX-License-Identifier: AGPL-3.0-or-later
//! Internal API surface (port 9742, peer-reachable). Façade module — handlers
//! live in endpoint-scoped submodules; this file re-exports them so
//! `server.rs` and the integration tests keep their existing import
//! paths.
//!
//! TRUST POSTURE — read this before putting anything expensive here.
//! This header said "mTLS" until 2026-07-27 and there is no TLS
//! anywhere in `server.rs`: `serve()` binds both listeners with plain
//! `TcpListener` + `axum::serve`. What actually guards this port is
//! the admission gate plus each handler's own mesh-id/join-key check —
//! NOT transport authentication. So treat every route here as
//! reachable by any peer that can route to this host, and do not mount
//! anything whose cost an unauthenticated caller shouldn't be able to
//! trigger. (`/internal/inference/warmup` was exactly that mistake: an
//! 18.5 GB model load, one unauthenticated POST away, on a port
//! described as mTLS-protected. It now lives on the client router.)
//!
//! The four shared wire types kept here (`IngestPartitionRequest`,
//! `IngestPartitionResponse`, `ErrorBody`, plus the submodule
//! re-exports) cross between siblings: `corpus_collaborate` POSTs an
//! `IngestPartitionRequest` to a peer's `corpus_queue::corpus_ingest_partition`
//! handler. Pulling either end into one submodule would force the
//! other to import across siblings.

use serde::{Deserialize, Serialize};

use commonwealth_inference::oicp::EmbedModelInfo;

mod atlas_status;
mod corpus_collaborate;
mod corpus_grant;
mod corpus_ingest;
mod corpus_queue;
mod corpus_sync;
mod enrichment_status;
mod gossip;
mod guest_grant;
mod knowledge;
mod mesh_admin;
mod model_files;
mod newsworthy_status;
mod pipeline_pause;
mod rpc_warm;

pub use atlas_status::{atlas_status, AtlasStatusResponse};
pub use corpus_collaborate::{corpus_collaborate, corpus_eligible_peers, CollaborateRequest};
pub use corpus_grant::{corpus_grant_issue, corpus_grant_revoke};
pub use corpus_ingest::{
    corpus_cancel, corpus_canonical_stream, corpus_expand, corpus_install, corpus_pause,
    corpus_progress, corpus_status, spawn_corpus_expand, spawn_corpus_install,
    spawn_corpus_install_with_parameters, CancelRequest, CancelResponse, CorpusStatusEntry,
    CorpusStatusResponse, ExpandRequest, InstallRequest, InstallResponse, PauseResponse,
    ProgressSnapshotResponse,
};
pub use guest_grant::{guest_grant_issue, guest_grant_list, guest_grant_revoke};
// Crate-internal: the OICP ingest routes (`routes_oicp_ingest`) reuse the
// same progress→fraction projection so the two surfaces can't diverge.
pub(crate) use corpus_ingest::progress_fraction;
pub use corpus_queue::{
    corpus_collaborate_status, corpus_complete_unit, corpus_heartbeat, corpus_ingest_partition,
    corpus_next_unit, corpus_partition_evict, CompleteUnitRequest, CompleteUnitResponse,
    HeartbeatRequest, HeartbeatResponseBody, NextUnitRequest, NextUnitResponse,
};
pub use corpus_sync::{index_serve, index_transfer, model_transfer};
pub use enrichment_status::{enrichment_status, EnrichmentStatusResponse};
pub use gossip::{
    gossip, scheduling_intent, scheduling_plan, GossipRejection, GossipRequest, GossipResponse,
    SchedulingIntent, SchedulingIntentResponse,
};
pub use knowledge::{knowledge_search, latency_probe};
pub use mesh_admin::{
    activity_recent, activity_summary, contribution_ceiling_set, contribution_pause,
    contribution_recent, contribution_resume, contribution_status, contribution_view,
    foreground_state, inference_warmup, ingest_budget_get, ingest_budget_set, join,
    mesh_quiesce_get, mesh_quiesce_set, models_inventory, models_load, models_unload,
    node_activity, recommended_storage_budget_bytes, storage_budget_get, storage_budget_set,
    ContributionStatusResponse, CorpusHostingView, ForegroundStateResponse, IngestBudgetState,
    InventoryEntry, InventoryResponse, JoinRejection, JoinRequest, JoinResponse, LoadModelRequest,
    LoadModelResponse, MeshQuiesceState, MeshWire, NodeActivityPayload, NodeContributionsView,
    PauseContributionsRequest, RecentContributionsParams, RecentContributionsResponse,
    SetContributionCeilingRequest, SetIngestBudgetRequest, SetMeshQuiesceRequest,
    SetStorageBudgetRequest, StorageBudgetState, UnloadModelRequest, UnloadModelResponse,
    WarmupResponse,
};
pub use model_files::{
    list_model_files, serve_model_file, ListResponse as ModelFileListResponse, ModelFileInfo,
};
pub use newsworthy_status::{
    newsworthy_status, newsworthy_tick, NewsworthyStatusResponse, NewsworthyTickResponse,
};
pub use pipeline_pause::{
    pipeline_pause, NodePauseResult, PipelinePauseRequest, PipelinePauseResponse,
};
pub use rpc_warm::rpc_warm;

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
