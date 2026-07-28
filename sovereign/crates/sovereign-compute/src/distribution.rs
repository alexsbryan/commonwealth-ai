// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon → compute-child handoff for a **distributed primary**.
//!
//! A model too big for one box loads across mesh RPC workers. Today that load
//! happens inside the daemon, which means ggml's RPC client — which has no
//! error path — can `GGML_ABORT` the whole daemon when a worker dies: mid-decode
//! (`ggml-rpc.cpp:491`) or, just as fatally, during the teardown/prune reload
//! that fires *because* the worker died (`:386`, captured live 2026-07-27
//! 23:13). Moving the load into a supervised child moves both abort faces out of
//! the control plane.
//!
//! Warming cannot move with it. The orchestrator needs the mesh member
//! directory, the iroh transport bases, and the daemon's resolved ports
//! (`rpc_warm_http::orchestrate_warm`), none of which a child has. So the split
//! is: **the daemon plans and warms, the child loads.** This type is what
//! crosses between them.
//!
//! Why the plan crosses too, and not just the worker list: the shard plan is
//! cached per `(model, worker set)` precisely because a worker's free VRAM
//! swings by its own cached shard, so re-planning after a warm cuts the blocks
//! differently and invalidates every warm cache. That cache is process-local. A
//! child that re-planned would miss every `SET_TENSOR_HASH`, fall back to bulk
//! weight send, and hit the send() deadlock the warm path exists to avoid.
//! Shipping the plan and pinning it in the child extends the plan-agreement
//! invariant across the process boundary.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sovereign_inference::embedded::NodeShard;

/// Everything a compute child needs to load the distributed primary against
/// workers the daemon has already seeded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DistributionHandoff {
    /// RPC worker endpoints (`host:port`) the daemon warmed and the child must
    /// load across. These are loopback iroh-bridge ports owned by the daemon
    /// process — a child on the same host can dial them.
    pub endpoints: Vec<String>,
    /// The shard plan those workers were warmed AGAINST. Pinned in the child so
    /// its `-ot` overrides and `tensor_split` derive from the identical cut.
    pub plan: Vec<NodeShard>,
}

impl DistributionHandoff {
    /// Serialize to `path` (JSON). The daemon writes this before spawning the
    /// child; it is deliberately a readable file rather than an env blob so an
    /// operator can `cat` exactly what the child was told.
    ///
    /// Written via temp-file + rename, because the reader is not only the child
    /// we are about to spawn: the supervisor re-reads this same path on every
    /// auto-restart, so a plain truncating write races a crash-looping child
    /// into parsing half a file.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    /// Read a handoff written by [`Self::write`].
    pub fn read(path: &Path) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Install this handoff into the CURRENT process, so the next model load
    /// distributes across exactly these workers with exactly this plan.
    ///
    /// Called by the child before it loads. Two effects:
    /// 1. the worker list becomes the process's RPC worker source, and
    /// 2. the shard plan is pinned into the plan cache, so `plan_distribution`
    ///    takes a cache hit instead of re-planning against post-warm VRAM.
    ///
    /// The child must ALSO have `SOVEREIGN_RPC_ASSUME_WARMED=1` in its
    /// environment (the manager sets it): no warm orchestrator is installed in
    /// a child, and without that assertion `classify_placement` would refuse to
    /// distribute a large model and fall back to a local load.
    pub fn install(&self, model_path: &Path) {
        let endpoints = self.endpoints.clone();
        sovereign_inference::embedded::set_rpc_worker_provider(move || endpoints.clone());
        sovereign_inference::embedded::pin_shard_plan(
            model_path,
            &self.endpoints,
            self.plan.clone(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard(device_index: usize, first: u32, last: u32) -> NodeShard {
        NodeShard {
            device_index,
            blocks: Some((first, last)),
            holds_output: device_index == 1,
            fraction: 0.5,
        }
    }

    #[test]
    fn handoff_round_trips_through_a_file() {
        let dir = std::env::temp_dir().join(format!("handoff-{}", std::process::id()));
        let path = dir.join("distribution.json");
        let handoff = DistributionHandoff {
            endpoints: vec!["127.0.0.1:35539".to_string(), "127.0.0.1:35540".to_string()],
            plan: vec![shard(0, 0, 11), shard(1, 12, 47)],
        };

        handoff.write(&path).expect("write handoff");
        let read_back = DistributionHandoff::read(&path).expect("read handoff");

        // The plan must survive byte-identically: a child that loads a
        // different cut than the workers were warmed against misses every
        // cache and falls back to bulk weight send.
        assert_eq!(handoff, read_back);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_of_a_missing_handoff_is_an_error_not_a_panic() {
        let missing = std::env::temp_dir().join("definitely-not-a-handoff-9f3a.json");
        assert!(DistributionHandoff::read(&missing).is_err());
    }
}
