//! Build this node's `NodeCapabilities` from live system state.
//!
//! Why this module exists: `MemberRecord.capabilities` is the field
//! that gossip carries to every peer, and it's the field every other
//! machine consults when deciding *"does anyone on this mesh host the
//! SEP corpus / have a beefier model?"*. Today every constructor of
//! `NodeCapabilities` across the Commonwealth tree hardcodes empty
//! values (see `membership.rs`, `monitor.rs`, `routes_status.rs`) —
//! structurally present, functionally always empty. That's why the
//! user's Joiner never learns Founder has SEP.
//!
//! This module is the one place that actually reads live state:
//!   - `hosted_corpora` from `CorpusEngine::installed_indexes()`,
//!     respecting each index's `mesh_sharing` flag so a user who
//!     doesn't want to share a private corpus doesn't accidentally
//!     advertise it.
//!   - `hardware` from `commonwealth_discovery::hardware::detect_hardware()`.
//!   - `available` — best-effort live readings (free RAM, disk, CPU%).
//!
//! Called from `gossip::run_one_round` on every tick so our own
//! record reflects reality as it changes (a corpus finishes
//! installing, disk fills up, etc.).
use std::sync::Arc;

use commonwealth_core::capabilities::{
    AvailableResources, HardwareProfile, NodeCapabilities,
};
use commonwealth_core::knowledge::{ChunkRange as CoreChunkRange, CorpusShardInfo};
use commonwealth_discovery::hardware;
use corpus_engine::engine::CorpusEngine;

/// Build a fresh `NodeCapabilities` describing this node right now.
///
/// `engine` is optional: during a brief startup window — and for
/// test daemons — Sovereign hasn't wired a `CorpusEngine` into the
/// mesh daemon yet. In that case we return a still-valid
/// capabilities struct with empty `hosted_corpora` rather than
/// gating the whole gossip round on corpus availability. The
/// hardware profile is still populated so peers at least know our
/// shape before we publish our corpus inventory.
pub async fn build_local_capabilities(
    engine: Option<&Arc<CorpusEngine>>,
    now_secs: u64,
    inference_availability: f32,
) -> NodeCapabilities {
    let hardware = hardware::detect_hardware();
    let available = live_available_resources(&hardware);
    let hosted_corpora = match engine {
        Some(e) => build_hosted_corpora(e).await,
        None => Vec::new(),
    };
    NodeCapabilities {
        hardware,
        available,
        active_processes: Vec::new(), // sovereign-mesh doesn't spawn
                                      // llama-server / rpc-server —
                                      // that's the full Commonwealth
                                      // orchestrator's job. Empty is
                                      // the honest answer here.
        hosted_corpora,
        reported_at: now_secs,
        inference_availability,
        inference_capable: false,
        loaded_models: Vec::new(),
    }
}

/// Map each installed index to a `CorpusShardInfo` that peers can
/// route on.
///
/// We filter on `query_sharing`, NOT `mesh_sharing`. The two flags
/// are semantically distinct:
///
///   - `mesh_sharing=false` means "don't let peers replicate my
///     bytes" (gates the scheduler's `/internal/index/transfer`).
///   - `query_sharing=false` means "don't let peers run federated
///     searches against my copy" (gates THIS path — advertising in
///     `hosted_corpora`).
///
/// For SEP: `mesh_sharing=false, query_sharing=true` — the license
/// forbids redistribution, but returning cited snippets in response
/// to queries is fair use (what Google does). For a private
/// `codebase` corpus: both false.
///
/// `IndexInfo.query_sharing` is resolved at open-time — an index
/// whose on-disk meta predates this split falls back to
/// `mesh_sharing` automatically, preserving pre-split behavior.
async fn build_hosted_corpora(engine: &CorpusEngine) -> Vec<CorpusShardInfo> {
    match engine.installed_indexes().await {
        Ok(indexes) => indexes
            .into_iter()
            .filter(|idx| idx.query_sharing)
            .map(|idx| CorpusShardInfo {
                corpus_id: idx.corpus_id,
                // `corpus_engine::ChunkRange` and
                // `commonwealth_core::knowledge::ChunkRange` are
                // structurally identical but different types —
                // they live in different crates by design (the
                // engine doesn't know Commonwealth exists). Copy.
                chunk_range: idx.chunk_range.map(|r| CoreChunkRange {
                    start_id: r.start_id,
                    end_id: r.end_id,
                }),
                is_replica: idx.is_shard && idx.chunk_range.is_some(),
                last_updated: idx.last_updated,
            })
            .collect(),
        Err(e) => {
            // Don't break gossip over a transient filesystem read
            // error — just publish no corpora this round. The next
            // round will retry.
            tracing::warn!(error = %e, "capabilities: installed_indexes failed");
            Vec::new()
        }
    }
}

/// Best-effort live resource snapshot. Numbers drift between rounds
/// (that's the whole point — the scheduler wants to know when a node
/// suddenly has free VRAM) but individual samples are approximate.
fn live_available_resources(hw: &HardwareProfile) -> AvailableResources {
    let (cpu_util, free_ram_gb) = hardware::read_cpu_ram_state();
    let free_storage_gb = hardware::read_disk_state();

    // GPU state: NVIDIA is the only vendor we can query cheaply here
    // without spawning a heavier process — and when there are no
    // NVIDIA GPUs, `read_nvidia_gpu_state()` returns an empty vec,
    // which falls back to `hw.gpus[0].vram_gb` as the static best
    // guess. Better-than-zero for Metal/ROCm; full live readings
    // are the orchestrator's job if/when we need them here.
    let nvidia = hardware::read_nvidia_gpu_state();
    let (gpu_util, free_vram_gb) = if let Some((u, v)) = nvidia.first() {
        (*u, *v)
    } else {
        (
            0.0,
            hw.gpus.first().map(|g| g.vram_gb as f32).unwrap_or(0.0),
        )
    };

    AvailableResources {
        free_vram_gb,
        free_ram_gb,
        free_storage_gb,
        gpu_utilization: gpu_util,
        cpu_utilization: cpu_util,
        available_for_mesh: true,
    }
}
