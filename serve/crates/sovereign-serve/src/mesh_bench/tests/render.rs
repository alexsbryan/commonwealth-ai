// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn a_record(verdict: mm::Verdict) -> mm::MeasurementRecord {
    mm::MeasurementRecord {
        witness: None,
        conditions: None,
        key: mm::MeasurementKey {
            probe_version: mm::PROBE_VERSION,
            model_fingerprint: "mf1:deadbeefdeadbeef".into(),
            placement_digest: "pd1:0123456789abcdef".into(),
            host_hw_fingerprint: 12345,
            n_ctx: 16384,
            link: mm::LinkClass::Local,
        },
        decode_tok_s: 14.1,
        decode_tok_s_min: 13.9,
        decode_tok_s_max: 14.4,
        ttft_ms: 820.0,
        itl_p50_ms: 70.9,
        itl_p95_ms: 91.2,
        prefill_tok_s: None,
        cold_load_s: Some(112.0),
        trials: 3,
        content_frames: 240,
        model_name: "Qwen3.5-122B".into(),
        placement_human: "36 local + 12 @BeefyMac".into(),
        nodes: 2,
        hops: 1,
        measured_at: 1_785_000_000,
        build: "0.1.0".into(),
        backend: Some("vulkan".into()),
        link_rtt_ms: None,
        verdict,
    }
}

/// A run that reached the mesh. The happy path, so the renderers are exercised
/// with the line present rather than with the degraded one.
fn published() -> crate::mesh_travel::Published {
    crate::mesh_travel::Published::Yes {
        key: "1785000000/aabbccdd11223344".into(),
    }
}

#[test]
fn an_absent_prefill_renders_as_na_never_as_a_number() {
    let out = render_bench_human(&a_record(mm::Verdict::Valid), "recorded", &published());
    assert!(out.contains("n/a (server omits stream usage)"), "{out}");
    let j = render_bench_json(&a_record(mm::Verdict::Valid), "recorded", &published());
    assert!(
        j["prefill_tok_s"].is_null(),
        "a consumer must handle the absence rather than divide by a fabrication"
    );
}

#[test]
fn an_invalid_run_leads_with_its_problems_and_keeps_its_numbers() {
    let r = a_record(mm::Verdict::Invalid {
        problems: vec!["WRONG SLOT: served by `fast`".into()],
    });
    let out = render_bench_human(&r, "recorded", &published());
    assert!(out.contains("INVALID"), "{out}");
    assert!(out.contains("WRONG SLOT"), "{out}");
    assert!(
        out.contains("14.10 tok/s"),
        "the numbers stay visible so the failure is inspectable: {out}"
    );
    assert_eq!(
        render_bench_json(&r, "recorded", &published())["verdict"],
        "invalid"
    );
}

#[test]
fn json_carries_the_key_so_a_plan_can_be_correlated_with_a_run() {
    let j = render_bench_json(&a_record(mm::Verdict::Valid), "recorded", &published());
    assert_eq!(j["key"]["placement_digest"], "pd1:0123456789abcdef");
    assert_eq!(j["key"]["model_fingerprint"], "mf1:deadbeefdeadbeef");
    assert_eq!(j["key"]["probe_version"], mm::PROBE_VERSION);
    assert_eq!(j["verdict"], "valid");
    assert!(j["link_rtt_ms"].is_null());
}

#[test]
fn placement_human_names_the_daemons_own_mode_when_it_is_not_the_plain_one() {
    let shards = vec![mm::PlacementShard {
        node_key: "RuggedFox".into(),
        hw: Some(0xF0F),
        blocks: Some((0, 47)),
        holds_output: true,
    }];
    assert_eq!(placement_human(&shards, "RuggedFox", "local"), "48 local");
    assert_eq!(
        placement_human(&shards, "RuggedFox", "child-distributed"),
        "48 local (child-distributed)",
        "a compute-child load must not silently read as a plain local one"
    );
}

#[test]
fn placement_human_puts_the_host_first_then_the_workers() {
    let shards = vec![
        mm::PlacementShard {
            node_key: "BeefyMac".into(),
            hw: Some(0xF0F),
            blocks: Some((0, 11)),
            holds_output: false,
        },
        mm::PlacementShard {
            node_key: "RuggedFox".into(),
            hw: Some(0xF0F),
            blocks: Some((12, 47)),
            holds_output: true,
        },
    ];
    assert_eq!(
        placement_human(&shards, "RuggedFox", "distributed"),
        "36 local + 12 @BeefyMac"
    );
}

// ---------------------------------------------------------------------------
// Run conditions — read from /status, so a run can say what else was running
// ---------------------------------------------------------------------------

/// The real shape, trimmed: `fast` and `embed` resident in-process, the primary
/// hosted by a compute child (so its own `resident` flag is `false`).
fn status_with_child_primary() -> serde_json::Value {
    serde_json::json!({
        "inference": {
            "resident": [
                {"role": "fast",    "model_id": "Qwen3.5-0.8B", "resident": true},
                {"role": "embed",   "model_id": "Qwen3-Embed",   "resident": true},
                {"role": "primary", "model_id": "Qwen3.5-122B",  "resident": false}
            ],
            "compute_children": [
                {"model_id": "Qwen3.5-122B", "lifecycle": "serving"}
            ]
        },
        "process": {"uptime_seconds": 2320, "rss_mb": 4187, "peak_rss_mb": 4329}
    })
}

#[test]
fn co_residents_exclude_the_primary_and_come_back_sorted() {
    assert_eq!(
        co_resident_roles(&status_with_child_primary(), "Qwen3.5-122B"),
        vec!["embed".to_string(), "fast".to_string()]
    );
}

/// The child-hosted mode is the one that matters on this fleet, and it is the
/// one an obvious implementation gets wrong: the primary runs in the child, so
/// walking `compute_children` too would list the measured model as competing
/// with itself.
#[test]
fn a_child_hosted_primary_is_never_its_own_co_resident() {
    let roles = co_resident_roles(&status_with_child_primary(), "Qwen3.5-122B");
    assert!(
        !roles.iter().any(|r| r == "primary"),
        "the measured model is not a co-resident: {roles:?}"
    );
    assert!(
        !roles.iter().any(|r| r.contains("122B")),
        "a child must not leak in under any name: {roles:?}"
    );
}

#[test]
fn a_slot_that_is_not_resident_is_not_counted() {
    let body = serde_json::json!({
        "inference": {"resident": [
            {"role": "fast",  "resident": false},
            {"role": "embed", "resident": true}
        ]}
    });
    assert_eq!(
        co_resident_roles(&body, "Qwen3.5-122B"),
        vec!["embed".to_string()]
    );
}

/// An unreadable `/status` yields no co-residents — and the caller must not read
/// that as "the box was quiet". `RunConditions::describe` is what distinguishes
/// them, which is why the empty case is a tested finding there.
#[test]
fn an_unreadable_status_yields_no_roles_rather_than_a_panic() {
    assert!(co_resident_roles(&serde_json::json!({}), "any").is_empty());
    assert!(co_resident_roles(&serde_json::json!({"inference": {}}), "any").is_empty());
    assert!(co_resident_roles(&serde_json::json!(null), "any").is_empty());
}

#[test]
fn rss_is_read_from_the_process_block() {
    assert_eq!(rss_mb_from_status(&status_with_child_primary()), Some(4187));
    assert_eq!(rss_mb_from_status(&serde_json::json!({})), None);
    assert_eq!(
        rss_mb_from_status(&serde_json::json!({"process": {}})),
        None,
        "a missing field is not a zero"
    );
}

// ---------------------------------------------------------------------------
// Placement ambiguity — the misreading that cost a false 26% variance
// ---------------------------------------------------------------------------

#[test]
fn a_digest_abbreviates_to_its_tag_and_eight_hex() {
    assert_eq!(abbreviated_digest("pd2:e45d4afa9fdc7cf3"), "pd2:e45d4afa");
    assert_eq!(abbreviated_digest("pd2:5019ecfb0aa9cf76"), "pd2:5019ecfb");
    assert_eq!(
        abbreviated_digest("pd2:abc"),
        "pd2:abc",
        "short hash survives"
    );
    assert_eq!(abbreviated_digest("nocolon"), "nocolon");
}

/// Two runs describing the same split under different keys are usually NOT
/// adjacent — the store orders by key, so other rows sit between them. An
/// adjacent-pairs check would have missed exactly the case this exists for.
#[test]
fn ambiguous_placement_is_detected_across_non_adjacent_rows() {
    let mut a = a_record(mm::Verdict::Valid);
    a.placement_human = "36 local + 12 @BeefyMac".into();
    a.key.placement_digest = "pd2:5019ecfb0aa9cf76".into();

    let mut middle = a_record(mm::Verdict::Valid);
    middle.placement_human = "48 local".into();
    middle.key.placement_digest = "pd2:1111111111111111".into();

    let mut b = a_record(mm::Verdict::Valid);
    b.placement_human = "36 local + 12 @BeefyMac".into();
    b.key.placement_digest = "pd2:e45d4afa9fdc7cf3".into();

    let rows = vec![&a, &middle, &b];
    assert!(
        has_ambiguous_placement(&rows),
        "same description, different keys, two rows apart"
    );
}

#[test]
fn one_description_under_one_key_is_not_ambiguous() {
    let mut a = a_record(mm::Verdict::Valid);
    a.placement_human = "36 local + 12 @BeefyMac".into();
    a.key.placement_digest = "pd2:e45d4afa9fdc7cf3".into();
    let b = a.clone();
    let rows = vec![&a, &b];
    assert!(
        !has_ambiguous_placement(&rows),
        "two runs of ONE configuration are the comparable case, not the ambiguous one"
    );
}

#[test]
fn different_descriptions_are_never_ambiguous() {
    let mut a = a_record(mm::Verdict::Valid);
    a.placement_human = "48 local".into();
    a.key.placement_digest = "pd2:1111111111111111".into();
    let mut b = a_record(mm::Verdict::Valid);
    b.placement_human = "36 local + 12 @BeefyMac".into();
    b.key.placement_digest = "pd2:e45d4afa9fdc7cf3".into();
    let rows = vec![&a, &b];
    assert!(!has_ambiguous_placement(&rows));
}

/// Observed live 2026-07-29, and it inverts the experiment if unhandled: with
/// `[models].fast` commented out, `fast_path()` falls back to the primary GGUF
/// and `/status` reports a `fast` slot holding the SAME model. Filtering on the
/// role name alone would record the measured model as its own co-resident, so a
/// run taken with the slot EVICTED would look like a co-resident run.
#[test]
fn a_fast_slot_aliased_to_the_primary_is_not_a_co_resident() {
    let body = serde_json::json!({
        "inference": {"resident": [
            {"role": "fast",    "model_id": "Qwen3.6-35B-A3B-MTP-UD-Q6_K", "resident": true},
            {"role": "primary", "model_id": "Qwen3.6-35B-A3B-MTP-UD-Q6_K", "resident": true},
            {"role": "embed",   "model_id": "Qwen3-Embedding-0.6B-Q8_0",   "resident": true}
        ]}
    });
    assert_eq!(
        co_resident_roles(&body, "Qwen3.6-35B-A3B-MTP-UD-Q6_K"),
        vec!["embed".to_string()],
        "the aliased `fast` slot IS the measured model, not something competing with it"
    );
}

/// The other half of the same rule: a genuinely distinct fast model must still
/// be counted, or the fix above would suppress every real co-resident.
#[test]
fn a_distinct_fast_model_is_still_a_co_resident() {
    let body = serde_json::json!({
        "inference": {"resident": [
            {"role": "fast",    "model_id": "Qwen3.5-0.8B-UD-Q6_K_XL",      "resident": true},
            {"role": "primary", "model_id": "Qwen3.6-35B-A3B-MTP-UD-Q6_K", "resident": true}
        ]}
    });
    assert_eq!(
        co_resident_roles(&body, "Qwen3.6-35B-A3B-MTP-UD-Q6_K"),
        vec!["fast".to_string()]
    );
}
