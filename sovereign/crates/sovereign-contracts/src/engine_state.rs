// SPDX-License-Identifier: AGPL-3.0-or-later
//! `serve`'s engine-state wire: the loader's CACHED view of device memory and
//! the pinned RPC block split, which the svrn daemon's `/v1/mesh/status`
//! reads once serving lives in serve (pb-svrn-dials-serve). serve produces it
//! and the daemon consumes it, so the shape lives in the serving contract.
//!
//! Cached, never sampled, on serve's side too: sampling an RPC device is a
//! round trip to a worker that can stall for a minute (the 2026-07-30 hang,
//! `sovereign_inference::embedded::last_device_memory`).
//!
//! [`Lap`] times one loopback request on either side under one target.

use serde::{Deserialize, Serialize};

/// Where serve answers the engine state.
pub const ENGINE_STATE_PATH: &str = "/v1/engine/state";

/// Where serve rebuilds its provider from the config on disk, through the one
/// serving assembly's `ReloadFactory`. The svrn daemon's `/v1/admin/reload`
/// forwards here when it dials serve (pb-svrn-dials-serve).
pub const RELOAD_PATH: &str = "/v1/engine/reload";

/// serve's answer to a reload that rebuilt: the models it now holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineReloaded {
    /// `model_id` of every resident slot after the swap.
    pub resident_models: Vec<String>,
}

/// What the loader last saw.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineState {
    /// `None` until the loader has planned a distributed load in that process
    /// ("not observed yet", never "no devices").
    pub device_memory: Option<DeviceMemoryReading>,
    /// `SOVEREIGN_RPC_BLOCK_SPLIT` as the loader reads it, raw.
    pub rpc_block_split_pin: Option<String>,
    /// The RPC workers this process's discovery holds, one eligibility row
    /// each (`sovereign_serving_host::worker_eligibility`'s status view:
    /// `node_id`, `endpoint`, …). svrn's `/v1/mesh/status` carried them
    /// until pb-mesh-exit-transport; `svrn mesh plan|bench` read them here.
    /// Empty where no discovery loop runs.
    #[serde(default)]
    pub rpc_workers: Vec<serde_json::Value>,
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

/// Where serve answers what its provider says about itself.
pub const SERVED_SELF_PATH: &str = "/v1/engine/self";

/// serve's provider, describing itself: what the svrn daemon's loopback
/// terminal arm answers `model_id_for`, `resident_slots` and
/// `edit_slot_info` from once the daemon holds no weights
/// (pb-svrn-dials-serve). Typed, because the OICP manifest carries neither
/// the slot roles nor the edit slot, and deriving them from it would be a
/// guess (principle 6).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServedSelf {
    /// `model_id_for(Speed::Slow)`.
    pub primary_model: String,
    /// `model_id_for(Speed::Medium)`.
    pub medium_model: String,
    /// `model_id_for(Speed::Fast)`.
    pub fast_model: String,
    /// `embed_model_id()`; `"unknown"` is the trait's own sentinel.
    pub embed_model: String,
    /// The embed slot's family as serve's assembly resolved it. Its
    /// `default_quirks().embed` is the query-instruction prefix, the same
    /// decider the engine applies in process, so a client embedding a query
    /// over the wire prepares it the same way.
    pub embed_family: crate::model_family::ModelFamily,
    /// `code_model_id()`.
    pub code_model: Option<String>,
    /// `resident_slots()`: every configured slot, `resident` as a flag.
    pub resident_slots: Vec<crate::oicp::ResidentSlot>,
    /// `edit_slot_info()`; `None` means no editing model at all.
    pub edit_slot: Option<crate::types::EditSlotInfo>,
    /// `effective_context_size()`: the chat slot's window as serve loaded it.
    pub context_size: Option<u32>,
    /// `compute_children()`: the supervised children serve runs, whose roles
    /// say which kinds (rerank among them) it serves out of process.
    pub compute_children: Vec<crate::oicp::ComputeChildStatus>,
}

/// The one tracing target for per-request timing across the serve loopback,
/// both sides (serve's handlers and the daemon's loopback client), at `debug`:
/// `RUST_LOG=serve_latency=debug` shows where a loopback request's time goes
/// (phase-b-25, attributing the loopback's first-token cost).
pub const LATENCY_TARGET: &str = "serve_latency";

/// One request's stopwatch. `mark` emits a `debug` event under
/// [`LATENCY_TARGET`] with the microseconds since `start`; `first` does so
/// only the first time a phase is reached (a stream's first frame).
#[derive(Debug)]
pub struct Lap {
    side: &'static str,
    op: &'static str,
    started: std::time::Instant,
    seen: std::sync::Mutex<Vec<&'static str>>,
}

impl Lap {
    /// Start timing `op` (`chat`, `embed`) on `side` (`serve`, `client`).
    pub fn start(side: &'static str, op: &'static str) -> Self {
        Self {
            side,
            op,
            started: std::time::Instant::now(),
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Emit `phase` with the microseconds since start.
    pub fn mark(&self, phase: &'static str) {
        let us = self.started.elapsed().as_micros() as u64;
        tracing::debug!(target: LATENCY_TARGET, side = self.side, op = self.op, phase, us, "lap");
    }

    /// [`Lap::mark`], the first time `phase` is reached only.
    pub fn first(&self, phase: &'static str) {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if !seen.contains(&phase) {
            seen.push(phase);
            drop(seen);
            self.mark(phase);
        }
    }
}
