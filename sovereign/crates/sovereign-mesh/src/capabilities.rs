// SPDX-License-Identifier: AGPL-3.0-or-later
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
//! ## Storage-budget clamp
//!
//! When a per-node storage budget is configured (Settings → Knowledge
//! in the desktop app, or `POST /internal/storage/budget`), this
//! module also clamps the published `free_storage_gb` — on both the
//! static `HardwareProfile` and the live `AvailableResources` — to
//! `min(actual_free, max(0, budget_remaining))`. The schedulers in
//! `commonwealth-inference::scheduler::knowledge_assignment` already
//! drive every distribution decision off `free_storage_gb`, so
//! lowering this single number at the publish boundary makes the
//! budget self-enforcing across both local install paths and any
//! peer-driven shard distribution. There is no second knob to tune.
//!
//! Called from `gossip::run_one_round` on every tick so our own
//! record reflects reality as it changes (a corpus finishes
//! installing, disk fills up, etc.).
use std::sync::Arc;

use commonwealth_api::state::AppState;
use commonwealth_core::capabilities::{
    AnchorProfile, AvailableResources, HardwareProfile, NodeCapabilities,
};
use commonwealth_core::knowledge::{ChunkRange as CoreChunkRange, CorpusShardInfo};
use commonwealth_core::oicp::EmbedModelInfo;
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
///
/// `app_state` is optional too. When present (the production path),
/// the storage budget — if any — is read off it and used to clamp
/// `free_storage_gb`, and the freshly-summed corpus usage is written
/// back into the AppState atomic so `GET /internal/storage/budget`
/// can serve it. When `None` (older callers / tests that haven't
/// been updated), the no-clamp behaviour matches the original.
pub async fn build_local_capabilities(
    engine: Option<&Arc<CorpusEngine>>,
    now_secs: u64,
    inference_availability: f32,
    embed_model: Option<EmbedModelInfo>,
    app_state: Option<&AppState>,
) -> NodeCapabilities {
    let mut hardware = hardware::detect_hardware();
    // Advertise the ggml device total VRAM (authoritative, backend-agnostic)
    // when it exceeds what sysfs detected — the sysfs path under-reports
    // unified-memory AMD APUs (it sees only the dedicated carveout, e.g. 0.5 GB
    // on Strix Halo, while ggml reports the real ~128 GB pool). This is the
    // figure `svrn mesh plan --from-mesh` reads, so the number peers see must be
    // real. Add the shortfall to the first GPU so the advertised sum matches;
    // synthesize an entry if sysfs found no GPU at all (the APU-under-sysfs case).
    if let Some(ggml_total) = sovereign_inference::embedded::local_gpu_total_vram_gb() {
        let detected: u32 = hardware.gpus.iter().map(|g| g.vram_gb).sum();
        if ggml_total > detected {
            if let Some(g0) = hardware.gpus.first_mut() {
                g0.vram_gb += ggml_total - detected;
            } else {
                hardware
                    .gpus
                    .push(commonwealth_core::capabilities::GpuInfo {
                        name: "GPU".to_string(),
                        vram_gb: ggml_total,
                        compute_type: commonwealth_core::capabilities::ComputeType::Vulkan,
                        estimated_tflops: 0.0,
                    });
            }
        }
    }
    // Walk the engine once, share the result between
    // `build_hosted_corpora` (CorpusShardInfo set) and the storage-
    // usage calc (sum of `index_size_bytes`). Walking the index
    // directory twice per gossip tick is wasteful and would also
    // race — the second read could see a different set of corpora
    // than the first.
    let installed = match engine {
        Some(e) => match e.installed_indexes().await {
            Ok(idxs) => Some(idxs),
            Err(err) => {
                // Don't break gossip over a transient filesystem read
                // error — publish no corpora this round and treat
                // storage_used as 0 (so the budget clamp doesn't
                // accidentally cap publish to "no headroom" while we
                // can't even see the indexes). The next round retries.
                tracing::warn!(error = %err, "capabilities: installed_indexes failed");
                None
            }
        },
        None => None,
    };
    let storage_used_bytes: u64 = installed
        .as_deref()
        .map(|idxs| idxs.iter().map(|i| i.index_size_bytes).sum())
        .unwrap_or(0);
    let hosted_corpora = match (engine, installed.as_deref()) {
        (Some(e), Some(idxs)) => build_hosted_corpora(e, idxs).await,
        _ => Vec::new(),
    };

    // Apply the storage-budget clamp before live_available_resources
    // so both the static `HardwareProfile.free_storage_gb` and the
    // live `AvailableResources.free_storage_gb` see the same ceiling.
    // Schedulers read both depending on path; clamping just one would
    // leak unbounded capacity through whichever channel was missed.
    let budget_remaining = if let Some(state) = app_state {
        state.set_storage_used_bytes(storage_used_bytes);
        state.storage_remaining_bytes()
    } else {
        None
    };
    let actual_free_gb = hardware.free_storage_gb;
    if let Some(remaining) = budget_remaining {
        let remaining_gb = (remaining / 1_073_741_824) as u32;
        if remaining_gb < hardware.free_storage_gb {
            // debug!, not info!: the clamp is a steady state (the budget
            // is simply smaller than actual free), so this fired every
            // capability-advertise tick (~13s) — 2897 lines in 21h
            // (2026-07-18). The clamp still happens; only the per-tick
            // narration is quiet. `RUST_LOG=sovereign_mesh=debug` to see it.
            tracing::debug!(
                actual_free_gb,
                budget_remaining_gb = remaining_gb,
                used_gb = (storage_used_bytes / 1_073_741_824),
                "storage_budget: clamping published free_storage_gb to budget remaining"
            );
            hardware.free_storage_gb = remaining_gb;
        }
    }
    let available = live_available_resources(&hardware, budget_remaining);

    // Shared-model anchor tier. `SOVEREIGN_RPC_SERVE` is the role-derived
    // signal that this node lends a GPU shard to the layer-split — set by
    // `apply_shared_model_role_to_env` for the anchor/host roles (and by CLI
    // power users directly). Reading it here keeps the gossiped profile in
    // lock-step with what the RPC serve path actually does, without threading
    // `[shared_model]` config across the mesh boundary. Consumers (no serve
    // bind) advertise `None`. VRAM is the sum of detected device VRAM.
    let anchor = std::env::var_os("SOVEREIGN_RPC_SERVE").map(|_| AnchorProfile {
        can_anchor: true,
        vram_gb: hardware.gpus.iter().map(|g| g.vram_gb).sum(),
        model_resident: std::env::var("SOVEREIGN_SHARED_MODEL_ID")
            .ok()
            .filter(|s| !s.is_empty()),
    });

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
        // The collaborative-ingestion planner filters candidates by
        // exact match against this field. `None` means "don't include
        // me in distribution" — safe default for nodes that haven't
        // bootstrapped an embed slot yet.
        embed_model,
        // ALWAYS `None`, and not because this builder declines to set
        // it. There is no startup probe and no `with_benchmark`
        // setter; this comment used to describe both as though they
        // existed (`SCHEDULER_QUALITY.md` F10). Nothing on this mesh
        // has ever advertised a `BenchmarkResult`.
        //
        // The consequence is not cosmetic: `throughput_factor`
        // (`oicp-types/src/scoring.rs:362`) has two sources and this
        // shuts one of them, while the other — an observed decode EWMA
        // — is gated behind a sample count the ranked dispatch path
        // never accumulates for a peer. So it returns neutral 1.0 for
        // every peer, and the scheduler's only heterogeneity term is a
        // constant. §4.5 prices that at −32% mean latency on the one
        // simulated fleet containing a node slower than the 20 tok/s
        // reference, and 0% on five others.
        //
        // Before wiring this: read §4.5's verdict. The probe that used
        // to be the obvious source, `run_baseline_benchmark`, measured
        // the `Speed::Fast` slot, so filling this field from it would
        // advertise a ~2.5 GB model's rate and leave
        // `throughput_factor` extrapolating up to a 21 GB one on a
        // linear size law that is false — measured as a quality
        // regression wearing a large latency win. It was deleted on
        // 2026-07-28 rather than left as an invitation.
        //
        // `svrn mesh bench` is the honest producer, and it deliberately
        // does not write here. Its number describes the model actually
        // being served, and its consumer is a human deciding whether to
        // add a machine — not the ranked dispatch, whose
        // `throughput_factor` would immediately extrapolate away from
        // it through a ≤20 tok/s clamp. Aiming a real measurement at
        // the wrong consumer ships §4.5's regression with no other code
        // change.
        benchmark: None,
        // Current local in-flight count for load-aware scheduling.
        // Read directly from the MIP-shared atomic via AppState:
        // lock-free, fresh as of *this gossip tick*. `None` when the
        // bootstrap hasn't installed a publisher yet (storage-only
        // nodes, tests). See `sovereign/docs/MESH_LOAD_AWARENESS.md`
        // for why this field exists and why gossiping it is the only
        // way the founder learns mac-peer is busy serving local
        // traffic the founder never dispatched.
        current_in_flight: app_state.and_then(|s| s.current_local_in_flight()),
        anchor,
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
async fn build_hosted_corpora(
    engine: &CorpusEngine,
    indexes: &[corpus_engine::IndexInfo],
) -> Vec<CorpusShardInfo> {
    let indexes_dir = engine.index_dir().to_path_buf();
    indexes
        .iter()
        .filter(|&idx| idx.query_sharing)
        .cloned()
        .map(|idx| {
            // Phase C1: read the atlas summary if the corpus
            // has one. The summary helper caches by atoms.json
            // mtime so this is O(1) read per gossip round
            // unless the atlas just changed.
            let atlas_dir = indexes_dir.join(&idx.corpus_id).join("atlas");
            let summary =
                corpus_engine::enrichment::atlas::read_or_compute_atlas_summary(&atlas_dir)
                    .ok()
                    .flatten();
            let (atom_count, tier2_count, fingerprint) = match summary {
                Some(s) => (s.atom_count, s.tier2_count, Some(s.fingerprint)),
                None => (0, 0, None),
            };
            CorpusShardInfo {
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
                // Phase 6 canonical-sync surface: chunk_count,
                // canonical_fingerprint, total_shards, processed_shards.
                // Peers compute coverage_ratio() on this struct to
                // decide which canonical to pull when several mirror
                // the same corpus.
                chunk_count: idx.chunk_count,
                canonical_fingerprint: idx.canonical_fingerprint,
                total_shards: idx.total_shards,
                processed_shards: idx.processed_shards,
                // Phase C1 — atlas advertisement.
                atlas_atom_count: atom_count,
                atlas_tier2_count: tier2_count,
                atlas_fingerprint: fingerprint,
            }
        })
        .collect()
}

/// Best-effort live resource snapshot. Numbers drift between rounds
/// (that's the whole point — the scheduler wants to know when a node
/// suddenly has free VRAM) but individual samples are approximate.
///
/// `budget_remaining_bytes` is `Some` whenever the operator has set a
/// storage budget; we clamp the live `free_storage_gb` reading down
/// to it so live readings stay consistent with the static
/// `HardwareProfile.free_storage_gb` clamp the caller already
/// applied. Without this second clamp, the schedulers that reach for
/// `AvailableResources.free_storage_gb` would see uncapped capacity
/// even though the budget is set.
fn live_available_resources(
    hw: &HardwareProfile,
    budget_remaining_bytes: Option<u64>,
) -> AvailableResources {
    let (cpu_util, free_ram_gb) = hardware::read_cpu_ram_state();
    let mut free_storage_gb = hardware::read_disk_state();
    if let Some(remaining) = budget_remaining_bytes {
        let remaining_gb = (remaining / 1_073_741_824) as f32;
        if remaining_gb < free_storage_gb {
            free_storage_gb = remaining_gb;
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_available_resources_clamps_free_storage_when_budget_lower() {
        let hw = HardwareProfile {
            gpus: vec![],
            system_ram_gb: 16,
            cpu_cores: 4,
            total_storage_gb: 500,
            free_storage_gb: 0, // unused by this fn
            network_bandwidth_mbps: None,
        };
        // 1 GiB remaining — well below whatever real disk reports.
        let avail = live_available_resources(&hw, Some(1_073_741_824));
        assert!(
            avail.free_storage_gb <= 1.0,
            "budget remaining of 1 GiB must clamp live free_storage_gb to ≤ 1, got {}",
            avail.free_storage_gb
        );
    }

    #[test]
    fn live_available_resources_no_clamp_when_budget_unset() {
        let hw = HardwareProfile {
            gpus: vec![],
            system_ram_gb: 16,
            cpu_cores: 4,
            total_storage_gb: 500,
            free_storage_gb: 0,
            network_bandwidth_mbps: None,
        };
        let unclamped = live_available_resources(&hw, None);
        // We can't pin an exact value (depends on the host), but it
        // must be > 0 on any developer machine running these tests
        // and not magically pulled to 0 by a phantom clamp.
        assert!(unclamped.free_storage_gb > 0.0);
    }

    #[test]
    fn live_available_resources_no_clamp_when_budget_higher_than_disk() {
        let hw = HardwareProfile {
            gpus: vec![],
            system_ram_gb: 16,
            cpu_cores: 4,
            total_storage_gb: 500,
            free_storage_gb: 0,
            network_bandwidth_mbps: None,
        };
        // 1 EB — far larger than any real free disk; clamp is a no-op.
        let avail = live_available_resources(&hw, Some(u64::MAX / 2));
        assert!(avail.free_storage_gb > 0.0);
    }

    /// Gossip must never advertise a `BenchmarkResult`. This is a
    /// behavioural guard, not a style preference, and it is cheap
    /// insurance against a very expensive mistake.
    ///
    /// `throughput_factor` (`oicp-types/src/scoring.rs:362`) takes the
    /// advertised benchmark and, absent enough observed samples,
    /// extrapolates it onto whatever candidate is being scored by a
    /// linear size ratio (`scoring.rs:384`):
    ///
    /// ```text
    /// bench.tg_tok_s * (bench.baseline_size_gb / candidate_size_gb)
    /// ```
    ///
    /// Its own comment concedes real scaling is sub-linear because
    /// memory bandwidth dominates. `SCHEDULER_QUALITY.md` §4.5 priced
    /// that extrapolation against the simulator and filed it
    /// **DO-NOT-BUILD**: reading −56%, with declined upgrades doubling
    /// and downgrades appearing.
    ///
    /// The trap is that arming it takes no new code. The consumer is
    /// already wired end to end; the only thing holding it back is that
    /// this builder leaves the field `None`. Set it here — from any
    /// probe, however well intentioned — and the regression ships on
    /// the next gossip tick with nothing else changed and no review
    /// step that would obviously catch it.
    ///
    /// A measured throughput number is a legitimate goal, and the
    /// capability-oracle work exists to produce one. It carries its own
    /// type in `sovereign-core::mesh_measurements`, keyed to the exact
    /// model and split it was measured on, and it reaches a human
    /// reading `svrn mesh plan` — not the scheduler's ranked dispatch,
    /// which is the consumer §4.5 measured. Route it through here and
    /// you have silently converted a measurement into an extrapolation.
    #[tokio::test]
    async fn gossip_never_advertises_a_benchmark() {
        let caps = build_local_capabilities(None, 0, 1.0, None, None).await;
        assert!(
            caps.benchmark.is_none(),
            "build_local_capabilities set NodeCapabilities.benchmark. That arms the \
             size-ratio extrapolation at oicp-types/src/scoring.rs:384, which \
             SCHEDULER_QUALITY.md §4.5 measured at −56% and filed DO-NOT-BUILD. If you \
             have a real measurement, it belongs in sovereign-core::mesh_measurements \
             keyed to the model and split it was taken on — not on the gossip path."
        );
    }
}
