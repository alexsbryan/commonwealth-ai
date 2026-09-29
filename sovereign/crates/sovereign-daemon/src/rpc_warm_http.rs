// SPDX-License-Identifier: AGPL-3.0-or-later
//! Distributed-inference auto-warm orchestration — the HTTP layer.
//!
//! When a host decides to distribute a large primary across the mesh, it must
//! NOT stream each worker its weight share at load time (the host-side `send()`
//! deadlock above ~800 MB). Instead every worker pre-seeds its RPC tensor cache
//! with its shard, so the host's `-ot` load is all `SET_TENSOR_HASH` cache hits
//! and sends zero bulk weight bytes. This module is both ends of that handshake:
//!
//! - **Worker side** ([`MeshRpcShardWarmer`], the `POST /internal/rpc-warm`
//!   backend): given the host's plan + this node's `device_index`, warm exactly
//!   this node's shard — from the whole GGUF the node already holds / fetches
//!   (`#5a`), or by range-fetching only its tensors (`#5b`, [`warm_cache_from_ranges`]).
//! - **Host side** ([`install_rpc_warm_orchestrator`]): the seam
//!   `sovereign-inference` calls during a distributing load. It fans the warm
//!   request out to every worker and blocks until all report warm — then the load
//!   proceeds with overrides. This replaces the manual `SOVEREIGN_RPC_ASSUME_WARMED`.
//!
//! The host computes the plan ONCE (`sovereign-inference::plan_distribution`) and
//! ships it whole, so warm-time placement and load-time placement derive from the
//! identical assignment and cannot diverge — the plan-agreement invariant.

use crate::state::AppState;
use sovereign_contracts::rpc_warm::WarmReach;

/// The worker's reach for one warm request, from the mesh this daemon holds
/// (pb-serve-distributes): the host's transport bases and this node's mesh
/// proof. The warm itself is the loader's (`sovereign_compute::distributed_warm`).
pub(crate) async fn warm_reach(state: &AppState, host_node_id: Option<&str>) -> WarmReach {
    let host_bases = host_transport_bases(state, host_node_id).await;
    let proof = state.mesh_proof_stamp().await.map(|s| {
        let (name, value) = s.pair();
        (name.to_string(), value.to_string())
    });
    WarmReach { host_bases, proof }
}

/// Resolve the HOST's fetch bases through THIS node's own transport, given the
/// `host_node_id` hex the warm request carried. On an iroh-routed mesh the
/// raw-IP bases in the request may be unroutable from here (host on a
/// different network) — but the mesh transport already reaches the member as a
/// loopback bridge. Empty when the id is absent/unparseable (legacy host) or
/// the host isn't in our membership; the caller then uses raw bases alone.
async fn host_transport_bases(state: &AppState, host_node_id: Option<&str>) -> Vec<String> {
    let Some(id) = host_node_id.and_then(|h| kernel_types::NodeId::from_hex(h)) else {
        return Vec::new();
    };
    let member = {
        state
            .inner
            .fabric
            .mesh
            .read()
            .await
            .members
            .get(&id)
            .cloned()
    };
    let Some(member) = member else {
        tracing::debug!(
            host = %id,
            "rpc-warm: host_node_id not in local membership; using raw bases only"
        );
        return Vec::new();
    };
    state
        .peer_transport()
        .endpoints(
            &commonwealth_transport::peer_contact(&member),
            commonwealth_transport::TrafficClass::ModelTransfer,
        )
        .await
        .into_iter()
        .map(|e| e.base_url)
        .collect()
}

// The host-side orchestrator, the worker and the wire types are the loader's
// (`sovereign_compute::distributed_warm`, pb-serve-distributes); re-exported
// at their historical paths.
pub use sovereign_compute::distributed_warm::{
    install_rpc_warm_orchestrator, warm_cache_from_ranges, MeshRpcShardWarmer, RpcWarmShardRequest,
    RpcWarmShardResponse, RpcWarmSource, TensorRange, WarmRangeStats,
};
