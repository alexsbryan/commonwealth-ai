// SPDX-License-Identifier: AGPL-3.0-or-later
//! The live mesh as `svrn mesh plan --from-mesh` and `svrn mesh bench` read
//! it: cw-rails' roster, projected onto svrn's member rows
//! (`sovereign_contracts::daemon_wire::RailsMeshStatus`), beside this node's
//! serve engine state (the discovered RPC workers).
//!
//! svrn's `/v1/mesh/status` answered both halves in one document until
//! pb-mesh-exit-transport; the roster is cw-rails' now and the loader rows
//! were always serve's, so this composes the same document from the two
//! owners and both verbs keep one parse of it.

use sovereign_contracts::daemon_wire::{MeshStatusSummary, RailsMeshStatus};
use sovereign_contracts::engine_state::{EngineState, ENGINE_STATE_PATH};
use tracing::debug;

const TARGET: &str = "mesh_plan";

/// cw-rails' status at `[daemon] rails_base`, projected, with this node's
/// engine rows merged in under the name svrn's status used: `rpc_workers`.
/// The loader's `device_memory` and `rpc_block_split_pin` were the embedded
/// engine's and are absent since its deletion (ENGINE_SWAP_PLAN G2), which
/// `mesh plan` reports as no live reading. An unreachable cw-rails is an `Err` naming it; a
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
    if let Some(obj) = doc.as_object_mut() {
        obj.insert("rpc_workers".into(), engine.rpc_workers.into());
    }
    Ok(doc)
}
