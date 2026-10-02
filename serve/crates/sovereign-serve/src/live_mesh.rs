// SPDX-License-Identifier: AGPL-3.0-or-later
//! The live mesh as `svrn mesh plan --from-mesh` and `svrn mesh bench` read
//! it: cw-rails' roster, projected onto svrn's member rows
//! (`sovereign_contracts::daemon_wire::RailsMeshStatus`), beside this node's
//! serve engine state (the loader's cached device memory, the block-split
//! pin, the discovered RPC workers).
//!
//! svrn's `/v1/mesh/status` answered both halves in one document until
//! pb-mesh-exit-transport; the roster is cw-rails' now and the loader rows
//! were always serve's, so this composes the same document from the two
//! owners and both verbs keep one parse of it.

use serde::Serialize;
use sovereign_contracts::daemon_wire::{MeshStatusSummary, RailsMeshStatus};
use sovereign_contracts::engine_state::{DeviceBytes, EngineState, ENGINE_STATE_PATH};
use tracing::debug;

const TARGET: &str = "mesh_plan";

/// One placed device's live memory, in MiB.
///
/// Both figures travel together on purpose: the difference between them is
/// "memory held by something else right now", which is the whole distinction
/// between "this device is too small for the job" and "this device is busy" —
/// diagnoses with opposite repairs. Moved from the daemon's `mesh_http`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DeviceMemoryView {
    /// The RPC worker endpoint behind this device; absent for this host's own
    /// GPU. Joins a row to its `rpc_workers` entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    endpoint: Option<String>,
    free_mb: u64,
    total_mb: u64,
    /// What the owning node keeps for itself and will not lend.
    reserve_mb: u64,
}

impl From<DeviceBytes> for DeviceMemoryView {
    fn from(m: DeviceBytes) -> Self {
        const MIB: u64 = 1024 * 1024;
        Self {
            free_mb: m.free_bytes / MIB,
            total_mb: m.total_bytes / MIB,
            reserve_mb: m.reserve_bytes / MIB,
            endpoint: m.endpoint,
        }
    }
}

/// cw-rails' status at `[daemon] rails_base`, projected, with this node's
/// engine rows merged in under the names svrn's status used:
/// `rpc_workers`, `device_memory`, `device_memory_observed_unix`,
/// `rpc_block_split_pin`. An unreachable cw-rails is an `Err` naming it; a
/// serve that does not answer leaves the engine rows empty ("not observed
/// yet"), which both verbs already report as unknown.
pub(crate) async fn status_doc(client: &reqwest::Client) -> Result<serde_json::Value, String> {
    let rails = crate::measurements_rail::rails_base(None);
    let url = format!("{rails}/v1/mesh/status");
    let resp = client.get(&url).send().await.map_err(|e| {
        format!(
            "cw-rails at {url} not reachable: {e}\n  hint: `{}` brings it up",
            sovereign_turn_client::rails_kv::RAILS_BRING_UP_VERB
        )
    })?;
    if !resp.status().is_success() {
        return Err(format!(
            "cw-rails returned HTTP {} from {url}",
            resp.status()
        ));
    }
    let roster: RailsMeshStatus = resp
        .json()
        .await
        .map_err(|e| format!("bad status JSON from {url}: {e}"))?;
    let mut doc = serde_json::to_value(MeshStatusSummary::from(roster))
        .map_err(|e| format!("the mesh view did not serialise: {e}"))?;

    let serve = sovereign_turn_client::serve_self::default_serve_base();
    let engine = match client
        .get(format!("{serve}{ENGINE_STATE_PATH}"))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r.json::<EngineState>().await.ok(),
        _ => None,
    };
    debug!(target: TARGET, rails = %rails, serve = %serve, engine = engine.is_some(),
           "live mesh: roster from cw-rails, engine rows from serve");
    let engine = engine.unwrap_or_default();
    let (devices, observed) = match engine.device_memory {
        Some(r) => (
            r.devices
                .into_iter()
                .map(DeviceMemoryView::from)
                .collect::<Vec<_>>(),
            Some(r.observed_unix),
        ),
        None => (Vec::new(), None),
    };
    if let Some(obj) = doc.as_object_mut() {
        obj.insert("rpc_workers".into(), engine.rpc_workers.into());
        obj.insert("device_memory".into(), serde_json::json!(devices));
        obj.insert(
            "device_memory_observed_unix".into(),
            serde_json::json!(observed),
        );
        obj.insert(
            "rpc_block_split_pin".into(),
            serde_json::json!(engine.rpc_block_split_pin),
        );
    }
    Ok(doc)
}
