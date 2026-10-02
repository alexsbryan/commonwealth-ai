// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// Any tokens-per-second figure carrying an actual number.
///
/// Deliberately not a bare `contains("tok/s")`: the hops advisor legitimately
/// says "Net tok/s depends on the host", which is prose about a tradeoff, not
/// a claim about this mesh. What must never appear unmeasured is a *number*.
fn quotes_a_rate(s: &str) -> bool {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|w| w[1].starts_with("tok/s") && w[0].chars().any(|c| c.is_ascii_digit()))
}

/// `--devices` describes hardware that is not here, so no measurement can
/// apply to it. Barred by construction, not by a runtime check.
#[test]
fn a_hypothetical_mesh_is_not_measurable() {
    let r = report(input(model(48, 1, 2, 0), 48, vec![64.0, 64.0]));
    assert!(matches!(
        r.speed,
        SpeedSection::NotMeasurable(NotMeasurable::HypotheticalDevices)
    ));
    assert!(r.speed_key.is_none(), "no key exists for absent hardware");

    let j = render_json(&r);
    assert_eq!(j["speed"]["status"], "not_measurable");
    assert_eq!(j["speed"]["reason"], "hypothetical-devices");
    assert!(j["speed"]["key"].is_null());
    assert!(render_human(&r).contains("not measurable"));
}

/// A host that advertises no fingerprint gets an honest refusal rather than
/// a placeholder key that would collide every unidentified machine.
#[test]
fn an_unidentified_host_is_not_measurable() {
    let r = report(live(mesh_devs(&["beefymac", "ruggedfox"], None)));
    assert!(matches!(
        r.speed,
        SpeedSection::NotMeasurable(NotMeasurable::HostUnidentified)
    ));
    assert_eq!(render_json(&r)["speed"]["reason"], "host-unidentified");
}

/// The host knows what it is; the peer holding half the model does not.
///
/// Keying on the peer's *name* alone would be the blind spot this field
/// closed: swap that machine's GPU, keep its name, and every number it ever
/// filed keeps answering. The refusal names the machine to go upgrade,
/// because the repair is on a different box than the one being asked.
#[test]
fn a_peer_without_a_fingerprint_is_not_measurable_and_is_named() {
    let mut devs = mesh_devs(&["beefymac", "ruggedfox"], Some(7));
    devs[0].hw_fingerprint = None;
    let r = report(live(devs));
    assert!(
        matches!(
            &r.speed,
            SpeedSection::NotMeasurable(NotMeasurable::PeerUnidentified { name })
                if name == "beefymac"
        ),
        "an unidentifiable peer must not be keyed on its name alone"
    );
    assert_eq!(
        render_json(&r)["speed"]["reason"],
        "peer-unidentified:beefymac"
    );
    let human = render_human(&r);
    assert!(human.contains("beefymac"), "name the machine: {human}");
    assert!(
        !quotes_a_rate(&human),
        "nothing may quote a rate for a placement it cannot attribute: {human}"
    );
}

/// The consumer half of the agreement, with a peer's hardware in play:
/// `mesh plan` must build the digest `mesh bench` files under, or every
/// record is written to a key nothing ever looks up.
///
/// This is the distributed counterpart to
/// `a_solo_bench_and_a_solo_plan_agree_on_the_digest` in `mesh_bench`.
#[test]
fn a_peers_hardware_reaches_the_digest_the_bench_would_file_under() {
    let with_beefy = mm::placement_digest(
        "distributed",
        48,
        &[
            mm::PlacementShard {
                node_key: "beefymac".into(),
                hw: Some(7),
                blocks: Some((0, 11)),
                holds_output: false,
            },
            mm::PlacementShard {
                node_key: "ruggedfox".into(),
                hw: Some(7),
                blocks: Some((12, 47)),
                holds_output: true,
            },
        ],
    );
    let after_gpu_swap = mm::placement_digest(
        "distributed",
        48,
        &[
            mm::PlacementShard {
                node_key: "beefymac".into(),
                hw: Some(8),
                blocks: Some((0, 11)),
                holds_output: false,
            },
            mm::PlacementShard {
                node_key: "ruggedfox".into(),
                hw: Some(7),
                blocks: Some((12, 47)),
                holds_output: true,
            },
        ],
    );
    assert_ne!(
        with_beefy, after_gpu_swap,
        "same name, same split, different silicon — a different measurement"
    );
}

/// An identified mesh with nothing recorded says so, and offers the command.
#[test]
fn an_identified_mesh_with_no_record_says_not_measured() {
    let r = report(live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))));
    let SpeedSection::NotMeasured { near } = &r.speed else {
        panic!("expected NotMeasured");
    };
    assert!(near.is_empty(), "an empty store has no near misses");

    let k = r.speed_key.as_ref().expect("an identified mesh has a key");
    assert_eq!(k.n_ctx, 32_768);
    assert_eq!(k.host_hw_fingerprint, 7);
    assert!(k.model_fingerprint.starts_with("mf1:"));
    // pd2 since 2026-07-29: the shard hashes each machine's hardware, not
    // just its name. This assertion is why the label and the construction
    // cannot drift apart unnoticed — it caught exactly that during the bump.
    assert!(k.placement_digest.starts_with("pd2:"));

    let out = render_human(&r);
    assert!(out.contains("not measured for this configuration"));
    assert!(out.contains("svrn mesh bench"));
    assert_eq!(render_json(&r)["speed"]["status"], "not_measured");
}

/// THE guard for week 1: with no measurement, no rate is quoted anywhere,
/// and every numeric field is null rather than zero. Zero is a number a
/// consumer will divide by; null is an absence it has to handle.
#[test]
fn no_rate_is_quoted_and_no_numeric_is_zero_when_unmeasured() {
    for r in [
        report(input(model(48, 1, 2, 0), 48, vec![64.0, 64.0])),
        report(live(mesh_devs(&["a", "b"], Some(7)))),
        report(live(mesh_devs(&["a", "b"], None))),
    ] {
        let out = render_human(&r);
        assert!(
            !quotes_a_rate(&out),
            "an unmeasured plan quoted a rate:\n{out}"
        );
        let s = &render_json(&r)["speed"];
        for k in [
            "decode_tok_s",
            "decode_tok_s_min",
            "decode_tok_s_max",
            "ttft_ms",
            "itl_p50_ms",
            "itl_p95_ms",
            "prefill_tok_s",
            "runs",
            "measured_at",
            "measured_build",
            "stale",
        ] {
            assert!(s[k].is_null(), "speed.{k} must be null, not a value");
        }
    }
}

/// A record filed under this exact configuration is served, and the whole
/// block is populated.
#[test]
fn a_matching_record_is_served_back() {
    let probe = report(live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))));
    let key = probe.speed_key.clone().expect("key");

    let mut file = mm::MeasurementFile::new();
    mm::record(
        &mut file,
        mm::MeasurementRecord {
            witness: None,
            conditions: None,
            key,
            decode_tok_s: 14.1,
            decode_tok_s_min: 13.9,
            decode_tok_s_max: 14.2,
            ttft_ms: 910.0,
            itl_p50_ms: 71.0,
            itl_p95_ms: 79.0,
            prefill_tok_s: None,
            cold_load_s: Some(112.3),
            trials: 3,
            content_frames: 256,
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

    let r = build_report(
        live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))),
        &file,
        &[],
        "test-build",
    );
    assert!(matches!(r.speed, SpeedSection::Measured { .. }));
    let out = render_human(&r);
    assert!(out.contains("14.1 tok/s decode"));
    assert!(out.contains("MEASURED on this exact split"));
    assert!(quotes_a_rate(&out), "a measured plan SHOULD quote a rate");

    let s = &render_json(&r)["speed"];
    assert_eq!(s["status"], "measured");
    assert_eq!(s["decode_tok_s"], 14.1);
    assert_eq!(s["runs"], 1);
    assert_eq!(s["stale"], false);
    assert!(
        s["prefill_tok_s"].is_null(),
        "unmeasured prefill stays null"
    );
}

/// The near miss says *how* the measured configuration differed, not merely
/// that it did.
///
/// This is the surface that has to carry the weight once a record can come
/// from a machine the reader has never seen: the key pins the exact split
/// and the exact silicon, so an exact hit is vanishingly unlikely, and
/// `differs by: split` gives a stranger nothing to judge with.
#[test]
fn a_near_miss_names_both_splits_when_the_record_kept_a_witness() {
    let mut key = report(live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))))
        .speed_key
        .expect("key");

    // The same model on the same two machines, cut 12/36 instead of evenly.
    let measured = mm::PlacementWitness {
        mode: "distributed".into(),
        total_blocks: 48,
        shards: vec![
            mm::PlacementShard {
                node_key: "beefymac".into(),
                hw: Some(7),
                blocks: Some((0, 11)),
                holds_output: false,
            },
            mm::PlacementShard {
                node_key: "ruggedfox".into(),
                hw: Some(7),
                blocks: Some((12, 47)),
                holds_output: true,
            },
        ],
        machines: vec![
            mm::MachineWitness {
                node_key: "beefymac".into(),
                vram_gb: 64,
                backend: Some("vulkan".into()),
            },
            mm::MachineWitness {
                node_key: "ruggedfox".into(),
                vram_gb: 64,
                backend: Some("vulkan".into()),
            },
        ],
    };
    key.placement_digest = measured.digest();

    let mut file = mm::MeasurementFile::new();
    mm::record(
        &mut file,
        mm::MeasurementRecord {
            witness: Some(measured),
            conditions: None,
            key,
            decode_tok_s: 11.7,
            decode_tok_s_min: 11.7,
            decode_tok_s_max: 11.7,
            ttft_ms: 800.0,
            itl_p50_ms: 80.0,
            itl_p95_ms: 90.0,
            prefill_tok_s: None,
            cold_load_s: None,
            trials: 3,
            content_frames: 128,
            model_name: "test.gguf".into(),
            placement_human: "36 local + 12 @beefymac".into(),
            nodes: 2,
            hops: 1,
            measured_at: 1_753_400_000,
            build: "test-build".into(),
            backend: Some("vulkan".into()),
            link_rtt_ms: None,
            verdict: mm::Verdict::Valid,
        },
    );

    let r = build_report(
        live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))),
        &file,
        &[],
        "test-build",
    );
    let SpeedSection::NotMeasured { near } = &r.speed else {
        panic!("a different split is not a hit");
    };
    assert_eq!(near[0].differs_by, vec!["split"]);
    assert_eq!(
        near[0].detail[0].theirs.as_deref(),
        Some("beefymac 12 · ruggedfox 36 +head")
    );
    let ours = near[0].detail[0]
        .ours
        .clone()
        .expect("the plan describes its own split");

    let out = render_human(&r);
    assert!(
        out.contains("measured: beefymac 12 · ruggedfox 36 +head"),
        "the human output must name the measured split, not just the facet:\n{out}"
    );
    assert!(
        out.contains(&format!("yours: {ours}")),
        "and the one being planned, to compare against:\n{out}"
    );

    let d = &render_json(&r)["speed"]["near_misses"][0]["differences"][0];
    assert_eq!(d["facet"], "split");
    assert_eq!(d["measured"], "beefymac 12 · ruggedfox 36 +head");
    assert_eq!(d["yours"], ours);
}

/// A record taken on a different split is named as context but never
/// becomes this plan's number.
#[test]
fn a_record_for_another_split_is_a_near_miss_not_an_answer() {
    let other = report(live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))));
    let mut key = other.speed_key.clone().expect("key");
    key.placement_digest = "pd1:0000000000000000".into();

    let mut file = mm::MeasurementFile::new();
    mm::record(
        &mut file,
        mm::MeasurementRecord {
            witness: None,
            conditions: None,
            key,
            decode_tok_s: 11.7,
            decode_tok_s_min: 11.7,
            decode_tok_s_max: 11.7,
            ttft_ms: 800.0,
            itl_p50_ms: 80.0,
            itl_p95_ms: 90.0,
            prefill_tok_s: None,
            cold_load_s: None,
            trials: 3,
            content_frames: 128,
            model_name: "test.gguf".into(),
            placement_human: "48 local (solo)".into(),
            nodes: 1,
            hops: 0,
            measured_at: 1_753_400_000,
            build: "test-build".into(),
            backend: Some("vulkan".into()),
            link_rtt_ms: None,
            verdict: mm::Verdict::Valid,
        },
    );

    let r = build_report(
        live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))),
        &file,
        &[],
        "test-build",
    );
    let SpeedSection::NotMeasured { near } = &r.speed else {
        panic!("a different split is not a hit");
    };
    assert_eq!(near.len(), 1);
    assert_eq!(near[0].differs_by, vec!["split"]);

    let out = render_human(&r);
    assert!(out.contains("not measured for this configuration"));
    assert!(out.contains("48 local (solo)"));
    assert!(
        out.contains("does not apply here"),
        "the other number must be explicitly disclaimed"
    );

    let s = &render_json(&r)["speed"];
    assert_eq!(s["status"], "not_measured");
    assert!(
        s["decode_tok_s"].is_null(),
        "a near miss must NEVER populate this plan's rate"
    );
    assert_eq!(s["near_misses"][0]["decode_tok_s"], 11.7);
    assert!(
        s["near_misses"][0]["taken_by"].is_null(),
        "null is this machine's own run"
    );
}

// -- Travel -------------------------------------------------------------

/// A peer's record, as serve's peer read (`mesh_travel::peer_history`) delivers it.
fn peer_record(key: mm::MeasurementKey, tok_s: f64, placement: &str) -> mm::ForeignRecord {
    mm::ForeignRecord {
        origin_node: "b88252e4325bc3771122334455667788".into(),
        origin_name: Some("BeefyMac".into()),
        record: mm::MeasurementRecord {
            witness: None,
            conditions: None,
            key,
            decode_tok_s: tok_s,
            decode_tok_s_min: tok_s - 0.1,
            decode_tok_s_max: tok_s + 0.1,
            ttft_ms: 2203.0,
            itl_p50_ms: 90.0,
            itl_p95_ms: 98.0,
            prefill_tok_s: None,
            cold_load_s: None,
            trials: 3,
            content_frames: 256,
            model_name: "test.gguf".into(),
            placement_human: placement.into(),
            nodes: 2,
            hops: 1,
            measured_at: 1_785_000_000,
            build: "test-build".into(),
            backend: Some("metal".into()),
            link_rtt_ms: None,
            verdict: mm::Verdict::Valid,
        },
    }
}

/// The whole point of travel: an empty local store still answers, because a
/// peer measured the thing being asked about.
#[test]
fn a_peer_measurement_reaches_the_plan_and_is_attributed() {
    let probe = report(live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))));
    let key = probe.speed_key.clone().expect("key");
    let file = mm::MeasurementFile::new();
    let peers = [peer_record(key, 11.08, "36 local + 12 @beefymac")];

    let r = build_report(
        live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))),
        &file,
        &peers,
        "test-build",
    );

    // Still "not measured" — `lookup` reads local records only, so a peer's
    // number never becomes this machine's measurement.
    let SpeedSection::NotMeasured { near } = &r.speed else {
        panic!("a peer's record must not be served as a local hit");
    };
    assert_eq!(near.len(), 1);
    assert_eq!(near[0].taken_by.as_deref(), Some("BeefyMac"));
    assert!(near[0].is_exact(), "same key, so nothing differs");

    let out = render_human(&r);
    assert!(out.contains("not measured for this configuration"));
    assert!(
        out.contains("BeefyMac measured this configuration: 11.1 tok/s"),
        "the peer's number must be named as theirs: {out}"
    );
    assert!(
        out.contains("their machine, so it is a report, not your measurement"),
        "and disclaimed as not the reader's own: {out}"
    );

    let s = &render_json(&r)["speed"];
    assert_eq!(s["status"], "not_measured");
    assert!(
        s["decode_tok_s"].is_null(),
        "a peer's number must NEVER populate this plan's rate"
    );
    assert_eq!(s["near_misses"][0]["taken_by"], "BeefyMac");
    assert_eq!(s["near_misses"][0]["exact"], true);
}

/// A local measurement wins the headline even when a peer also has one: the
/// reader's own hardware is the fact, the peer's is a report about it.
#[test]
fn a_local_hit_still_beats_a_peer_with_the_same_key() {
    let probe = report(live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))));
    let key = probe.speed_key.clone().expect("key");
    let mut file = mm::MeasurementFile::new();
    mm::record(&mut file, peer_record(key.clone(), 7.75, "mine").record);
    let peers = [peer_record(key, 11.08, "theirs")];

    let r = build_report(
        live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))),
        &file,
        &peers,
        "test-build",
    );
    let SpeedSection::Measured { summary } = &r.speed else {
        panic!("a local record under the asked-for key is a hit");
    };
    assert_eq!(summary.decode_tok_s, 7.75);
    let out = render_human(&r);
    assert!(
        !out.contains("11.1"),
        "a peer's faster number must not appear beside a local hit as if it \
             were an alternative reading of the same machine: {out}"
    );
}

/// A peer on a machine that differs is a near miss like any other, and the
/// facets that differ are named.
#[test]
fn a_peer_on_different_hardware_is_a_named_near_miss() {
    let probe = report(live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))));
    let mut other = probe.speed_key.clone().expect("key");
    other.host_hw_fingerprint = 0xdead_beef;
    let file = mm::MeasurementFile::new();
    let peers = [peer_record(other, 22.4, "24 local + 24 @othermac")];

    let r = build_report(
        live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))),
        &file,
        &peers,
        "test-build",
    );
    let SpeedSection::NotMeasured { near } = &r.speed else {
        panic!("different host hardware is not a hit");
    };
    assert_eq!(near[0].differs_by, vec!["host-hardware"]);
    assert!(!near[0].is_exact());

    let out = render_human(&r);
    assert!(
        out.contains("Measured by BeefyMac: 24 local + 24 @othermac → 22.4 tok/s"),
        "{out}"
    );
    assert!(out.contains("does not apply here"));
}

/// With no daemon there are no peers, and that must read exactly as it did
/// before travel existed.
#[test]
fn no_peers_is_the_pre_travel_behaviour_unchanged() {
    let file = mm::MeasurementFile::new();
    let r = build_report(
        live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))),
        &file,
        &[],
        "test-build",
    );
    let out = render_human(&r);
    assert!(out.contains("Sovereign does not quote throughput it has not measured."));
    assert!(!out.contains("Measured by"));
}

/// A record from a different build is still shown — with a warning. Hiding
/// it would cost a re-measurement for nothing.
#[test]
fn a_record_from_another_build_is_shown_and_flagged() {
    let probe = report(live(mesh_devs(&["a", "b"], Some(7))));
    let key = probe.speed_key.clone().expect("key");
    let mut file = mm::MeasurementFile::new();
    mm::record(
        &mut file,
        mm::MeasurementRecord {
            witness: None,
            conditions: None,
            key,
            decode_tok_s: 14.1,
            decode_tok_s_min: 14.0,
            decode_tok_s_max: 14.2,
            ttft_ms: 900.0,
            itl_p50_ms: 70.0,
            itl_p95_ms: 78.0,
            prefill_tok_s: None,
            cold_load_s: None,
            trials: 3,
            content_frames: 256,
            model_name: "test.gguf".into(),
            placement_human: "36/12".into(),
            nodes: 2,
            hops: 1,
            measured_at: 1_753_000_000,
            build: "0.9.1".into(),
            backend: Some("vulkan".into()),
            link_rtt_ms: None,
            verdict: mm::Verdict::Valid,
        },
    );
    let r = build_report(live(mesh_devs(&["a", "b"], Some(7))), &file, &[], "0.10.0");
    assert!(render_human(&r).contains("(!) recorded on a different build"));
    assert_eq!(render_json(&r)["speed"]["stale"], true);
}
