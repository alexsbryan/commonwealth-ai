// SPDX-License-Identifier: AGPL-3.0-or-later
//! The placement plan: which contiguous block range each device holds, and
//! whether a plan fits the devices it names.

use super::*;

/// One device's shard under our placement plan: the contiguous block range it
/// holds, whether it owns the output head, and its normalized fraction.
/// `device_index` is the position in the RPC-first device list (matching
/// `with_devices` / `SOVEREIGN_RPC_TENSOR_SPLIT`). The input embedding always
/// stays on the host CPU (llama.cpp does this unconditionally) so it's never a shard.
///
/// Serializable: the host computes the plan ONCE and ships it whole to each
/// worker (the auto-warm orchestration), so warm-time placement and load-time
/// `-ot` overrides derive from the identical plan and cannot diverge — the
/// plan-agreement invariant that keeps every weight a cache hit (no bulk send).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NodeShard {
    pub device_index: usize,
    /// Inclusive `(first, last)` block range, or `None` if this device got no blocks.
    pub blocks: Option<(u32, u32)>,
    /// Owns the output head (`output.weight`).
    pub holds_output: bool,
    /// Normalized weight fraction.
    pub fraction: f32,
}

/// Round a device's free-VRAM weight to a coarse bucket (4 GiB) so transient
/// fluctuation can't change the shard apportionment when the device set is
/// unchanged. A belt alongside the per-device-set plan cache: small allocator
/// churn between reloads stays in-bucket → `plan_shards` returns the same split →
/// workers' warm caches stay valid. A nonzero-but-tiny device still gets one
/// bucket so it isn't apportioned to zero. Pure.
pub fn quantize_vram(bytes: u64) -> u64 {
    const BUCKET: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB
    if bytes == 0 {
        return 0;
    }
    (((bytes + BUCKET / 2) / BUCKET).max(1)) * BUCKET
}

/// Our OWN contiguous placement policy: apportion `n_layer` transformer blocks
/// across devices proportional to `weights` (RPC-first order) BY COUNT, with
/// exact integer counts via largest-remainder so they sum to `n_layer`. The
/// output head goes on the last block-holding device. We do NOT predict
/// llama.cpp's split — we ENFORCE this assignment via `tensor_buft_overrides` at
/// load and warm the same assignment, so there is nothing to diverge from.
///
/// Count-proportional is only byte-balanced when every block has ~equal mass (a
/// dense model, or a uniform all-MoE stack). For NON-UNIFORM models — a hybrid
/// SSM+MoE (a light attention/SSM block next to a 60x heavier MoE block) or a
/// DeepSeek-style leading-dense stack — block COUNT is a poor proxy for bytes,
/// so a count split can hand a small node a heavy contiguous run and OOM it.
/// [`plan_shards_weighted`] apportions by real byte mass instead; this is its
/// `block_bytes == []` case. Pure + deterministic.
pub fn plan_shards(n_layer: u32, weights: &[f32]) -> Vec<NodeShard> {
    plan_shards_weighted(n_layer, weights, &[], 0)
}

/// Byte-mass-aware contiguous placement — the split the live load needs on
/// NON-UNIFORM (MoE / hybrid) models. `block_bytes[i]` is block `i`'s resident
/// weight in bytes (len must equal `n_layer`); `head_bytes` is the output-head
/// mass, which rides on the last block-holding device. Each device's resident
/// bytes come out proportional to its `weights` entry — so a 64 GB node and a
/// 32 GB node hold ~2:1 of the *mass*, not of the block *count*. The head is
/// folded in: the head-holder is handed a smaller block budget (its share minus
/// the head it already carries) so it doesn't run ~`head_bytes` heavy.
///
/// Ranges stay CONTIGUOUS, so hops stay `D-1` and `tensor_device` /
/// `override_patterns` are unchanged — only the cut points move. Falls back to
/// the count-based apportionment of [`plan_shards`] when `block_bytes` is empty
/// or all-zero (a uniform model, or a caller with no size table). Note that when
/// a single block's mass exceeds a small node's whole share, no CONTIGUOUS split
/// can keep that node within budget — the caller (`mesh plan`) surfaces the
/// residual per-device overflow; splitting a block's experts off is a separate,
/// heavier lever we deliberately don't take here. Pure + deterministic.
pub fn plan_shards_weighted(
    n_layer: u32,
    weights: &[f32],
    block_bytes: &[u64],
    head_bytes: u64,
) -> Vec<NodeShard> {
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }
    // Effective device weights (all-zero → equal share).
    let wsum_f32: f32 = weights.iter().sum();
    let w: Vec<f64> = if wsum_f32 > 0.0 {
        weights.iter().map(|&x| x as f64).collect()
    } else {
        vec![1.0; n]
    };
    let wsum: f64 = w.iter().sum::<f64>().max(f64::MIN_POSITIVE);

    let have_mass = block_bytes.len() == n_layer as usize && block_bytes.iter().any(|&b| b > 0);

    let counts: Vec<u32> = if !have_mass {
        // ── Count-based (largest-remainder) apportionment ──
        let ideal: Vec<f64> = w.iter().map(|&wd| wd / wsum * n_layer as f64).collect();
        let mut counts: Vec<u32> = ideal.iter().map(|x| x.floor() as u32).collect();
        let mut remainder = n_layer.saturating_sub(counts.iter().sum());
        let mut by_frac: Vec<usize> = (0..n).collect();
        by_frac.sort_by(|&a, &b| {
            let (fa, fb) = (ideal[a] - ideal[a].floor(), ideal[b] - ideal[b].floor());
            fb.partial_cmp(&fa).unwrap_or(std::cmp::Ordering::Equal)
        });
        for &d in &by_frac {
            if remainder == 0 {
                break;
            }
            counts[d] += 1;
            remainder -= 1;
        }
        counts
    } else {
        // ── Byte-mass-aware apportionment: contiguous cuts by cumulative bytes ──
        let total_block: u64 = block_bytes.iter().sum();
        let total_mass = total_block as f64 + head_bytes as f64;
        // The head lands on the last device with weight > 0 (device order is
        // RPC-workers-first, host last; the host holds the head).
        let head_dev = (0..n).rev().find(|&d| w[d] > 0.0);
        let mut tgt_block: Vec<f64> = w.iter().map(|&wd| wd / wsum * total_mass).collect();
        if let Some(hd) = head_dev {
            tgt_block[hd] = (tgt_block[hd] - head_bytes as f64).max(0.0);
        }
        // prefix[i] = Σ block_bytes[0..i]; prefix[n_layer] = total_block.
        let mut prefix = vec![0u64; n_layer as usize + 1];
        for i in 0..n_layer as usize {
            prefix[i + 1] = prefix[i] + block_bytes[i];
        }
        let mut counts = vec![0u32; n];
        let mut cum_tgt = 0.0f64;
        let mut prev_cut = 0u32;
        for d in 0..n {
            cum_tgt += tgt_block[d];
            // Last device takes the remainder so the cuts always tile [0, n_layer)
            // with no rounding gap.
            let cut = if d == n - 1 {
                n_layer
            } else {
                closest_boundary(&prefix, cum_tgt, prev_cut, n_layer).max(prev_cut)
            };
            counts[d] = cut - prev_cut;
            prev_cut = cut;
        }
        counts
    };

    build_shards_from_counts(&counts, &w)
}

/// Smallest boundary `b` in `[lo, hi]` whose `prefix[b]` is closest to `target`.
/// `prefix` is non-decreasing, so we advance while it improves and stop once we
/// pass the target without improving. Deterministic; ties resolve to the smaller
/// `b` (the earlier cut). Used only by the byte-aware apportionment.
fn closest_boundary(prefix: &[u64], target: f64, lo: u32, hi: u32) -> u32 {
    let mut best = lo;
    let mut best_d = (prefix[lo as usize] as f64 - target).abs();
    let mut b = lo + 1;
    while b <= hi {
        let cur = prefix[b as usize] as f64;
        let d = (cur - target).abs();
        if d < best_d {
            best_d = d;
            best = b;
        } else if cur >= target {
            break; // past the target and not improving → monotonic, stop
        }
        b += 1;
    }
    best
}

/// Turn per-device contiguous block `counts` into the [`NodeShard`] plan: assign
/// each device its next contiguous range, put the output head on the last device
/// that got any blocks, and record its normalized weight fraction. Shared by both
/// apportionment paths so they can't diverge on shard shape.
fn build_shards_from_counts(counts: &[u32], eff_weights: &[f64]) -> Vec<NodeShard> {
    let n = counts.len();
    let wsum: f64 = eff_weights.iter().sum::<f64>().max(f64::MIN_POSITIVE);
    let last_with_blocks = (0..n).rev().find(|&d| counts[d] > 0);
    let mut cur = 0u32;
    (0..n)
        .map(|d| {
            let blocks = if counts[d] > 0 {
                let b = (cur, cur + counts[d] - 1);
                cur += counts[d];
                Some(b)
            } else {
                None
            };
            NodeShard {
                device_index: d,
                blocks,
                holds_output: Some(d) == last_with_blocks,
                fraction: (eff_weights[d] / wsum) as f32,
            }
        })
        .collect()
}

/// Parse an EXPLICIT per-device block count list (e.g. `"11,21"` for a 32-layer
/// model across two devices), in the same device order as `weights` — RPC
/// workers first, then local. `None` unless the list is well-formed AND tiles
/// the model exactly: right number of devices, counts summing to `n_layer`.
///
/// This exists because the byte-mass apportionment above derives its cut points
/// from advertised VRAM, so there is otherwise NO way to aim the device boundary
/// at a chosen layer. Aiming it is the only way to run the discriminating
/// experiment for the distributed-decode crash: on a hybrid model like
/// Qwen3.5-4B (32 layers, 3:1 — layers 3/7/11/…/31 are full attention, the rest
/// Gated DeltaNet), the fault appears at a boundary that lands immediately
/// before a Gated DeltaNet layer, and `resolve_fused_ops` disables the fused GDN
/// kernel at exactly that layer. Moving the cut onto an attention layer and
/// watching whether the fault follows separates "boundary-on-GDN" from "any
/// boundary at all" in one run each.
///
/// Rejecting a malformed list rather than repairing it is deliberate: a split
/// that silently differs from what the operator asked for would make the
/// experiment's negative result meaningless.
pub fn parse_block_split(raw: &str, n_layer: u32, n_devices: usize) -> Option<Vec<u32>> {
    let counts: Vec<u32> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<u32>().ok())
        .collect::<Option<Vec<u32>>>()?;
    if counts.len() != n_devices || counts.iter().sum::<u32>() != n_layer {
        return None;
    }
    Some(counts)
}

/// Build a plan from EXPLICIT per-device block counts, bypassing the byte-mass
/// apportionment. Shares `build_shards_from_counts` with the computed paths, so
/// `holds_output` placement, `fraction`, and contiguity are identical — only the
/// cut points differ. `None` if `counts` doesn't tile `n_layer` across
/// `weights`. Pure + deterministic.
pub fn plan_shards_explicit(
    n_layer: u32,
    weights: &[f32],
    counts: &[u32],
) -> Option<Vec<NodeShard>> {
    if counts.len() != weights.len() || counts.iter().sum::<u32>() != n_layer {
        return None;
    }
    let wsum_f32: f32 = weights.iter().sum();
    let w: Vec<f64> = if wsum_f32 > 0.0 {
        weights.iter().map(|&x| x as f64).collect()
    } else {
        vec![1.0; weights.len()]
    };
    Some(build_shards_from_counts(counts, &w))
}

// ───────────────────────────────────────────────────────────────────────────
// Does the plan actually FIT? — the shared decider
//
// `plan_shards_weighted` decides where each block GOES. Nothing above decides
// whether the device it goes to can HOLD it, and until 2026-07-28 nothing did:
// the live load gated only on POOLED memory, so a plan whose aggregate fit
// comfortably could still hand one worker more than it had. `mesh plan` grew
// the per-device check first, in its own private fold, which meant the preview
// and the load could disagree — the exact drift the preview exists to close.
//
// So the check lives here, beside the planner, and both callers use it.
// ───────────────────────────────────────────────────────────────────────────

/// A model's resident byte mass, decomposed the way a placement decision needs
/// it.
///
/// Read from the GGUF's tensor table — a header parse, no weight load — so this
/// is cheap enough to compute on every plan. The decomposition matters because
/// the pieces land in different places: block mass follows the shard split, the
/// output head rides with the last block-holder, and `token_embd` stays in host
/// RAM rather than on any device.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelMass {
    /// Per-block resident bytes, indexed by block number. Empty when the tensor
    /// table could not be read — see [`ModelMass::is_known`].
    pub block_bytes: Vec<u64>,
    /// Output head (`output.weight`) bytes. Rides with the last block-holder.
    pub head_bytes: u64,
    /// Input embedding (`token_embd`) bytes. Stays in host system RAM and is
    /// therefore NOT charged to any device's share.
    pub embd_bytes: u64,
    /// Global tensors that are neither head nor embedding, plus any tensor
    /// tagged with an out-of-range layer. Host overhead.
    pub other_global_bytes: u64,
    /// Routed-expert (`_exps`) mass — the COLD part of a mixture-of-experts
    /// model. Resident, so it counts against fit; only the router's top-k are
    /// read per token, so it is a small fraction of the per-token work.
    pub routed_expert_bytes: u64,
    /// The model carries `ssm_*` weights — a hybrid (Gated DeltaNet) stack.
    pub recurrent: bool,
}

impl ModelMass {
    /// Every resident byte, including the pieces that live in host RAM.
    pub fn total_bytes(&self) -> u64 {
        self.block_bytes.iter().sum::<u64>()
            + self.head_bytes
            + self.embd_bytes
            + self.other_global_bytes
    }

    /// Whether this mass can support a fit judgment.
    ///
    /// An all-zero block table means the tensor table was unreadable. The
    /// distinction is load-bearing: a fit check run against zeros would pass
    /// every device trivially, turning a failed header read into a clean bill
    /// of health.
    pub fn is_known(&self) -> bool {
        self.block_bytes.iter().any(|&b| b > 0)
    }
}

/// Decompose a GGUF tensor table into a [`ModelMass`].
///
/// `sizes` is the `(tensor_name, layer, nbytes)` table `tensor_sizes` returns.
/// `n_layer` is the header's block count; a tensor tagged with a layer outside
/// `0..n_layer` is counted as a global rather than silently dropped, because a
/// dropped byte is a byte the fit check will not charge anyone for. Pure.
pub fn model_mass_from_sizes(sizes: &[(String, Option<u32>, u64)], n_layer: u32) -> ModelMass {
    let mut m = ModelMass {
        block_bytes: vec![0u64; n_layer as usize],
        ..Default::default()
    };
    for (name, layer, nbytes) in sizes {
        if name.contains(".ssm_") {
            m.recurrent = true;
        }
        if is_routed_expert_tensor(name) {
            m.routed_expert_bytes += *nbytes;
        }
        match layer {
            Some(l) if (*l as usize) < m.block_bytes.len() => m.block_bytes[*l as usize] += *nbytes,
            Some(_) => m.other_global_bytes += *nbytes,
            None if is_output_tensor(name) => m.head_bytes += *nbytes,
            None if name.contains("token_embd") => m.embd_bytes += *nbytes,
            None => m.other_global_bytes += *nbytes,
        }
    }
    m
}

/// One device's share of a plan, weighed against what that device has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShardFit {
    /// Index into the plan's device order (RPC workers first, host last) — the
    /// same index [`NodeShard::device_index`] carries. A caller displaying this
    /// against its own device numbering must map back through the order it
    /// passed in; the two spaces are easy to confuse and nothing here can catch
    /// it for you.
    pub device_index: usize,
    /// Resident bytes this device would hold: its block range plus the output
    /// head if it carries it.
    pub held_bytes: u64,
    /// Projected non-weight bytes (KV share + compute scratch) this device
    /// needs on top of the weights — `0` when no [`PlanOverheads`] was
    /// supplied and the multiplicative headroom is the only margin.
    pub overhead_bytes: u64,
    /// `held_bytes × headroom + overhead_bytes` — what must actually fit.
    pub need_bytes: u64,
    /// What this device has.
    pub capacity_bytes: u64,
}

/// Non-weight memory a distributed load needs, projected by llama.cpp's own
/// three-term accountant (`llama_cpp_4::fit::get_device_memory_data` →
/// `common_device_memory_collect`) rather than guessed from the weights.
///
/// Exists because the fit gate used to charge devices for weights only: the
/// 2026-08-02 loads passed `shard_fits` with a 0.07 GiB margin, reached
/// `serving`, and died allocating KV + compute buffers on the first inference.
/// The opposite error was live on the host side, where a `weights/8` KV proxy
/// over-charged an MLA model ~5× and refused loads that fit. The projection
/// measured ~278 ms warm on a 155 GB sharded GGUF (`no_alloc` load, freed
/// before returning — tests/device_memory_probe.rs), so it can sit in the
/// plan path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanOverheads {
    /// KV / recurrent-cache bytes for the WHOLE model at the load's context
    /// size. Charged to each device in proportion to the blocks it holds.
    pub context_total_bytes: u64,
    /// Compute-scratch bytes per accelerator device (the largest projected
    /// per-device compute buffer). Charged to every device in the plan: each
    /// backend reserves its own graph workspace.
    pub compute_accel_bytes: u64,
    /// The host CPU buffer's compute scratch — the scheduler-side workspace
    /// that only exists in the process driving the graph. Charged to the plan's
    /// LAST device (plan order is RPC workers first, host last).
    pub compute_host_bytes: u64,
    /// Model WEIGHT bytes llama.cpp projects onto the host CPU buffer, i.e.
    /// tensors it will leave resident in the mmap rather than copy into a
    /// device buffer. These are file-backed and reclaimable, so they are NOT
    /// demand the local-fit gate should charge against system memory.
    ///
    /// ONLY trustworthy when the load sets `no_host`. Without it the host
    /// entry is the device's PINNED host buffer (`Vulkan_Host`), whose pages
    /// are not reclaimable — discounting those would under-charge the gate by
    /// exactly the bytes that can starve the host. See `gate_local`.
    pub model_host_bytes: u64,
}

impl PlanOverheads {
    /// Non-weight bytes device `idx` of `n_devices` needs when holding
    /// `blocks` of `total_blocks`.
    pub fn device_bytes(
        &self,
        idx: usize,
        n_devices: usize,
        blocks: u64,
        total_blocks: u64,
    ) -> u64 {
        let ctx = if total_blocks == 0 {
            0
        } else {
            (self.context_total_bytes as u128 * blocks as u128 / total_blocks as u128) as u64
        };
        let host_extra = if idx + 1 == n_devices {
            self.compute_host_bytes
        } else {
            0
        };
        ctx + self.compute_accel_bytes + host_extra
    }
}

impl ShardFit {
    /// Whether this device can hold its share with headroom.
    pub fn fits(&self) -> bool {
        self.need_bytes <= self.capacity_bytes
    }

    /// Spare room in bytes; negative when this device overflows.
    pub fn slack_bytes(&self) -> i128 {
        self.capacity_bytes as i128 - self.need_bytes as i128
    }
}

/// Judge a shard plan device by device.
///
/// Returns **one row per shard**, in plan order, fitting and overflowing alike —
/// not a pass/fail. A `Result<(), Overflow>` shape would force every caller that
/// wants to *show* the fit (which `mesh plan` does, `ok +12.4 GB` per row) to
/// keep its own traversal, and a second traversal is exactly the drift this
/// function exists to remove.
///
/// `capacities` is in **plan order** — the same order `plan_shards_weighted`'s
/// `weights` were in, RPC workers first and the host last. Passing it in the
/// caller's own device numbering silently mis-attributes every row.
///
/// `None` means **"cannot judge"**, and it is not a pass. It is returned when
/// the inputs do not describe each other — a capacity list of the wrong length,
/// an empty plan, a nonsensical headroom — or when [`ModelMass::is_known`] is
/// false, because judging against an unread tensor table would clear every
/// device on the strength of zeros. A caller that gets `None` must say so; it
/// must not report a fit.
pub fn shard_fits(
    plan: &[NodeShard],
    capacities: &[u64],
    mass: &ModelMass,
    headroom: f64,
    overheads: Option<&PlanOverheads>,
) -> Option<Vec<ShardFit>> {
    if plan.is_empty() || capacities.len() != plan.len() {
        return None;
    }
    if !headroom.is_finite() || headroom < 1.0 {
        return None;
    }
    if !mass.is_known() {
        return None;
    }
    let total_blocks = mass.block_bytes.len() as u64;
    let mut out = Vec::with_capacity(plan.len());
    for (pos, shard) in plan.iter().enumerate() {
        // The plan's own `device_index` is the authority on which capacity a
        // shard is judged against; position is only a fallback for a
        // hand-built plan that did not set it.
        let idx = if shard.device_index < capacities.len() {
            shard.device_index
        } else {
            pos
        };
        let mut held = 0u64;
        let mut blocks = 0u64;
        if let Some((a, b)) = shard.blocks {
            blocks = (b - a + 1) as u64;
            for blk in a..=b {
                // A range past the end of the table means the plan and the mass
                // describe different models. Refuse rather than under-charge.
                held = held.checked_add(*mass.block_bytes.get(blk as usize)?)?;
            }
        }
        if shard.holds_output {
            held += mass.head_bytes;
        }
        // With a projection, need is what the load will actually allocate:
        // weights (× the operator's safety headroom) plus this device's KV
        // share and compute scratch. Without one, headroom alone stands in for
        // the missing terms — the pre-2026-08-02 behaviour, kept as the
        // fallback so a failed projection can never brick a load.
        let overhead = overheads
            .map(|o| o.device_bytes(idx, capacities.len(), blocks, total_blocks))
            .unwrap_or(0);
        out.push(ShardFit {
            device_index: idx,
            held_bytes: held,
            overhead_bytes: overhead,
            need_bytes: (held as f64 * headroom) as u64 + overhead,
            capacity_bytes: capacities[idx],
        });
    }
    Some(out)
}

/// The cacheable output head (LM projection) — placed with the last block-holding
/// device, distinct from `token_embd` (input) which stays on the host CPU.
pub fn is_output_tensor(name: &str) -> bool {
    name == "output.weight"
}

/// A routed-expert weight tensor — `blk.N.ffn_{gate,up,down}_exps.weight`, the
/// `_exps` fused-expert stack. This is the COLD mass of an MoE model: only the
/// router's top-k experts are read per token, so it can be ~90% of the bytes yet
/// a small fraction of the per-token work. Distinct from the SHARED expert
/// (`_shexp`, run every token → hot) and the router (`ffn_gate_inp` → hot). Used
/// by `mesh plan` to report hot/cold mass; the split itself stays per-block
/// (a whole block, experts included, on one device) so single-stream decode
/// keeps its `D-1` hops rather than scattering a layer's experts across nodes.
pub fn is_routed_expert_tensor(name: &str) -> bool {
    name.contains("_exps.weight")
}

/// Which device a tensor lands on under `plan`, or `None` if it stays on the host
/// CPU (input embedding / non-block, non-output tensors). A worker warms a tensor
/// iff `tensor_device(...) == Some(its index)` — the single source of truth shared
/// by warming and the load-time overrides, so they cannot disagree.
pub fn tensor_device(name: &str, layer: Option<u32>, plan: &[NodeShard]) -> Option<usize> {
    match layer {
        Some(l) => plan
            .iter()
            .find(|s| s.blocks.is_some_and(|(a, b)| l >= a && l <= b))
            .map(|s| s.device_index),
        None if is_output_tensor(name) => {
            plan.iter().find(|s| s.holds_output).map(|s| s.device_index)
        }
        None => None,
    }
}

/// Warm `cache_dir` with exactly the tensors device `device_index` is assigned
/// under `plan` — the per-node entry point. A worker calls this for its own
/// device; it materializes only its shard (≈ size/N), never the whole model.
pub fn warm_cache_for_device(
    gguf_path: &Path,
    cache_dir: &Path,
    plan: &[NodeShard],
    device_index: usize,
) -> std::io::Result<WarmCacheStats> {
    warm_cache_slice(gguf_path, cache_dir, |name, layer| {
        tensor_device(name, layer, plan) == Some(device_index)
    })
}

/// Build the `-ot` regex overrides that ENFORCE `plan` at load: one
/// `^blk\.(L|…|M)\.` per block-holding device, plus `^output\.weight` on the output
/// owner. Returns `(regex, device_index)`; the caller resolves each `device_index`
/// to that device's `ggml_backend_dev_buffer_type`. `token_embd` gets no override
/// (llama.cpp keeps the input embedding on the host CPU).
pub fn override_patterns(plan: &[NodeShard]) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for s in plan {
        if let Some((l, m)) = s.blocks {
            let nums: Vec<String> = (l..=m).map(|i| i.to_string()).collect();
            out.push((format!("^blk\\.({})\\.", nums.join("|")), s.device_index));
        }
    }
    if let Some(s) = plan.iter().find(|s| s.holds_output) {
        out.push(("^output\\.weight".to_string(), s.device_index));
    }
    out
}

/// The `tensor_split` that makes llama.cpp's PER-LAYER device map agree with
/// `plan` — required alongside the `-ot` overrides, not optional.
///
/// llama.cpp keeps two placement systems that never talk to each other. Our
/// `-ot` overrides place the WEIGHTS; `model.dev_layer(il)` — which
/// `resolve_fused_ops` consults to decide whether fused kernels stay enabled —
/// is computed at load from `tensor_split` (advertised free memory when unset)
/// over **`n_layer + 1` units**, the output head counting as the extra unit
/// (llama-model.cpp: `act_gpu_layers = min(n_gpu_layers, n_layer + 1)`, then
/// `upper_bound(splits, il / act_gpu_layers)`). Overrides are never consulted.
/// Left unpinned, dev_layer's boundary can straddle our cut by one layer; on a
/// hybrid (Gated DeltaNet) model one straddled layer disables the fused GDN
/// kernel globally, the unfused path emits `GGML_OP_SET`, and the first
/// distributed decode kills the host at ggml-rpc.cpp:498
/// (docs/internal/DISTRIBUTED_GDN_CRASH_STATUS.md).
///
/// So hand llama.cpp cut points that sit HALFWAY between our block boundaries
/// in `(n_layer + 1)`-unit space: a device whose last block is `b` gets its
/// upper cut at `b + 0.5` — strictly between layer `b` and layer `b + 1`, so no
/// float tie can flip a layer (a cut at exactly `b + 1` would tie against
/// `upper_bound`'s strict `>`). The output-holding device's cut extends to
/// `n_layer + 1`, landing the output unit exactly where [`override_patterns`]
/// pins `output.weight`. Returns per-device weights in plan device order
/// (llama.cpp normalizes internally); blockless devices get zero width. Pure.
pub fn dev_layer_tensor_split(plan: &[NodeShard], n_layer: u32) -> Vec<f32> {
    let n_dev = plan.iter().map(|s| s.device_index + 1).max().unwrap_or(0);
    let mut out = vec![0.0f32; n_dev];
    let mut prev_cut = 0.0f32;
    for d in 0..n_dev {
        let shard = plan.iter().find(|s| s.device_index == d);
        let cut = match shard.and_then(|s| s.blocks) {
            Some(_) if shard.is_some_and(|s| s.holds_output) => (n_layer + 1) as f32,
            Some((_, last)) => last as f32 + 0.5,
            None => prev_cut,
        };
        out[d] = cut - prev_cut;
        prev_cut = cut;
    }
    out
}

/// Read the transformer block count (`<arch>.block_count`) from a GGUF's metadata
/// — needed to plan the per-block split before the model is loaded. `None` if
/// absent / not a u32.
pub fn gguf_block_count(path: &Path) -> std::io::Result<Option<u32>> {
    let mut r = std::io::BufReader::new(fs::File::open(path)?);
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != b"GGUF" {
        return Ok(None);
    }
    let _version = read_u32(&mut r)?;
    let _tensor_count = read_u64(&mut r)?;
    let kv_count = read_u64(&mut r)?;
    for _ in 0..kv_count {
        let key = read_gguf_string(&mut r)?;
        let vtype = read_u32(&mut r)?;
        if key.ends_with(".block_count") && vtype == 4 {
            return Ok(Some(read_u32(&mut r)?));
        }
        skip_metadata_value(&mut r, vtype)?;
    }
    Ok(None)
}
