// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

mod more;

// --- the speed section ------------------------------------------------

use crate::mesh_measurements as mm;

fn mesh_devs(names: &[&str], fp: Option<u64>) -> Vec<MeshDevice> {
    mesh_devs_linked(names, fp, Some(mm::LinkClass::Direct))
}

/// `mesh_devs` with the link spelled out, for the tests that turn on it.
fn mesh_devs_linked(
    names: &[&str],
    fp: Option<u64>,
    link: Option<mm::LinkClass>,
) -> Vec<MeshDevice> {
    names
        .iter()
        .map(|n| MeshDevice {
            name: (*n).into(),
            vram_gb: 64.0,
            // No live reading — the speed tests are about identity and link
            // class, not capacity. `two_capacity_*` covers the live basis.
            free_vram_gb: None,
            hw_fingerprint: fp,
            backend: Some("vulkan".into()),
            link,
        })
        .collect()
}

fn live(mesh: Vec<MeshDevice>) -> PlanInput {
    let devices_gb = mesh.iter().map(|d| d.vram_gb).collect::<Vec<_>>();
    let mut i = input(model(48, 1, 2, 0), 48, devices_gb);
    i.mesh = Some(mesh);
    i
}

// --- two capacities ----------------------------------------------------
//
// These pin the shape Alex asked for on 2026-07-29: report what is POSSIBLE
// and what is SAFE NOW, name the gap, and never silently pick one. The
// regression they exist to prevent is subtler than a wrong number — it is a
// plan that predicts a cut the loader would not run, and therefore looks up a
// measurement key nothing can ever be filed under.

/// Devices with BOTH capacities spelled out: `(name, total_gb, free_gb)`.
/// `None` free = no live reading for that device.
fn devs_with_free(spec: &[(&str, f64, Option<f64>)]) -> Vec<MeshDevice> {
    spec.iter()
        .map(|(name, total, free)| MeshDevice {
            name: (*name).into(),
            vram_gb: *total,
            free_vram_gb: *free,
            hw_fingerprint: Some(7),
            backend: Some("vulkan".into()),
            link: Some(mm::LinkClass::Direct),
        })
        .collect()
}

/// The live two-basis shape: worker(s) first, host last.
fn live_two(mesh: Vec<MeshDevice>) -> PlanInput {
    let devices_gb = mesh.iter().map(|d| d.vram_gb).collect::<Vec<_>>();
    let free = mesh
        .iter()
        .map(|d| d.free_vram_gb)
        .collect::<Option<Vec<_>>>();
    let mut i = input(model(48, 1, 2, 0), 48, devices_gb);
    i.devices_free_gb = free;
    i.mesh = Some(mesh);
    i
}

/// The measured mesh, as it actually stood on 2026-07-29: a 51 GB worker with
/// most of its memory held by an outgoing generation, and a 124 GB host.
fn the_real_mesh() -> Vec<MeshDevice> {
    devs_with_free(&[
        ("beefymac", 51.0, Some(19.5)),
        ("ruggedfox", 124.0, Some(110.0)),
    ])
}

/// The two bases produce DIFFERENT cuts, and the report says so rather than
/// presenting one as the plan.
#[test]
fn two_capacities_cut_the_model_differently_and_the_gap_is_named() {
    let r = report(live_two(the_real_mesh()));
    let sn = r.safe_now.as_ref().expect("a live basis");

    let blocks = |a: &[DeviceRow]| -> Vec<u32> {
        let mut v: Vec<u32> = a
            .iter()
            .map(|d| d.blocks.map(|(x, y)| y - x + 1).unwrap_or(0))
            .collect();
        v.reverse(); // host last → worker share first
        v
    };
    assert_ne!(
        blocks(&r.rows),
        blocks(&sn.rows),
        "51 GB total vs 19.5 GB free must apportion the worker differently — \
             if these ever match, the test mesh stopped exercising the bug"
    );
    // The worker holds LESS on the live basis, because it has less room.
    let worker_possible = r.rows.iter().find(|d| !d.is_host).unwrap();
    let worker_safe = sn.rows.iter().find(|d| !d.is_host).unwrap();
    assert!(
        worker_safe.blocks.map(|(x, y)| y - x + 1).unwrap_or(0)
            < worker_possible.blocks.map(|(x, y)| y - x + 1).unwrap_or(0),
        "the busy worker must be given a smaller share, not a larger one"
    );

    let out = render_human(&r);
    assert!(out.contains("Two capacities"), "both bases must be shown");
    assert!(out.contains("possible (device total)"));
    assert!(out.contains("safe now (live free)"));
    assert!(
        out.contains("held by other work right now"),
        "the gap must be NAMED, not left for the operator to subtract: {out}"
    );
    assert!(
        out.contains("Different cut"),
        "a differing cut must be called out: {out}"
    );
}

/// THE regression this change exists for.
///
/// A run is filed under the cut the loader EXECUTED (the live-free basis). The
/// plan must find it. Before 2026-07-30 the plan keyed on the totals basis,
/// predicted a different split, and reported "not measured" about a
/// configuration it held a real number for.
#[test]
fn speed_is_looked_up_under_the_cut_the_loader_would_execute() {
    // The key the plan now queries.
    let probe = report(live_two(the_real_mesh()));
    let executed_key = probe.speed_key.clone().expect("key");

    // Sanity: that key is NOT the one the totals basis would have produced.
    // Without this, the test could pass while both bases agreed.
    let totals_only = {
        let mut i = live_two(the_real_mesh());
        i.devices_free_gb = None;
        report(i)
    };
    assert_ne!(
        executed_key.placement_digest,
        totals_only.speed_key.expect("key").placement_digest,
        "the two bases must key differently, or this test proves nothing"
    );

    let mut file = mm::MeasurementFile::new();
    mm::record(
        &mut file,
        mm::MeasurementRecord {
            witness: None,
            conditions: None,
            key: executed_key,
            decode_tok_s: 10.48,
            decode_tok_s_min: 10.27,
            decode_tok_s_max: 10.52,
            ttft_ms: 2444.0,
            itl_p50_ms: 72.9,
            itl_p95_ms: 158.1,
            prefill_tok_s: Some(13.0),
            cold_load_s: None,
            trials: 3,
            content_frames: 170,
            model_name: "test.gguf".into(),
            placement_human: "36 local + 12 @beefymac".into(),
            nodes: 2,
            hops: 1,
            measured_at: 1_753_500_000,
            build: "test-build".into(),
            backend: Some("vulkan".into()),
            link_rtt_ms: Some(0.4),
            verdict: mm::Verdict::Valid,
        },
    );

    let r = build_report(live_two(the_real_mesh()), &file, &[], "test-build");
    assert!(
        matches!(r.speed, SpeedSection::Measured { .. }),
        "a record filed under the executed cut must be FOUND, not reported as a near miss"
    );
    assert!(render_human(&r).contains("10.5 tok/s decode"));
}

/// A model the hardware can hold, that cannot load this second, is reported as
/// exactly that — and does NOT fail the command. A busy device is not a wrong
/// plan; conflating the two is how one number came to mix two defects.
#[test]
fn fits_the_hardware_but_not_right_now() {
    // 48 GB of blocks + a 2 GB head. Ample on totals; not against 12 GB free.
    let r = report(live_two(devs_with_free(&[
        ("beefymac", 51.0, Some(2.0)),
        ("ruggedfox", 124.0, Some(12.0)),
    ])));

    assert!(
        r.gate_pass && r.overflows().is_empty(),
        "possible basis fits"
    );
    let sn = r.safe_now.as_ref().expect("a live basis");
    assert!(!sn.fits(), "the live basis must refuse");
    assert_eq!(
        r.exit_code(),
        0,
        "exit code follows POSSIBLE — a transient residual does not make the plan wrong"
    );

    let out = render_human(&r);
    assert!(
        out.contains("FITS this hardware but will NOT load right now"),
        "the operator must be told which of the two problems they have: {out}"
    );
    assert!(
        out.contains("Free the"),
        "the repair is to free memory, not to buy VRAM: {out}"
    );

    let j = render_json(&r);
    assert_eq!(j["aggregate_gate_pass"], true);
    assert_eq!(j["safe_now"]["fits"], false);
}

/// One device without a live reading means there is no coherent live basis.
/// Mixing free and total readings would invent a third cut matching nothing.
#[test]
fn a_partial_live_reading_yields_no_live_basis() {
    let r = report(live_two(devs_with_free(&[
        ("beefymac", 51.0, None),
        ("ruggedfox", 124.0, Some(110.0)),
    ])));
    assert!(
        r.safe_now.is_none(),
        "partial knowledge must not be averaged into a plausible-looking basis"
    );
    let out = render_human(&r);
    assert!(out.contains("Safe now:       UNKNOWN"), "{out}");
    assert!(render_json(&r)["safe_now"].is_null());
}

/// A pin overrides BOTH capacity bases, and the plan says so instead of
/// presenting a VRAM-derived cut that will not load.
///
/// This is the real 2026-07-29 configuration: `SOVEREIGN_RPC_BLOCK_SPLIT=12,36`
/// pinned in a systemd drop-in since 2026-07-27, against a mesh whose
/// capacities apportion 14/34.
#[test]
fn a_pinned_split_overrides_capacity_and_is_named() {
    let mut i = live_two(the_real_mesh());
    i.block_split_pin = Some("12,36".into());
    let r = report(i);

    let p = r.pinned.as_ref().expect("a valid pin applies");
    let n = |a: &[DeviceRow], dev: usize| -> u32 {
        a.iter()
            .find(|d| d.dev == dev)
            .and_then(|d| d.blocks.map(|(x, y)| y - x + 1))
            .unwrap_or(0)
    };
    assert_eq!(
        (n(&p.rows, 0), n(&p.rows, 1)),
        (12, 36),
        "the pin is obeyed"
    );
    // The derived cut depends on the model's mass profile (the real 122B gives
    // 14/34; this synthetic uniform model gives 15/33). What must hold on ANY
    // model is that capacity did NOT choose the pinned cut — otherwise the two
    // agree by luck and this test would pass while proving nothing.
    assert_ne!(
        (n(&r.rows, 0), n(&r.rows, 1)),
        (12, 36),
        "capacity must not have independently chosen the pinned cut"
    );

    let out = render_human(&r);
    assert!(out.contains("PINNED SPLIT"), "{out}");
    assert!(out.contains("SOVEREIGN_RPC_BLOCK_SPLIT=12,36"), "{out}");
    assert!(
        out.contains("NOT the VRAM-derived cut"),
        "the operator must be told the table above is not what loads: {out}"
    );

    let j = render_json(&r);
    assert_eq!(j["pinned"]["is_executed_cut"], true);
    assert_eq!(
        j["safe_now"]["is_executed_cut"], false,
        "a pin outranks the live-free basis"
    );
}

/// Speed is keyed on the PINNED cut, because that is what the loader runs.
/// Without this the plan queries a key no run can ever file under — the exact
/// failure that made the 10.48 tok/s two-node record unquotable.
#[test]
fn speed_is_keyed_on_the_pinned_cut() {
    let pinned_input = || {
        let mut i = live_two(the_real_mesh());
        i.block_split_pin = Some("12,36".into());
        i
    };
    let probe = report(pinned_input());
    let pinned_key = probe.speed_key.clone().expect("key");

    // The derived bases must key differently, or this proves nothing.
    assert_ne!(
        pinned_key.placement_digest,
        report(live_two(the_real_mesh()))
            .speed_key
            .expect("key")
            .placement_digest,
        "12/36 and 14/34 must hash differently"
    );

    let mut file = mm::MeasurementFile::new();
    mm::record(
        &mut file,
        mm::MeasurementRecord {
            witness: None,
            conditions: None,
            key: pinned_key,
            decode_tok_s: 10.48,
            decode_tok_s_min: 10.27,
            decode_tok_s_max: 10.52,
            ttft_ms: 2444.0,
            itl_p50_ms: 72.9,
            itl_p95_ms: 158.1,
            prefill_tok_s: Some(13.0),
            cold_load_s: None,
            trials: 3,
            content_frames: 170,
            model_name: "test.gguf".into(),
            placement_human: "36 local + 12 @beefymac".into(),
            nodes: 2,
            hops: 1,
            measured_at: 1_753_500_000,
            build: "test-build".into(),
            backend: Some("vulkan".into()),
            link_rtt_ms: Some(0.4),
            verdict: mm::Verdict::Valid,
        },
    );

    let r = build_report(pinned_input(), &file, &[], "test-build");
    assert!(
        matches!(r.speed, SpeedSection::Measured { .. }),
        "a record filed under the pinned cut must be FOUND"
    );
    assert!(render_human(&r).contains("10.5 tok/s decode"));
}

/// A pin the LOADER would reject must be rejected here identically, and named
/// as having no effect — an operator who set it believes it is in force.
#[test]
fn an_invalid_pin_is_refused_not_repaired() {
    let mut i = live_two(the_real_mesh());
    i.block_split_pin = Some("10,10".into()); // sums to 20, not 48
    let r = report(i);

    assert!(r.pinned.is_none(), "a non-tiling pin must not be applied");
    let out = render_human(&r);
    assert!(out.contains("does NOT apply"), "{out}");
    assert!(
        out.contains("having no effect"),
        "the operator must learn the pin is inert: {out}"
    );
    assert!(render_json(&r)["pinned"].is_null());
    assert_eq!(render_json(&r)["block_split_pin"], "10,10");
}

/// An idle mesh has no gap, and the report says that plainly instead of
/// printing a zero-width table nobody can read.
#[test]
fn an_idle_mesh_reports_no_gap() {
    let r = report(live_two(devs_with_free(&[
        ("beefymac", 51.0, Some(51.0)),
        ("ruggedfox", 124.0, Some(124.0)),
    ])));
    let sn = r.safe_now.as_ref().expect("a live basis");
    assert_eq!(sn.pooled, r.pooled, "identical capacities → identical pool");
    let out = render_human(&r);
    assert!(out.contains("No gap"), "{out}");
    assert!(
        !out.contains("Different cut"),
        "identical capacities cannot cut differently: {out}"
    );
}

/// A single-node plan has no link, and must key as `Local` rather than
/// inheriting whatever the host's own row happens to say.
#[test]
fn a_single_node_plan_keys_as_local() {
    let mesh = mesh_devs(&["ruggedfox"], Some(7));
    let mut i = input(
        model(48, 1, 2, 0),
        48,
        mesh.iter().map(|d| d.vram_gb).collect(),
    );
    i.host = 0;
    i.mesh = Some(mesh);
    let r = report(i);
    assert_eq!(
        r.speed_key.expect("live mesh key").link,
        mm::LinkClass::Local,
        "no workers means no link to classify"
    );
}

/// A peer carrying blocks that discovery has NOT found a worker for cannot
/// be attributed. The plan must not assume the good case: `Unknown` keys
/// never match, so the reader is told "not measured" instead of being shown
/// a direct-link number for a placement that might tunnel.
#[test]
fn a_peer_with_no_discovered_worker_makes_the_link_unknown() {
    let mesh = mesh_devs_linked(&["beefymac", "ruggedfox"], Some(7), None);
    let mut i = input(
        model(48, 1, 2, 0),
        48,
        mesh.iter().map(|d| d.vram_gb).collect(),
    );
    i.host = 1;
    i.mesh = Some(mesh);
    let r = report(i);
    let key = r.speed_key.expect("live mesh key");
    assert_eq!(key.link, mm::LinkClass::Unknown);
    // `lookup` refuses an Unknown link outright — proven against a stored
    // record in `mesh_measurements::an_unknown_link_never_matches_even_another_unknown`.
    assert!(mm::lookup(&mm::MeasurementFile::new(), &key, "0.0.0").is_none());
}

/// The same plan over a tunnel and over a direct link are different
/// questions, and must not share an answer.
#[test]
fn the_link_is_the_only_difference_and_it_still_changes_the_key() {
    let plan_over = |link: mm::LinkClass| {
        let mesh = mesh_devs_linked(&["beefymac", "ruggedfox"], Some(7), Some(link));
        let mut i = input(
            model(48, 1, 2, 0),
            48,
            mesh.iter().map(|d| d.vram_gb).collect(),
        );
        i.host = 1;
        i.mesh = Some(mesh);
        report(i).speed_key.expect("live mesh key")
    };
    let direct = plan_over(mm::LinkClass::Direct);
    let tunnel = plan_over(mm::LinkClass::Tunnel);

    assert_eq!(
        direct.placement_digest, tunnel.placement_digest,
        "same machines, same split — the digest cannot tell these apart"
    );
    assert_eq!(direct.host_hw_fingerprint, tunnel.host_hw_fingerprint);
    assert_eq!(direct.n_ctx, tunnel.n_ctx);
    assert_ne!(direct, tunnel, "…but the key must, via the link");
}

/// An idle peer must not change the key.
///
/// A machine apportioned no blocks is not part of the placement — it changes
/// nothing about how the model decodes. If it entered the digest, a
/// measurement taken today would stop matching the moment an unrelated peer
/// came online, and `mesh bench` (which builds its shards from what the
/// daemon reports is *loaded*, and so has no idle device to report) could
/// never produce a key this side would look up.
#[test]
fn a_peer_holding_no_blocks_does_not_enter_the_digest() {
    // One block cannot be spread, so every device past the block-holder is
    // idle however much memory it advertises. (Shrinking a device does NOT
    // idle it: `quantize_vram` floors at one 4 GiB bucket, so a nominally
    // tiny peer still gets a share.)
    //
    // The device NAMES are ordered so that the same machine — beefymac —
    // ends up holding the block in both plans. That is deliberate and it is
    // the whole subtlety: adding a device changes the apportionment, so
    // "the same plan plus an idle peer" is not something you get by
    // appending a device. What is being asserted is narrower and true: two
    // plans in which the same machine holds the same blocks digest the same,
    // however many idle machines stand alongside.
    let plan_with = |names: &[&str], host: usize| {
        let mesh = mesh_devs(names, Some(7));
        let mut i = input(
            model(1, 4, 2, 0),
            1,
            mesh.iter().map(|d| d.vram_gb).collect(),
        );
        i.host = host;
        i.mesh = Some(mesh);
        report(i)
    };

    let two = plan_with(&["beefymac", "ruggedfox"], 1);
    let three = plan_with(&["idlepeer", "ruggedfox", "beefymac"], 1);

    let holder = |r: &PlanReport| {
        let row = r
            .rows
            .iter()
            .find(|d| d.blocks.is_some())
            .expect("a holder");
        (row.dev, row.blocks, row.holds_output)
    };
    assert_eq!(holder(&two).1, holder(&three).1, "same blocks…");
    assert_eq!(
        three.rows.iter().filter(|r| r.blocks.is_none()).count(),
        2,
        "…and the three-device plan must really have two idle devices, or \
             this proves nothing"
    );
    assert_eq!(
        two.speed_key.expect("two-device key").placement_digest,
        three.speed_key.expect("three-device key").placement_digest,
    );
}

/// Why the filter above is needed at all: an idle shard, if it reached the
/// digest, would change it — and `mesh bench` builds its shards from what
/// the daemon reports is LOADED, so it has no idle device to contribute and
/// could never reproduce such a key.
#[test]
fn an_idle_shard_would_change_the_digest_if_it_reached_it() {
    let held = mm::PlacementShard {
        node_key: "beefymac".into(),
        hw: Some(0xF0F),
        blocks: Some((0, 47)),
        holds_output: true,
    };
    let idle = mm::PlacementShard {
        node_key: "idlepeer".into(),
        hw: Some(0xF0F),
        blocks: None,
        holds_output: false,
    };
    assert_ne!(
        mm::placement_digest("local", 48, &[held.clone()]),
        mm::placement_digest("local", 48, &[held, idle]),
    );
}
