// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use kernel_types::hardware_fingerprint;

mod witness;

fn sizes() -> Vec<(String, Option<u32>, u64)> {
    vec![
        ("blk.0.attn_q.weight".into(), Some(0), 1_000),
        ("blk.1.ffn_gate.weight".into(), Some(1), 2_000),
        ("output.weight".into(), None, 3_000),
    ]
}

fn shards() -> Vec<PlacementShard> {
    vec![
        PlacementShard {
            node_key: "beefymac".into(),
            hw: Some(0xBEEF),
            blocks: Some((0, 11)),
            holds_output: false,
        },
        PlacementShard {
            node_key: "ruggedfox".into(),
            hw: Some(0xF0F),
            blocks: Some((12, 47)),
            holds_output: true,
        },
    ]
}

fn key() -> MeasurementKey {
    MeasurementKey::for_plan(
        HostIdentity::from_live_mesh(Some(42)).unwrap(),
        model_fingerprint(&sizes(), 48),
        placement_digest("distributed", 48, &shards()),
        32_768,
        LinkClass::Direct,
    )
}

/// [`key`] over a tunnel instead of a direct link. Everything else — model,
/// split, host, context — is byte-identical.
fn key_tunnelled() -> MeasurementKey {
    MeasurementKey::for_plan(
        HostIdentity::from_live_mesh(Some(42)).unwrap(),
        model_fingerprint(&sizes(), 48),
        placement_digest("distributed", 48, &shards()),
        32_768,
        LinkClass::Tunnel,
    )
}

// --- link classification -------------------------------------------------

#[test]
fn loopback_endpoints_are_tunnels_and_routable_ones_are_direct() {
    for ep in [
        "127.0.0.1:50052",
        "127.0.0.53:50052",
        "localhost:50052",
        "LOCALHOST:50052",
        "[::1]:50052",
    ] {
        assert_eq!(
            link_class_of_endpoint(ep),
            LinkClass::Tunnel,
            "{ep} is a loopback proxy — the far end is a tunnel"
        );
    }
    for ep in [
        "192.168.1.2:50052",
        "100.104.36.28:50052",
        "beefymac.local:50052",
        "[fd7a:115c:a1e0::a3a:241c]:50052",
    ] {
        assert_eq!(
            link_class_of_endpoint(ep),
            LinkClass::Direct,
            "{ep} is routable — ggml dials it directly"
        );
    }
}

/// A bare IPv6 literal has no port, so it must not be truncated at its
/// first colon. `::1` splitting to an empty host would classify the
/// loopback address as `Unknown` instead of `Tunnel`.
#[test]
fn bare_ipv6_is_not_truncated_at_its_first_colon() {
    assert_eq!(link_class_of_endpoint("::1"), LinkClass::Tunnel);
    assert_eq!(
        link_class_of_endpoint("fd7a:115c:a1e0::a3a:241c"),
        LinkClass::Direct
    );
    assert_eq!(link_class_of_endpoint(""), LinkClass::Unknown);
    assert_eq!(link_class_of_endpoint(":50052"), LinkClass::Unknown);
}

#[test]
fn summarize_takes_the_worst_link_and_local_means_no_workers() {
    use LinkClass::*;
    assert_eq!(LinkClass::summarize(&[]), Local);
    assert_eq!(LinkClass::summarize(&[Direct, Direct]), Direct);
    // One tunnelled hop gates the whole pipeline.
    assert_eq!(LinkClass::summarize(&[Direct, Tunnel]), Tunnel);
    // Unknown dominates even a tunnel: we cannot attribute the run at all.
    assert_eq!(LinkClass::summarize(&[Tunnel, Unknown]), Unknown);
    assert_eq!(LinkClass::summarize(&[Direct, Unknown]), Unknown);
}

// --- the link is part of the identity ------------------------------------

/// The defect this field exists to prevent, stated as a test.
///
/// Same model, same split, same host, same context — measured once over a
/// tunnel. Asking about the direct-link configuration must NOT return that
/// number. On this fleet the two differ by ~2.3×, so serving one for the
/// other is not a rounding error, it is a wrong answer delivered
/// confidently.
#[test]
fn a_tunnelled_measurement_is_never_served_for_a_direct_plan() {
    let mut f = MeasurementFile::new();
    record(&mut f, rec_at(key_tunnelled(), 100, 17.35, Verdict::Valid));

    assert!(
        lookup(&f, &key(), "0.10.0").is_none(),
        "the direct-link plan must not be answered by a tunnelled run"
    );
    assert_eq!(
        lookup(&f, &key_tunnelled(), "0.10.0")
            .expect("the tunnelled configuration WAS measured")
            .decode_tok_s,
        17.35
    );
}

/// …and the operator is told why, rather than just "no data".
#[test]
fn a_link_mismatch_surfaces_as_a_near_miss_naming_the_link() {
    let mut f = MeasurementFile::new();
    record(&mut f, rec_at(key_tunnelled(), 100, 17.35, Verdict::Valid));

    let near = near_misses(&f, &[], &key(), None);
    assert_eq!(near.len(), 1, "the tunnelled run is a near miss");
    assert_eq!(near[0].differs_by, vec!["link"]);
    assert_eq!(near[0].decode_tok_s, 17.35);
}

/// `Unknown` is an absence of evidence, not a value. Two runs nobody could
/// classify are not thereby the same run.
#[test]
fn an_unknown_link_never_matches_even_another_unknown() {
    let unknown = MeasurementKey::for_plan(
        HostIdentity::from_live_mesh(Some(42)).unwrap(),
        model_fingerprint(&sizes(), 48),
        placement_digest("distributed", 48, &shards()),
        32_768,
        LinkClass::Unknown,
    );
    let mut f = MeasurementFile::new();
    record(&mut f, rec_at(unknown.clone(), 100, 14.1, Verdict::Valid));

    assert!(
        lookup(&f, &unknown, "0.10.0").is_none(),
        "an unclassifiable link cannot be answered, even by another one"
    );
    // But the record is still visible as a near miss, so nothing is hidden.
    assert!(!near_misses(&f, &[], &key(), None).is_empty());
}

fn rec_at(k: MeasurementKey, at: u64, tok_s: f64, verdict: Verdict) -> MeasurementRecord {
    MeasurementRecord {
        key: k,
        decode_tok_s: tok_s,
        decode_tok_s_min: tok_s - 0.2,
        decode_tok_s_max: tok_s + 0.1,
        ttft_ms: 910.0,
        itl_p50_ms: 71.0,
        itl_p95_ms: 79.0,
        prefill_tok_s: None,
        cold_load_s: Some(112.3),
        trials: 3,
        content_frames: 256,
        model_name: "Qwen3.5-122B".into(),
        placement_human: "36 local + 12 @beefymac".into(),
        nodes: 2,
        hops: 1,
        measured_at: at,
        build: "0.10.0".into(),
        backend: Some("vulkan".into()),
        link_rtt_ms: Some(0.4),
        verdict,
        witness: None,
        conditions: None,
    }
}

/// The witness that explains [`key`]'s placement digest.
fn witness() -> PlacementWitness {
    PlacementWitness {
        mode: "distributed".into(),
        total_blocks: 48,
        shards: shards(),
        machines: vec![
            MachineWitness {
                node_key: "beefymac".into(),
                vram_gb: 51,
                backend: Some("metal".into()),
            },
            MachineWitness {
                node_key: "ruggedfox".into(),
                vram_gb: 128,
                backend: Some("vulkan".into()),
            },
        ],
    }
}

// --- key discrimination: too-coarse failures -------------------------

#[test]
fn key_changes_when_split_changes() {
    let a = placement_digest("distributed", 48, &shards());
    let mut moved = shards();
    moved[0].blocks = Some((0, 17));
    moved[1].blocks = Some((18, 47));
    let b = placement_digest("distributed", 48, &moved);
    assert_ne!(a, b, "a 36/12 split must not match a 30/18 one");
}

#[test]
fn key_changes_when_worker_identity_changes() {
    let a = placement_digest("distributed", 48, &shards());
    let mut other = shards();
    other[0].node_key = "someone-elses-mac".into();
    let b = placement_digest("distributed", 48, &other);
    assert_ne!(
        a, b,
        "the same split on a different peer is a different measurement"
    );
}

/// The digest must announce the generation it was actually built with.
///
/// Written because the `pd1`→`pd2` bump half-landed: the hash input changed
/// and the printed label did not, so for one build every digest was new
/// bytes wearing the old name — the exact confusion the prefix exists to
/// prevent, and invisible to every other test here because they all compare
/// digests to each other rather than to a literal. When the construction
/// changes again, change this literal in the same commit.
#[test]
fn the_digest_label_matches_the_generation_that_produced_it() {
    let d = placement_digest("distributed", 48, &shards());
    assert!(
        d.starts_with("pd2:"),
        "hashing `hw` is the pd2 construction; a pd1 label on it would tell a \
             reader these digests are comparable with older ones: {d}"
    );
}

/// The blind spot this field exists to close.
///
/// Same peer name, same split, same everything a `pd1` digest could see —
/// different silicon. Before `hw` was part of the shard these two hashed
/// identically, so the number measured on the old GPU answered for the new
/// one. A name is not hardware.
#[test]
fn key_changes_when_a_peer_swaps_hardware_but_keeps_its_name() {
    let a = placement_digest("distributed", 48, &shards());
    let mut regunned = shards();
    regunned[0].hw = Some(0xDEAD);
    let b = placement_digest("distributed", 48, &regunned);
    assert_eq!(
        regunned[0].node_key,
        shards()[0].node_key,
        "precondition: the peer kept its name — only the silicon changed"
    );
    assert_ne!(
        a, b,
        "the same split on the same peer's NEW hardware is a different measurement"
    );
}

/// A machine that never said what it is must not be confused with one that
/// did — in either direction. Both callers refuse to build a key from an
/// unfingerprinted shard, so this is the backstop for that promise rather
/// than a path production takes.
#[test]
fn an_unfingerprinted_shard_never_collides_with_a_fingerprinted_one() {
    let known = placement_digest("distributed", 48, &shards());
    let mut anonymous = shards();
    anonymous[0].hw = None;
    assert_ne!(
        known,
        placement_digest("distributed", 48, &anonymous),
        "absence of a fingerprint must not hash like the presence of one"
    );
}

#[test]
fn key_changes_between_solo_and_distributed() {
    let solo = placement_digest("local", 48, &shards());
    let dist = placement_digest("distributed", 48, &shards());
    assert_ne!(solo, dist);
}

#[test]
fn key_changes_when_host_hardware_changes() {
    let a = hardware_fingerprint(32, 128, &[("Radeon 8060S".into(), 128, "vulkan".into())]);
    let b = hardware_fingerprint(32, 128, &[("RTX 4090".into(), 24, "cuda".into())]);
    assert_ne!(a, b);
}

#[test]
fn key_changes_when_backend_changes_on_identical_silicon() {
    let vulkan = hardware_fingerprint(32, 128, &[("Radeon 8060S".into(), 128, "vulkan".into())]);
    let rocm = hardware_fingerprint(32, 128, &[("Radeon 8060S".into(), 128, "rocm".into())]);
    assert_ne!(
        vulkan, rocm,
        "same GPU under a different backend runs at a different rate — the key must break"
    );
}

#[test]
fn key_changes_when_ctx_changes() {
    let small = MeasurementKey::for_plan(
        HostIdentity::from_live_mesh(Some(42)).unwrap(),
        model_fingerprint(&sizes(), 48),
        placement_digest("distributed", 48, &shards()),
        8_192,
        LinkClass::Direct,
    );
    assert_ne!(small, key());
}

#[test]
fn model_fingerprint_is_quant_sensitive() {
    let mut requantised = sizes();
    requantised[0].2 = 1_500;
    assert_ne!(
        model_fingerprint(&sizes(), 48),
        model_fingerprint(&requantised, 48)
    );
}

// --- key stability: too-fine failures --------------------------------

#[test]
fn model_fingerprint_is_order_independent() {
    let mut permuted = sizes();
    permuted.reverse();
    assert_eq!(
        model_fingerprint(&sizes(), 48),
        model_fingerprint(&permuted, 48)
    );
}

#[test]
fn placement_digest_is_order_independent() {
    let mut permuted = shards();
    permuted.reverse();
    assert_eq!(
        placement_digest("distributed", 48, &shards()),
        placement_digest("distributed", 48, &permuted)
    );
}

#[test]
fn model_fingerprint_is_stable_across_repeated_reads() {
    assert_eq!(
        model_fingerprint(&sizes(), 48),
        model_fingerprint(&sizes(), 48)
    );
}

// --- the hypothetical bar --------------------------------------------

#[test]
fn an_unidentified_host_yields_no_identity() {
    assert!(
        HostIdentity::from_live_mesh(None).is_none(),
        "without a host fingerprint there is no key, so `--devices` cannot match a record"
    );
}

// --- lookup ----------------------------------------------------------

#[test]
fn lookup_returns_none_for_an_unmeasured_key() {
    let f = MeasurementFile::new();
    assert!(lookup(&f, &key(), "0.10.0").is_none());
}

#[test]
fn lookup_ignores_invalid_runs_under_the_exact_key() {
    let mut f = MeasurementFile::new();
    record(
        &mut f,
        rec_at(
            key(),
            100,
            14.1,
            Verdict::Invalid {
                problems: vec!["peer went offline mid-run".into()],
            },
        ),
    );
    assert!(
        lookup(&f, &key(), "0.10.0").is_none(),
        "a run that tripped a guard is kept for glassbox but must never be served"
    );
}

#[test]
fn lookup_reports_the_median_run_and_the_observed_spread_of_runs() {
    let mut f = MeasurementFile::new();
    record(&mut f, rec_at(key(), 100, 13.0, Verdict::Valid));
    record(&mut f, rec_at(key(), 200, 14.1, Verdict::Valid));
    let s = lookup(&f, &key(), "0.10.0").expect("two valid runs");
    assert_eq!(s.runs, 2);
    assert!(
        (s.decode_tok_s - 13.0).abs() < 1e-9,
        "even count: the lower middle, a run that actually happened"
    );
    assert!(
        (s.decode_tok_s_min - 13.0).abs() < 1e-9,
        "min is the slowest RUN, not the slowest trial"
    );
    assert!(
        (s.decode_tok_s_max - 14.1).abs() < 1e-9,
        "max is the fastest RUN, not the fastest trial"
    );
    assert!(!s.stale);
}

#[test]
fn one_outlier_run_cannot_set_the_headline_or_arrive_last_and_steal_it() {
    // The real store that forced the policy: 7.75/8.38/8.53/11.08, where
    // the 11.08 was one coherently-fast outlier and the OLD latest-run
    // headline would have quoted whatever ran most recently.
    let mut f = MeasurementFile::new();
    record(&mut f, rec_at(key(), 100, 7.75, Verdict::Valid));
    record(&mut f, rec_at(key(), 200, 8.53, Verdict::Valid));
    record(&mut f, rec_at(key(), 300, 8.38, Verdict::Valid));
    record(&mut f, rec_at(key(), 400, 11.08, Verdict::Valid));
    let s = lookup(&f, &key(), "0.10.0").expect("four valid runs");
    assert_eq!(s.runs, 4);
    assert!(
        (s.decode_tok_s - 8.38).abs() < 1e-9,
        "median run headlines even though the outlier arrived last"
    );
    assert!((s.decode_tok_s_min - 7.75).abs() < 1e-9);
    assert!(
        (s.decode_tok_s_max - 11.08).abs() < 1e-9,
        "the outlier is still visible in the observed range, just not the headline"
    );
    assert!(
        (s.measured_at as i64 - 300).abs() < 1,
        "companion fields travel with the median run, not the latest"
    );
}

#[test]
fn a_record_from_another_build_is_served_but_flagged_stale() {
    let mut f = MeasurementFile::new();
    record(&mut f, rec_at(key(), 100, 14.1, Verdict::Valid));
    let s = lookup(&f, &key(), "0.11.0").expect("still served");
    assert!(
        s.stale,
        "a build change is a warning, not a reason to hide the number"
    );
    assert_eq!(s.measured_build, "0.10.0");
}

// --- near misses ------------------------------------------------------

#[test]
fn a_near_miss_names_the_other_config_and_carries_no_rate_for_ours() {
    let mut f = MeasurementFile::new();
    record(&mut f, rec_at(key(), 100, 14.1, Verdict::Valid));

    let mut moved = shards();
    moved[0].blocks = Some((0, 17));
    moved[1].blocks = Some((18, 47));
    let asked = MeasurementKey::for_plan(
        HostIdentity::from_live_mesh(Some(42)).unwrap(),
        model_fingerprint(&sizes(), 48),
        placement_digest("distributed", 48, &moved),
        32_768,
        LinkClass::Direct,
    );

    assert!(
        lookup(&f, &asked, "0.10.0").is_none(),
        "the configuration asked about was never measured"
    );
    let misses = near_misses(&f, &[], &asked, None);
    assert_eq!(misses.len(), 1);
    assert_eq!(misses[0].differs_by, vec!["split"]);
    assert!((misses[0].decode_tok_s - 14.1).abs() < 1e-9);
}

#[test]
fn a_different_model_is_not_a_near_miss() {
    let mut f = MeasurementFile::new();
    record(&mut f, rec_at(key(), 100, 14.1, Verdict::Valid));

    let mut other_model = sizes();
    other_model[0].2 = 9_999;
    let asked = MeasurementKey::for_plan(
        HostIdentity::from_live_mesh(Some(42)).unwrap(),
        model_fingerprint(&other_model, 48),
        placement_digest("distributed", 48, &shards()),
        32_768,
        LinkClass::Direct,
    );
    assert!(near_misses(&f, &[], &asked, None).is_empty());
}

#[test]
fn an_exact_hit_is_not_also_reported_as_a_near_miss() {
    let mut f = MeasurementFile::new();
    record(&mut f, rec_at(key(), 100, 14.1, Verdict::Valid));
    assert!(near_misses(&f, &[], &key(), None).is_empty());
}

#[test]
fn an_invalid_run_is_not_a_near_miss_either() {
    let mut f = MeasurementFile::new();
    record(
        &mut f,
        rec_at(
            key(),
            100,
            14.1,
            Verdict::Invalid {
                problems: vec!["only 10 content frames".into()],
            },
        ),
    );
    let mut moved = shards();
    moved[0].blocks = Some((0, 17));
    let asked = MeasurementKey::for_plan(
        HostIdentity::from_live_mesh(Some(42)).unwrap(),
        model_fingerprint(&sizes(), 48),
        placement_digest("distributed", 48, &moved),
        32_768,
        LinkClass::Direct,
    );
    assert!(near_misses(&f, &[], &asked, None).is_empty());
}

// --- record retention -------------------------------------------------

#[test]
fn runs_accumulate_then_evict_oldest_first_per_key() {
    let mut f = MeasurementFile::new();
    for i in 0..(MAX_RUNS_PER_KEY as u64 + 3) {
        record(&mut f, rec_at(key(), 100 + i, 14.0, Verdict::Valid));
    }
    assert_eq!(f.records().len(), MAX_RUNS_PER_KEY);
    let oldest = f.records().iter().map(|r| r.measured_at).min().unwrap();
    assert_eq!(oldest, 103, "the three oldest runs were evicted");
}

#[test]
fn eviction_is_scoped_to_one_key() {
    let mut f = MeasurementFile::new();
    let other = MeasurementKey::for_plan(
        HostIdentity::from_live_mesh(Some(42)).unwrap(),
        model_fingerprint(&sizes(), 48),
        placement_digest("local", 48, &shards()),
        32_768,
        LinkClass::Direct,
    );
    record(&mut f, rec_at(other.clone(), 1, 11.7, Verdict::Valid));
    for i in 0..(MAX_RUNS_PER_KEY as u64 + 3) {
        record(&mut f, rec_at(key(), 100 + i, 14.0, Verdict::Valid));
    }
    assert!(
        lookup(&f, &other, "0.10.0").is_some(),
        "a busy configuration must not push another one's history out"
    );
}

// --- persistence ------------------------------------------------------

#[test]
fn a_store_round_trips() {
    let mut f = MeasurementFile::new();
    record(&mut f, rec_at(key(), 100, 14.1, Verdict::Valid));
    let json = serde_json::to_string(&f).unwrap();
    let back = parse(&json);
    assert_eq!(
        lookup(&back, &key(), "0.10.0"),
        lookup(&f, &key(), "0.10.0")
    );
}

#[test]
fn a_store_from_an_incompatible_schema_is_discarded_not_misread() {
    let json = r#"{"schema_version":9999,"records":[]}"#;
    assert!(parse(json).records().is_empty());
}

#[test]
fn an_unreadable_store_degrades_to_empty_rather_than_failing() {
    assert!(parse("{ this is not json").records().is_empty());
}

#[test]
fn an_invalid_verdict_round_trips_with_its_problems() {
    let r = rec_at(
        key(),
        100,
        0.0,
        Verdict::Invalid {
            problems: vec!["served by the wrong model".into()],
        },
    );
    let back: MeasurementRecord =
        serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(back.verdict, r.verdict);
}
