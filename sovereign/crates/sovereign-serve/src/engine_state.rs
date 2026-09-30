// SPDX-License-Identifier: AGPL-3.0-or-later
//! `GET` engine state: the loader's cached device-memory reading and the
//! pinned block split (moved from lib.rs, pb-serve-distributes-standalone).

use axum::Json;
use tracing::debug;

/// The loader's CACHED view: the device memory it read the last time it
/// planned a distributed load, and the pinned block split. Never sampled
/// here — sampling an RPC device can stall on a busy worker (the 2026-07-30
/// hang) — so this answers as fast as the daemon's own `/v1/mesh/status`
/// did when the loader lived there (pb-svrn-dials-serve).
pub(crate) async fn engine_state() -> Json<sovereign_contracts::engine_state::EngineState> {
    use sovereign_contracts::engine_state::{DeviceBytes, DeviceMemoryReading, EngineState};
    use sovereign_inference::embedded::{DeviceMemory, DeviceMemorySnapshot};
    // Destructured exhaustively: a field added to the loader's reading is a
    // compile error here, never a field that silently stops at serve.
    let device_memory = sovereign_inference::embedded::last_device_memory().map(
        |DeviceMemorySnapshot {
             observed_unix,
             devices,
         }| DeviceMemoryReading {
            observed_unix,
            devices: devices
                .into_iter()
                .map(
                    |DeviceMemory {
                         endpoint,
                         free_bytes,
                         total_bytes,
                         reserve_bytes,
                     }| DeviceBytes {
                        endpoint,
                        free_bytes,
                        total_bytes,
                        reserve_bytes,
                    },
                )
                .collect(),
        },
    );
    let state = EngineState {
        device_memory,
        rpc_block_split_pin: sovereign_inference::embedded::pinned_block_split_raw(),
    };
    debug!(target: "serve", observed = state.device_memory.is_some(), pinned = state.rpc_block_split_pin.is_some(), "engine state: the loader's cached view");
    Json(state)
}
