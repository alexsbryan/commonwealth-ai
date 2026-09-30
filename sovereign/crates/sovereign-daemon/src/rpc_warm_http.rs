// SPDX-License-Identifier: AGPL-3.0-or-later
//! Distributed-inference auto-warm — the daemon's half: the worker's reach
//! into this daemon's mesh for one `POST /internal/rpc-warm`. Both ends of the
//! warm handshake (the host-side orchestrator, the worker's warmer and the
//! wire types) are the loader's, `sovereign_compute::distributed_warm`
//! (pb-serve-distributes); the daemon links no loader crate.

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
            mesh_reach::TrafficClass::ModelTransfer,
        )
        .await
        .into_iter()
        .map(|e| e.base_url)
        .collect()
}
