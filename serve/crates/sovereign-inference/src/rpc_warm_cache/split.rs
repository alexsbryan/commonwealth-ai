// SPDX-License-Identifier: AGPL-3.0-or-later
//! The byte-aware block split a distributed load computes, as a function of
//! plain numbers: device capacities, llama.cpp's projected overheads, the
//! model's byte mass and the operator's pinned split. No device handle and no
//! FFI, so a planner that never links llama.cpp computes the same cut the
//! embedded load does.

use super::*;

/// The per-device block split for one model over one device set, in plan order
/// (RPC workers first, the host last).
///
/// `device_vram_bytes` is each device's capacity from ONE memory reading — the
/// same reading the fit gate judges against, so a device cannot be handed a
/// share it is then refused for. `overheads` is `None` when llama.cpp could not
/// project (judged weights-only); `mass` is `None` when the tensor table could
/// not be read (count split). `pinned_split` is the raw
/// `SOVEREIGN_RPC_BLOCK_SPLIT`; a malformed pin is warned and ignored. `model`
/// labels the trace events only.
pub fn plan_byte_aware_split(
    model: &str,
    n_layer: u32,
    device_vram_bytes: &[u64],
    overheads: Option<&PlanOverheads>,
    mass: Option<&ModelMass>,
    pinned_split: Option<&str>,
) -> Vec<NodeShard> {
    // Weight by advertised free VRAM (host RAM for a CPU-backed worker),
    // quantized to coarse buckets.
    //
    // Apportion against EFFECTIVE capacity: what remains after each
    // device's projected compute scratch and (pro-rata) KV share.
    // Without this, the split fills devices to their raw capacity and
    // the corrected fit gate below then refuses the very plan we just
    // computed — the fix would have turned "dies at first inference"
    // into "refuses to load". The KV share is apportioned by capacity
    // fraction as a first-order stand-in for the block fraction it
    // becomes; on a 1–2 GiB context total the circularity is not worth
    // an iteration loop.
    let total_capacity: u64 = device_vram_bytes.iter().sum();
    let effective = |i: usize, &b: &u64| -> u64 {
        let Some(o) = overheads else {
            return b;
        };
        let ctx_share = if total_capacity == 0 {
            0
        } else {
            (o.context_total_bytes as u128 * b as u128 / total_capacity as u128) as u64
        };
        let host_extra = if i + 1 == device_vram_bytes.len() {
            o.compute_host_bytes
        } else {
            0
        };
        b.saturating_sub(o.compute_accel_bytes + host_extra + ctx_share)
    };
    let weights: Vec<f32> = device_vram_bytes
        .iter()
        .enumerate()
        .map(|(i, b)| quantize_vram(effective(i, b)) as f32)
        .collect();
    // Byte-mass-aware split: overlay the model's REAL per-block byte mass
    // so a non-uniform (MoE / hybrid) model apportions by BYTES, not block
    // count — a count split can hand a small node a heavy contiguous run
    // and OOM it. On a header-read failure `mass` is `None` and we fall
    // back to the count split rather than wedge.
    //
    // Byte-mass apportionment is safe on hybrid (recurrent) models too,
    // PROVIDED the load also pins `tensor_split` to the plan (see
    // `DistributionPlan::tensor_split`). An earlier hybrid→count-based
    // forcing here was H3 of the 2026-07-27 crash hunt and was
    // FALSIFIED: count-based produced the identical 8/24 cut and the
    // identical abort, because llama.cpp's per-layer device map follows
    // tensor_split (default: advertised free VRAM over n_layer+1 units),
    // not our overrides — WHICH apportionment rule we use never decided
    // anything.
    let block_bytes: &[u64] = mass.map_or(&[], |m| m.block_bytes.as_slice());
    let head_bytes = mass.map_or(0, |m| m.head_bytes);
    if mass.is_some_and(|m| m.recurrent) {
        tracing::debug!(
            model,
            "plan_distribution: hybrid/recurrent model (ssm_* blocks) — \
             byte-mass split, dev_layer pinned via tensor_split"
        );
    }
    // Diagnostic override: `SOVEREIGN_RPC_BLOCK_SPLIT=11,21` pins the
    // per-device block counts (device order: RPC workers first, then
    // local), bypassing the VRAM-derived apportionment. The computed
    // split's cut points follow advertised VRAM, so without this there
    // is no way to AIM the device boundary at a chosen layer — which is
    // what separates "the distributed-decode fault follows a boundary
    // that lands on a Gated DeltaNet layer" from "any RPC boundary
    // faults". A malformed or non-tiling value is REFUSED (warned, then
    // ignored) rather than repaired: a run that silently used a
    // different split than the operator asked for would make the
    // experiment's result meaningless.
    let explicit =
        pinned_split.and_then(|raw| match parse_block_split(raw, n_layer, weights.len()) {
            Some(counts) => plan_shards_explicit(n_layer, &weights, &counts),
            None => {
                tracing::warn!(
                    raw = %raw,
                    n_layer,
                    devices = weights.len(),
                    "SOVEREIGN_RPC_BLOCK_SPLIT ignored — must be one count per device, \
                     summing to n_layer; falling back to the computed split"
                );
                None
            }
        });
    let computed = match explicit {
        Some(plan) => {
            tracing::warn!(
                blocks = ?plan.iter().map(|s| s.blocks).collect::<Vec<_>>(),
                "SOVEREIGN_RPC_BLOCK_SPLIT active — placement is PINNED by env, \
                 NOT derived from VRAM (diagnostic mode)"
            );
            plan
        }
        None => plan_shards_weighted(n_layer, &weights, block_bytes, head_bytes),
    };

    // Glassbox: log the resulting per-device byte balance so an operator
    // can see WHY each node got its range — and spot a residual overflow a
    // contiguous split can't avoid on a very skewed model. Same
    // `shard_fits` the gate judges on, at headroom 1.0, so the log
    // and the refusal can never describe different numbers.
    if let Some(m) = mass {
        let total_mass = m.block_bytes.iter().sum::<u64>() + m.head_bytes;
        if let Some(fits) = shard_fits(&computed, device_vram_bytes, m, 1.0, overheads) {
            for f in &fits {
                tracing::info!(
                    device = f.device_index,
                    blocks = ?computed.get(f.device_index).and_then(|s| s.blocks),
                    held_gb = f.held_bytes as f64 / 1.073_741_824e9,
                    overhead_gb = f.overhead_bytes as f64 / 1.073_741_824e9,
                    capacity_gb = f.capacity_bytes as f64 / 1.073_741_824e9,
                    share_pct = if total_mass > 0 { 100.0 * f.held_bytes as f64 / total_mass as f64 } else { 0.0 },
                    vram_weight = weights[f.device_index],
                    "plan_distribution: byte-mass shard"
                );
            }
        }
    }
    computed
}
