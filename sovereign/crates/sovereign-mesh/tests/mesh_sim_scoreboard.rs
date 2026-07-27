// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tier-1 scoreboard — `SCHEDULER_QUALITY.md` Phase 1, step S0.
//!
//! Exit criterion for S0, verbatim: *"F1/F3/F5 reproduce against the
//! **real** scorer, or are retired as artifacts of my
//! transcription."* §3's numbers came from a 12-node model that
//! transcribed the scoring arithmetic by hand. These runs drive the
//! production decision function itself
//! (`sovereign_mesh::scheduler_core::rank`) over a simulated fleet,
//! so a finding that survives here survives against the code.
//!
//! Run it and read the table:
//!
//! ```text
//! cargo test -p sovereign-mesh --features mesh-sim,treesitter \
//!     --test mesh_sim_scoreboard -- --nocapture
//! ```
//!
//! The assertions below are the *hard invariants* of §5 — the things
//! that are true or the mesh is broken. The rest of the scoreboard is
//! reported, not asserted: a metric with a threshold nobody agreed on
//! is a flaky test, and a scoreboard that fails the build every time
//! a number moves stops being read.
//!
//! Without the feature this is an empty test binary, so the default
//! workspace `cargo test` neither compiles nor runs it — same
//! discipline as `dst_scenarios.rs`.
#![cfg(feature = "mesh-sim")]

use sovereign_mesh::mesh_sim::scenario::{self, RequestClass, Scenario};
use sovereign_mesh::mesh_sim::scoreboard::{render, score, ArmScore};
use sovereign_mesh::mesh_sim::{run, run_with, Arm, RunReport, SimConfig, ALL_ARMS};
use sovereign_mesh::predicted_time::{self, PredictInputs, RequestShape};

const SEED: u64 = 20_260_726;
const GOSSIP_WINDOW_MS: u64 = 10_000;

/// Run every arm over one scenario and score them against the
/// oracle's mean latency.
fn sweep(scenario: &Scenario) -> (Vec<RunReport>, Vec<ArmScore>) {
    let reports: Vec<RunReport> = ALL_ARMS
        .iter()
        .map(|arm| run(scenario, *arm, SEED))
        .collect();
    let oracle_mean = reports
        .iter()
        .find(|r| r.arm == Arm::Oracle)
        .map(|r| {
            r.truth.iter().map(|f| f.total_ms as f64).sum::<f64>() / r.truth.len().max(1) as f64
        })
        .expect("the oracle arm is always run");
    let scores = reports
        .iter()
        .map(|r| {
            let denom = (r.arm != Arm::Oracle).then_some(oracle_mean);
            score(r, GOSSIP_WINDOW_MS, denom)
        })
        .collect();
    (reports, scores)
}

/// Dump the full `ScoreBreakdown` of every candidate for the first
/// `n` decisions that scored more than one peer.
///
/// This is the Phase-0 glassbox payoff: "why did this go to the hub"
/// is answerable from the record alone, in the sim exactly as in
/// production. It exists because the first run showed a *mean
/// eligible set of 1.00 peers*, and the only honest way to find out
/// why is to read what the scorer saw.
fn print_candidate_breakdown(report: &RunReport, n: usize) {
    use sovereign_mesh::decision_log::DecisionEvent;
    println!("── what the scorer saw (first {n} multi-candidate decisions) ──");
    let mut shown = 0;
    for ev in &report.records {
        let DecisionEvent::Decision(d) = ev else {
            continue;
        };
        if d.candidates.len() < 2 || shown >= n {
            continue;
        }
        shown += 1;
        println!("  decision {} ({:?})", d.oicp_request_id, d.verdict);
        for c in &d.candidates {
            let s = &c.score;
            println!(
                "    {:<12} final {:>6.3} = claim {:>5.3} × obs {:>5.3} × load {:>5.3} \
                 × loc {:>5.3} × cold {:>5.3} × tput {:>5.3} ({}) × avail {:>5.3}   \
                 [in_flight {} src {:?} gossip_age {:?}s samples {}]",
                c.name,
                s.final_score,
                s.claim_score,
                s.observation_mult,
                s.load_penalty,
                s.locality_bonus,
                s.cold_start_weight,
                s.throughput_factor,
                s.throughput_source,
                s.availability,
                c.inputs.in_flight,
                c.inputs.in_flight_source,
                c.inputs.gossip_age_secs,
                c.inputs.samples,
            );
        }
    }
    if shown == 0 {
        println!("  (no decision scored more than one candidate)");
    }
}

/// §5's hard invariants. Assertions, not scores.
fn assert_hard_invariants(reports: &[RunReport], scores: &[ArmScore]) {
    for (report, score) in reports.iter().zip(scores.iter()) {
        let arm = report.arm.label();
        for fact in &report.truth {
            match fact.class {
                // `LocalOnly` is the privacy contract; `Fast` is
                // SLOT_POLICY §5. Neither may ever cross the wire.
                RequestClass::Private | RequestClass::Fast => assert_eq!(
                    fact.origin, fact.server,
                    "{arm}: a {:?} request was offloaded — hard invariant violated",
                    fact.class
                ),
                RequestClass::Knowledge => {}
            }
        }
        // Every decision joins to exactly one outcome, or the
        // calibration contract has nothing to compare.
        assert!(
            (score.records.join_rate - 1.0).abs() < 1e-9,
            "{arm}: join rate {:.3} — decisions and outcomes must join 1:1",
            score.records.join_rate
        );
        assert_eq!(
            score.records.shed, 0,
            "{arm}: something shed, but no admission gate is enabled (F4's caveat)"
        );

        // §5's third hard invariant — "no request served by a node
        // lacking the claimed capability" — which had no
        // implementation until capability became a banded, recorded
        // property (`crate::tier`). It binds only where a floor is
        // declared, so it is asserted per arm rather than fleet-wide.
        if matches!(
            report.arm,
            Arm::TierFloor | Arm::PredictedTimeTierFloor | Arm::PredictedTimeTierFloorTwoChoices
        ) {
            // A silent shortfall would satisfy the assertion below for
            // the wrong reason: nothing can be served below its band if
            // nothing has a band. Every simulated node advertises a
            // size, so this must be zero, and it is checked first so a
            // green invariant is never a vacuous one.
            assert_eq!(
                score.tier.unbanded_decisions, 0,
                "{arm}: {} decisions had no banded candidate — the tier invariant below \
                 would pass vacuously",
                score.tier.unbanded_decisions
            );
            assert_eq!(
                score.tier.downgrades, 0,
                "{arm}: {} turns were served in a WEAKER band than the origin's own local \
                 model despite a binding tier floor — the filter is not doing what the \
                 arm's latency numbers claim",
                score.tier.downgrades
            );
        }
    }
}

/// Is the latency an arm produces **queueing or serving** — and is the
/// queue stable or growing?
///
/// The discriminator that keeps "the tier floor is slow" from being a
/// conclusion instead of an observation. `queue_wait_ms` already
/// separates "the node was busy" from "the node was slower"; splitting
/// it by dispatch quartile adds the second half: a stable queue holds
/// roughly flat across the run, an **oversubscribed** one climbs
/// monotonically because arrivals outpace service and the backlog
/// never drains. The first is a scheduling result. The second is a
/// capacity fact about the fleet, and no scheduler can fix it.
fn print_saturation(report: &RunReport) {
    let Some(s) = saturation(report) else {
        return;
    };
    println!(
        "      {:<26} queue wait Q1 {:>6.1}s → Q4 {:>6.1}s   (service {:.1}s flat)  {}",
        report.arm.label(),
        s.q1_wait_s,
        s.q4_wait_s,
        s.service_s,
        if s.growing() {
            "← QUEUE GROWING: offered load exceeds capacity"
        } else {
            "← queue stable"
        }
    );
}

/// The numbers behind [`print_saturation`], for a caller that needs to
/// assert on them rather than read them.
struct Saturation {
    q1_wait_s: f64,
    q4_wait_s: f64,
    service_s: f64,
}

impl Saturation {
    /// The discriminator [`print_saturation`] has always printed: a
    /// backlog that never drains shows up as the last quartile waiting
    /// several times longer than the first.
    ///
    /// It is a *screen*, not a gate. The first quartile of a run that
    /// starts with every queue empty is always flattering, so the ratio
    /// fires on any fleet loaded enough to build a queue at all — which
    /// is most of them. Use [`backlog_depth`](Self::backlog_depth) when
    /// something has to be decided on the answer.
    fn growing(&self) -> bool {
        self.q4_wait_s > 3.0 * self.q1_wait_s.max(0.1)
    }

    /// How many whole turns deep the queue is by the end of the run —
    /// final-quartile wait expressed in units of this fleet's own
    /// service time.
    ///
    /// This is the scale-free version of the question, and it separates
    /// the §4.1.1 fleets cleanly where the ratio does not:
    /// household+floor waits 1020s against 26.8s of service (**38 turns
    /// deep**, and climbing), heterogeneous+floor 182s against 27.5s
    /// (**6.6**), twin-hubs+floor 8.6s against 26.8s (**0.32** — less
    /// than one turn). A queue that is a fraction of a job deep at the
    /// end of a run is a scheduling result; one that is dozens deep is
    /// a fleet that cannot serve its load.
    fn backlog_depth(&self) -> f64 {
        self.q4_wait_s / self.service_s.max(0.001)
    }
}

fn saturation(report: &RunReport) -> Option<Saturation> {
    let mut facts: Vec<&sovereign_mesh::mesh_sim::ServedFact> = report
        .truth
        .iter()
        .filter(|f| f.class == RequestClass::Knowledge)
        .collect();
    if facts.is_empty() {
        return None;
    }
    facts.sort_by_key(|f| f.dispatched_at_ms);
    let q = facts.len() / 4;
    let mean_wait = |slice: &[&sovereign_mesh::mesh_sim::ServedFact]| -> f64 {
        if slice.is_empty() {
            return 0.0;
        }
        slice.iter().map(|f| f.queue_wait_ms as f64).sum::<f64>() / slice.len() as f64 / 1000.0
    };
    let mean_service = |slice: &[&sovereign_mesh::mesh_sim::ServedFact]| -> f64 {
        if slice.is_empty() {
            return 0.0;
        }
        slice
            .iter()
            .map(|f| (f.total_ms.saturating_sub(f.queue_wait_ms)) as f64)
            .sum::<f64>()
            / slice.len() as f64
            / 1000.0
    };
    let first = &facts[..q.max(1)];
    let last = &facts[facts.len() - q.max(1)..];
    Some(Saturation {
        q1_wait_s: mean_wait(first),
        q4_wait_s: mean_wait(last),
        service_s: mean_service(&facts),
    })
}

/// Print the capability columns for a named subset of arms.
fn print_tier_block(scores: &[ArmScore], arms: &[Arm]) {
    println!("── capability (§4.1 tier floor) ──");
    for arm in arms {
        let Some(s) = scores.iter().find(|s| s.arm == *arm) else {
            continue;
        };
        let t = &s.tier;
        println!(
            "  {:<28} p50 {:>5.1}s  mean {:>5.1}s  eff {:>4}  down {:>3.0}%  declUp {:>3.0}%  \
             off {:>3.0}%  servedBand {:.2}",
            arm.label(),
            s.records.p50_total_ms / 1000.0,
            s.truth.mean_total_ms / 1000.0,
            s.efficiency_ratio
                .map(|e| format!("{e:.2}"))
                .unwrap_or_else(|| "—".into()),
            100.0 * t.downgrade_rate(),
            100.0 * t.declined_upgrade_rate(),
            100.0 * s.records.offloaded as f64 / s.records.decisions.max(1) as f64,
            t.mean_served_band,
        );
        let mut served: Vec<(&String, &usize)> = s.records.served_by.iter().collect();
        served.sort_by(|a, b| b.1.cmp(a.1));
        let top: Vec<String> = served
            .iter()
            .take(4)
            .map(|(name, n)| format!("{name}×{n}"))
            .collect();
        println!("      served by: {}", top.join("  "));
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
            sovereign_mesh::decision_log::DecisionEvent::Decision(d) => Some(d),
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
    let all_saturated = throughput_factors.iter().all(|(_, f)| (*f - 1.0).abs() < 1e-6);
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

/// Every scored candidate record in a run, flattened.
fn candidates(report: &RunReport) -> Vec<&sovereign_mesh::decision_log::CandidateRecord> {
    use sovereign_mesh::decision_log::DecisionEvent;
    report
        .records
        .iter()
        .filter_map(|e| match e {
            DecisionEvent::Decision(d) => Some(d),
            _ => None,
        })
        .flat_map(|d| d.candidates.iter())
        .collect()
}

fn mean_total_ms(report: &RunReport) -> f64 {
    report.truth.iter().map(|f| f.total_ms as f64).sum::<f64>() / report.truth.len().max(1) as f64
}

fn offloads(report: &RunReport) -> usize {
    report.truth.iter().filter(|f| f.origin != f.server).count()
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
                .filter(|c| c.kind == sovereign_mesh::decision_log::CandidateKind::Peer)
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
                .filter(|c| c.kind == sovereign_mesh::decision_log::CandidateKind::Peer)
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

/// The sample count a decider starts with, which `peer_samples` is
/// measured against.
fn seed_floor(arm: Arm) -> u32 {
    if arm.warm_start() {
        20
    } else {
        0
    }
}

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

/// Every decision in a run, paired with the candidate it chose and the
/// predicted time that candidate would have been given.
///
/// Recomputed **from the record**, not from the sim's internals, and
/// that is the load-bearing part: it demonstrates that a decision
/// record already carries everything §4.1 needs
/// (`in_flight`, `rtt_ms`, `bench_*` on `CandidateInputs`; the token
/// shape on `RequestFacts`), so the objective can be scored against a
/// **production** capture with no new instrumentation.
fn chosen_predictions(report: &RunReport) -> Vec<(String, f64, f64)> {
    use sovereign_mesh::decision_log::{CandidateKind, DecisionEvent};

    // decision_id → actual total, from the outcome half of the join.
    let mut actual: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    for f in &report.truth {
        actual.insert(f.decision_id.as_str(), f.total_ms as f64);
    }

    let mut out = Vec::new();
    for ev in &report.records {
        let DecisionEvent::Decision(d) = ev else {
            continue;
        };
        let shape = RequestShape::from_facts(&d.request);
        // The winner, or local when the decision stayed home. (A
        // gated decision records no candidates at all.)
        let Some(chosen) = d
            .candidates
            .iter()
            .find(|c| c.selected)
            .or_else(|| d.candidates.iter().find(|c| c.kind == CandidateKind::Local))
        else {
            continue;
        };
        let Ok(p) = predicted_time::predict(&PredictInputs::from_candidate(&chosen.inputs), shape)
        else {
            continue;
        };
        let Some(got) = actual.get(d.decision_id.as_str()) else {
            continue;
        };
        out.push((chosen.name.clone(), p.total_ms, *got));
    }
    out
}

/// **§4.1, priced: how much of the gap is a wrong objective, and how
/// much is imperfect information?**
///
/// `AsImplemented` and `Oracle` bracket the problem but do not
/// decompose it. `PredictedTime` is the missing middle term — the
/// oracle's *objective* (minimise time to answer) computed from what a
/// decider can actually see:
///
///   - `arm 0 → predicted` — the cost of a **wrong objective**, with
///     the information held constant.
///   - `predicted → oracle` — the cost of **imperfect information**,
///     with the objective held constant. The only term the two
///     disagree on is the queue: the oracle knows `backlog_ms`, a
///     decider knows a gossiped in-flight *count*.
///
/// Also prints the estimator's own error — predicted vs actual, joined
/// through the record — because a decomposition whose middle term is
/// built on a bad estimate is a decomposition of nothing.
///
/// Reported, not asserted, apart from two wiring checks. F7's lesson:
/// a knob that turns out to be disconnected prints the same flat table
/// as a mechanism that does not matter.
#[test]
fn predicted_time_decomposes_the_oracle_gap() {
    for s in [
        scenario::household_evening_12(SEED),
        scenario::twin_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
        scenario::isolation(SEED),
    ] {
        let arm0 = run(&s, Arm::AsImplemented, SEED);
        let pred = run(&s, Arm::PredictedTime, SEED);
        let oracle = run(&s, Arm::Oracle, SEED);

        // Wiring check 1: the arm must actually decide differently. If
        // the objective never reached `rank`, the world is identical
        // and so is the outcome.
        assert_ne!(
            offloads(&arm0),
            offloads(&pred),
            "{}: predicted-time took exactly as many offloads as the product — \
             the objective is not wired through RankInputs",
            s.name
        );

        // Wiring check 2: predictions must exist. A request with no
        // token shape is unpredictable for *every* candidate including
        // local, which collapses the arm into stay-local-always — a
        // table that would look like a strong result and mean nothing.
        let joined = chosen_predictions(&pred);
        assert!(
            !joined.is_empty(),
            "{}: no decision under predicted-time yielded a prediction — the OICP \
             envelope is carrying no token shape, so the arm degenerated to \
             stay-local and its numbers are meaningless",
            s.name
        );

        let mean_of = |r: &RunReport| mean_total_ms(r) / 1000.0;
        let (a, p, o) = (mean_of(&arm0), mean_of(&pred), mean_of(&oracle));
        let pct = |from: f64, to: f64| 100.0 * (to - from) / from.max(1.0);

        // How wrong was the estimate, as a fraction of what actually
        // happened? Median *and* p90: with an exact rate card an idle
        // candidate predicts exactly, so the median is 0 and says
        // nothing — the whole error lives in the tail, where the queue
        // substitution bites.
        let mut rel: Vec<f64> = joined
            .iter()
            .map(|(_, predicted, got)| (predicted - got).abs() / got.max(1.0))
            .collect();
        rel.sort_by(|x, y| x.total_cmp(y));
        let at = |q: f64| -> f64 {
            let i = ((q * rel.len() as f64).ceil() as usize).clamp(1, rel.len().max(1)) - 1;
            rel.get(i).copied().unwrap_or(0.0)
        };
        let (median_rel, p90_rel) = (at(0.5), at(0.9));

        // Where the work actually went. This is the line that matters
        // most for the landing: ranking on time alone prefers whichever
        // node answers soonest, which on this fleet is a small fast
        // model rather than the big capable one.
        let inbound = |r: &RunReport| -> String {
            let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
            for f in r.truth.iter().filter(|f| f.origin != f.server) {
                *counts.entry(r.node_names[f.server].as_str()).or_default() += 1;
            }
            if counts.is_empty() {
                return "nobody".into();
            }
            counts
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        };

        println!("── §4.1 decomposition — `{}` ──", s.name);
        println!(
            "  {:<22} mean {:>7.1}s  offloads {:>4}",
            "arm 0 (product)",
            a,
            offloads(&arm0)
        );
        println!(
            "  {:<22} mean {:>7.1}s  offloads {:>4}   ← wrong objective costs {:+.1}%",
            "predicted-time",
            p,
            offloads(&pred),
            pct(p, a)
        );
        println!(
            "  {:<22} mean {:>7.1}s  offloads {:>4}   ← imperfect info costs {:+.1}%",
            "oracle (perfect info)",
            o,
            offloads(&oracle),
            pct(o, p)
        );
        println!(
            "  estimator error |predicted − actual| / actual: median {:.0}%, p90 {:.0}% \
             over {} joined decisions",
            100.0 * median_rel,
            100.0 * p90_rel,
            joined.len()
        );
        println!("  arm 0 offloads landed on:          {}", inbound(&arm0));
        println!("  predicted-time offloads landed on: {}", inbound(&pred));
        println!(
            "  READ WITH CARE, two ways. (1) At `advertised_rate_error: 0.0` every node's \
             rate card is EXACT TRUTH by construction, so the middle term above carries \
             queue error only and no model error — see \
             `how_much_of_predicted_times_win_survives_a_wrong_rate_card`. (2) This arm \
             ranks on TIME ALONE. Compare the two `landed on` lines: it prefers whichever \
             node answers soonest, which is a small fast model, not the big capable one. \
             §4.1 requires a tier floor as a SEPARATE explicit input, and this arm does \
             not have one — no metric on this scoreboard can see what that costs."
        );
    }
}

/// **The harness's most flattering assumption, priced.**
///
/// `Arm::PredictedTime` reads `pp_tok_s` / `tg_tok_s` off each node's
/// advertised `BenchmarkResult` — and in this sim that benchmark is
/// built from the very `Hardware` the service-time model consumes. So
/// at the default `advertised_rate_error: 0.0` the rate card is *exact
/// truth*, the predictor's only error is the queue-count substitution,
/// and its efficiency ratio is an upper bound no real fleet can reach.
///
/// Publishing the decomposition without this table would repeat exactly
/// the mistake F7's first write-up made: a number that one mechanism
/// explains, presented as though only that mechanism could.
///
/// The oracle is unaffected — it reads true hardware, never the
/// advertised card — so it stays a valid denominator at every error
/// level. Arm 0 *is* affected (`throughput_factor` reads the same
/// benchmark), which keeps the comparison honest.
///
/// Reported, not asserted, apart from the knob's own wiring.
#[test]
fn how_much_of_predicted_times_win_survives_a_wrong_rate_card() {
    let advertised_rates = |r: &RunReport| -> Vec<String> {
        let mut v: Vec<String> = candidates(r)
            .iter()
            .filter_map(|c| c.inputs.bench_tg_tok_s)
            .map(|t| format!("{t:.3}"))
            .collect();
        v.sort();
        v.dedup();
        v
    };

    for s in [
        scenario::household_evening_12(SEED),
        scenario::twin_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
    ] {
        println!("── predicted-time vs a wrong rate card — `{}` ──", s.name);
        println!(
            "  {:>9}  {:>14}  {:>14}  {:>10}",
            "rate err", "arm 0 eff", "predicted eff", "pred mean"
        );
        let mut baseline_rates: Option<Vec<String>> = None;
        for err in [0.0f32, 0.10, 0.25, 0.50, 1.00] {
            let cfg = SimConfig {
                advertised_rate_error: err,
                ..Default::default()
            };
            let oracle = run_with(&s, Arm::Oracle, SEED, cfg.clone());
            let arm0 = run_with(&s, Arm::AsImplemented, SEED, cfg.clone());
            let pred = run_with(&s, Arm::PredictedTime, SEED, cfg.clone());

            // Knob wiring: at err > 0 the advertised rates must differ
            // from the perfect-card set, or this whole table is one
            // number printed five times.
            let rates = advertised_rates(&pred);
            match &baseline_rates {
                None => baseline_rates = Some(rates),
                Some(base) => assert_ne!(
                    base, &rates,
                    "{}: advertised_rate_error {err} left the rate card unchanged — \
                     the knob is not wired",
                    s.name
                ),
            }

            let o = mean_total_ms(&oracle);
            let eff = |r: &RunReport| (o / mean_total_ms(r).max(1.0)).clamp(0.0, 1.0);
            println!(
                "  {:>9.2}  {:>14.2}  {:>14.2}  {:>9.1}s",
                err,
                eff(&arm0),
                eff(&pred),
                mean_total_ms(&pred) / 1000.0,
            );
        }
        println!(
            "  a rate card off by ±{}× is the realistic case (hardware changes, \
             quantisation swaps, a benchmark measured under different thermal \
             conditions); ±0.0 is the harness being kind to itself.",
            2.0
        );
    }
}

/// **Does mis-attributed load hurt the new objective MORE than it hurts
/// the product?**
///
/// This closes a real hole in the §4.1 arm as first landed:
/// `Arm::published_load()` returned `Total` for every arm but one, so
/// predicted-time had never once seen a gossiped in-flight count that
/// missed inbound peer work.
///
/// The structural prior says it should hurt more. The product passes
/// `in_flight` through `load_penalty`, a **bounded** multiplier — a
/// wrong count moves the score a little. The predicted time
/// **multiplies it by a service time**, so the same wrong count is a
/// first-order error that scales with the queue. An objective that
/// trades the product's fudge factors for accuracy is only as good as
/// the inputs it trusts, and this is the input F2 says may be broken.
///
/// Reported, not asserted, apart from the wiring check — the sign of
/// the comparison is the finding.
#[test]
fn does_mis_attributed_load_hurt_predicted_time_more_than_the_product() {
    let published_sum = |r: &RunReport| -> u64 {
        candidates(r)
            .iter()
            .filter_map(|c| c.inputs.gossiped_in_flight)
            .map(u64::from)
            .sum()
    };
    for s in [
        scenario::household_evening_12(SEED),
        scenario::twin_hubs(SEED),
        scenario::isolation(SEED),
    ] {
        let p_total = run(&s, Arm::PredictedTime, SEED);
        let p_out = run(&s, Arm::PredictedTimeOutboundOnly, SEED);
        let a_total = run(&s, Arm::AsImplemented, SEED);
        let a_out = run(&s, Arm::OutboundOnlyLoad, SEED);

        // Wiring: the composed arm must actually publish less, or a
        // flat result means "no inbound work existed" rather than "the
        // objective is robust".
        assert!(
            published_sum(&p_out) < published_sum(&p_total),
            "{}: the composed arm published as much as the honest one — \
             `published_load()` is not reaching the predicted-time path",
            s.name
        );

        let pct = |from: &RunReport, to: &RunReport| {
            100.0 * (mean_total_ms(to) - mean_total_ms(from)) / mean_total_ms(from).max(1.0)
        };
        let product_damage = pct(&a_total, &a_out);
        let predicted_damage = pct(&p_total, &p_out);

        println!("── F2 × §4.1 — `{}` ──", s.name);
        println!(
            "  product:        {:>7.1}s → {:>7.1}s  ({:+.1}%)  offloads {} → {}",
            mean_total_ms(&a_total) / 1000.0,
            mean_total_ms(&a_out) / 1000.0,
            product_damage,
            offloads(&a_total),
            offloads(&a_out),
        );
        println!(
            "  predicted-time: {:>7.1}s → {:>7.1}s  ({:+.1}%)  offloads {} → {}",
            mean_total_ms(&p_total) / 1000.0,
            mean_total_ms(&p_out) / 1000.0,
            predicted_damage,
            offloads(&p_total),
            offloads(&p_out),
        );
        // The exposure is conditional on how much the objective offloads
        // at all: a wrong peer-queue count cannot hurt a decision that
        // stayed local. Print the share so the conditional is legible
        // rather than inferred from two raw counts.
        let offload_share = |r: &RunReport| {
            100.0 * offloads(r) as f64 / r.truth.len().max(1) as f64
        };
        println!(
            "  predicted-time offloads {:.0}% of traffic → {}",
            offload_share(&p_total),
            if predicted_damage > product_damage + 5.0 {
                "MORE damage than the product. The multiply-by-service-time exposure is real \
                 wherever the objective actually hops, so the two-daemon audit is a \
                 PREREQUISITE for landing §4.1 in this regime, not merely earned."
            } else if predicted_damage < product_damage - 5.0 {
                "LESS damage than the product — but not because it is robust. It declines most \
                 hops on this fleet, so a corrupted peer-queue count has little to corrupt. \
                 Do NOT read this as the objective being immune."
            } else {
                "comparable damage — F2's priority is unchanged by the objective here."
            }
        );
    }
}

/// **What model-load time does to the objective.** `predict()` charges
/// it as a single additive term; `SimConfig::model_load_sec_per_gb`
/// makes the world charge it too.
///
/// This term was missing when the arm landed, and its absence was the
/// most expensive of the three gaps: paging in a 21GB model is tens of
/// seconds, which dwarfs every other addend. With nothing in the
/// harness charging for it, the arm could not have found the mistake
/// itself — the same class of blind spot as the exact rate card.
///
/// What to read: whether pricing load changes *where* work goes. A cold
/// big model should stop being attractive, and a warm small one should
/// not — so this is also the first knob that gives the objective a
/// reason to prefer an already-loaded peer.
#[test]
fn what_model_load_time_does_to_the_predicted_time_objective() {
    for s in [
        scenario::household_evening_12(SEED),
        scenario::twin_hubs(SEED),
    ] {
        println!("── model-load time × §4.1 — `{}` ──", s.name);
        println!(
            "  {:>10}  {:>10}  {:>14}  {:>10}",
            "load s/GB", "arm 0 eff", "predicted eff", "pred mean"
        );
        for load in [0.0f64, 1.0, 3.0] {
            let cfg = SimConfig {
                model_load_sec_per_gb: load,
                ..Default::default()
            };
            let oracle = run_with(&s, Arm::Oracle, SEED, cfg.clone());
            let arm0 = run_with(&s, Arm::AsImplemented, SEED, cfg.clone());
            let pred = run_with(&s, Arm::PredictedTime, SEED, cfg.clone());

            // Wiring: above zero, candidates must actually advertise a
            // cold model with a load estimate, or the objective is
            // charging nothing and this row is a duplicate of the first.
            if load > 0.0 {
                let cold_advertised = candidates(&pred).iter().any(|c| {
                    c.inputs.model_loaded == Some(false)
                        && c.inputs.estimated_load_ms.unwrap_or(0) > 0
                });
                assert!(
                    cold_advertised,
                    "{}: load {load} s/GB but no candidate advertised a cold model with an \
                     estimate — the load term is not reaching the record or the objective",
                    s.name
                );
            }

            let o = mean_total_ms(&oracle);
            let eff = |r: &RunReport| (o / mean_total_ms(r).max(1.0)).clamp(0.0, 1.0);
            println!(
                "  {:>10.1}  {:>10.2}  {:>14.2}  {:>9.1}s",
                load,
                eff(&arm0),
                eff(&pred),
                mean_total_ms(&pred) / 1000.0,
            );
        }
    }
}

/// The defaults must stay inert, or every number recorded before the
/// rate-card and load-time knobs existed silently stops reproducing.
#[test]
fn a_perfect_rate_card_is_the_default_and_changes_nothing() {
    let s = scenario::household_evening_12(SEED);
    for arm in ALL_ARMS {
        let plain = run(&s, arm, SEED);
        let explicit = run_with(
            &s,
            arm,
            SEED,
            SimConfig {
                advertised_rate_error: 0.0,
                model_load_sec_per_gb: 0.0,
                ..Default::default()
            },
        );
        let key = |r: &RunReport| -> Vec<(usize, usize, u64, u64)> {
            r.truth
                .iter()
                .map(|f| (f.origin, f.server, f.dispatched_at_ms, f.total_ms))
                .collect()
        };
        assert_eq!(
            key(&plain),
            key(&explicit),
            "{}: the rate-card knob's default is not inert",
            arm.label()
        );
    }
}

/// The property every Tier-1 claim rests on.
#[test]
fn runs_are_bit_reproducible() {
    let s = scenario::household_evening_12(SEED);
    for arm in ALL_ARMS {
        let a = run(&s, arm, SEED);
        let b = run(&s, arm, SEED);
        let key = |r: &RunReport| -> Vec<(usize, usize, u64, u64)> {
            r.truth
                .iter()
                .map(|f| (f.origin, f.server, f.dispatched_at_ms, f.total_ms))
                .collect()
        };
        assert_eq!(key(&a), key(&b), "{} is not reproducible", arm.label());
    }
}

/// **§4.1's landing gate: what does respecting capability cost?**
///
/// `Arm::PredictedTime` measured a large latency win and, in the same
/// run, exposed why it could not ship: ranking on time alone prefers
/// whichever node answers soonest, and on every fleet here that is a
/// small fast model. Nothing on §5's scoreboard could see it — latency,
/// fairness and waste all read the choice as an improvement.
///
/// So this test does two things the arm alone could not:
///
///   1. **Makes the damage a number.** `declUp%` is the share of turns
///      that passed over a strictly more capable feasible node. It is
///      the finding, counted rather than described.
///   2. **Prices the fix.** `predicted-time+tier-floor` is §4.1 made to
///      respect capability. The question is not whether it is faster —
///      it will not be. It is how much of the win survives.
///
/// Reported, not asserted, apart from the structural claims: nobody has
/// agreed a latency threshold for the trade, and asserting one would
/// invent the very fudge §4.1 exists to remove.
#[test]
fn the_tier_floor_prices_capability_against_latency() {
    let arms = [
        Arm::AsImplemented,
        Arm::TierFloor,
        Arm::PredictedTime,
        Arm::PredictedTimeTierFloor,
        Arm::Oracle,
    ];

    for scenario in [
        scenario::household_evening_12(SEED),
        scenario::twin_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
    ] {
        let (reports, scores) = sweep(&scenario);
        assert_hard_invariants(&reports, &scores);
        println!("\n=== {} ===", scenario.name);
        print_tier_block(&scores, &arms);

        // Whether the floor's latency is a scheduling result or a
        // capacity fact. Asserting nothing — reading it is the point.
        println!("── is the latency queueing, and is the queue stable? ──");
        for arm in [Arm::AsImplemented, Arm::PredictedTime, Arm::PredictedTimeTierFloor] {
            if let Some(r) = reports.iter().find(|r| r.arm == arm) {
                print_saturation(r);
            }
        }

        let pick = |arm: Arm| {
            scores
                .iter()
                .find(|s| s.arm == arm)
                .unwrap_or_else(|| panic!("{} was not run", arm.label()))
        };
        let arm0 = pick(Arm::AsImplemented);
        let pred = pick(Arm::PredictedTime);
        let floor = pick(Arm::PredictedTimeTierFloor);

        let price = |a: &ArmScore, b: &ArmScore| {
            100.0 * (b.truth.mean_total_ms - a.truth.mean_total_ms) / a.truth.mean_total_ms.max(1.0)
        };
        println!(
            "  §4.1 win vs arm 0: {:+.0}%   ·   what the floor costs §4.1: {:+.0}%   ·   \
             floor vs arm 0: {:+.0}%",
            price(arm0, pred),
            price(pred, floor),
            price(arm0, floor),
        );
        println!(
            "  quality traded: predicted-time declined {} upgrades ({:.0}%); with the floor, {} ({:.0}%)",
            pred.tier.declined_upgrades,
            100.0 * pred.tier.declined_upgrade_rate(),
            floor.tier.declined_upgrades,
            100.0 * floor.tier.declined_upgrade_rate(),
        );

        // The floor's whole promise, and the only thing strong enough
        // to assert: with it, no turn is served below the best band
        // that was available to it. Both counts, so a zero in one
        // cannot hide a non-zero in the other.
        assert_eq!(
            floor.tier.downgrades, 0,
            "{}: tier floor still allowed a downgrade",
            scenario.name
        );
        assert_eq!(
            floor.tier.declined_upgrades, 0,
            "{}: tier floor still allowed {} declined upgrades — a binding floor admits \
             only band 0, so anything served below it means the filter did not run",
            scenario.name,
            floor.tier.declined_upgrades
        );
        // And the baseline it is measured against must be untouched by
        // any of it. If adding the floor arms moved arm 0, every
        // recorded number in §3/§4.1 is invalidated and the comparison
        // above is meaningless.
        assert!(
            arm0.tier.banded_decisions > 0,
            "{}: arm 0 recorded no banded decisions — the fleet advertises no sizes and \
             the tier columns are vacuous",
            scenario.name
        );
    }
}

/// **§4.1.1 consequence 1, with the sample it lacked.**
///
/// The one result that changed the plan was a *constant-quality*
/// comparison: put the tier floor on both arms, so both answer from
/// band 0, and ask whether ranking on predicted time still beats
/// ranking on the product. §4.1.1 could only run that comparison on
/// `twin-hubs`, because it was the suite's only fleet whose top band
/// could absorb the offered load — everywhere else the floor's latency
/// is a queue that never drains, and two schedulers inside an unbounded
/// queue are both just measuring the fleet's capacity. One fleet, one
/// seed, and a −5% headline that contradicted §4.1's +126–250%.
///
/// This widens the sample on both axes that were n=1:
///
///   - **Seeds.** Five, per fleet, world and policy both re-seeded.
///   - **Fleets.** `mixed-hubs` joins it, and it is deliberately the
///     *opposite* bracket. `twin-hubs` band 0 is three identical hubs,
///     so predicted time has nothing to discriminate on but a stale
///     queue count — the condition most hostile to it. `mixed-hubs`
///     band 0 spans 34 / 25 / 11 tok/s, which is what predicting a
///     completion time is *for*. If the objective loses on both, the
///     −5% was not an artifact of homogeneity. If it wins on one, the
///     honest answer is "it depends on the fleet", and that is a
///     different plan than either headline implies.
///
/// The precondition is asserted rather than assumed, because it is the
/// only thing that makes the comparison mean anything: both fleets must
/// still be *unsaturated* under the floor, or this test silently
/// becomes another capacity measurement. Everything else is reported.
#[test]
fn does_predicted_time_beat_the_product_where_the_top_band_has_capacity() {
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    let fleets: [(&str, fn(u64) -> Scenario); 2] = [
        ("twin-hubs", scenario::twin_hubs),
        ("mixed-hubs", scenario::mixed_hubs),
    ];

    for (label, build) in fleets {
        println!("\n=== {label} — constant quality (both arms wear the tier floor) ===");
        println!(
            "  {:<8} {:>18} {:>22} {:>9}  {}",
            "seed", "arm0+floor mean/p95", "predicted+floor mean/p95", "Δ mean", "top-server share"
        );
        let mut base_means = Vec::new();
        let mut pred_means = Vec::new();
        let mut pred_wins = 0;
        for seed in seeds {
            let s = build(seed);
            let base_report = run(&s, Arm::TierFloor, seed);
            let pred_report = run(&s, Arm::PredictedTimeTierFloor, seed);
            let base = score(&base_report, GOSSIP_WINDOW_MS, None);
            let pred = score(&pred_report, GOSSIP_WINDOW_MS, None);

            // Constant quality is the premise, not a hope: if either
            // arm served a turn below the best band available to it,
            // the latency columns below are comparing two different
            // products and the whole test is void.
            for (arm, sc) in [("arm0+floor", &base), ("predicted+floor", &pred)] {
                assert_eq!(
                    (sc.tier.downgrades, sc.tier.declined_upgrades),
                    (0, 0),
                    "{label}/{seed}: {arm} traded quality ({} downgrades, {} declined \
                     upgrades) — the constant-quality comparison below is void",
                    sc.tier.downgrades,
                    sc.tier.declined_upgrades
                );
            }

            // The property the fleet exists to provide. Asserted for
            // the same reason: an unbounded queue makes both arms
            // measure capacity instead of policy. Three turns is a
            // deliberately loose gate — the fleets §4.1.1 called
            // saturated sit at 6.6 and 38, these two at well under 1 —
            // so it fails on the thing it is watching for and not on
            // the ordinary queueing a loaded fleet does.
            let mut depths = Vec::new();
            for (arm, report) in [("arm0+floor", &base_report), ("predicted+floor", &pred_report)] {
                let Some(sat) = saturation(report) else {
                    continue;
                };
                depths.push(sat.backlog_depth());
                assert!(
                    sat.backlog_depth() < 3.0,
                    "{label}/{seed}: {arm} ended the run {:.1} turns deep in queue \
                     (wait {:.1}s → {:.1}s against {:.1}s of service) — this fleet's top band \
                     is saturated under the floor, so it cannot host a constant-quality \
                     comparison of schedulers",
                    sat.backlog_depth(),
                    sat.q1_wait_s,
                    sat.q4_wait_s,
                    sat.service_s
                );
            }

            let b = base.truth.mean_total_ms / 1000.0;
            let p = pred.truth.mean_total_ms / 1000.0;
            base_means.push(b);
            pred_means.push(p);
            if p < b {
                pred_wins += 1;
            }
            println!(
                "  {:<8} {:>10.1}s {:>6.1}s {:>14.1}s {:>6.1}s {:>+8.0}%   {:.2} → {:.2}   \
                 backlog {}",
                seed % 1000,
                b,
                base.records.p95_total_ms / 1000.0,
                p,
                pred.records.p95_total_ms / 1000.0,
                100.0 * (p - b) / b.max(0.001),
                base.records.top_server_share,
                pred.records.top_server_share,
                depths
                    .iter()
                    .map(|d| format!("{d:.2}"))
                    .collect::<Vec<_>>()
                    .join(" → "),
            );
        }
        let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len() as f64;
        let (b, p) = (mean(&base_means), mean(&pred_means));
        println!(
            "  ── {label}: predicted-time is {:+.0}% vs the product at constant quality \
             ({:.1}s → {:.1}s), winning {}/{} seeds",
            100.0 * (p - b) / b.max(0.001),
            b,
            p,
            pred_wins,
            seeds.len()
        );

        // Where the work actually went, on one seed. A latency delta
        // without this is a number without a mechanism.
        let s = build(SEED);
        for (arm_label, arm) in [("arm0+floor", Arm::TierFloor), ("pred+floor", Arm::PredictedTimeTierFloor)] {
            let sc = score(&run(&s, arm, SEED), GOSSIP_WINDOW_MS, None);
            let mut by: Vec<(&String, &usize)> = sc.records.served_by.iter().collect();
            by.sort_by(|a, b| b.1.cmp(a.1));
            let cols: Vec<String> = by.iter().map(|(n, c)| format!("{n}×{c}")).collect();
            println!("     {arm_label:<12} {}", cols.join("  "));
        }
    }

    // **Which half of the fleet's heterogeneity is doing the work?**
    //
    // `mixed-hubs` band 0 contains two different gaps: 11 tok/s vs the
    // rest, which the product *can* see (11/20 scores 0.55), and 34
    // vs 25 tok/s, which it cannot (`throughput_factor` clamps both to
    // 1.0). If predicted-time's win survives deleting the first gap,
    // the mechanism is the clamp — F3 costing real latency — and not
    // merely "one node was obviously bad".
    //
    // The counterfactual is exact: same seed, same arrival stream, same
    // sizes and therefore the same bands. Only `hub-slow`'s hardware
    // changes, and arrival generation reads neither field.
    println!("\n=== mixed-hubs with the slow hub deleted (34/25/25 — every gap invisible to the scorer) ===");
    let mut no_slow = scenario::mixed_hubs(SEED);
    let mid = no_slow.nodes[1].hardware;
    no_slow.nodes[2].hardware = mid;
    no_slow.name = "mixed-hubs-no-slow".into();
    let with_slow = scenario::mixed_hubs(SEED);
    assert_eq!(
        no_slow.arrivals.len(),
        with_slow.arrivals.len(),
        "changing a node's hardware must not change the arrival stream"
    );
    for (fleet_label, fleet) in [("34/25/11", &with_slow), ("34/25/25", &no_slow)] {
        let base = score(&run(fleet, Arm::TierFloor, SEED), GOSSIP_WINDOW_MS, None);
        let pred = score(
            &run(fleet, Arm::PredictedTimeTierFloor, SEED),
            GOSSIP_WINDOW_MS,
            None,
        );
        let (b, p) = (
            base.truth.mean_total_ms / 1000.0,
            pred.truth.mean_total_ms / 1000.0,
        );
        println!(
            "  band 0 = {fleet_label}   arm0+floor {:>5.1}s   predicted+floor {:>5.1}s   \
             Δ {:+.0}%",
            b,
            p,
            100.0 * (p - b) / b.max(0.001)
        );
    }

    // **The flattering assumption underneath all of it.**
    //
    // `Arm::PredictedTime`'s own doc bounds what may be claimed from
    // it: the objective consumes `pp_tok_s` / `tg_tok_s` directly, and
    // this module's service-time model is computed from those same two
    // fields. On a fleet built out of *speed variance*, that is not a
    // small caveat — it hands the objective a perfect model of the
    // world it is predicting, which is exactly the advantage being
    // measured. A win that only exists at zero rate-card error is a
    // property of the simulator, not of the objective.
    //
    // `advertised_rate_error` is the instrument that already exists for
    // this: nodes still *serve* at their true rate, they only advertise
    // a perturbed one. It is two-sided, so it degrades the product's
    // `throughput_factor` too — the comparison stays fair.
    //
    // One asymmetry could have broken that fairness, so it is counted
    // rather than argued. The product has an error-correcting path the
    // predicted time does not: `throughput_factor` switches to the
    // *observed* decode EWMA past five samples, while
    // `PredictInputs::from_candidate` reads `bench_tg_tok_s` and
    // nothing else. If that path were hot, the sweep would be
    // handicapping only one arm. The printed share says it is not —
    // about 5% of candidate scorings, because most peers never
    // accumulate five samples in half an hour, which is F7's ramp
    // wearing a different hat. Both objectives are therefore reading
    // the same perturbed number in ~95% of decisions.
    println!("\n=== mixed-hubs: how much of the win survives a wrong rate card? ===");
    println!("   (both arms wear the floor; nodes serve at the true rate, advertise a perturbed one)");
    for err in [0.0_f32, 0.25, 0.5, 1.0] {
        let mut base_means = Vec::new();
        let mut pred_means = Vec::new();
        let mut pred_wins = 0;
        let mut observed = 0usize;
        let mut estimated = 0usize;
        for seed in seeds {
            let s = scenario::mixed_hubs(seed);
            let cfg = SimConfig {
                advertised_rate_error: err,
                ..SimConfig::default()
            };
            let base_report = run_with(&s, Arm::TierFloor, seed, cfg.clone());
            // Which rate did the *product* actually score on? It is the
            // only one of the two objectives with an error-correcting
            // path — `throughput_factor` prefers the observed EWMA past
            // five samples, where `predicted_time` reads
            // `bench_tg_tok_s` and nothing else. Whether that path was
            // hot decides how the rows below may be read.
            for ev in &base_report.records {
                if let sovereign_mesh::decision_log::DecisionEvent::Decision(d) = ev {
                    for c in &d.candidates {
                        match c.score.throughput_source.as_str() {
                            "observed" => observed += 1,
                            "benchmark_estimate" => estimated += 1,
                            _ => {}
                        }
                    }
                }
            }
            let b = score(&base_report, GOSSIP_WINDOW_MS, None)
                .truth
                .mean_total_ms
                / 1000.0;
            let p = score(
                &run_with(&s, Arm::PredictedTimeTierFloor, seed, cfg),
                GOSSIP_WINDOW_MS,
                None,
            )
            .truth
            .mean_total_ms
                / 1000.0;
            if p < b {
                pred_wins += 1;
            }
            base_means.push(b);
            pred_means.push(p);
        }
        let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len() as f64;
        let (b, p) = (mean(&base_means), mean(&pred_means));
        println!(
            "  rate error ±{:>4.0}%   arm0+floor {:>5.1}s   predicted+floor {:>5.1}s   \
             Δ {:+.0}%   predicted wins {}/{} seeds   (product scored on observed rate \
             {:.0}% of the time)",
            100.0 * err,
            b,
            p,
            100.0 * (p - b) / b.max(0.001),
            pred_wins,
            seeds.len(),
            100.0 * observed as f64 / (observed + estimated).max(1) as f64,
        );
    }
}

/// **Is breaking the herd the prerequisite §4.1.1 said it was?**
///
/// §4.1.1's third consequence was that predicted time *concentrates*
/// harder than the product once the floor makes candidates homogeneous
/// — 40/28/10 across three identical hubs against the product's
/// 31/27/18 — and concluded that §4.2 step 2 is a prerequisite for the
/// floor rather than a follow-on. That was a mechanism inferred from a
/// distribution. This measures it, by putting the sampler the sim has
/// had since S0 in front of the §4.1 landing candidate.
///
/// The two unsaturated fleets should disagree, and the disagreement is
/// the finding:
///
///   - `twin-hubs` — band 0 is three identical hubs, so a uniform draw
///     over the ranked list *is* a draw over near-ties. Sampling can
///     only help, and how much it helps is the price of the herd.
///   - `mixed-hubs` — band 0 spans 34 / 25 / 11 tok/s, so a uniform
///     draw discards the information the objective exists to use.
///
/// If both improve, §4.2 step 2 can ship as written and blunt. If
/// `mixed-hubs` regresses, the "within noise" qualifier in §4.2 step 2
/// is the load-bearing part of that sentence and a blunt sampler is a
/// quality-neutral latency regression waiting to happen.
///
/// Reported, not asserted, other than the floor's own invariant: the
/// trade between a fleet-mean and a tail has no agreed threshold.
#[test]
fn does_breaking_the_herd_recover_what_the_floor_costs() {
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    let fleets: [(&str, fn(u64) -> Scenario); 2] = [
        ("twin-hubs", scenario::twin_hubs),
        ("mixed-hubs", scenario::mixed_hubs),
    ];
    let arms = [
        Arm::TierFloor,
        Arm::PredictedTimeTierFloor,
        Arm::PredictedTimeTierFloorTwoChoices,
    ];

    for (label, build) in fleets {
        println!("\n=== {label} — does sampling break the herd, and what does it cost? ===");
        let mut baseline = None;
        for arm in arms {
            let (mut means, mut p95s, mut shares, mut covs) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for seed in seeds {
                let s = build(seed);
                let report = run(&s, arm, seed);
                let sc = score(&report, GOSSIP_WINDOW_MS, None);
                // The floor still has to hold, or the arm is buying its
                // latency with the quality the floor exists to protect.
                assert_eq!(
                    (sc.tier.downgrades, sc.tier.declined_upgrades),
                    (0, 0),
                    "{label}/{seed}/{}: sampling escaped the tier floor",
                    arm.label()
                );
                means.push(sc.truth.mean_total_ms / 1000.0);
                p95s.push(sc.records.p95_total_ms / 1000.0);
                shares.push(sc.records.top_server_share);
                if let Some(cov) = sc.records.herding_cov {
                    covs.push(cov);
                }
            }
            let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len().max(1) as f64;
            let m = mean(&means);
            let base = *baseline.get_or_insert(m);
            println!(
                "  {:<40} mean {:>5.1}s  p95 {:>5.1}s  top-server {:.2}  herding CoV {:.2}   \
                 {:+.0}% vs arm0+floor",
                arm.label(),
                m,
                mean(&p95s),
                mean(&shares),
                mean(&covs),
                100.0 * (m - base) / base.max(0.001),
            );
        }
    }
}

/// **The tier floor's flattering assumption, priced.**
///
/// `size_gb` is peer-advertised, and it is the *only* input to a
/// quality gate that a node states about itself. `advertised_rate_error`
/// exists because a rate card built from the same `Hardware` the sim
/// serves at is exact truth by construction; the same objection applies
/// here, and the same instrument answers it.
///
/// The adversarial direction is a small model over-selling its way into
/// the top band — which is exactly how a 4B would come to serve
/// synthesis despite the floor. Nothing here scores or serves
/// differently, so any movement is the floor mis-banding somebody and
/// nothing else.
///
/// Reported, not asserted, apart from the knob's own wiring: what
/// counts as an acceptable mis-banding rate is a policy question nobody
/// has answered yet.
#[test]
fn what_a_dishonest_size_advertisement_does_to_the_tier_floor() {
    use sovereign_mesh::decision_log::DecisionEvent;

    // Band 0 membership is what the floor reads; everything below it is
    // scenery. Reported per seed because whether a lie crosses a band
    // edge depends on which way each node's draw went, and one seed
    // would report an accident as a property.
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    println!("\n── tier floor under mis-advertised size (household-evening-12) ──");
    println!("   the floor reads band 0 only. hub 21.0 GB is 3.5x the next model,");
    println!("   and the band edge is 2.0x — so a lie must move the RATIO by 1.75x to matter.");
    for err in [0.0_f32, 0.25, 0.5, 1.0] {
        let mut intruded = 0;
        let mut downgrades = 0;
        let mut means = Vec::new();
        for seed in seeds {
            let s = scenario::household_evening_12(seed);
            let cfg = SimConfig {
                advertised_size_error: err,
                ..SimConfig::default()
            };
            let report = run_with(&s, Arm::PredictedTimeTierFloor, seed, cfg);
            let sc = score(&report, GOSSIP_WINDOW_MS, None);
            downgrades += sc.tier.downgrades;
            means.push(sc.truth.mean_total_ms / 1000.0);
            // Did anything but the hub reach band 0?
            let non_hub_in_top = report.records.iter().any(|ev| match ev {
                DecisionEvent::Decision(d) => d
                    .candidates
                    .iter()
                    .any(|c| c.tier_band == Some(0) && c.name != "hub" && c.name != "local"),
                _ => false,
            });
            if non_hub_in_top {
                intruded += 1;
            }
        }
        println!(
            "  size error +/-{:>4.0}%   seeds where a non-hub reached band 0: {}/{}   \
             downgrades {}   mean {:>6.1}s",
            100.0 * err,
            intruded,
            seeds.len(),
            downgrades,
            means.iter().sum::<f64>() / means.len() as f64,
        );
        if err == 0.0 {
            assert_eq!(
                intruded, 0,
                "an honest fleet put something other than the 35B hub in the top band"
            );
        }
        // The invariant is about the floor's INTEGRITY, not the fleet's
        // honesty: whatever the decider believes the bands are, it must
        // never serve a turn below the origin's own local band. A lie
        // can move a node between bands; it must not be able to make
        // the filter stop filtering.
        assert_eq!(
            downgrades, 0,
            "size error +/-{:.0}%: the floor allowed {} downgrades — a mis-advertisement \
             changed WHICH band a node is in, which is expected, but it must not defeat \
             the filter itself",
            100.0 * err,
            downgrades
        );
    }

    // The knob's default must be inert, or every number recorded before
    // it existed silently changed meaning.
    let s = scenario::household_evening_12(SEED);
    let plain = run(&s, Arm::PredictedTimeTierFloor, SEED);
    let explicit = run_with(
        &s,
        Arm::PredictedTimeTierFloor,
        SEED,
        SimConfig {
            advertised_size_error: 0.0,
            ..SimConfig::default()
        },
    );
    let key = |r: &RunReport| -> Vec<(usize, usize, u64, u64)> {
        r.truth
            .iter()
            .map(|f| (f.origin, f.server, f.dispatched_at_ms, f.total_ms))
            .collect()
    };
    assert_eq!(
        key(&plain),
        key(&explicit),
        "the size-advertisement knob's default is not inert"
    );
}
