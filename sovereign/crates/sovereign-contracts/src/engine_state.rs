// SPDX-License-Identifier: AGPL-3.0-or-later
//! `serve`'s engine-state wire: the loader's CACHED view of device memory and
//! the pinned RPC block split, which the svrn daemon's `/v1/mesh/status`
//! reads once serving lives in serve (pb-svrn-dials-serve). serve produces it
//! and the daemon consumes it, so the shape lives in the serving contract.
//!
//! Cached, never sampled, on serve's side too: sampling an RPC device is a
//! round trip to a worker that can stall for a minute (the 2026-07-30 hang,
//! `sovereign_inference::embedded::last_device_memory`).

use serde::{Deserialize, Serialize};

/// Where serve answers the engine state.
pub const ENGINE_STATE_PATH: &str = "/v1/engine/state";

/// What the loader last saw.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineState {
    /// `None` until the loader has planned a distributed load in that process
    /// ("not observed yet", never "no devices").
    pub device_memory: Option<DeviceMemoryReading>,
    /// `SOVEREIGN_RPC_BLOCK_SPLIT` as the loader reads it, raw.
    pub rpc_block_split_pin: Option<String>,
}

/// One cached reading, with its age.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceMemoryReading {
    /// Unix seconds at which the loader took the reading.
    pub observed_unix: u64,
    /// Plan order: eligible RPC workers first, local GPU last.
    pub devices: Vec<DeviceBytes>,
}

/// One device as the loader saw it, in bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceBytes {
    /// The RPC worker endpoint behind the device; `None` for a local one.
    pub endpoint: Option<String>,
    /// Available at the instant of the reading.
    pub free_bytes: u64,
    /// Physical device memory.
    pub total_bytes: u64,
    /// What the owning node keeps for itself and will not lend.
    pub reserve_bytes: u64,
}
