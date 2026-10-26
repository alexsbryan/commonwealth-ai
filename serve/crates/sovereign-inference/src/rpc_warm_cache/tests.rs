// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

/// Verbatim replica of llama.cpp's layer→device assignment
/// (llama-model.cpp `get_layer_buft_list`, llama-cpp-sys-4 0.4.2): cumulative
/// sum → normalize → `upper_bound(splits, il / (n_layer + 1))`. `il == n_layer`
/// is the output head (`dev_output`). Assumes `n_gpu_layers ≥ n_layer + 1`
/// (we pass 999), so `i_gpu_start == 0`.
fn llama_dev_layer(tensor_split: &[f32], il: u32, n_layer: u32) -> usize {
    let mut splits = tensor_split.to_vec();
    let mut sum = 0.0f32;
    for s in splits.iter_mut() {
        sum += *s;
        *s = sum;
    }
    for s in splits.iter_mut() {
        *s /= sum;
    }
    let v = il as f32 / (n_layer + 1) as f32;
    splits
        .iter()
        .position(|&s| s > v) // std::upper_bound: first element strictly greater
        .expect("il < n_layer + 1 ⇒ v < 1.0 = last split point")
}

/// Every layer (and the output unit) must land on the device the plan
/// assigned — this is the invariant whose violation was the distributed-GDN
/// host abort (DISTRIBUTED_GDN_CRASH_STATUS.md §4).
fn assert_dev_layer_agrees(plan: &[NodeShard], n_layer: u32) {
    let split = dev_layer_tensor_split(plan, n_layer);
    for il in 0..n_layer {
        let want = tensor_device("blk.x.weight", Some(il), plan).expect("plan covers every block");
        let got = llama_dev_layer(&split, il, n_layer);
        assert_eq!(
            got, want,
            "layer {il} straddles: plan={want} dev_layer={got}"
        );
    }
    let want_out = plan.iter().find(|s| s.holds_output).unwrap().device_index;
    assert_eq!(
        llama_dev_layer(&split, n_layer, n_layer),
        want_out,
        "output unit"
    );
}

#[test]
fn dev_layer_split_agrees_on_the_crash_repro_cut() {
    // The exact 2026-07-27 repro: Qwen3.5-4B, 32 layers, RPC0 blocks 0..=7,
    // Vulkan0 blocks 8..=31 + output. Layer 8 (a Gated DeltaNet layer) is the
    // one llama.cpp put on RPC0 while our -ot put its weights on Vulkan0.
    let plan = vec![
        NodeShard {
            device_index: 0,
            blocks: Some((0, 7)),
            holds_output: false,
            fraction: 0.25,
        },
        NodeShard {
            device_index: 1,
            blocks: Some((8, 31)),
            holds_output: true,
            fraction: 0.75,
        },
    ];
    assert_dev_layer_agrees(&plan, 32);
    // And the specific regression layer, named:
    let split = dev_layer_tensor_split(&plan, 32);
    assert_eq!(llama_dev_layer(&split, 8, 32), 1, "layer 8 must be Vulkan0");
}

#[test]
fn naive_block_count_weights_reproduce_the_off_by_one() {
    // Documents WHY the midpoint math is load-bearing: the naive pin
    // (H2, 2026-07-27) passed block-count fractions [8, 24] ≡ [0.25, 0.75].
    // Over llama.cpp's n_layer+1 = 33 units, 0.25 × 33 = 8.25 → layer 8
    // lands on device 0 — the byte-identical warning H2 observed.
    assert_eq!(
        llama_dev_layer(&[8.0, 24.0], 8, 32),
        0,
        "the falsified H2 pin straddles"
    );
    // The corrected split puts the cut at 7.5/33 → layer 8 on device 1.
    assert_eq!(llama_dev_layer(&[7.5, 25.5], 8, 32), 1);
}

#[test]
fn dev_layer_split_agrees_across_shapes() {
    // Three devices, uneven cut (the 122B-style multi-worker case).
    assert_dev_layer_agrees(&plan_shards(48, &[0.3, 0.2, 0.5]), 48);
    // A blockless middle device (quarantined worker got weight ~0).
    let plan = vec![
        NodeShard {
            device_index: 0,
            blocks: Some((0, 10)),
            holds_output: false,
            fraction: 0.34,
        },
        NodeShard {
            device_index: 1,
            blocks: None,
            holds_output: false,
            fraction: 0.0,
        },
        NodeShard {
            device_index: 2,
            blocks: Some((11, 31)),
            holds_output: true,
            fraction: 0.66,
        },
    ];
    assert_dev_layer_agrees(&plan, 32);
    // Single device degenerate case.
    assert_dev_layer_agrees(&plan_shards(32, &[1.0]), 32);
    // Boundary at every possible cut point of a small model: no layer count
    // or cut position may straddle.
    for cut in 1..8u32 {
        let plan = vec![
            NodeShard {
                device_index: 0,
                blocks: Some((0, cut - 1)),
                holds_output: false,
                fraction: 0.5,
            },
            NodeShard {
                device_index: 1,
                blocks: Some((cut, 7)),
                holds_output: true,
                fraction: 0.5,
            },
        ];
        assert_dev_layer_agrees(&plan, 8);
    }
}

#[test]
fn plan_shards_apportions_contiguously_and_sums() {
    let p = plan_shards(12, &[0.5, 0.25, 0.25]);
    let total: u32 = p
        .iter()
        .filter_map(|s| s.blocks)
        .map(|(a, b)| b - a + 1)
        .sum();
    assert_eq!(total, 12, "every block assigned exactly once");
    assert_eq!(p[0].blocks, Some((0, 5)));
    assert_eq!(p[1].blocks, Some((6, 8)));
    assert_eq!(p[2].blocks, Some((9, 11)));
    assert!(p[2].holds_output, "output on the last block-holding device");
    assert!(!p[0].holds_output);
}

#[test]
fn plan_shards_equal_weights_when_all_zero() {
    let p = plan_shards(4, &[0.0, 0.0]);
    assert_eq!(p[0].blocks, Some((0, 1)));
    assert_eq!(p[1].blocks, Some((2, 3)));
}

#[test]
fn quantize_vram_is_stable_within_a_bucket() {
    let gb = 1024 * 1024 * 1024u64;
    assert_eq!(quantize_vram(0), 0);
    // ~44.8GB and ~45.1GB land in the same bucket → stable plan across a
    // reload where free VRAM jittered by a few hundred MB.
    assert_eq!(
        quantize_vram(44_800 * 1024 * 1024),
        quantize_vram(45_100 * 1024 * 1024)
    );
    // A tiny-but-nonzero device still gets one bucket (never apportioned to 0).
    assert_eq!(quantize_vram(200 * 1024 * 1024), 4 * gb);
    // Monotonic across buckets.
    assert!(quantize_vram(50 * gb) >= quantize_vram(44 * gb));
}

#[test]
fn tensor_device_and_overrides_share_one_assignment() {
    let plan = plan_shards(4, &[0.5, 0.5]); // dev0:0-1, dev1:2-3 + output
    assert_eq!(
        tensor_device("blk.0.attn_q.weight", Some(0), &plan),
        Some(0)
    );
    assert_eq!(
        tensor_device("blk.3.ffn_down.weight", Some(3), &plan),
        Some(1)
    );
    assert_eq!(tensor_device("output.weight", None, &plan), Some(1));
    assert_eq!(tensor_device("token_embd.weight", None, &plan), None);
    let pats = override_patterns(&plan);
    assert!(pats.iter().any(|(p, d)| *d == 0 && p == "^blk\\.(0|1)\\."));
    assert!(pats.iter().any(|(p, d)| *d == 1 && p == "^blk\\.(2|3)\\."));
    assert!(pats.iter().any(|(p, d)| *d == 1 && p == "^output\\.weight"));
}

#[test]
fn tensor_layer_parses_block_index() {
    assert_eq!(tensor_layer("blk.27.attn_q.weight"), Some(27));
    assert_eq!(tensor_layer("blk.0.ffn_gate.weight"), Some(0));
    // Globals (non-layer) have no block index.
    assert_eq!(tensor_layer("token_embd.weight"), None);
    assert_eq!(tensor_layer("output.weight"), None);
    assert_eq!(tensor_layer("output_norm.weight"), None);
    // Malformed / non-numeric block index is not a layer.
    assert_eq!(tensor_layer("blk.x.weight"), None);
    assert_eq!(tensor_layer("blk."), None);
}

#[test]
fn fnv1a_matches_canonical_and_ggml() {
    // The canonical FNV-1a/64 test vector for "a" — also exactly what
    // ggml-rpc.cpp's fnv_hash produces, which is the whole correctness basis
    // for the cache (host hash == warm-file name).
    let mut h = Fnv1a::new();
    h.update(b"a");
    assert_eq!(h.finish(), 0xaf63_dc4c_8601_ec8c);
    // Empty input is the offset basis.
    assert_eq!(Fnv1a::new().finish(), 0xcbf2_9ce4_8422_2325);
    // Streaming in pieces must equal hashing the whole — the property the
    // byte-range warmer relies on when it hashes HTTP chunks incrementally.
    let mut split = Fnv1a::new();
    split.update(b"he");
    split.update(b"llo");
    let mut whole = Fnv1a::new();
    whole.update(b"hello");
    assert_eq!(split.finish(), whole.finish());
    // Cache filename is the zero-padded 16-hex of the hash.
    assert_eq!(cache_file_name(0xaf63_dc4c_8601_ec8c), "af63dc4c8601ec8c");
}

// ── byte-mass-aware split (plan_shards_weighted) ──

fn assigned_blocks(plan: &[NodeShard]) -> u32 {
    plan.iter()
        .filter_map(|s| s.blocks)
        .map(|(a, b)| b - a + 1)
        .sum()
}
fn held_bytes(s: &NodeShard, block_bytes: &[u64]) -> u64 {
    s.blocks
        .map(|(a, b)| (a..=b).map(|i| block_bytes[i as usize]).sum::<u64>())
        .unwrap_or(0)
}
fn count(s: &NodeShard) -> u32 {
    s.blocks.map(|(a, b)| b - a + 1).unwrap_or(0)
}

#[test]
fn weighted_empty_bytes_is_the_count_split() {
    // No size table (or a wrong-length one) → byte-aware degrades to the exact
    // count-based plan, so `plan_shards` stays a pure special-case of it.
    let w = [0.5, 0.5];
    assert_eq!(plan_shards_weighted(4, &w, &[], 0), plan_shards(4, &w));
    assert_eq!(
        plan_shards_weighted(4, &w, &[1, 2, 3], 0),
        plan_shards(4, &w)
    );
}

#[test]
fn weighted_uniform_mass_matches_count_split() {
    // When every block has equal mass, byte-proportional == count-proportional,
    // so the byte-aware split reproduces the count split (no regression on dense
    // or uniform all-MoE models).
    let bb = vec![500u64; 12];
    let w = [0.5, 0.25, 0.25];
    let byte: Vec<_> = plan_shards_weighted(12, &w, &bb, 0)
        .iter()
        .map(|s| s.blocks)
        .collect();
    let cnt: Vec<_> = plan_shards(12, &w).iter().map(|s| s.blocks).collect();
    assert_eq!(
        byte, cnt,
        "uniform mass → byte-aware ranges == count ranges"
    );
}

/// covers: FE-70
#[test]
fn weighted_balances_clustered_mass_better_than_count() {
    // A DeepSeek-shaped model: 3 tiny leading dense blocks, then 9 heavy MoE
    // blocks. Two EQUAL devices should each end with ~half the BYTE mass — but
    // a count split (6 blocks each) gives dev0 only 3 of the 9 heavy blocks.
    let n = 12u32;
    let mut bb = vec![100u64; 3];
    bb.extend(std::iter::repeat(1000u64).take(9)); // total = 300 + 9000 = 9300
    let total: u64 = bb.iter().sum();

    let byte = plan_shards_weighted(n, &[1.0, 1.0], &bb, 0);
    assert_eq!(assigned_blocks(&byte), n, "tiles every block exactly once");

    let (b0, b1) = (held_bytes(&byte[0], &bb), held_bytes(&byte[1], &bb));
    let target = total / 2;
    assert!(
        (b0 as i64 - target as i64).unsigned_abs() <= total / 8,
        "dev0 byte mass {b0} within 12.5% of {target}"
    );

    // The win, made explicit: byte-aware is strictly more balanced than count.
    let cnt = plan_shards(n, &[1.0, 1.0]);
    let (c0, c1) = (held_bytes(&cnt[0], &bb), held_bytes(&cnt[1], &bb));
    let byte_ratio = b0.max(b1) as f64 / b0.min(b1) as f64;
    let count_ratio = c0.max(c1) as f64 / c0.min(c1) as f64;
    assert!(
        byte_ratio < count_ratio,
        "byte-aware imbalance {byte_ratio:.2}x < count imbalance {count_ratio:.2}x"
    );
}

#[test]
fn weighted_folds_output_head_onto_last_device() {
    // The head rides on the last (host) device, so it should be handed a
    // smaller BLOCK budget to compensate — else it runs ~head_bytes heavy.
    let bb = vec![100u64; 10]; // block mass 1000
    let head = 300u64; // ~3 blocks of head mass on the host (dev1)
    let plan = plan_shards_weighted(10, &[1.0, 1.0], &bb, head);
    assert_eq!(assigned_blocks(&plan), 10);
    assert!(plan[1].holds_output, "last device holds the output head");
    assert!(
        count(&plan[0]) > count(&plan[1]),
        "head-holder gets fewer blocks: dev0={} dev1={}",
        count(&plan[0]),
        count(&plan[1])
    );
}

#[test]
fn weighted_zero_weight_device_gets_no_blocks() {
    // A member advertising 0 VRAM must not be handed a shard.
    let bb = vec![100u64; 6];
    let plan = plan_shards_weighted(6, &[1.0, 0.0, 1.0], &bb, 0);
    assert!(plan[1].blocks.is_none(), "zero-VRAM device holds no blocks");
    assert_eq!(
        assigned_blocks(&plan),
        6,
        "the other two still tile all blocks"
    );
}

/// covers: FE-70
#[test]
fn weighted_contiguous_and_gapless_on_heterogeneous_vram() {
    // Contiguity is load-bearing (hops stay D-1; override_patterns/tensor_device
    // assume it). Heterogeneous VRAM + skewed mass must still tile [0, n) with no
    // gap or overlap and ranges in ascending device order.
    let mut bb: Vec<u64> = (0..40)
        .map(|i| if i % 2 == 0 { 20 } else { 1200 })
        .collect();
    bb[0] = 5; // a lighter leading block for good measure
    let plan = plan_shards_weighted(40, &[4.0, 2.0, 2.0], &bb, 800);
    assert_eq!(assigned_blocks(&plan), 40);
    let mut next = 0u32;
    for s in &plan {
        if let Some((a, b)) = s.blocks {
            assert_eq!(a, next, "no gap/overlap between contiguous shards");
            assert!(b >= a);
            next = b + 1;
        }
    }
    assert_eq!(next, 40, "shards cover the whole block range");
}

#[test]
fn routed_expert_classifier_splits_hot_from_cold() {
    // Cold routed experts (`_exps`) vs hot shared expert (`_shexp`) / router
    // (`ffn_gate_inp`) / attention — the distinction the hot/cold report rests on.
    assert!(is_routed_expert_tensor("blk.3.ffn_down_exps.weight"));
    assert!(is_routed_expert_tensor("blk.0.ffn_gate_exps.weight"));
    assert!(is_routed_expert_tensor("blk.7.ffn_up_exps.weight"));
    assert!(!is_routed_expert_tensor("blk.3.ffn_down_shexp.weight")); // shared = hot
    assert!(!is_routed_expert_tensor("blk.3.ffn_gate_inp.weight")); // router = hot
    assert!(!is_routed_expert_tensor("blk.3.attn_q.weight"));
    assert!(!is_routed_expert_tensor("output.weight"));
}

// ── the shared fit decider ──────────────────────────────────────────────

const GIB: u64 = 1024 * 1024 * 1024;

/// A 4-block model: 1 GiB per block, a 1 GiB head, 2 GiB of embeddings.
fn simple_sizes() -> Vec<(String, Option<u32>, u64)> {
    let mut v: Vec<(String, Option<u32>, u64)> = (0..4)
        .map(|i| (format!("blk.{i}.attn_q.weight"), Some(i), GIB))
        .collect();
    v.push(("output.weight".into(), None, GIB));
    v.push(("token_embd.weight".into(), None, 2 * GIB));
    v
}

#[test]
fn model_mass_separates_what_lands_in_different_places() {
    let m = model_mass_from_sizes(&simple_sizes(), 4);
    assert_eq!(m.block_bytes, vec![GIB; 4]);
    assert_eq!(
        m.head_bytes, GIB,
        "the head rides with the last block-holder"
    );
    assert_eq!(
        m.embd_bytes,
        2 * GIB,
        "token_embd stays in host RAM and must not be charged to a device"
    );
    assert_eq!(m.other_global_bytes, 0);
    assert_eq!(m.total_bytes(), 7 * GIB);
    assert!(m.is_known());
    assert!(!m.recurrent);
}

#[test]
fn model_mass_counts_an_out_of_range_layer_rather_than_dropping_it() {
    // A dropped byte is a byte the fit check will not charge anyone for.
    let sizes = vec![
        ("blk.0.attn_q.weight".into(), Some(0), GIB),
        ("blk.99.attn_q.weight".into(), Some(99), 5 * GIB),
    ];
    let m = model_mass_from_sizes(&sizes, 1);
    assert_eq!(m.block_bytes, vec![GIB]);
    assert_eq!(m.other_global_bytes, 5 * GIB);
    assert_eq!(m.total_bytes(), 6 * GIB);
}

#[test]
fn model_mass_notices_moe_and_recurrent_stacks() {
    let sizes = vec![
        ("blk.0.ffn_gate_exps.weight".into(), Some(0), 8 * GIB),
        ("blk.0.ssm_a".into(), Some(0), GIB),
    ];
    let m = model_mass_from_sizes(&sizes, 1);
    assert_eq!(m.routed_expert_bytes, 8 * GIB);
    assert!(m.recurrent);
    assert_eq!(
        m.block_bytes[0],
        9 * GIB,
        "routed-expert mass is resident and still counts against fit"
    );
}

/// covers: FE-76
#[test]
fn an_unread_tensor_table_is_never_a_pass() {
    // This is the whole reason `shard_fits` returns Option. Judging against
    // an empty mass would clear every device on the strength of zeros —
    // turning a failed header parse into a clean bill of health.
    let mass = ModelMass::default();
    assert!(!mass.is_known());
    let plan = plan_shards(4, &[1.0, 1.0]);
    assert_eq!(shard_fits(&plan, &[GIB, GIB], &mass, 1.2, None), None);

    // The shape the guard is ACTUALLY for, and the one the empty mass
    // above does not exercise: a table that was read and came back all
    // zeros. `block_bytes` then has the right LENGTH, so the per-block
    // lookup inside `shard_fits` succeeds and nothing else refuses —
    // every device would clear on `need = 0`, which is the "clean bill of
    // health from a failed header read" this returns `None` to prevent.
    // Deleting the `is_known` guard leaves the empty-table case above
    // refusing anyway (the block lookup runs off the end), so without
    // this arm the test passes for a reason that is not the guard.
    let zeroed = ModelMass {
        block_bytes: vec![0; 4],
        ..ModelMass::default()
    };
    assert!(!zeroed.is_known(), "a read-but-zero table is not knowledge");
    assert_eq!(
        shard_fits(&plan, &[GIB, GIB], &zeroed, 1.2, None),
        None,
        "zeros are not a fit — cannot-judge must stay distinct from pass"
    );
}

/// covers: FE-75
#[test]
fn shard_fits_reports_every_device_not_just_the_failures() {
    // A Result-shaped decider would force `mesh plan` to keep its own
    // traversal to print the fitting rows — and a second traversal is the
    // drift this function exists to remove.
    let mass = model_mass_from_sizes(&simple_sizes(), 4);
    let plan = plan_shards(4, &[1.0, 1.0]);
    let fits = shard_fits(&plan, &[10 * GIB, 10 * GIB], &mass, 1.2, None).expect("judgeable");
    assert_eq!(fits.len(), 2, "one row per shard, fitting rows included");
    assert!(fits.iter().all(|f| f.fits()));
    // Device 1 holds blocks 2-3 plus the head: 3 GiB, needing 3.6 with headroom.
    assert_eq!(fits[1].held_bytes, 3 * GIB);
    assert_eq!(fits[1].need_bytes, (3.0 * GIB as f64 * 1.2) as u64);
    assert!(fits[1].slack_bytes() > 0);
}

#[test]
fn a_device_that_cannot_hold_its_share_overflows() {
    let mass = model_mass_from_sizes(&simple_sizes(), 4);
    let plan = plan_shards(4, &[1.0, 1.0]);
    // Device 0 holds blocks 0-1 = 2 GiB, needing 2.4 GiB; it has 2.
    let fits = shard_fits(&plan, &[2 * GIB, 10 * GIB], &mass, 1.2, None).expect("judgeable");
    assert!(!fits[0].fits());
    assert!(fits[0].slack_bytes() < 0);
    assert!(fits[1].fits(), "one overflow must not condemn the others");
}

/// covers: FE-74
#[test]
fn pooled_memory_can_pass_where_a_device_fails() {
    // The exact hole the per-device gate closes: 12 GiB pooled against a
    // 7 GiB model is comfortable, and the split still overflows one device.
    let mass = model_mass_from_sizes(&simple_sizes(), 4);
    let plan = plan_shards(4, &[1.0, 1.0]);
    let capacities = [1 * GIB, 11 * GIB];
    assert!(
        capacities.iter().sum::<u64>() > (mass.total_bytes() as f64 * 1.2) as u64,
        "the aggregate gate would wave this through"
    );
    let fits = shard_fits(&plan, &capacities, &mass, 1.2, None).expect("judgeable");
    assert!(fits.iter().any(|f| !f.fits()));
}

#[test]
fn headroom_scales_the_requirement_so_lowering_it_helps() {
    // `need = held × headroom`. The refusal message tells operators to
    // LOWER the headroom; this is the arithmetic that makes that correct.
    let mass = model_mass_from_sizes(&simple_sizes(), 4);
    let plan = plan_shards(4, &[1.0, 1.0]);
    let capacities = [2 * GIB, 10 * GIB];
    assert!(!shard_fits(&plan, &capacities, &mass, 1.2, None).expect("judgeable")[0].fits());
    assert!(
        shard_fits(&plan, &capacities, &mass, 1.0, None).expect("judgeable")[0].fits(),
        "lowering the headroom must be able to rescue a marginal fit"
    );
}

/// THE 2026-08-02 FAILURE SHAPE. A share whose weights squeeze inside the
/// capacity at headroom 1.0 — a 0.07 GiB-class margin — passes a
/// weights-only judgement, reaches `serving`, and dies allocating KV +
/// compute on the first inference. With the projected overheads the same
/// plan is refused BEFORE the minutes-long warm is spent on it.
#[test]
fn a_weights_only_pass_with_no_room_for_kv_is_refused_with_overheads() {
    let mass = model_mass_from_sizes(&simple_sizes(), 4);
    let plan = plan_shards(4, &[1.0, 1.0]);
    // Device 0 holds blocks 0-1 = 2 GiB and has 2 GiB + a sliver.
    let capacities = [2 * GIB + GIB / 16, 10 * GIB];
    assert!(
        shard_fits(&plan, &capacities, &mass, 1.0, None).expect("judgeable")[0].fits(),
        "precondition: weights-only judgement passes on the sliver margin"
    );
    let o = PlanOverheads {
        context_total_bytes: GIB,     // 256 MiB/block over 4 blocks
        compute_accel_bytes: GIB / 2, // every device reserves scratch
        compute_host_bytes: GIB,      // host-side scheduler buffer
        model_host_bytes: 0,
    };
    let fits = shard_fits(&plan, &capacities, &mass, 1.0, Some(&o)).expect("judgeable");
    assert!(
        !fits[0].fits(),
        "2 GiB weights + 512 MiB KV share + 512 MiB scratch cannot live in 2.06 GiB"
    );
    // Overheads are charged where they land: device 0 gets its KV share +
    // accel scratch; device 1 (the host, last in plan order) additionally
    // carries the scheduler buffer.
    assert_eq!(fits[0].overhead_bytes, GIB / 2 + GIB / 2);
    assert_eq!(fits[1].overhead_bytes, GIB / 2 + GIB / 2 + GIB);
    assert_eq!(
        fits[1].need_bytes,
        fits[1].held_bytes + fits[1].overhead_bytes,
        "at headroom 1.0 the need is exactly weights + projected overheads"
    );
}

/// The KV share follows the blocks, not the device count — a device
/// holding no blocks is charged no context (only its compute scratch).
#[test]
fn overhead_context_is_charged_pro_rata_by_blocks() {
    let mass = model_mass_from_sizes(&simple_sizes(), 4);
    // All four blocks on device 1; device 0 idles.
    let plan = vec![
        NodeShard {
            device_index: 0,
            blocks: None,
            holds_output: false,
            fraction: 0.0,
        },
        NodeShard {
            device_index: 1,
            blocks: Some((0, 3)),
            holds_output: true,
            fraction: 1.0,
        },
    ];
    let o = PlanOverheads {
        context_total_bytes: 4 * GIB,
        compute_accel_bytes: GIB / 4,
        compute_host_bytes: 0,
        model_host_bytes: 0,
    };
    let fits = shard_fits(&plan, &[10 * GIB, 10 * GIB], &mass, 1.0, Some(&o)).expect("judgeable");
    assert_eq!(
        fits[0].overhead_bytes,
        GIB / 4,
        "no blocks → no KV, scratch only"
    );
    assert_eq!(
        fits[1].overhead_bytes,
        4 * GIB + GIB / 4,
        "all blocks → all KV"
    );
}

#[test]
fn inputs_that_do_not_describe_each_other_cannot_be_judged() {
    let mass = model_mass_from_sizes(&simple_sizes(), 4);
    let plan = plan_shards(4, &[1.0, 1.0]);
    assert_eq!(
        shard_fits(&plan, &[GIB], &mass, 1.2, None),
        None,
        "one capacity for two devices is not a verdict"
    );
    assert_eq!(shard_fits(&[], &[], &mass, 1.2, None), None);
    assert_eq!(
        shard_fits(&plan, &[GIB, GIB], &mass, 0.5, None),
        None,
        "headroom below 1.0 would ask a device to hold less than it holds"
    );
    assert_eq!(shard_fits(&plan, &[GIB, GIB], &mass, f64::NAN, None), None);
}

#[test]
fn a_plan_for_a_different_model_cannot_be_judged() {
    // A block range past the end of the mass table means the two describe
    // different models. Refusing beats silently under-charging the device.
    let mass = model_mass_from_sizes(&simple_sizes(), 4);
    let plan = plan_shards(8, &[1.0, 1.0]);
    assert_eq!(shard_fits(&plan, &[GIB, GIB], &mass, 1.2, None), None);
}

#[test]
fn a_device_holding_no_blocks_needs_nothing() {
    let mass = model_mass_from_sizes(&simple_sizes(), 4);
    // Four blocks across three devices weighted 10:1:0 — the last gets none.
    let plan = plan_shards_weighted(4, &[10.0, 1.0, 0.0], &mass.block_bytes, mass.head_bytes);
    let fits = shard_fits(&plan, &[10 * GIB, 10 * GIB, 0], &mass, 1.2, None).expect("judgeable");
    let idle = fits.last().expect("three rows");
    assert_eq!(idle.held_bytes, 0);
    assert!(
        idle.fits(),
        "a device holding nothing must not be reported as overflowing"
    );
}

#[test]
fn fit_rows_follow_the_plans_device_index_not_their_position() {
    // The index-space trap: capacities arrive in PLAN order, and a caller
    // that maps them through its own device numbering silently attributes
    // every row to the wrong machine.
    let mass = model_mass_from_sizes(&simple_sizes(), 4);
    let plan = plan_shards(4, &[1.0, 1.0]);
    let fits = shard_fits(&plan, &[2 * GIB, 10 * GIB], &mass, 1.2, None).expect("judgeable");
    for (pos, f) in fits.iter().enumerate() {
        assert_eq!(f.device_index, plan[pos].device_index);
    }
    assert_eq!(fits[0].capacity_bytes, 2 * GIB);
    assert_eq!(fits[1].capacity_bytes, 10 * GIB);
}
