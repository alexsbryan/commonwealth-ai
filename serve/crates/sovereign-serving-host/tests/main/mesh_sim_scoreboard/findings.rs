// SPDX-License-Identifier: AGPL-3.0-or-later
//! Scoreboard tests: the §3 findings, re-run against the real scorer (household evening, heterogeneous fleets, cold-start ramp, warm start, outbound-only load, isolation, the pair case). Split out of
//! mesh_sim_scoreboard.rs when it moved here (pb-mesh-dissolve).

use super::*;

/// **The inbound-load question, priced before it costs two daemons.**
///
/// `MESH_LOAD_AWARENESS.md` states the intent: a node gossips its
/// *whole* in-flight count, peer-served work included. Every bump
/// site for that counter (`peer_inference.rs::enter_local_total`)
/// nonetheless sits in the joiner-side provider — the outbound path —
/// so whether production achieves the documented intent depends on
/// whether an inbound peer request passes through it. Answering that
/// for real means two daemons, `SOVEREIGN_DECISION_LOG` on both,
/// driving A→B and reading B's `FleetSnapshot.local.in_flight_published`.
///
/// This arm asks the prior question: *would it matter?* If routing
/// barely moves when the counter misses inbound work, the audit drops
/// down the list. If it moves a lot, the two daemons are earned.
#[test]
fn does_publishing_only_outbound_load_change_routing() {
    for s in [
        scenario::household_evening_12(SEED),
        scenario::twin_hubs(SEED),
        scenario::isolation(SEED),
    ] {
        let total = run(&s, Arm::AsImplemented, SEED);
        let outbound = run(&s, Arm::OutboundOnlyLoad, SEED);

        // Wiring check: the published number must actually shrink, or
        // a flat result means "no inbound work existed", not "inbound
        // attribution does not matter".
        let published_sum = |r: &RunReport| -> u64 {
            candidates(r)
                .iter()
                .filter_map(|c| c.inputs.gossiped_in_flight)
                .map(u64::from)
                .sum()
        };
        let total_sum = published_sum(&total);
        let outbound_sum = published_sum(&outbound);
        assert!(
            outbound_sum < total_sum,
            "{}: outbound-only published {outbound_sum} vs total {total_sum} — \
             the arm is not wired, or this fleet never served inbound work",
            s.name
        );

        let top_share = |r: &RunReport| -> f64 {
            let mut counts = vec![0usize; r.node_names.len()];
            for f in &r.truth {
                counts[f.server] += 1;
            }
            counts.iter().copied().max().unwrap_or(0) as f64 / r.truth.len().max(1) as f64
        };

        println!("── inbound-load attribution — `{}` ──", s.name);
        println!(
            "  {:<18} mean {:>7.1}s  offloads {:>4}  top-server share {:.2}  Σ published {}",
            "total (intended)",
            mean_total_ms(&total) / 1000.0,
            offloads(&total),
            top_share(&total),
            total_sum,
        );
        println!(
            "  {:<18} mean {:>7.1}s  offloads {:>4}  top-server share {:.2}  Σ published {}",
            "outbound-only",
            mean_total_ms(&outbound) / 1000.0,
            offloads(&outbound),
            top_share(&outbound),
            outbound_sum,
        );
        println!(
            "  Δ mean {:+.1}%   Δ offloads {:+}   under-report {:.0}% of the true signal",
            100.0 * (mean_total_ms(&outbound) - mean_total_ms(&total))
                / mean_total_ms(&total).max(1.0),
            offloads(&outbound) as i64 - offloads(&total) as i64,
            100.0 * (1.0 - outbound_sum as f64 / total_sum.max(1) as f64),
        );
    }
}

#[test]
fn isolation_between_an_interactive_and_a_background_actor() {
    let s = scenario::isolation(SEED);
    let (reports, scores) = sweep(&s);
    assert_hard_invariants(&reports, &scores);
    println!("\n{}", render(&s.name, SEED, &scores));

    // Per-origin p95 for the two named actors, arm by arm.
    let interactive = s
        .nodes
        .iter()
        .position(|n| n.name == "interactive")
        .expect("scenario defines an interactive actor");
    println!("── isolation ──");
    for report in &reports {
        let mut lats: Vec<f64> = report
            .truth
            .iter()
            .filter(|f| f.origin == interactive)
            .map(|f| f.total_ms as f64)
            .collect();
        lats.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p95 = lats
            .get(((0.95 * lats.len() as f64).ceil() as usize).saturating_sub(1))
            .copied()
            .unwrap_or(0.0);
        println!(
            "  {:<18} interactive p95 {:>7.1}s over {} turns",
            report.arm.label(),
            p95 / 1000.0,
            lats.len()
        );
    }
}

/// At N=2 the decider's self-observed load *is* the peer's true load,
/// so F1 cannot appear. This is why no existing test caught it: every
/// test has one decider.
#[test]
fn the_pair_case_hides_what_the_twelve_node_case_shows() {
    let pair = scenario::household_evening_12(SEED);
    let (_, big) = sweep(&pair);
    let small = scenario::pair(SEED);
    let (_, two_node) = sweep(&small);
    println!("\n{}", render(&small.name, SEED, &two_node));
    println!(
        "── scale ──\n  N=2  herding CoV {}, top-server share {:.2}\n  N=12 herding CoV {}, top-server share {:.2}",
        two_node[0].records.herding_cov.map(|c| format!("{c:.2}")).unwrap_or_else(|| "—".into()),
        two_node[0].records.top_server_share,
        big[0].records.herding_cov.map(|c| format!("{c:.2}")).unwrap_or_else(|| "—".into()),
        big[0].records.top_server_share,
    );
}

/// **F7, priced.** The finding says the cold-start ramp is
/// self-locking: a peer starts at `COLD_START_MIN_WEIGHT` = 0.7,
/// only earns samples by being dispatched to, and at household
/// volumes never earns enough to lift the penalty — so the "ramp" is
/// a permanent flat 0.7 on every peer and on no local slot.
///
/// That much is established. What was *not* established is whether it
/// **costs** anything, and the two possible answers want different
/// responses: if warm-starting moves nothing, F7 is a false doc
/// comment (a documentation fix), and if it moves a lot, F7 is a code
/// defect that earns a Phase-2 arm. This is the cheapest experiment
/// that separates them.
///
/// Reported, not asserted — except for the wiring check, which is the
/// part a null result depends on. A knob that turns out to be
/// disconnected produces the same "nothing moved" table as a
/// mechanism that does not matter.
#[test]
fn does_warm_starting_the_cold_start_ramp_change_anything() {
    for s in [
        scenario::household_evening_12(SEED),
        scenario::twin_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
    ] {
        let cold = run(&s, Arm::AsImplemented, SEED);
        let warm = run(&s, Arm::WarmStart, SEED);

        // Wiring check. Under arm 0 a peer's first decisions carry the
        // 0.7 floor; under warm-start they must not. Without this, a
        // flat table below would be unreadable.
        let peer_cold_weights = |r: &RunReport| -> Vec<f32> {
            candidates(r)
                .iter()
                .filter(|c| c.kind == sovereign_scheduler::decision_log::CandidateKind::Peer)
                .map(|c| c.score.cold_start_weight)
                .collect()
        };
        let cold_weights = peer_cold_weights(&cold);
        let warm_weights = peer_cold_weights(&warm);
        assert!(
            cold_weights.iter().any(|w| *w < 0.99),
            "{}: arm 0 never applied a cold-start penalty — nothing to warm-start",
            s.name
        );
        assert!(
            warm_weights.iter().all(|w| (*w - 1.0).abs() < 1e-6),
            "{}: warm-start left a cold-start penalty in place — the arm is not wired",
            s.name
        );

        let peers_touched = |r: &RunReport| -> usize {
            let n = r.node_names.len();
            (0..n)
                .filter(|peer| {
                    r.peer_samples
                        .iter()
                        .enumerate()
                        // Warm-start seeds every entry at 20, so
                        // "was dispatched to" means *above* the seed.
                        .any(|(decider, row)| decider != *peer && row[*peer] > seed_floor(r.arm))
                })
                .count()
        };

        // `samples` feeds the throughput *source* as well as the
        // cold-start ramp, so print the mix rather than claiming a
        // single-factor isolation. If the two arms score peers from
        // the same source, the latency delta is the ramp alone; if
        // they diverge, the delta is the whole stranger penalty and
        // the report has to say so.
        let source_mix = |r: &RunReport| -> String {
            let mut observed = 0usize;
            let mut estimate = 0usize;
            let mut neutral = 0usize;
            for c in candidates(r)
                .iter()
                .filter(|c| c.kind == sovereign_scheduler::decision_log::CandidateKind::Peer)
            {
                match c.score.throughput_source.as_str() {
                    "observed" => observed += 1,
                    "benchmark_estimate" => estimate += 1,
                    _ => neutral += 1,
                }
            }
            format!("obs {observed} / bench {estimate} / neutral {neutral}")
        };

        println!("── F7 priced — `{}` ──", s.name);
        println!(
            "  {:<16} mean {:>7.1}s  offloads {:>4}  peers dispatched to {:>2}/{}  tput src: {}",
            "arm 0",
            mean_total_ms(&cold) / 1000.0,
            offloads(&cold),
            peers_touched(&cold),
            cold.node_names.len(),
            source_mix(&cold),
        );
        println!(
            "  {:<16} mean {:>7.1}s  offloads {:>4}  peers dispatched to {:>2}/{}  tput src: {}",
            "warm-start",
            mean_total_ms(&warm) / 1000.0,
            offloads(&warm),
            peers_touched(&warm),
            warm.node_names.len(),
            source_mix(&warm),
        );
        println!(
            "  Δ mean {:+.1}%   Δ offloads {:+}",
            100.0 * (mean_total_ms(&warm) - mean_total_ms(&cold)) / mean_total_ms(&cold).max(1.0),
            offloads(&warm) as i64 - offloads(&cold) as i64,
        );
    }
}

/// **Why does warm-start hurt?** The previous test establishes *that*
/// it does. This one discriminates the mechanism, because "the ramp is
/// a brake masking F1" is a claim about causation and the latency
/// table alone cannot support it.
///
/// Two candidate explanations fit that table equally well:
///
///   1. **F1.** Lifting the cold-start floor unlocks offloads, and a
///      decider cannot see the queue it is offloading into.
///   2. **Offloading is just unprofitable here.** The extra hops would
///      lose even with a perfect load signal, and the ramp was
///      suppressing them for an unrelated reason.
///
/// `fresh+warm-start` separates them in one run: under explanation 1
/// the damage largely disappears when the signal is fresh; under
/// explanation 2 it survives. Read the 2×2 — that is the point of
/// printing all four cells rather than the two deltas.
#[test]
fn is_warm_starts_damage_actually_f1() {
    for s in [
        scenario::household_evening_12(SEED),
        scenario::heterogeneous_fleet(SEED),
        scenario::twin_hubs(SEED),
    ] {
        let cell = |arm: Arm| {
            let r = run(&s, arm, SEED);
            (mean_total_ms(&r) / 1000.0, offloads(&r))
        };
        let (stale_cold, oc) = cell(Arm::AsImplemented);
        let (stale_warm, ow) = cell(Arm::WarmStart);
        let (fresh_cold, fc) = cell(Arm::FreshSignals);
        let (fresh_warm, fw) = cell(Arm::FreshWarmStart);

        let pct = |from: f64, to: f64| 100.0 * (to - from) / from.max(1.0);
        println!("── is warm-start's damage F1's? — `{}` ──", s.name);
        println!("  {:<14} {:>12} {:>12}", "", "cold ramp", "warm start");
        println!(
            "  {:<14} {:>9.1}s{:>3} {:>9.1}s{:>3}   → warm costs {:+.1}%",
            "stale signal",
            stale_cold,
            format!("({oc})"),
            stale_warm,
            format!("({ow})"),
            pct(stale_cold, stale_warm),
        );
        println!(
            "  {:<14} {:>9.1}s{:>3} {:>9.1}s{:>3}   → warm costs {:+.1}%",
            "fresh signal",
            fresh_cold,
            format!("({fc})"),
            fresh_warm,
            format!("({fw})"),
            pct(fresh_cold, fresh_warm),
        );
        println!(
            "  verdict: warm-start's penalty is {:+.1}% under staleness vs {:+.1}% under \
             fresh signals — {}",
            pct(stale_cold, stale_warm),
            pct(fresh_cold, fresh_warm),
            if pct(fresh_cold, fresh_warm) < pct(stale_cold, stale_warm) - 5.0 {
                "F1 explains most of it (the brake reading holds)"
            } else {
                "F1 does NOT explain it — offloading loses on its own merits here"
            }
        );
    }
}

#[test]
fn household_evening_reproduces_the_findings_or_retires_them() {
    let s = scenario::household_evening_12(SEED);
    let (reports, scores) = sweep(&s);
    assert_hard_invariants(&reports, &scores);
    println!("\n{}", render(&s.name, SEED, &scores));

    let arm0 = &scores[0];
    let fresh = &scores[1];
    let two = &scores[2];

    println!("── F1 (dead time) ──");
    println!(
        "  arm 0 p50 {:.1}s → fresh-signals p50 {:.1}s  ({:+.1}%)",
        arm0.records.p50_total_ms / 1000.0,
        fresh.records.p50_total_ms / 1000.0,
        100.0 * (fresh.records.p50_total_ms - arm0.records.p50_total_ms)
            / arm0.records.p50_total_ms.max(1.0)
    );
    println!(
        "  median load-signal age: {:.1}s true / {:.1}s as recorded",
        arm0.truth.median_true_signal_age_ms / 1000.0,
        arm0.truth.median_recorded_signal_age_ms / 1000.0
    );
    let cov = |s: &ArmScore| {
        s.records
            .herding_cov
            .map(|c| format!("{c:.2}"))
            .unwrap_or_else(|| "— (fewer than two peers were ever eligible)".into())
    };
    println!("── F5 (herding) ──");
    println!(
        "  arm 0 p95 {:.1}s, top-server share {:.2}, CoV {}",
        arm0.records.p95_total_ms / 1000.0,
        arm0.records.top_server_share,
        cov(arm0),
    );
    println!(
        "  two-choices p95 {:.1}s, top-server share {:.2}, CoV {}",
        two.records.p95_total_ms / 1000.0,
        two.records.top_server_share,
        cov(two),
    );
    println!(
        "  eligible set: mean {:.2} peers strictly beat local; {}/{} offloads were single-candidate",
        arm0.truth.mean_eligible_peers, arm0.truth.singleton_choices, arm0.truth.offloads
    );
    println!("── waste ──");
    println!(
        "  arm 0: {}/{} offloads slower than local; of those, {} lost to the peer's QUEUE \
         (the rest bought capability with latency)",
        arm0.truth.slower_than_local, arm0.truth.offloads, arm0.truth.wasted_offloads
    );
    print_candidate_breakdown(&reports[0], 3);

    // The one structural claim strong enough to assert: a decision
    // taken on gossiped state is taken on *stale* state. If this ever
    // fails, the sim stopped modelling gossip and every F1 number
    // above is meaningless.
    assert!(
        arm0.truth.median_true_signal_age_ms > 0.0,
        "no staleness at all — the gossip model is not running"
    );
}

#[test]
fn a_heterogeneous_fleet_is_invisible_to_the_scorer() {
    // F3: `throughput_factor` divides by a 20 tok/s reference and
    // clamps to 1.0, so every node above 20 tok/s scores identically.
    // The fleet here spans 25 → 120 tok/s.
    let s = scenario::heterogeneous_fleet(SEED);
    let (reports, scores) = sweep(&s);
    assert_hard_invariants(&reports, &scores);
    println!("\n{}", render(&s.name, SEED, &scores));

    let arm0 = &reports[0];
    let throughput_factors: Vec<(String, f32)> = arm0
        .records
        .iter()
        .filter_map(|e| match e {
            sovereign_scheduler::decision_log::DecisionEvent::Decision(d) => Some(d),
            _ => None,
        })
        .flat_map(|d| {
            d.candidates
                .iter()
                .map(|c| (c.name.clone(), c.score.throughput_factor))
        })
        .collect();
    let distinct: std::collections::BTreeSet<String> = throughput_factors
        .iter()
        .map(|(name, f)| format!("{name}={f:.3}"))
        .collect();
    println!("── F3 (heterogeneity term) ──");
    println!("  distinct (node, throughput_factor) pairs actually scored:");
    for d in &distinct {
        println!("    {d}");
    }
    let all_saturated = throughput_factors
        .iter()
        .all(|(_, f)| (*f - 1.0).abs() < 1e-6);
    println!(
        "  every node's throughput_factor == 1.0: {all_saturated}  \
         (F3 predicts true across a 25→120 tok/s fleet)"
    );
}

/// F5's remedy needs a tie to break. `household_evening_12` has a
/// unique capability winner, so its eligible set is a singleton and
/// two-choices is a no-op there. This fleet has three identical hubs:
/// every laptop's eligible set holds three candidates scoring equal
/// to the last bit, which is the precise condition F5 names.
#[test]
fn three_identical_hubs_are_what_a_sampling_policy_needs_to_bite_on() {
    let s = scenario::twin_hubs(SEED);
    let (reports, scores) = sweep(&s);
    assert_hard_invariants(&reports, &scores);
    println!("\n{}", render(&s.name, SEED, &scores));

    println!("── F5 (herding), with a non-unique winner ──");
    for (report, sc) in reports.iter().zip(scores.iter()) {
        let inbound: Vec<String> = sc
            .records
            .served_by
            .iter()
            .filter(|(k, _)| !k.starts_with("<local"))
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        println!(
            "  {:<18} eligible {:.2}  CoV {:>5}  p95 {:>6.1}s  inbound: {}",
            report.arm.label(),
            sc.truth.mean_eligible_peers,
            sc.records
                .herding_cov
                .map(|c| format!("{c:.2}"))
                .unwrap_or_else(|| "—".into()),
            sc.records.p95_total_ms / 1000.0,
            inbound.join(" ")
        );
    }
}

/// `cold_start_weight`'s doc comment: the ramp exists "so new peers
/// still receive routable traffic (otherwise they'd never accumulate
/// history)". Multiplied against a locality bonus that favours local,
/// a cold peer needs a large claim advantage just to break even — so
/// the ramp can become self-locking: never chosen, therefore never
/// sampled, therefore never un-penalised.
///
/// Reported, not asserted: what this prints is a property of the
/// fleet as much as of the code, and the right response to a bad
/// number is a spec conversation, not a red build.
#[test]
fn does_the_cold_start_ramp_let_peers_accumulate_history() {
    for scenario in [
        scenario::household_evening_12(SEED),
        scenario::twin_hubs(SEED),
    ] {
        let report = run(&scenario, Arm::AsImplemented, SEED);
        let n = report.node_names.len();
        let mut ever_sampled = vec![false; n];
        let mut warm = vec![false; n];
        for row in &report.peer_samples {
            for (peer, samples) in row.iter().enumerate() {
                if *samples > 0 {
                    ever_sampled[peer] = true;
                }
                // COLD_START_SAMPLES = 20 in oicp-types.
                if *samples >= 20 {
                    warm[peer] = true;
                }
            }
        }
        println!("── cold-start ramp — `{}` ──", scenario.name);
        println!(
            "  peers ever dispatched to by anyone: {}/{}",
            ever_sampled.iter().filter(|x| **x).count(),
            n
        );
        println!(
            "  peers that ever reached a full ramp (20 samples) for some decider: {}/{}",
            warm.iter().filter(|x| **x).count(),
            n
        );
        let never: Vec<&str> = report
            .node_names
            .iter()
            .enumerate()
            .filter(|(i, _)| !ever_sampled[*i])
            .map(|(_, name)| name.as_str())
            .collect();
        if !never.is_empty() {
            println!("  never received a single request: {}", never.join(" "));
        }
    }
}
