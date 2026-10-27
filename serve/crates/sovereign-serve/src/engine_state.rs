// SPDX-License-Identifier: AGPL-3.0-or-later
//! `GET` engine state: the RPC workers serve's discovery holds (moved from
//! lib.rs, pb-serve-distributes-standalone).

use axum::Json;
use tracing::debug;

/// The RPC workers this process's discovery holds. The device-memory reading
/// and the block-split pin were the embedded engine's and went with it
/// (ENGINE_SWAP_PLAN G2).
pub(crate) async fn engine_state() -> Json<sovereign_contracts::engine_state::EngineState> {
    let state = sovereign_contracts::engine_state::EngineState {
        rpc_workers: crate::rpc_worker_views(),
    };
    debug!(target: "serve", rpc_workers = state.rpc_workers.len(), "engine state: the loader's discovery view");
    Json(state)
}
