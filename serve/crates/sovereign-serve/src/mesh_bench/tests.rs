// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unit tests for `mesh bench`'s pure core.
//!
//! The point of the seam is that all nine validity guards can be made to fire
//! here — with no daemon, no peer, no GPU and no model. A guard that cannot be
//! tested is a guard nobody knows still works, and the shell script this
//! command replaces had exactly that problem: its guards could only be
//! exercised by reproducing the failure they were written for.

use super::*;

mod link;
mod render;

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
        Some(p) => format!(
            r#","usage":{{"prompt_tokens":{p},"completion_tokens":1,"total_tokens":{}}}"#,
            p + 1
        ),
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
    s.trials = vec![parse_trial(&stream(
        40,
        0.5,
        0.1,
        "some-other-model",
        "stop",
    ))];
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
    assert!(p.iter().any(|m| m.contains("placement changed")), "{p:#?}");
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
    assert!(
        p.iter().any(|m| m.contains("finished with reason")),
        "{p:#?}"
    );
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
    assert_eq!(
        t.content_frames, 5,
        "[DONE] and the finish frame don't count"
    );
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
    assert!(
        (a.decode_tok_s - 11.0).abs() < 0.1,
        "got {}",
        a.decode_tok_s
    );
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

/// No endpoint resolves to a mesh member.
fn no_names(_: &str) -> Option<NodeIdentity> {
    None
}

/// A machine that has said what it is.
///
/// Most of these tests are about block arithmetic, not identity, so they use a
/// fingerprinted node and let the identity tests below be the only ones that
/// vary it.
fn node(name: &str) -> NodeIdentity {
    NodeIdentity {
        name: name.into(),
        hw: Some(0xF0F),
    }
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
    let shards =
        shards_from_placement(&p, &node("RuggedFox"), 48, &no_names).expect("local is describable");
    assert_eq!(
        shards,
        vec![mm::PlacementShard {
            node_key: "RuggedFox".into(),
            hw: Some(0xF0F),
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
    let names = |ep: &str| (ep == "192.168.1.2:50052").then(|| node("BeefyMac"));
    let shards = shards_from_placement(&p, &node("RuggedFox"), 48, &names).expect("distributed");
    assert_eq!(
        shards,
        vec![
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
        ],
        "workers take the low blocks in device order and the host takes the tail — \
         the order `plan_shards_weighted` is called with"
    );
    assert_eq!(digest_mode(&shards), "distributed");
}

/// Replaces `an_unresolvable_endpoint_falls_back_to_its_host_without_the_port`,
/// which asserted the opposite until 2026-07-29.
///
/// That fallback keyed the shard on the endpoint's host (`192.168.1.2`) when no
/// mesh member owned it. It looked forgiving and was not: `mesh plan` builds
/// its shards by walking mesh *members*, so it can never produce a shard named
/// after a bare endpoint, and every record filed through the fallback was
/// therefore unfindable by the only thing that reads them. A store that grows
/// and never answers is worse than a refusal, because it looks like it worked.
#[test]
fn a_worker_that_is_not_a_mesh_member_is_refused_rather_than_keyed_on_its_ip() {
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
    let err = shards_from_placement(&p, &node("RuggedFox"), 48, &no_names)
        .expect_err("an unidentifiable worker cannot be keyed");
    assert!(
        err.contains("192.168.1.2") && !err.contains("50052"),
        "the operator needs to know WHICH worker, but not via a port that churns \
         across restarts: {err}"
    );
}

/// The same refusal one step later: the endpoint resolves to a real member, but
/// that member is on a daemon too old to say what hardware it is.
#[test]
fn a_worker_on_a_daemon_too_old_to_report_hardware_is_refused_by_name() {
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
    let anonymous = |_: &str| {
        Some(NodeIdentity {
            name: "BeefyMac".into(),
            hw: None,
        })
    };
    let err = shards_from_placement(&p, &node("RuggedFox"), 48, &anonymous)
        .expect_err("a peer that never said what it is cannot be keyed");
    assert!(
        err.contains("BeefyMac"),
        "name the machine to go upgrade: {err}"
    );
}

/// An idle peer is not part of the placement, so its missing fingerprint is not
/// a reason to refuse. The refusal must key off *carrying weight*, not off
/// merely being in the worker list — otherwise one old peer idling on the mesh
/// would block every measurement on it.
#[test]
fn an_idle_unidentified_peer_does_not_block_the_run() {
    let p = PlacementSnapshot {
        mode: "distributed".into(),
        total_blocks: 48,
        local_blocks: 48,
        workers: vec![WorkerSnapshot {
            endpoint: "192.168.1.9:50052".into(),
            blocks: 0,
            holds_output: false,
        }],
    };
    let shards = shards_from_placement(&p, &node("RuggedFox"), 48, &no_names)
        .expect("an idle peer carries nothing and is not part of the key");
    assert_eq!(shards.len(), 1);
    assert_eq!(shards[0].node_key, "RuggedFox");
}
