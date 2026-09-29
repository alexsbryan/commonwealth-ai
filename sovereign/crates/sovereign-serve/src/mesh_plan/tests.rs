// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

mod speed;

const GB: u64 = 1024 * 1024 * 1024;

/// A model with `n` uniform blocks, an output head, and a token embedding.
fn model(n: u32, block_gb: u64, head_gb: u64, embd_gb: u64) -> Vec<(String, Option<u32>, u64)> {
    let mut v: Vec<(String, Option<u32>, u64)> = (0..n)
        .map(|i| (format!("blk.{i}.attn_q.weight"), Some(i), block_gb * GB))
        .collect();
    v.push(("output.weight".into(), None, head_gb * GB));
    v.push(("token_embd.weight".into(), None, embd_gb * GB));
    v
}

fn input(sizes: Vec<(String, Option<u32>, u64)>, n: u32, devices_gb: Vec<f64>) -> PlanInput {
    let host = devices_gb.len() - 1;
    PlanInput {
        model_name: "test.gguf".into(),
        n_layer: n,
        sizes,
        devices_gb,
        // Default to the single-basis shape: these tests assert on the
        // POSSIBLE allocation, which the top-level report fields carry
        // whether or not a live reading exists. The two-basis tests set this
        // explicitly.
        devices_free_gb: None,
        // No daemon in these tests, so no pin. The pin tests set it.
        block_split_pin: None,
        host,
        headroom: 1.2,
        headroom_from_flag: false,
        // These tests exercise the fit computation, not the speed lookup;
        // `mesh: None` is the `--devices` shape, which is barred from
        // matching a measurement by construction.
        mesh: None,
        n_ctx: 32_768,
        // No llama backend in these tests — weights-only fit, the same
        // fallback the live gate takes when the projection is unavailable.
        overheads: None,
    }
}

/// `build_report` against an empty measurement store — the week-1 state, and
/// the state every fit assertion below cares about.
fn report(i: PlanInput) -> PlanReport {
    build_report(
        i,
        &crate::mesh_measurements::MeasurementFile::new(),
        &[],
        "test-build",
    )
}

/// The index-space trap, pinned.
///
/// `shard_fits` receives capacities in PLAN order — RPC workers first, host
/// last — while a row is displayed under the index the operator typed in
/// `--devices`. The two are different permutations whenever `--host` is not
/// the last device, and mixing them up attributes every row to the wrong
/// machine with no error and no visible symptom. This asserts the mapping
/// end to end: whatever the plan order, each row's capacity is the VRAM of
/// the device that row NAMES.
#[test]
fn each_rows_capacity_belongs_to_the_device_that_row_names() {
    let devices = vec![64.0, 32.0, 16.0];
    for host in 0..devices.len() {
        let mut i = input(model(48, 1, 2, 3), 48, devices.clone());
        i.host = host;
        let r = report(i);
        for row in &r.rows {
            assert_eq!(
                row.vram(),
                (devices[row.dev] * GIB) as u64,
                "host={host}: row for device {} carries another device's capacity",
                row.dev
            );
        }
        let head_holder = r.rows.iter().find(|d| d.holds_output).expect("a head");
        assert_eq!(
            head_holder.dev, host,
            "host={host}: the output head must land on the host"
        );
    }
}

/// The preview's verdict IS the live gate's verdict — same function, same
/// numbers. Before 2026-07-28 this file had its own fold and its own
/// comparison, so the two could drift apart silently; a preview that
/// disagrees with the load it previews is worse than no preview.
#[test]
fn the_rows_come_from_the_shared_decider() {
    use sovereign_inference::embedded as inf;
    let sizes = model(48, 1, 2, 3);
    let devices = vec![64.0, 32.0, 32.0];
    let r = report(input(sizes.clone(), 48, devices.clone()));

    // Rebuild the same inputs the live load would hand `shard_fits`.
    let mass = inf::model_mass_from_sizes(&sizes, 48);
    let vram: Vec<u64> = devices.iter().map(|&g| (g * GIB) as u64).collect();
    let host = devices.len() - 1;
    let mut order: Vec<usize> = (0..vram.len()).filter(|&d| d != host).collect();
    order.push(host);
    let weights: Vec<f32> = order
        .iter()
        .map(|&d| inf::quantize_vram(vram[d]) as f32)
        .collect();
    let plan = inf::plan_shards_weighted(48, &weights, &mass.block_bytes, mass.head_bytes);
    let capacities: Vec<u64> = order.iter().map(|&d| vram[d]).collect();
    let fits = inf::shard_fits(&plan, &capacities, &mass, 1.2, None).expect("judgeable");

    for (pos, &d) in order.iter().enumerate() {
        let row = r
            .rows
            .iter()
            .find(|x| x.dev == d)
            .expect("a row per device");
        assert_eq!(row.fit, fits[pos], "device {d} disagrees with shard_fits");
    }
}

/// The defect the aggregate gate cannot see: pooled memory is ample, yet one
/// device's own share does not fit it. The preview surfaced this first; as
/// of 2026-07-28 the live load refuses on it too, through this same
/// `shard_fits` call.
///
/// The mechanism is `quantize_vram`'s 4 GiB bucket floor — a 2 GB device is
/// weighted as though it had 4 GiB, so the split hands it roughly twice the
/// mass it can hold. Aggregate arithmetic cannot see this, which is exactly
/// why the per-device pass exists.
#[test]
fn a_small_device_overflows_while_the_pooled_gate_passes() {
    let r = report(input(model(300, 2, 0, 0), 300, vec![2.0, 1000.0]));
    assert!(
        r.gate_pass,
        "1002 GB pooled against a 600 GB model must clear the aggregate gate"
    );
    let overflows = r.overflows();
    assert_eq!(
        overflows.len(),
        1,
        "exactly the small device should overflow"
    );
    assert_eq!(overflows[0].dev, 0);
    assert!(overflows[0].need() > overflows[0].vram());
    assert_eq!(
        r.exit_code(),
        1,
        "a per-device overflow is not a passing plan"
    );
}

/// Every byte of the model is charged to exactly one device. If this drifts,
/// the fit verdict is meaningless in a way no other assertion would catch.
#[test]
fn every_block_is_charged_exactly_once() {
    let r = report(input(model(48, 1, 2, 3), 48, vec![64.0, 32.0, 32.0]));
    let charged: u64 = r.rows.iter().map(|d| d.weight()).sum();
    assert_eq!(
        charged,
        48 * GB + 2 * GB,
        "block mass plus the output head, and nothing else"
    );
    // token_embd is host system RAM, never a device's share.
    assert_eq!(r.embd_bytes, 3 * GB);
    assert_eq!(r.total_weight, 48 * GB + 2 * GB + 3 * GB);
}

/// The output head rides the host, and the host is charged for it.
#[test]
fn the_host_is_charged_for_the_output_head() {
    let r = report(input(model(48, 1, 8, 0), 48, vec![64.0, 64.0]));
    let holder = r
        .rows
        .iter()
        .find(|d| d.holds_output)
        .expect("someone holds the head");
    assert!(holder.is_host, "the head belongs to the host");
    let blocks_only: u64 = holder
        .blocks
        .map(|(a, b)| (a..=b).count() as u64 * GB)
        .unwrap_or(0);
    assert_eq!(holder.weight(), blocks_only + 8 * GB);
}

/// `--host` moves both the head and the star in the table.
#[test]
fn the_host_index_selects_which_device_holds_the_head() {
    let mut i = input(model(48, 1, 4, 0), 48, vec![64.0, 64.0, 64.0]);
    i.host = 0;
    let r = report(i);
    assert!(r.rows[0].is_host && r.rows[0].holds_output);
    assert!(!r.rows[1].holds_output && !r.rows[2].holds_output);
    assert!(render_human(&r).contains("*   0  host"));
}

/// Need is `weight × headroom`, using the same truncating cast the table
/// prints — so the gate and the displayed number agree at the boundary.
#[test]
fn need_is_weight_times_headroom_exactly() {
    let mut i = input(model(48, 1, 0, 0), 48, vec![512.0]);
    i.headroom = 1.35;
    let r = report(i);
    let d = &r.rows[0];
    assert_eq!(d.need(), (d.weight() as f64 * 1.35) as u64);
}

#[test]
fn a_cluster_too_small_fails_the_aggregate_gate() {
    let r = report(input(model(48, 1, 0, 0), 48, vec![2.0, 2.0]));
    assert!(!r.gate_pass);
    assert_eq!(r.exit_code(), 1);
    assert!(render_human(&r).contains("FAIL — cluster too small"));
}

#[test]
fn a_comfortable_fit_passes_both_gates() {
    let r = report(input(model(48, 1, 2, 1), 48, vec![256.0, 256.0]));
    assert!(r.gate_pass);
    assert!(r.overflows().is_empty());
    assert_eq!(r.exit_code(), 0);
    assert!(render_human(&r).contains("Per-device:     all devices fit ok"));
}

/// A dense model reports no MoE section; a routed-expert model does, and the
/// expert mass is counted as cold rather than as per-token work.
#[test]
fn moe_is_reported_only_when_routed_experts_exist() {
    let dense = report(input(model(48, 1, 0, 0), 48, vec![256.0]));
    assert!(dense.moe.is_none());
    assert!(!render_human(&dense).contains("MoE:"));

    let mut sizes = model(48, 1, 0, 0);
    sizes.push(("blk.0.ffn_gate_exps.weight".into(), Some(0), 90 * GB));
    let moe = report(input(sizes, 48, vec![512.0]));
    let m = moe.moe.as_ref().expect("routed experts make this an MoE");
    assert_eq!(m.routed_expert_bytes, 90 * GB);
    assert_eq!(
        m.hot_bytes,
        (48 + 90) * GB - 90 * GB,
        "hot mass excludes the cold experts"
    );
    assert!(render_human(&moe).contains("MoE:"));
}

/// Uniform mass says heterogeneous VRAM is safe; skewed mass warns instead.
#[test]
fn block_mass_spread_drives_the_uniformity_verdict() {
    let uniform = report(input(model(48, 1, 0, 0), 48, vec![256.0]));
    assert!(uniform.block_mass.uniform);
    assert!(render_human(&uniform).contains("UNIFORM mass"));

    let mut skewed = model(48, 1, 0, 0);
    skewed[0].2 = 40 * GB;
    let r = report(input(skewed, 48, vec![256.0]));
    assert!(!r.block_mass.uniform);
    assert!(r.block_mass.spread > 1.15);
    assert!(render_human(&r).contains("NON-UNIFORM mass"));
}

/// Fewer nodes means fewer per-token hops — reported as a cost, never as a
/// recommendation, because on a bandwidth-bound host offloading can still win.
#[test]
fn the_hop_count_follows_the_node_count_without_claiming_a_winner() {
    let r = report(input(model(48, 1, 0, 0), 48, vec![256.0, 256.0, 256.0]));
    assert_eq!(r.nodes.active_nodes, 3);
    assert_eq!(r.nodes.hops_now, 2);
    assert_eq!(r.nodes.min_nodes, 1, "one 256 GB node holds a 48 GB model");
    let out = render_human(&r);
    assert!(out.contains("3 holding blocks → 2 network hops per token"));
    assert!(
        out.contains("Measure both."),
        "the advisor must not claim fewer nodes is always faster"
    );
}

/// The headroom line distinguishes a what-if from the value the load will use.
#[test]
fn headroom_source_is_reported_honestly() {
    let mut i = input(model(48, 1, 0, 0), 48, vec![256.0]);
    i.headroom_from_flag = true;
    let flagged = report(i);
    assert!(render_human(&flagged).contains("WHAT-IF"));
    assert_eq!(render_json(&flagged)["headroom_source"], "flag");

    let configured = report(input(model(48, 1, 0, 0), 48, vec![256.0]));
    assert!(render_human(&configured).contains("matches the load's configured headroom"));
    assert_eq!(render_json(&configured)["headroom_source"], "config");
}

/// The JSON contract every scripted consumer reads.
#[test]
fn render_json_carries_the_whole_device_table() {
    let r = report(input(model(48, 1, 2, 1), 48, vec![64.0, 32.0, 32.0]));
    let j = render_json(&r);
    assert_eq!(j["blocks"], 48);
    assert!(j["aggregate_gate_pass"].as_bool().unwrap());
    assert_eq!(j["moe"], serde_json::Value::Null);
    let devices = j["devices"].as_array().expect("a row per device");
    assert_eq!(devices.len(), 3);
    for d in devices {
        for k in [
            "device",
            "role",
            "vram_gb",
            "blocks",
            "block_count",
            "holds_output",
            "weight_gb",
            "need_gb",
            "fits",
        ] {
            assert!(
                !d[k].is_null() || k == "blocks",
                "device row is missing {k}"
            );
        }
    }
    assert_eq!(j["devices"][2]["role"], "host");
}

/// A device that gets no blocks holds nothing and cannot manufacture a
/// refusal.
#[test]
fn a_device_with_no_blocks_holds_nothing_and_fits() {
    let r = report(input(model(2, 1, 0, 0), 2, vec![64.0, 64.0, 64.0]));
    for d in r.rows.iter().filter(|d| d.blocks.is_none()) {
        assert_eq!(d.weight(), 0);
        assert!(d.fits());
    }
}
