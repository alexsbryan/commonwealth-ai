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

    /// The `/status`-shaped placement this handoff describes: which blocks the
    /// child put on which remote worker, and how many stayed local.
    ///
    /// **Why this exists at all.** `rpc_distribution::summarize_placement` already
    /// answers this question — but it answers it in the process that *performs*
    /// the load, publishing into a process-global cell. For a distributed primary
    /// that process is the compute child, while `/status` is served by the parent.
    /// The parent therefore had no split to report and stated
    /// `total_blocks: 0, local_blocks: 0, workers: []` under mode
    /// `child-distributed` (manager.rs, live 2026-07-29). That blank is not
    /// cosmetic: `svrn mesh bench` hashes `placement_digest` from this report, so
    /// a genuine two-node run was keyed, labelled, and filed as a one-node local
    /// one — and `mesh plan`, which builds its digest by walking mesh members,
    /// could never compute the same key to look it up.
    ///
    /// This handoff is the parent's own copy of the cut it warmed against and
    /// handed the child, so it is the same ground truth without crossing the
    /// process boundary to fetch it.
    ///
    /// **The device-order contract.** Device index `i` names `endpoints[i]`;
    /// indices past the end are this host. That is not a convention invented
    /// here — it is `rpc_distribution`'s documented plan order ("device index `i`
    /// is `REGISTERED_RPC[i]`", RPC workers first and the host last), and the
    /// child derives its own `-ot` overrides from the identical rule. A shard
    /// holding no blocks contributes nothing, matching `summarize_placement`.
    pub fn placement(&self) -> sovereign_core::traits::SlotPlacement {
        use sovereign_core::traits::{SlotPlacement, WorkerPlacement};
        let (mut total, mut local) = (0u32, 0u32);
        let mut workers = Vec::new();
        for shard in &self.plan {
            let n = shard
                .blocks
                .map(|(f, l)| l.saturating_sub(f) + 1)
                .unwrap_or(0);
            total += n;
            match self.endpoints.get(shard.device_index) {
                Some(endpoint) => workers.push(WorkerPlacement {
                    endpoint: endpoint.clone(),
                    blocks: n,
                    holds_output: shard.holds_output,
                }),
                None => local += n,
            }
        }
        SlotPlacement {
            // Deliberately NOT "distributed". The load is real and remote, but it
            // is owned by a child process, and `mesh bench` keys on the mode
            // string — collapsing the two would make an in-daemon load and a
            // child-owned one compare equal when they are not the same system.
            mode: "child-distributed".to_string(),
            total_blocks: total,
            local_blocks: local,
            workers,
        }
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

    /// Byte-for-byte the handoff the live daemon wrote for the 122B on
    /// 2026-07-29 (`~/.svrnmesh/compute-distribution/Qwen3.5-122B-…json`): one
    /// remote worker on the LAN holding blocks 0–11, the host holding 12–47 plus
    /// the output head.
    fn live_122b_handoff() -> DistributionHandoff {
        DistributionHandoff {
            endpoints: vec!["192.168.1.2:50052".to_string()],
            plan: vec![
                NodeShard {
                    device_index: 0,
                    blocks: Some((0, 11)),
                    holds_output: false,
                    fraction: 0.2631579,
                },
                NodeShard {
                    device_index: 1,
                    blocks: Some((12, 47)),
                    holds_output: true,
                    fraction: 0.7368421,
                },
            ],
        }
    }

    #[test]
    fn placement_states_the_real_split_the_child_was_handed() {
        let p = live_122b_handoff().placement();

        assert_eq!(p.mode, "child-distributed");
        assert_eq!(p.total_blocks, 48);
        // 36 of the 48 stayed here; the other 12 are on the peer.
        assert_eq!(p.local_blocks, 36);
        assert_eq!(p.workers.len(), 1);
        assert_eq!(p.workers[0].endpoint, "192.168.1.2:50052");
        assert_eq!(p.workers[0].blocks, 12);
        // The host keeps the output head in this cut, so no worker claims it.
        assert!(!p.workers[0].holds_output);
    }

    /// The regression this function exists for. `/status` reported all three
    /// numbers as zero for a child-distributed primary, so `mesh bench` hashed a
    /// two-node run into the same `placement_digest` as a solo local one and
    /// filed a record `mesh plan` could never look up.
    #[test]
    fn a_distributed_handoff_never_reports_as_an_empty_local_placement() {
        let p = live_122b_handoff().placement();

        assert_ne!(
            p.total_blocks, 0,
            "a placement apportioning 48 blocks must not report zero — that is \
             the blank that made a 2-node measurement indistinguishable from a \
             1-node one"
        );
        assert!(
            !p.workers.is_empty(),
            "a handoff naming a remote endpoint must surface that worker"
        );
        // Every block is accounted for on exactly one device.
        let worker_blocks: u32 = p.workers.iter().map(|w| w.blocks).sum();
        assert_eq!(worker_blocks + p.local_blocks, p.total_blocks);
    }

    #[test]
    fn a_handoff_with_no_remote_endpoints_is_entirely_local() {
        let handoff = DistributionHandoff {
            endpoints: Vec::new(),
            plan: vec![NodeShard {
                device_index: 0,
                blocks: Some((0, 47)),
                holds_output: true,
                fraction: 1.0,
            }],
        };
        let p = handoff.placement();

        assert_eq!(p.total_blocks, 48);
        assert_eq!(p.local_blocks, 48);
        assert!(p.workers.is_empty());
    }

    #[test]
    fn a_device_assigned_no_blocks_contributes_nothing() {
        let handoff = DistributionHandoff {
            endpoints: vec!["10.0.0.9:50052".to_string()],
            plan: vec![
                NodeShard {
                    device_index: 0,
                    blocks: None,
                    holds_output: false,
                    fraction: 0.0,
                },
                NodeShard {
                    device_index: 1,
                    blocks: Some((0, 47)),
                    holds_output: true,
                    fraction: 1.0,
                },
            ],
        };
        let p = handoff.placement();

        assert_eq!(p.total_blocks, 48);
        assert_eq!(p.local_blocks, 48);
        // The worker is still named — it is part of the dialled set — but it
        // carries no weight, which is exactly what `summarize_placement` reports.
        assert_eq!(p.workers.len(), 1);
        assert_eq!(p.workers[0].blocks, 0);
    }
}
