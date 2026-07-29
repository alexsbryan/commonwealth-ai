// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unit tests for `mesh bench`'s pure core.
//!
//! The point of the seam is that all nine validity guards can be made to fire
//! here — with no daemon, no peer, no GPU and no model. A guard that cannot be
//! tested is a guard nobody knows still works, and the shell script this
//! command replaces had exactly that problem: its guards could only be
//! exercised by reproducing the failure they were written for.

use super::*;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// An SSE content frame at `t` seconds, attributed to `model`.
fn content(t: f64, model: &str, piece: &str) -> Frame {
    Frame::from_line(
        t,
        &format!(
            r#"data: {{"model":"{model}","choices":[{{"index":0,"delta":{{"content":"{piece}"}},"finish_reason":null}}]}}"#
        ),
    )
}

/// The terminal frame, with an optional usage block.
fn finish(t: f64, model: &str, reason: &str, prompt_tokens: Option<u32>) -> Frame {
    let usage = match prompt_tokens {
        Some(p) => format!(r#","usage":{{"prompt_tokens":{p},"completion_tokens":1,"total_tokens":{}}}"#, p + 1),
        None => String::new(),
    };
    Frame::from_line(
        t,
        &format!(
            r#"data: {{"model":"{model}","choices":[{{"index":0,"delta":{{}},"finish_reason":"{reason}"}}]{usage}}}"#
        ),
    )
}

/// `n` content frames one `gap` apart starting at `t0`, then a terminal frame.
fn stream(n: u32, t0: f64, gap: f64, model: &str, reason: &str) -> Vec<Frame> {
    let mut f: Vec<Frame> = (0..n)
        .map(|i| content(t0 + gap * i as f64, model, "x"))
        .collect();
    f.push(finish(t0 + gap * n as f64, model, reason, Some(12)));
    f.push(Frame::from_line(t0 + gap * n as f64, "data: [DONE]"));
    f
}

/// A trial that passes every per-trial guard, at roughly `rate` tok/s.
fn good_trial(rate: f64) -> Trial {
    parse_trial(&stream(40, 0.5, 1.0 / rate, "primary-model", "stop"))
}

/// A guard input where every guard passes, so a test can break exactly one
/// thing and attribute the resulting problem to it.
struct Scenario {
    trials: Vec<Trial>,
    primary_model_id: String,
    primary_serving_before: bool,
    primary_serving_after: Option<bool>,
    canary_tokens: u32,
    placement_before: PlacementSnapshot,
    placement_after: Option<PlacementSnapshot>,
    peers_before: Vec<(String, bool)>,
    peers_after: Vec<(String, bool)>,
    host_alive_after: HostLiveness,
}

impl Scenario {
    fn clean() -> Self {
        let p = PlacementSnapshot {
            mode: "local".into(),
            total_blocks: 0,
            local_blocks: 0,
            workers: Vec::new(),
        };
        Self {
            trials: vec![good_trial(10.0), good_trial(10.2), good_trial(9.9)],
            primary_model_id: "primary-model".into(),
            primary_serving_before: true,
            primary_serving_after: Some(true),
            canary_tokens: 8,
            placement_before: p.clone(),
            placement_after: Some(p),
            peers_before: Vec::new(),
            peers_after: Vec::new(),
            host_alive_after: HostLiveness::Alive,
        }
    }

    fn judge(&self) -> Vec<String> {
        evaluate_guards(&GuardInput {
            trials: &self.trials,
            primary_model_id: &self.primary_model_id,
            primary_serving_before: self.primary_serving_before,
            primary_serving_after: self.primary_serving_after,
            canary_tokens: self.canary_tokens,
            placement_before: &self.placement_before,
            placement_after: self.placement_after.as_ref(),
            peers_before: &self.peers_before,
            peers_after: &self.peers_after,
            host_alive_after: self.host_alive_after,
        })
    }
}

/// Assert exactly one problem fired and that it mentions `needle`.
fn only_problem(problems: &[String], needle: &str) {
    assert_eq!(
        problems.len(),
        1,
        "expected exactly one problem, got {problems:#?}"
    );
    assert!(
        problems[0].contains(needle),
        "problem did not mention {needle:?}: {}",
        problems[0]
    );
}

// ---------------------------------------------------------------------------
// The baseline: a clean run trips nothing
// ---------------------------------------------------------------------------

#[test]
fn a_clean_run_trips_no_guards() {
    assert!(
        Scenario::clean().judge().is_empty(),
        "the clean fixture must pass every guard, or no other test in this file \
         can attribute a failure to the thing it broke"
    );
}

// ---------------------------------------------------------------------------
// Ported guard 1 — which slot served it (the Fast-slot trap)
// ---------------------------------------------------------------------------

/// The trap, in the shape it actually takes on this server.
///
/// Observed live on 2026-07-28: the 122B primary's compute child was not
/// serving, requests to `commonwealth/primary` were answered anyway at ~100
/// tok/s (impossible for that model), and **every SSE frame said
/// `commonwealth/primary`** — because this server echoes the requested model
/// string back verbatim. The frame-name check passed cleanly. Residency is what
/// catches it.
#[test]
fn guard_wrong_slot_catches_a_hijack_the_frames_cannot_show() {
    let mut s = Scenario::clean();
    // Fast, successful, correctly-labelled, and completely useless.
    s.trials = vec![parse_trial(&stream(40, 0.02, 0.005, PRIMARY_ALIAS, "stop"))];
    s.primary_serving_before = false;
    s.primary_serving_after = Some(false);

    let p = s.judge();
    assert!(
        p.iter().any(|m| m.contains("WRONG SLOT")),
        "a run the primary slot did not serve must not pass: {p:#?}"
    );
    assert!(
        !p.iter().any(|m| m.contains("WRONG MODEL REQUESTED")),
        "the frame-name check cannot see this, which is exactly the point: {p:#?}"
    );
}

#[test]
fn guard_wrong_slot_fires_when_the_primary_falls_out_mid_run() {
    let mut s = Scenario::clean();
    s.primary_serving_after = Some(false);
    only_problem(&s.judge(), "WRONG SLOT");
}

#[test]
fn guard_unreadable_residency_after_is_invalid() {
    let mut s = Scenario::clean();
    s.primary_serving_after = None;
    only_problem(&s.judge(), "could not re-read the primary slot's residency");
}

#[test]
fn guard_catches_a_request_addressed_to_the_wrong_model() {
    // The frame-name check still earns its keep: it catches a CLIENT mistake,
    // which is a different failure from a hijack and worth naming separately.
    let mut s = Scenario::clean();
    s.trials = vec![parse_trial(&stream(40, 0.5, 0.1, "some-other-model", "stop"))];
    only_problem(&s.judge(), "WRONG MODEL REQUESTED");
}

#[test]
fn guard_accepts_the_alias_the_request_was_made_under() {
    let mut s = Scenario::clean();
    s.trials = vec![
        parse_trial(&stream(40, 0.5, 0.1, PRIMARY_ALIAS, "stop")),
        parse_trial(&stream(40, 0.5, 0.1, "primary", "stop")),
    ];
    assert!(
        s.judge().is_empty(),
        "the server resolving `commonwealth/primary` to the primary slot is the \
         normal case, not a mismatch"
    );
}

#[test]
fn guard_accepts_a_truncated_or_suffixed_model_id() {
    let mut s = Scenario::clean();
    s.primary_model_id = "Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003".into();
    s.trials = vec![parse_trial(&stream(
        40,
        0.5,
        0.1,
        "Qwen3.5-122B-A10B-UD-Q5_K_XL",
        "stop",
    ))];
    assert!(s.judge().is_empty(), "{:#?}", s.judge());
}

#[test]
fn guard_unattributed_run_is_invalid() {
    let mut s = Scenario::clean();
    // 40 content frames with no `model` field anywhere.
    let mut frames: Vec<Frame> = (0..40)
        .map(|i| {
            Frame::from_line(
                0.5 + 0.1 * i as f64,
                r#"data: {"choices":[{"index":0,"delta":{"content":"x"},"finish_reason":null}]}"#,
            )
        })
        .collect();
    frames.push(Frame::from_line(
        4.5,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
    ));
    s.trials = vec![parse_trial(&frames)];
    only_problem(&s.judge(), "cannot be attributed");
}

// ---------------------------------------------------------------------------
// Ported guard 2 — real per-frame timing
// ---------------------------------------------------------------------------

#[test]
fn decode_rate_excludes_time_to_first_token() {
    // 11 frames: a 5-second prefill, then 10 gaps of 0.1s. A wall-clock rate
    // would read 11/6.0 = 1.8 tok/s; the steady-state rate is 10 tok/s.
    let t = parse_trial(&stream(11, 5.0, 0.1, "primary-model", "stop"));
    assert_eq!(t.content_frames, 11);
    assert!(
        (t.decode_tok_s - 10.0).abs() < 0.01,
        "expected ~10 tok/s steady state, got {}",
        t.decode_tok_s
    );
    assert!((t.ttft_s.unwrap_or(0.0) - 5.0).abs() < 1e-9);
}

#[test]
fn guard_single_frame_is_not_a_rate() {
    let mut s = Scenario::clean();
    s.trials = vec![parse_trial(&[
        content(0.5, "primary-model", "x"),
        finish(0.6, "primary-model", "stop", Some(12)),
    ])];
    let p = s.judge();
    assert!(
        p.iter().any(|m| m.contains("at least two timestamps")),
        "{p:#?}"
    );
}

// ---------------------------------------------------------------------------
// Ported guard 3 — placement re-read after the run
// ---------------------------------------------------------------------------

#[test]
fn guard_placement_reverting_mid_run_is_invalid() {
    let mut s = Scenario::clean();
    s.placement_before = PlacementSnapshot {
        mode: "distributed".into(),
        total_blocks: 48,
        local_blocks: 36,
        workers: vec![WorkerSnapshot {
            endpoint: "192.168.1.2:50052".into(),
            blocks: 12,
            holds_output: false,
        }],
    };
    // Quarantine reverted the slot to local: the tail of the timed run was
    // local decode, which is faster and proves nothing about the split.
    s.placement_after = Some(PlacementSnapshot {
        mode: "local".into(),
        total_blocks: 0,
        local_blocks: 0,
        workers: Vec::new(),
    });
    let p = s.judge();
    assert!(
        p.iter().any(|m| m.contains("placement changed")),
        "{p:#?}"
    );
}

#[test]
fn guard_unreadable_placement_after_is_invalid() {
    let mut s = Scenario::clean();
    s.placement_after = None;
    only_problem(&s.judge(), "could not re-read the placement");
}

// ---------------------------------------------------------------------------
// Ported guard 4 — peer liveness before AND after
// ---------------------------------------------------------------------------

#[test]
fn guard_peer_offline_before_the_run_is_invalid() {
    let mut s = Scenario::clean();
    s.peers_before = vec![("BeefyMac".into(), false)];
    s.peers_after = vec![("BeefyMac".into(), true)];
    only_problem(&s.judge(), "was not online when the run started");
}

#[test]
fn guard_peer_leaving_during_the_run_is_invalid() {
    let mut s = Scenario::clean();
    s.peers_before = vec![("BeefyMac".into(), true)];
    s.peers_after = vec![("BeefyMac".into(), false)];
    only_problem(&s.judge(), "went offline during the run");
}

#[test]
fn an_online_peer_throughout_is_fine() {
    let mut s = Scenario::clean();
    s.peers_before = vec![("BeefyMac".into(), true)];
    s.peers_after = vec![("BeefyMac".into(), true)];
    assert!(s.judge().is_empty());
}

// ---------------------------------------------------------------------------
// Ported guard 5 — canary first
// ---------------------------------------------------------------------------

#[test]
fn guard_zero_token_canary_is_invalid() {
    let mut s = Scenario::clean();
    s.canary_tokens = 0;
    only_problem(&s.judge(), "canary produced zero tokens");
}

// ---------------------------------------------------------------------------
// Ported guard 6 — host survival
// ---------------------------------------------------------------------------

#[test]
fn guard_host_restart_is_invalid_and_reported_first() {
    let mut s = Scenario::clean();
    s.host_alive_after = HostLiveness::Restarted;
    let p = s.judge();
    assert!(p[0].contains("DIED during the run"), "{p:#?}");
}

#[test]
fn guard_host_gone_is_invalid() {
    let mut s = Scenario::clean();
    s.host_alive_after = HostLiveness::Gone;
    only_problem(&s.judge(), "stopped answering /status");
}

#[test]
fn liveness_reads_uptime_going_backwards_as_a_restart() {
    assert_eq!(liveness(Some(31_600), Some(31_700)), HostLiveness::Alive);
    assert_eq!(liveness(Some(31_600), Some(4)), HostLiveness::Restarted);
    assert_eq!(liveness(Some(31_600), None), HostLiveness::Gone);
    // No reading before means nothing to compare against; the absence of
    // evidence is not evidence of a restart.
    assert_eq!(liveness(None, Some(10)), HostLiveness::Alive);
}

// ---------------------------------------------------------------------------
// New guard 1 — the 32-frame floor
// ---------------------------------------------------------------------------

#[test]
fn guard_short_run_is_invalid() {
    let mut s = Scenario::clean();
    s.trials = vec![parse_trial(&stream(20, 0.5, 0.1, "primary-model", "stop"))];
    let p = s.judge();
    assert!(
        p.iter()
            .any(|m| m.contains("only 20 content frames") && m.contains("floor 32")),
        "{p:#?}"
    );
}

#[test]
fn exactly_the_floor_passes() {
    let mut s = Scenario::clean();
    s.trials = vec![parse_trial(&stream(
        MIN_CONTENT_FRAMES,
        0.5,
        0.1,
        "primary-model",
        "stop",
    ))];
    assert!(s.judge().is_empty(), "{:#?}", s.judge());
}

// ---------------------------------------------------------------------------
// New guard 2 — inter-trial spread
// ---------------------------------------------------------------------------

#[test]
fn guard_unsteady_machine_is_invalid() {
    let mut s = Scenario::clean();
    // 10 vs 14 tok/s: 40% spread, well over the 25% limit.
    s.trials = vec![good_trial(10.0), good_trial(14.0)];
    let p = s.judge();
    assert!(p.iter().any(|m| m.contains("trials disagree by")), "{p:#?}");
}

#[test]
fn spread_is_undefined_for_a_single_trial() {
    // One sample cannot disagree with itself. Reporting 0% would claim a
    // steadiness that was never tested.
    assert_eq!(trial_spread(&[good_trial(10.0)]), None);
    let two = [good_trial(10.0), good_trial(12.0)];
    let spread = trial_spread(&two).expect("two trials have a spread");
    assert!((spread - 0.2).abs() < 0.02, "got {spread}");
}

#[test]
fn a_single_trial_run_is_still_valid() {
    let mut s = Scenario::clean();
    s.trials = vec![good_trial(10.0)];
    assert!(
        s.judge().is_empty(),
        "one trial is a smaller sample, not an invalid one: {:#?}",
        s.judge()
    );
}

// ---------------------------------------------------------------------------
// New guard 3 — the generation completed
// ---------------------------------------------------------------------------

#[test]
fn guard_error_finish_reason_is_invalid() {
    let mut s = Scenario::clean();
    s.trials = vec![parse_trial(&stream(40, 0.5, 0.1, "primary-model", "error"))];
    let p = s.judge();
    assert!(p.iter().any(|m| m.contains("finished with reason")), "{p:#?}");
}

#[test]
fn guard_missing_finish_reason_is_invalid() {
    let mut s = Scenario::clean();
    let frames: Vec<Frame> = (0..40)
        .map(|i| content(0.5 + 0.1 * i as f64, "primary-model", "x"))
        .collect();
    s.trials = vec![parse_trial(&frames)];
    only_problem(&s.judge(), "never sent a terminal");
}

#[test]
fn both_length_and_stop_are_complete_generations() {
    for reason in ["stop", "length"] {
        let mut s = Scenario::clean();
        s.trials = vec![parse_trial(&stream(40, 0.5, 0.1, "primary-model", reason))];
        assert!(s.judge().is_empty(), "{reason}: {:#?}", s.judge());
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[test]
fn a_non_sse_error_body_is_kept_not_dropped() {
    // The failure this protects against: an error body yields zero data frames,
    // and a reader that drops non-SSE lines reports "0 frames" for a request
    // that was actually rejected with a reason.
    let t = parse_trial(&[Frame::from_line(
        0.1,
        r#"{"error":{"message":"model is loading"}}"#,
    )]);
    assert_eq!(t.content_frames, 0);
    assert_eq!(t.non_sse_lines.len(), 1);
    assert!(t.non_sse_lines[0].contains("model is loading"));
}

#[test]
fn done_sentinel_is_not_a_content_frame() {
    let t = parse_trial(&stream(5, 0.0, 0.1, "m", "stop"));
    assert_eq!(t.content_frames, 5, "[DONE] and the finish frame don't count");
}

#[test]
fn empty_content_deltas_do_not_count_as_tokens() {
    // A keep-alive or role-only frame carries `content: ""`. Counting it would
    // inflate the rate with a frame that carried no token.
    let frames = vec![
        Frame::from_line(
            0.1,
            r#"data: {"model":"m","choices":[{"delta":{"content":""},"finish_reason":null}]}"#,
        ),
        content(0.2, "m", "a"),
        content(0.3, "m", "b"),
        finish(0.4, "m", "stop", None),
    ];
    assert_eq!(parse_trial(&frames).content_frames, 2);
}

#[test]
fn prefill_comes_only_from_server_reported_prompt_tokens() {
    let with_usage = parse_trial(&stream(40, 2.0, 0.1, "m", "stop"));
    assert_eq!(with_usage.prompt_tokens, Some(12));
    let agg = aggregate(&[with_usage]).expect("a timed trial aggregates");
    // 12 prompt tokens over a 2.0s TTFT.
    assert!(
        (agg.prefill_tok_s.expect("usage was present") - 6.0).abs() < 0.01,
        "got {:?}",
        agg.prefill_tok_s
    );

    // No usage block → None, never an estimate from string length. This is the
    // exact mistake the deleted `run_baseline_benchmark` made.
    let mut frames: Vec<Frame> = (0..40)
        .map(|i| content(2.0 + 0.1 * i as f64, "m", "x"))
        .collect();
    frames.push(finish(6.0, "m", "stop", None));
    let no_usage = parse_trial(&frames);
    assert_eq!(no_usage.prompt_tokens, None);
    assert_eq!(
        aggregate(&[no_usage]).and_then(|a| a.prefill_tok_s),
        None,
        "an absent prompt-token count must render as n/a, not as a fabricated rate"
    );
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

#[test]
fn the_headline_is_the_median_and_the_spread_travels_with_it() {
    let trials = vec![good_trial(10.0), good_trial(20.0), good_trial(11.0)];
    let a = aggregate(&trials).expect("three timed trials aggregate");
    assert_eq!(a.trials, 3);
    // Median, not mean: the 20 tok/s outlier does not drag the headline...
    assert!((a.decode_tok_s - 11.0).abs() < 0.1, "got {}", a.decode_tok_s);
    // ...but it is not hidden either.
    assert!((a.decode_tok_s_max - 20.0).abs() < 0.1);
    assert!((a.decode_tok_s_min - 10.0).abs() < 0.1);
}

#[test]
fn nothing_timed_aggregates_to_nothing() {
    assert!(aggregate(&[]).is_none());
    assert!(
        aggregate(&[Trial::default()]).is_none(),
        "a trial with no rate must not aggregate to 0.0 tok/s — that would read \
         as a measurement of a very slow machine"
    );
}

#[test]
fn latency_percentiles_pool_across_trials() {
    // 39 gaps at 100ms, then one trial with a 1000ms stall: p50 stays at the
    // steady state while p95 exposes the jitter.
    let mut stalled = good_trial(10.0);
    stalled.itl_ms.push(1000.0);
    let a = aggregate(&[good_trial(10.0), stalled]).expect("aggregates");
    assert!((a.itl_p50_ms - 100.0).abs() < 1.0, "p50 {}", a.itl_p50_ms);
    assert!(a.itl_p95_ms >= a.itl_p50_ms);
}

// ---------------------------------------------------------------------------
// Placement → shards (the half that must agree with `mesh plan`)
// ---------------------------------------------------------------------------

fn no_names(_: &str) -> Option<String> {
    None
}

#[test]
fn a_local_load_takes_its_block_range_from_the_gguf() {
    // `/status` reports total_blocks: 0 for a plain local load — it computes no
    // block plan — so the range has to come from the model's own layer count,
    // which is what `mesh plan` hashes for the same configuration.
    let p = PlacementSnapshot {
        mode: "local".into(),
        total_blocks: 0,
        local_blocks: 0,
        workers: Vec::new(),
    };
    let shards = shards_from_placement(&p, "RuggedFox", 48, &no_names).expect("local is describable");
    assert_eq!(
        shards,
        vec![mm::PlacementShard {
            node_key: "RuggedFox".into(),
            blocks: Some((0, 47)),
            holds_output: true,
        }]
    );
    assert_eq!(digest_mode(&shards), "local");
}

#[test]
fn a_distributed_load_lays_workers_first_then_the_host() {
    let p = PlacementSnapshot {
        mode: "distributed".into(),
        total_blocks: 48,
        local_blocks: 36,
        workers: vec![WorkerSnapshot {
            endpoint: "192.168.1.2:50052".into(),
            blocks: 12,
            holds_output: false,
        }],
    };
    let names = |ep: &str| (ep == "192.168.1.2:50052").then(|| "BeefyMac".to_string());
    let shards = shards_from_placement(&p, "RuggedFox", 48, &names).expect("distributed");
    assert_eq!(
        shards,
        vec![
            mm::PlacementShard {
                node_key: "BeefyMac".into(),
                blocks: Some((0, 11)),
                holds_output: false,
            },
            mm::PlacementShard {
                node_key: "RuggedFox".into(),
                blocks: Some((12, 47)),
                holds_output: true,
            },
        ],
        "workers take the low blocks in device order and the host takes the tail — \
         the order `plan_shards_weighted` is called with"
    );
    assert_eq!(digest_mode(&shards), "distributed");
}

#[test]
fn an_unresolvable_endpoint_falls_back_to_its_host_without_the_port() {
    let p = PlacementSnapshot {
        mode: "distributed".into(),
        total_blocks: 48,
        local_blocks: 40,
        workers: vec![WorkerSnapshot {
            endpoint: "192.168.1.2:50052".into(),
            blocks: 8,
            holds_output: false,
        }],
    };
    let shards = shards_from_placement(&p, "RuggedFox", 48, &no_names).expect("distributed");
    assert_eq!(
        shards[0].node_key, "192.168.1.2",
        "ports churn across restarts; including one would miss on every lookup"
    );
}

#[test]
fn endpoint_host_survives_an_ipv6_literal() {
    assert_eq!(endpoint_host("192.168.1.2:50052"), "192.168.1.2");
    assert_eq!(endpoint_host("beefymac.local:50052"), "beefymac.local");
    assert_eq!(endpoint_host("[fd7a:115c::1]:50052"), "[fd7a:115c::1]");
    // A bare IPv6 address ends in an all-digit segment. Truncating it at the
    // last colon produces a "host" that is a prefix of an address — which then
    // becomes a node key that no plan will ever match.
    assert_eq!(endpoint_host("fd7a:115c::1"), "fd7a:115c::1");
    assert_eq!(endpoint_host("[fd7a:115c::1]"), "[fd7a:115c::1]");
    // Nothing after the colon is not a port.
    assert_eq!(endpoint_host("192.168.1.2:"), "192.168.1.2:");
}

#[test]
fn a_placement_that_does_not_add_up_is_refused() {
    let p = PlacementSnapshot {
        mode: "distributed".into(),
        total_blocks: 48,
        local_blocks: 30,
        workers: vec![WorkerSnapshot {
            endpoint: "w:1".into(),
            blocks: 12,
            holds_output: false,
        }],
    };
    let err = shards_from_placement(&p, "host", 48, &no_names).expect_err("30 + 12 != 48");
    assert!(err.contains("does not add up"), "{err}");
}

#[test]
fn a_placement_for_a_different_model_is_refused() {
    // The daemon holds 48 blocks but the config's GGUF has 64: the header that
    // produced the fingerprint is not the model that is loaded, so any record
    // filed now would describe the wrong thing.
    let p = PlacementSnapshot {
        mode: "distributed".into(),
        total_blocks: 48,
        local_blocks: 36,
        workers: vec![WorkerSnapshot {
            endpoint: "w:1".into(),
            blocks: 12,
            holds_output: false,
        }],
    };
    let err = shards_from_placement(&p, "host", 64, &no_names).expect_err("48 != 64");
    assert!(err.contains("not the one whose header was read"), "{err}");
}

#[test]
fn a_worker_holding_nothing_is_not_part_of_the_placement() {
    // An idle peer joining or leaving must not change the digest — it changes
    // nothing about how the model decodes.
    let p = PlacementSnapshot {
        mode: "distributed".into(),
        total_blocks: 48,
        local_blocks: 48,
        workers: vec![WorkerSnapshot {
            endpoint: "idle:1".into(),
            blocks: 0,
            holds_output: false,
        }],
    };
    let shards = shards_from_placement(&p, "RuggedFox", 48, &no_names).expect("describable");
    assert_eq!(shards.len(), 1);
    assert_eq!(shards[0].node_key, "RuggedFox");
    assert_eq!(digest_mode(&shards), "local");
}

#[test]
fn a_zero_block_model_is_not_describable() {
    let p = PlacementSnapshot::default();
    assert!(shards_from_placement(&p, "host", 0, &no_names).is_err());
}

// ---------------------------------------------------------------------------
// The digest must be the one `mesh plan` looks up
// ---------------------------------------------------------------------------

#[test]
fn a_solo_bench_and_a_solo_plan_agree_on_the_digest() {
    // This is the property that makes the whole store useful: bench files a
    // record under the key plan will construct for the same configuration. If
    // it ever breaks, every record written is unfindable and the tool silently
    // reports "not measured" forever.
    let bench_shards = shards_from_placement(
        &PlacementSnapshot {
            mode: "local".into(),
            total_blocks: 0,
            local_blocks: 0,
            workers: Vec::new(),
        },
        "RuggedFox",
        48,
        &no_names,
    )
    .expect("describable");

    // What `mesh plan --from-mesh` builds for a single-node fit: one row, the
    // host, holding every block and the output head.
    let plan_shards = vec![mm::PlacementShard {
        node_key: "RuggedFox".into(),
        blocks: Some((0, 47)),
        holds_output: true,
    }];

    assert_eq!(
        mm::placement_digest(digest_mode(&bench_shards), 48, &bench_shards),
        mm::placement_digest("local", 48, &plan_shards)
    );
}

#[test]
fn a_different_split_of_the_same_model_digests_differently() {
    let solo = vec![mm::PlacementShard {
        node_key: "RuggedFox".into(),
        blocks: Some((0, 47)),
        holds_output: true,
    }];
    let split = vec![
        mm::PlacementShard {
            node_key: "BeefyMac".into(),
            blocks: Some((0, 11)),
            holds_output: false,
        },
        mm::PlacementShard {
            node_key: "RuggedFox".into(),
            blocks: Some((12, 47)),
            holds_output: true,
        },
    ];
    assert_ne!(
        mm::placement_digest("local", 48, &solo),
        mm::placement_digest("distributed", 48, &split),
        "if these collided, a measured solo run would be reported as the speed of \
         a split nobody has ever run"
    );
}

// ---------------------------------------------------------------------------
// Reading the daemon's JSON
// ---------------------------------------------------------------------------

#[test]
fn primary_slot_is_read_out_of_a_real_status_body() {
    let body: serde_json::Value = serde_json::from_str(
        r#"{"inference":{"resident":[
             {"role":"fast","model_id":"Qwen3.5-0.8B","resident":true},
             {"role":"primary","model_id":"Qwen3.5-122B","resident":false,
              "placement":{"mode":"child-distributed","total_blocks":0,"local_blocks":0,"workers":[]}}
           ]},"process":{"uptime_seconds":31603}}"#,
    )
    .expect("fixture parses");
    let (id, resident, placement) = primary_from_status(&body).expect("a primary slot is present");
    assert_eq!(id, "Qwen3.5-122B");
    assert!(!resident, "the lazy primary reports idle-unloaded");
    assert_eq!(placement.mode, "child-distributed");
    assert_eq!(uptime_from_status(&body), Some(31603));
}

#[test]
fn a_child_hosted_primary_counts_as_serving_though_resident_is_false() {
    // The false positive this nearly shipped with. `ComputeRoutedProvider::
    // resident_slots()` forwards the IN-PROCESS engine's view, and the
    // in-process engine never loaded this model — the child did. So `resident`
    // is false forever on a perfectly healthy child-hosted primary, and a guard
    // reading only that field would make a VALID measurement impossible here.
    let body: serde_json::Value = serde_json::from_str(
        r#"{"inference":{
             "resident":[{"role":"primary","model_id":"Qwen3.5-122B","resident":false,
                          "placement":{"mode":"child-distributed","total_blocks":0,
                                       "local_blocks":0,"workers":[]}}],
             "compute_children":[{"name":"Qwen3.5-122B","role":"generate",
                                  "model_id":"Qwen3.5-122B","lifecycle":"serving"}]}}"#,
    )
    .expect("fixture parses");
    assert!(primary_is_serving(&body, "Qwen3.5-122B"));
}

#[test]
fn a_child_that_is_only_starting_or_warming_is_not_serving() {
    // These are the states in which something ELSE answers the request — the
    // case being caught. Observed live 2026-07-28 at ~100 tok/s from a 122B.
    for phase in ["starting", "warming", "degraded", "restarting", "failed"] {
        let body: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"inference":{{
                 "resident":[{{"role":"primary","model_id":"M","resident":false}}],
                 "compute_children":[{{"model_id":"M","lifecycle":"{phase}"}}]}}}}"#
        ))
        .expect("fixture parses");
        assert!(
            !primary_is_serving(&body, "M"),
            "`{phase}` must not count as serving"
        );
    }
}

#[test]
fn an_in_process_resident_primary_needs_no_child() {
    let body: serde_json::Value = serde_json::from_str(
        r#"{"inference":{"resident":[{"role":"primary","model_id":"M","resident":true}]}}"#,
    )
    .expect("fixture parses");
    assert!(primary_is_serving(&body, "M"));
}

#[test]
fn a_different_childs_health_does_not_vouch_for_the_primary() {
    let body: serde_json::Value = serde_json::from_str(
        r#"{"inference":{
             "resident":[{"role":"primary","model_id":"M","resident":false}],
             "compute_children":[{"model_id":"SomethingElse","lifecycle":"serving"}]}}"#,
    )
    .expect("fixture parses");
    assert!(!primary_is_serving(&body, "M"));
}

#[test]
fn a_status_body_with_no_primary_yields_nothing() {
    let body: serde_json::Value =
        serde_json::from_str(r#"{"inference":{"resident":[{"role":"fast","model_id":"x"}]}}"#)
            .expect("fixture parses");
    assert!(primary_from_status(&body).is_none());
}

#[test]
fn mesh_view_resolves_rpc_endpoints_to_member_names() {
    let body: serde_json::Value = serde_json::from_str(
        r#"{"members":[
             {"node_id":"aaa","name":"RuggedFox","is_self":true,"status":"online",
              "hw_fingerprint":12345,"backend":"vulkan"},
             {"node_id":"bbb","name":"BeefyMac","is_self":false,"status":"online"},
             {"node_id":"ccc","name":"LittleMac","is_self":false,"status":"offline"}
           ],
           "rpc_workers":[{"node_id":"bbb","endpoint":"192.168.1.2:50052"}]}"#,
    )
    .expect("fixture parses");
    let v = MeshView::parse(&body);
    assert_eq!(v.self_name, "RuggedFox");
    assert_eq!(v.self_hw_fingerprint, Some(12345));
    assert_eq!(v.self_backend.as_deref(), Some("vulkan"));
    assert_eq!(
        v.endpoint_names.get("192.168.1.2:50052").map(String::as_str),
        Some("BeefyMac")
    );
    assert_eq!(v.online.get("BeefyMac"), Some(&true));
    assert_eq!(v.online.get("LittleMac"), Some(&false));
}

#[test]
fn a_daemon_that_advertises_no_fingerprint_yields_no_host_identity() {
    // The structural bar from week 1: without a fingerprint there is no
    // `HostIdentity`, so `MeasurementKey::for_plan` cannot be called at all and
    // the command must refuse rather than file under a placeholder.
    let body: serde_json::Value = serde_json::from_str(
        r#"{"members":[{"node_id":"aaa","name":"RuggedFox","is_self":true,"status":"online"}]}"#,
    )
    .expect("fixture parses");
    let v = MeshView::parse(&body);
    assert_eq!(v.self_hw_fingerprint, None);
    assert!(mm::HostIdentity::from_live_mesh(v.self_hw_fingerprint).is_none());
}

#[test]
fn peer_liveness_excludes_the_host_itself() {
    let mut mesh = MeshView {
        self_name: "RuggedFox".into(),
        ..Default::default()
    };
    mesh.online.insert("BeefyMac".into(), true);
    let shards = vec![
        mm::PlacementShard {
            node_key: "BeefyMac".into(),
            blocks: Some((0, 11)),
            holds_output: false,
        },
        mm::PlacementShard {
            node_key: "RuggedFox".into(),
            blocks: Some((12, 47)),
            holds_output: true,
        },
    ];
    assert_eq!(
        peer_liveness(&shards, &mesh),
        vec![("BeefyMac".to_string(), true)],
        "the host's own liveness is the HostLiveness check, not this one"
    );
}

#[test]
fn a_peer_absent_from_the_mesh_reads_as_offline() {
    // Absence of evidence is not evidence of health: a shard-holder we cannot
    // see in the member list has not been shown to be up.
    let mesh = MeshView {
        self_name: "RuggedFox".into(),
        ..Default::default()
    };
    let shards = vec![mm::PlacementShard {
        node_key: "Ghost".into(),
        blocks: Some((0, 11)),
        holds_output: false,
    }];
    assert_eq!(peer_liveness(&shards, &mesh), vec![("Ghost".into(), false)]);
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn a_record(verdict: mm::Verdict) -> mm::MeasurementRecord {
    mm::MeasurementRecord {
        key: mm::MeasurementKey {
            probe_version: mm::PROBE_VERSION,
            model_fingerprint: "mf1:deadbeefdeadbeef".into(),
            placement_digest: "pd1:0123456789abcdef".into(),
            host_hw_fingerprint: 12345,
            n_ctx: 16384,
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

#[test]
fn an_absent_prefill_renders_as_na_never_as_a_number() {
    let out = render_bench_human(&a_record(mm::Verdict::Valid), "recorded");
    assert!(out.contains("n/a (server omits stream usage)"), "{out}");
    let j = render_bench_json(&a_record(mm::Verdict::Valid), "recorded");
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
    let out = render_bench_human(&r, "recorded");
    assert!(out.contains("INVALID"), "{out}");
    assert!(out.contains("WRONG SLOT"), "{out}");
    assert!(
        out.contains("14.10 tok/s"),
        "the numbers stay visible so the failure is inspectable: {out}"
    );
    assert_eq!(render_bench_json(&r, "recorded")["verdict"], "invalid");
}

#[test]
fn json_carries_the_key_so_a_plan_can_be_correlated_with_a_run() {
    let j = render_bench_json(&a_record(mm::Verdict::Valid), "recorded");
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
            blocks: Some((0, 11)),
            holds_output: false,
        },
        mm::PlacementShard {
            node_key: "RuggedFox".into(),
            blocks: Some((12, 47)),
            holds_output: true,
        },
    ];
    assert_eq!(
        placement_human(&shards, "RuggedFox", "distributed"),
        "36 local + 12 @BeefyMac"
    );
}
