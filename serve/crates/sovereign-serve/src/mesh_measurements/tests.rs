// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use kernel_types::hardware_fingerprint;

mod witness;

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
        "mf1:00000000000000aa".into(),
        "pd2:00000000000000bb".into(),
        32_768,
        LinkClass::Direct,
    )
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
        "mf1:00000000000000aa".into(),
        "pd2:00000000000000bb".into(),
        8_192,
        LinkClass::Direct,
    );
    assert_ne!(small, key());
}

// --- the hypothetical bar --------------------------------------------

#[test]
fn an_unidentified_host_yields_no_identity() {
    assert!(
        HostIdentity::from_live_mesh(None).is_none(),
        "without a host fingerprint there is no key, so `--devices` cannot match a record"
    );
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
        "mf1:00000000000000aa".into(),
        "pd2:00000000000000cc".into(),
        32_768,
        LinkClass::Direct,
    );
    record(&mut f, rec_at(other.clone(), 1, 11.7, Verdict::Valid));
    for i in 0..(MAX_RUNS_PER_KEY as u64 + 3) {
        record(&mut f, rec_at(key(), 100 + i, 14.0, Verdict::Valid));
    }
    assert!(
        f.records().iter().any(|r| r.key == other),
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
    assert_eq!(back.records(), f.records());
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
