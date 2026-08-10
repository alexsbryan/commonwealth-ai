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
            Arm::TierFloor
                | Arm::PredictedTimeTierFloor
                | Arm::PredictedTimeTierFloorTwoChoices
                | Arm::PredictedTimeTierFloorWithinNoise
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
        let offload_share =
            |r: &RunReport| 100.0 * offloads(r) as f64 / r.truth.len().max(1) as f64;
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
        for arm in [
            Arm::AsImplemented,
            Arm::PredictedTime,
            Arm::PredictedTimeTierFloor,
        ] {
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
            scenario.name, floor.tier.declined_upgrades
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
            for (arm, report) in [
                ("arm0+floor", &base_report),
                ("predicted+floor", &pred_report),
            ] {
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
        for (arm_label, arm) in [
            ("arm0+floor", Arm::TierFloor),
            ("pred+floor", Arm::PredictedTimeTierFloor),
        ] {
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
    println!(
        "   (both arms wear the floor; nodes serve at the true rate, advertise a perturbed one)"
    );
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
        // §4.2 step 2 as written. The blunt arm above reads the two
        // fleets in opposite directions; this one is the claim that
        // restricting the draw to the tie band keeps BOTH readings —
        // twin-hubs' recovery and mixed-hubs' win. Printed beside its
        // predecessor because the comparison is the whole point.
        Arm::PredictedTimeTierFloorWithinNoise,
    ];

    for (label, build) in fleets {
        println!("\n=== {label} — does sampling break the herd, and what does it cost? ===");
        let mut baseline = None;
        for arm in arms {
            let (mut means, mut p95s, mut shares, mut covs) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            // Summed across seeds: how often the sampler HAD a choice,
            // and how often it took one. Without it, "same mean as the
            // arm below" is ambiguous between "never fired" and "fired
            // constantly and it was a wash".
            let (mut fired, mut moved, mut decided, mut band_sum) = (0u64, 0u64, 0u64, 0u64);
            for seed in seeds {
                let s = build(seed);
                let report = run(&s, arm, seed);
                fired += report.sampler.band_at_least_two;
                moved += report.sampler.moved_off_argmax;
                decided += report.sampler.decisions;
                band_sum += report.sampler.band_total;
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
            if decided > 0 {
                let pct = |n: u64| 100.0 * n as f64 / decided as f64;
                println!(
                    "  {:<40}   └─ band ≥2 on {:.0}% of {decided} decisions \
                     (mean band {:.2}), moved off the argmax on {:.0}%",
                    "",
                    pct(fired),
                    band_sum as f64 / decided as f64,
                    pct(moved),
                );
            }
        }
    }
}

/// **The within-noise band's stated limit, measured rather than
/// asserted.**
///
/// `predicted_time::tie_band` admits a candidate only when its
/// *uncontended* prediction does not separate it from the leader's, and
/// on `twin-hubs` that is every band-0 hub — identical hardware on a
/// uniform LAN advertises an identical rate card, so nothing separates
/// them. That identity is precisely the flattering assumption
/// `advertised_rate_error` exists to price (note 963a8d88's method
/// rule: an arm must price the harness assumption that most flatters
/// it, and this arm's band is built out of one). Perturb the card, the
/// hubs stop looking identical *to the decider*, the band narrows, and
/// §4.1.2's −4% recovery should decay with it.
///
/// The decay is the safe direction — a narrow band falls back to the
/// argmax, which is the arm this one refines, so the cost is a
/// forfeited recovery and not a regression. But "conservative" is a
/// claim about a number, and §6's rule is that claims about numbers get
/// numbers. A real fleet of near-identical hubs (34 vs 33 tok/s) sits
/// somewhere on this curve, and this is the table that says where.
///
/// Reported, not asserted, apart from the mechanical claim: the band
/// has to actually narrow, or the paragraph above describes something
/// the code does not do.
#[test]
fn what_the_within_noise_band_costs_when_identical_hubs_stop_looking_identical() {
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    println!("\n=== twin-hubs: the band is built on an exact rate card — what if it is wrong? ===");
    println!(
        "   (identical hubs; only what they ADVERTISE is perturbed, never what they serve at)"
    );
    let mut first_band = None;
    let mut last_band = 0.0;
    for err in [0.0_f32, 0.1, 0.25, 0.5, 1.0] {
        let (mut floor_means, mut argmax_means, mut blunt_means, mut noise_means) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut decided, mut band_sum, mut fired) = (0u64, 0u64, 0u64);
        for seed in seeds {
            let s = scenario::twin_hubs(seed);
            let cfg = SimConfig {
                advertised_rate_error: err,
                ..SimConfig::default()
            };
            let mean_of = |r: &_| score(r, GOSSIP_WINDOW_MS, None).truth.mean_total_ms / 1000.0;
            floor_means.push(mean_of(&run_with(&s, Arm::TierFloor, seed, cfg.clone())));
            argmax_means.push(mean_of(&run_with(
                &s,
                Arm::PredictedTimeTierFloor,
                seed,
                cfg.clone(),
            )));
            blunt_means.push(mean_of(&run_with(
                &s,
                Arm::PredictedTimeTierFloorTwoChoices,
                seed,
                cfg.clone(),
            )));
            let sampled = run_with(&s, Arm::PredictedTimeTierFloorWithinNoise, seed, cfg);
            decided += sampled.sampler.decisions;
            band_sum += sampled.sampler.band_total;
            fired += sampled.sampler.band_at_least_two;
            noise_means.push(mean_of(&sampled));
        }
        let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len().max(1) as f64;
        let (floor, argmax, blunt, noise) = (
            mean(&floor_means),
            mean(&argmax_means),
            mean(&blunt_means),
            mean(&noise_means),
        );
        let band = band_sum as f64 / decided.max(1) as f64;
        first_band.get_or_insert(band);
        last_band = band;
        println!(
            "  ±{:>4.0}%  arm0+floor {:>5.1}s  argmax {:>5.1}s  blunt {:>5.1}s  \
             within-noise {:>5.1}s ({:+.0}% vs argmax)   mean band {:.2}, ≥2 on {:.0}%",
            err * 100.0,
            floor,
            argmax,
            blunt,
            noise,
            100.0 * (noise - argmax) / argmax.max(0.001),
            band,
            100.0 * fired as f64 / decided.max(1) as f64,
        );
    }
    let first = first_band.expect("the sweep ran at least one row");
    assert!(
        last_band < first,
        "a perturbed rate card must narrow the band ({first:.2} → {last_band:.2}); if it does \
         not, the band is not reading the rate card the way tie_band's docs claim"
    );
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
            downgrades,
            0,
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

/// **§4.2 step 1, priced: what does the *implementable* half of fresh
/// signals actually buy?**
///
/// `fresh-signals` is the arm every other finding in this file leans
/// on, and it is not a policy anybody can ship. It hands every decider
/// the truth about every peer at every instant. §3.1 priced it at −11%
/// p95 on `household-evening-12` and −51% on `twin-hubs`, and §4.2
/// step 1 proposes to collect that by piggybacking the serving node's
/// load on the responses it already sends.
///
/// Those are not the same thing, and the difference is not a detail. A
/// response can only carry news about the peer that *answered*, so the
/// mechanism is fresh on a subset of the candidate set and stale
/// everywhere else. `response-backpressure` is that subset made
/// explicit. Read the three arms as a bracket:
///
///   - `as-implemented → response-backpressure` — what the mechanism
///     is worth.
///   - `response-backpressure → fresh-signals` — what it cannot reach,
///     and therefore what a shorter gossip interval would still have
///     to buy.
///
/// The **recovery** column is the ratio of the two, and it is the
/// number §4.2 step 1 should be judged on. A recovery near 1.0 means
/// the response channel is sufficient and the gossip interval can be
/// left alone. Near 0.0 means the win lives entirely in peers a
/// decider never talks to, and the proposal is aimed at the wrong
/// place.
///
/// The coverage line is not decoration. A null result here has two
/// incompatible explanations — the mechanism fired and did not help,
/// or it never fired — and the latency column cannot tell them apart.
/// That is F7's lesson, and the wiring assertions below are the part
/// of this test that is allowed to fail the build.
#[test]
fn what_does_piggybacked_backpressure_recover_of_fresh_signals() {
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    let fleets: [(&str, fn(u64) -> Scenario); 4] = [
        ("household-evening-12", scenario::household_evening_12),
        ("twin-hubs", scenario::twin_hubs),
        ("mixed-hubs", scenario::mixed_hubs),
        // The density control. A response can only be fresher than
        // gossip inside the window between it landing and the next
        // gossip round, so this mechanism's coverage is a function of
        // how often a decider talks to the *same* peer — a property of
        // the traffic, not of the code. `isolation` carries a
        // background actor dispatching every ~8s against a household's
        // ~4 min, which is the widest density contrast the scenario set
        // offers. If coverage does not move across this span, low
        // coverage is not a traffic artifact.
        ("isolation", scenario::isolation),
    ];
    let arms = [
        Arm::AsImplemented,
        Arm::ResponseBackpressure,
        Arm::FreshSignals,
    ];

    for (label, build) in fleets {
        println!("\n=== {label} — §4.2 step 1: fresh where a response can reach ===");
        let mut cells: Vec<(f64, f64)> = Vec::new();
        for arm in arms {
            let (mut means, mut p95s, mut ages) = (Vec::new(), Vec::new(), Vec::new());
            let (mut with_signal, mut from_response, mut offs) = (0u64, 0u64, 0usize);
            for seed in seeds {
                let s = build(seed);
                let report = run(&s, arm, seed);
                with_signal += report.backpressure.dispatches_with_signal;
                from_response += report.backpressure.dispatches_from_response;
                offs += offloads(&report);
                let sc = score(&report, GOSSIP_WINDOW_MS, None);
                means.push(sc.truth.mean_total_ms / 1000.0);
                p95s.push(sc.records.p95_total_ms / 1000.0);
                ages.push(sc.truth.median_true_signal_age_ms / 1000.0);
            }
            let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len().max(1) as f64;
            let (m, p95) = (mean(&means), mean(&p95s));
            cells.push((m, p95));
            println!(
                "  {:<24} mean {:>5.1}s  p95 {:>5.1}s  median signal age {:>5.1}s  \
                 {:>3.0}% of {} peer-dispatches decided on a response",
                arm.label(),
                m,
                p95,
                mean(&ages),
                100.0 * from_response as f64 / with_signal.max(1) as f64,
                with_signal,
            );

            // The wiring checks. Both directions, because "the arm did
            // nothing" and "the arm is not connected" print the same
            // latency row and mean opposite things.
            match arm {
                Arm::AsImplemented | Arm::FreshSignals => assert_eq!(
                    from_response,
                    0,
                    "{label}/{}: a non-backpressure arm consumed a response-carried \
                     signal — the arm predicate leaks",
                    arm.label()
                ),
                Arm::ResponseBackpressure => {
                    assert!(
                        offs > 0,
                        "{label}: no offloads, so this fleet cannot test §4.2 step 1"
                    );
                    assert!(
                        from_response > 0,
                        "{label}: the backpressure arm never consumed a response-carried \
                         signal despite {offs} offloads — the mechanism is not wired"
                    );
                }
                _ => unreachable!("arms list is closed"),
            }
        }

        let (base_m, base_p95) = cells[0];
        let (bp_m, bp_p95) = cells[1];
        let (fresh_m, fresh_p95) = cells[2];
        // Recovery: how much of the oracle-freshness win the mechanism
        // collects. `None` when fresh-signals did not win by enough to
        // divide by — a **relative** floor, not an absolute one,
        // because a saturated fleet's 86s baseline turns a 1.5s wobble
        // into "recovers 305%". That is a denominator artifact, and
        // §6 has collected enough of those.
        let recovery = |base: f64, bp: f64, fresh: f64| {
            let gap = base - fresh;
            (gap.abs() >= 0.02 * base.abs()).then(|| 100.0 * (base - bp) / gap)
        };
        let show = |r: Option<f64>| match r {
            Some(v) => format!("{v:.0}%"),
            None => "n/a (fresh signals bought <2% here — no gap to recover)".to_string(),
        };
        println!(
            "  → mean: {:+.1}% vs arm 0 (fresh-signals {:+.1}%) — recovers {}",
            100.0 * (bp_m - base_m) / base_m.max(0.001),
            100.0 * (fresh_m - base_m) / base_m.max(0.001),
            show(recovery(base_m, bp_m, fresh_m)),
        );
        println!(
            "  → p95:  {:+.1}% vs arm 0 (fresh-signals {:+.1}%) — recovers {}",
            100.0 * (bp_p95 - base_p95) / base_p95.max(0.001),
            100.0 * (fresh_p95 - base_p95) / base_p95.max(0.001),
            show(recovery(base_p95, bp_p95, fresh_p95)),
        );
    }
}

/// **Is §4.2 step 1 a prerequisite for §4.1, or an independent
/// improvement?**
///
/// §4.2 asserts the first: the tier floor's relaxation rule and the
/// within-noise band both want an *observed* rate rather than an
/// advertised one, and §4.1.3 closed on exactly that sentence. But
/// there is a sharper reason to expect it, and it is measurable here.
///
/// The two objectives consume `in_flight` differently. The product
/// passes it through `load_penalty`, a **bounded** multiplier — a
/// stale count moves the score a little. Predicted time **multiplies
/// it by a service time**, so a stale count is a first-order error
/// that scales with the queue. The same asymmetry
/// `predicted-time+outbound-only` exists to price for load
/// *attribution* should appear here for load *staleness*.
///
/// So: run the mechanism under both objectives and compare the two
/// deltas. If freshness is worth materially more to predicted time
/// than to the product, §4.2's ordering is measured rather than
/// argued, and the two changes should land together. If the deltas
/// match, they are independent and can be sequenced by cost.
///
/// Reported, not asserted, apart from the tier floor's own invariant —
/// a freshness change must not become a quality change by relaxing the
/// floor through the back door.
#[test]
fn is_fresh_backpressure_worth_more_to_predicted_time_than_to_the_product() {
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    let fleets: [(&str, fn(u64) -> Scenario); 2] = [
        ("twin-hubs", scenario::twin_hubs),
        ("mixed-hubs", scenario::mixed_hubs),
    ];
    // (objective label, without the mechanism, with it)
    let pairs = [
        (
            "product (arm 0)",
            Arm::AsImplemented,
            Arm::ResponseBackpressure,
        ),
        (
            "predicted-time+floor",
            Arm::PredictedTimeTierFloor,
            Arm::PredictedTimeTierFloorBackpressure,
        ),
    ];

    for (label, build) in fleets {
        println!("\n=== {label} — what is a fresh load count worth, per objective? ===");
        for (objective, without, with) in pairs {
            let cell = |arm: Arm| {
                let (mut means, mut p95s) = (Vec::new(), Vec::new());
                for seed in seeds {
                    let s = build(seed);
                    let report = run(&s, arm, seed);
                    let sc = score(&report, GOSSIP_WINDOW_MS, None);
                    // Only the floor arms owe the floor's invariant.
                    // Arm 0 declines upgrades by design — it has no
                    // floor to escape, and asserting on it would be
                    // asserting that the baseline is the treatment.
                    if matches!(
                        arm,
                        Arm::PredictedTimeTierFloor | Arm::PredictedTimeTierFloorBackpressure
                    ) {
                        assert_eq!(
                            (sc.tier.downgrades, sc.tier.declined_upgrades),
                            (0, 0),
                            "{label}/{seed}/{}: a freshness arm escaped the tier floor",
                            arm.label()
                        );
                    }
                    means.push(sc.truth.mean_total_ms / 1000.0);
                    p95s.push(sc.records.p95_total_ms / 1000.0);
                }
                let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len().max(1) as f64;
                (mean(&means), mean(&p95s))
            };
            let (m0, p0) = cell(without);
            let (m1, p1) = cell(with);
            println!(
                "  {:<22} mean {:>5.1}s → {:>5.1}s ({:+.1}%)   p95 {:>5.1}s → {:>5.1}s ({:+.1}%)",
                objective,
                m0,
                m1,
                100.0 * (m1 - m0) / m0.max(0.001),
                p0,
                p1,
                100.0 * (p1 - p0) / p0.max(0.001),
            );
        }
    }
}

/// **F9, priced — and the two halves point in opposite directions.**
///
/// The finding is that the scorer reads a local-load counter nothing
/// writes: `record_dispatch(None)` has zero callers, so
/// `load_penalty` is permanently 1.0 for the local candidate. The
/// same is true of peers on the ranked path — the sole
/// `record_dispatch(Some(..))` call site is the non-streaming *named*
/// arm — so peer `samples` never leaves 0 either.
///
/// The awkward part is that arm 0 does **not** model any of this: it
/// hands `rank` an exact local queue depth and lets peer samples
/// accumulate. Arm 0 is therefore the as-*designed* system, and every
/// number recorded against it is a comparison against a mesh that
/// does not exist. [`Arm::BlindObservations`] is the as-*shipped*
/// one, and the arms in between exist to say which half of the
/// blindness carries the difference.
///
/// The reason to split rather than report one total: the two halves
/// bias in opposite directions. A blind local slot never looks busy,
/// so the origin keeps work it should send away. A permanently cold
/// peer never looks trustworthy, so the origin sends away less than
/// it otherwise would. Reporting only the sum would net two real
/// effects into one small number.
///
/// The wiring checks are asserted because a null result depends on
/// them, and one directional claim is asserted because it is a
/// property of the arithmetic rather than of these fleets:
/// `load_penalty` is monotonically decreasing in `in_flight`, so
/// zeroing the local count can only raise the local score, and can
/// therefore only reduce offload. A run where blinding the local
/// count *increased* offload would mean the arm is wired backwards.
#[test]
fn what_the_scorer_loses_by_never_seeing_its_own_load() {
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    let arms = [
        Arm::AsImplemented,
        Arm::BlindLocalLoad,
        Arm::BlindPeerRamp,
        Arm::BlindObservations,
    ];

    // ---- wiring, on one scenario, before any table is believed ----
    let s = scenario::isolation(SEED);
    let local_loads = |r: &RunReport| -> Vec<f32> {
        candidates(r)
            .iter()
            .filter(|c| c.kind == sovereign_mesh::decision_log::CandidateKind::Local)
            .map(|c| c.score.load_penalty)
            .collect()
    };
    let peer_samples = |r: &RunReport| -> Vec<u32> {
        candidates(r)
            .iter()
            .filter(|c| c.kind == sovereign_mesh::decision_log::CandidateKind::Peer)
            .map(|c| c.inputs.samples)
            .collect()
    };
    let arm0 = run(&s, Arm::AsImplemented, SEED);
    let blind_local = run(&s, Arm::BlindLocalLoad, SEED);
    let blind_ramp = run(&s, Arm::BlindPeerRamp, SEED);

    assert!(
        local_loads(&arm0).iter().any(|p| *p < 0.999),
        "arm 0 never penalised the local slot for its own load — \
         nothing for the blind arm to take away, so the table below is unreadable"
    );
    assert!(
        local_loads(&blind_local)
            .iter()
            .all(|p| (*p - 1.0).abs() < 1e-6),
        "blind-local-load left a local load penalty in place — the arm is not wired"
    );
    assert!(
        peer_samples(&arm0).iter().any(|n| *n > 0),
        "arm 0 never accumulated a peer sample — nothing for blind-peer-ramp to freeze"
    );
    assert!(
        peer_samples(&blind_ramp).iter().all(|n| *n == 0),
        "blind-peer-ramp let a peer accumulate samples — the arm is not wired"
    );

    // ---- the table ----
    println!("\n=== F9 — what each half of the observation blindness costs ===");
    println!(
        "  {:<20} {:>9} {:>9} {:>9}   (mean over {} seeds)",
        "arm",
        "mean",
        "p95",
        "offloads",
        seeds.len()
    );
    for sc in [
        scenario::household_evening_12(SEED),
        scenario::pair(SEED),
        scenario::twin_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
        scenario::isolation(SEED),
    ] {
        println!("── {} ──", sc.name);
        let mut baseline = (0.0f64, 0.0f64);
        for (i, arm) in arms.iter().enumerate() {
            let (mut means, mut p95s, mut offs) = (0.0f64, 0.0f64, 0.0f64);
            for seed in seeds {
                let r = run(&sc, *arm, seed);
                let scored = score(&r, GOSSIP_WINDOW_MS, None);
                means += mean_total_ms(&r) / 1000.0;
                p95s += scored.records.p95_total_ms / 1000.0;
                offs += offloads(&r) as f64;
            }
            let n = seeds.len() as f64;
            let (m, p, o) = (means / n, p95s / n, offs / n);
            if i == 0 {
                baseline = (m, p);
                println!("  {:<20} {m:>8.1}s {p:>8.1}s {o:>9.1}", arm.label());
            } else {
                println!(
                    "  {:<20} {m:>8.1}s {p:>8.1}s {o:>9.1}   mean {:+.0}%  p95 {:+.0}%",
                    arm.label(),
                    100.0 * (m - baseline.0) / baseline.0.max(0.001),
                    100.0 * (p - baseline.1) / baseline.1.max(0.001),
                );
            }
        }
    }

    // ---- the one directional claim that is arithmetic, not fleet ----
    for sc in [scenario::isolation(SEED), scenario::twin_hubs(SEED)] {
        for seed in seeds {
            let wired = offloads(&run(&sc, Arm::BlindPeerRamp, seed));
            let blind = offloads(&run(&sc, Arm::BlindObservations, seed));
            assert!(
                blind <= wired,
                "{} seed {seed}: blinding the local load INCREASED offload ({wired} → {blind}). \
                 `load_penalty` is monotonically decreasing in `in_flight`, so zeroing the local \
                 count can only raise the local score — the arm must be wired backwards",
                sc.name
            );
        }
    }
    let _ = (blind_local, blind_ramp);
}

/// F10's second half: no node on this mesh has ever advertised a
/// `BenchmarkResult`, so `throughput_factor` has neither of its two
/// sources and returns neutral 1.0 for every candidate on every fleet.
///
/// The test asserts the *structural* claim — that the term is a
/// constant in production, not merely a weak signal — and prints the
/// latency table for the arm that models tonight's mesh. The structural
/// claim is the durable one: it is arithmetic from `scoring.rs:362`'s
/// `(None, None)` branch, and it holds on any fleet, whereas the table
/// is fleet-specific by construction.
#[test]
fn what_the_scorer_loses_by_never_measuring_anyone() {
    let seeds = [SEED, SEED + 1, SEED + 2, SEED + 3, SEED + 4];
    let arms = [
        Arm::AsImplemented,
        Arm::BlindPeerRamp,
        Arm::BlindRateCard,
        Arm::BlindShipped,
    ];

    // ---- wiring, before any table is believed ----
    let s = scenario::mixed_hubs(SEED);
    let sources = |r: &RunReport| -> Vec<String> {
        candidates(r)
            .iter()
            .map(|c| c.score.throughput_source.clone())
            .collect()
    };
    let factors = |r: &RunReport| -> Vec<f32> {
        candidates(r)
            .iter()
            .map(|c| c.score.throughput_factor)
            .collect()
    };
    let peer_factors = |r: &RunReport| -> Vec<f32> {
        candidates(r)
            .iter()
            .filter(|c| c.kind == sovereign_mesh::decision_log::CandidateKind::Peer)
            .map(|c| c.score.throughput_factor)
            .collect()
    };
    let local_factors = |r: &RunReport| -> Vec<f32> {
        candidates(r)
            .iter()
            .filter(|c| c.kind == sovereign_mesh::decision_log::CandidateKind::Local)
            .map(|c| c.score.throughput_factor)
            .collect()
    };

    let arm0 = run(&s, Arm::AsImplemented, SEED);
    assert!(
        sources(&arm0).iter().any(|s| s == "benchmark_estimate"),
        "arm 0 never consulted a rate card on `mixed-hubs` — a fleet built out of \
         speed variance. Nothing for the blind arm to take away, so the table is unreadable"
    );
    assert!(
        factors(&arm0).iter().any(|f| *f < 0.999),
        "arm 0 never discriminated on throughput — F3's term is already inert in the \
         baseline, which would make this whole comparison vacuous"
    );

    // The finding, stated as an assertion rather than as prose — and it
    // is an *asymmetry*, not a uniform blindness. The first draft of
    // this test asserted every candidate scored neutral and failed on
    // the local one, which is the more interesting answer:
    //
    //   * Every **peer** is neutral 1.0. Both of `throughput_factor`'s
    //     sources are shut for peers — no rate card (F10) and no
    //     samples (F9's peer half) — so it takes the `(None, None)`
    //     branch every time.
    //   * The **local** node is scored on its observed decode rate.
    //     Production seeds local `samples` above the cold-start
    //     threshold at construction (`peer_inference.rs:559`) and keeps
    //     `tg_tok_s_ewma` current via `ThroughputTarget::Local`, so the
    //     observed gate opens for the local candidate and only for it.
    //
    // `throughput_factor` clamps to `[FLOOR, 1.0]`, so the local
    // candidate can only be scored *down* from the constant every peer
    // enjoys. F10's blindness therefore biases **toward offload** —
    // the opposite direction to F9's local half, which is why the two
    // must not be reported as one number.
    for sc in [
        scenario::mixed_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
        scenario::twin_hubs(SEED),
    ] {
        let shipped = run(&sc, Arm::BlindShipped, SEED);
        assert!(
            peer_factors(&shipped)
                .iter()
                .all(|f| (*f - 1.0).abs() < 1e-6),
            "{}: a PEER was scored with a non-neutral throughput factor on the \
             as-shipped arm. Production gossips no rate card and never reaches the \
             observed-EWMA gate for a peer, so this term cannot be anything but 1.0 \
             — if it is, the arm is not wired",
            sc.name
        );
        assert!(
            !peer_factors(&shipped).is_empty(),
            "{}: no peer was ever scored, so the assertion above is vacuous",
            sc.name
        );
    }

    // The asymmetry itself, on the fleet built to expose it: `isolation`
    // sustains local contention, so the local EWMA is live and the
    // clamp has something to bite on.
    let shipped_iso = run(&scenario::isolation(SEED), Arm::BlindShipped, SEED);
    let locals = local_factors(&shipped_iso);
    let peers = peer_factors(&shipped_iso);
    assert!(
        peers.iter().all(|f| (*f - 1.0).abs() < 1e-6),
        "isolation: a peer escaped the neutral constant on the as-shipped arm"
    );
    println!(
        "\n  as-shipped throughput_factor — local min {:.3} / peers all {:.3}  \
         ({} local, {} peer scorings)",
        locals.iter().copied().fold(f32::INFINITY, f32::min),
        peers.first().copied().unwrap_or(f32::NAN),
        locals.len(),
        peers.len(),
    );

    // ---- the scope limit, pinned rather than argued ----
    // This is the caveat that decides whether the tables below license
    // a production landing, so it is an assertion and not a paragraph.
    //
    // `throughput_factor` does not read a rate card directly: it scales
    // `bench.tg_tok_s` by `baseline_size_gb / candidate_size_gb`
    // (`scoring.rs:384`) to extrapolate from the model that was
    // benchmarked to the model being scored. In this sim every node
    // advertises exactly one model and benchmarks *that* model
    // (`NodeSpec::benchmark`), so the ratio is 1.0 at every scoring and
    // the extrapolation never runs.
    //
    // Production would not have that property. `run_baseline_benchmark`
    // (deleted 2026-07-28 — see below) probed the **`Speed::Fast`
    // slot** — a ~4B model — while the
    // candidate being scored is whatever the peer advertises, often a
    // 35B. The ratio would be ~0.1 and the estimate an order of
    // magnitude below the measured rate. So a shipped probe activates a
    // linear-extrapolation heuristic that nothing in this suite
    // exercises, and the −32% below is NOT a prediction about it.
    for sc in [
        scenario::mixed_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
        scenario::household_evening_12(SEED),
    ] {
        for n in &sc.nodes {
            if let Some(b) = n.benchmark() {
                assert!(
                    (b.baseline_size_gb - n.size_gb).abs() < 1e-6,
                    "{}/{}: this sim's rate card is measured on the node's own \
                     serving model, so `throughput_factor`'s size-ratio \
                     extrapolation is inert here. If that ever stops being true, \
                     the scope note above this assertion needs rewriting before \
                     the F10 tables are quoted at anyone.",
                    sc.name,
                    n.name
                );
            }
        }
    }

    // ---- the table ----
    println!("\n=== F10 — what the missing rate card costs ===");
    println!(
        "  {:<20} {:>9} {:>9} {:>9}   (mean over {} seeds)",
        "arm",
        "mean",
        "p95",
        "offloads",
        seeds.len()
    );
    for sc in [
        scenario::household_evening_12(SEED),
        scenario::pair(SEED),
        scenario::twin_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
        scenario::mixed_hubs(SEED),
        scenario::isolation(SEED),
    ] {
        println!("── {} ──", sc.name);
        let mut baseline = (0.0f64, 0.0f64);
        for (i, arm) in arms.iter().enumerate() {
            let (mut means, mut p95s, mut offs) = (0.0f64, 0.0f64, 0.0f64);
            for seed in seeds {
                let r = run(&sc, *arm, seed);
                let scored = score(&r, GOSSIP_WINDOW_MS, None);
                means += mean_total_ms(&r) / 1000.0;
                p95s += scored.records.p95_total_ms / 1000.0;
                offs += offloads(&r) as f64;
            }
            let n = seeds.len() as f64;
            let (m, p, o) = (means / n, p95s / n, offs / n);
            if i == 0 {
                baseline = (m, p);
                println!("  {:<20} {m:>8.1}s {p:>8.1}s {o:>9.1}", arm.label());
            } else {
                println!(
                    "  {:<20} {m:>8.1}s {p:>8.1}s {o:>9.1}   mean {:+.0}%  p95 {:+.0}%",
                    arm.label(),
                    100.0 * (m - baseline.0) / baseline.0.max(0.001),
                    100.0 * (p - baseline.1) / baseline.1.max(0.001),
                );
            }
        }
    }

    // ---- the landing question, isolated ----
    // If the rate card were wired tomorrow, the mesh moves from
    // `blind-shipped` to `blind-peer-ramp` — the peer ramp stays frozen
    // either way, because §4.4 measured it protective. That pair, and
    // not anything against arm 0, is the delta an operator would feel.
    println!("\n=== F10 — the landing case: wiring the probe, peer ramp left alone ===");
    println!("  {:<26} {:>9} {:>9}", "fleet", "shipped", "+rate-card");
    for sc in [
        scenario::household_evening_12(SEED),
        scenario::pair(SEED),
        scenario::twin_hubs(SEED),
        scenario::heterogeneous_fleet(SEED),
        scenario::mixed_hubs(SEED),
        scenario::isolation(SEED),
    ] {
        let (mut before, mut after) = (0.0f64, 0.0f64);
        for seed in seeds {
            before += mean_total_ms(&run(&sc, Arm::BlindShipped, seed)) / 1000.0;
            after += mean_total_ms(&run(&sc, Arm::BlindPeerRamp, seed)) / 1000.0;
        }
        let n = seeds.len() as f64;
        let (b, a) = (before / n, after / n);
        println!(
            "  {:<26} {b:>8.1}s {a:>8.1}s   {:+.0}%",
            sc.name,
            100.0 * (a - b) / b.max(0.001)
        );
    }

    // ---- and the flattery, priced ----
    // The sim builds each node's advertised rate card from the same
    // `Hardware` its service-time model consumes, so at
    // `advertised_rate_error: 0.0` a wired probe is *exact truth by
    // construction* — a real 10-second llama.cpp probe is not. The rows
    // above must not be read without this sweep (`SimConfig`'s own doc
    // comment, and note 963a8d88's method rule).
    //
    // `blind-shipped` is the control: it consults no rate card, so its
    // column must be flat across the sweep. If it moves, the harness is
    // perturbing something other than the thing under test.
    println!("\n=== F10 — does the win survive a mis-measured probe? (mixed-hubs) ===");
    println!(
        "  {:<12} {:>9} {:>11} {:>8}",
        "rate error", "shipped", "+rate-card", "Δ"
    );
    for err in [0.0_f32, 0.25, 0.5, 1.0] {
        let (mut before, mut after) = (0.0f64, 0.0f64);
        for seed in seeds {
            // Scenario fixed at `SEED`, varying only the run seed —
            // the same convention as the two tables above and as F9's
            // table in §4.4, so the ±0% row is directly comparable to
            // the `mixed-hubs` row of the landing case. (§4.1.2's
            // sweeps rebuild the fleet per seed instead; the two
            // conventions give different absolute numbers and must not
            // be read across.)
            let sc = scenario::mixed_hubs(SEED);
            let cfg = SimConfig {
                advertised_rate_error: err,
                ..SimConfig::default()
            };
            before += mean_total_ms(&run_with(&sc, Arm::BlindShipped, seed, cfg.clone())) / 1000.0;
            after += mean_total_ms(&run_with(&sc, Arm::BlindPeerRamp, seed, cfg)) / 1000.0;
        }
        let n = seeds.len() as f64;
        let (b, a) = (before / n, after / n);
        println!(
            "  ±{:<11.0}% {b:>8.1}s {a:>10.1}s {:>7.0}%",
            err * 100.0,
            100.0 * (a - b) / b.max(0.001)
        );
    }

    // ---- and the mechanism production would actually ship ----
    // Everything above prices a rate card measured on the model each
    // node serves. `run_baseline_benchmark` measured the `Speed::Fast`
    // slot instead, so a shipped card would describe a ~2.5 GB model
    // and `throughput_factor` would extrapolate from it to whatever is
    // being scored, assuming rate scales as 1/size. This arm is why
    // that probe was deleted on 2026-07-28 rather than wired up: the
    // number it produced was aimed at a consumer that would misuse it.
    // `svrn mesh bench` measures the model actually being served and
    // reports to a human, not to this scorer.
    //
    // β = 1.0 is that assumption, and it must reproduce the rows above
    // exactly — asserted below rather than eyeballed, because the whole
    // reading of the sweep depends on the knob being an identity there.
    // Below 1.0 the probe over-states the hardware per GB and every
    // large candidate is extrapolated low; the clamp is one-sided, so
    // the error can only push candidates down.
    const FAST_SLOT_GB: f32 = 2.5;
    println!("\n=== F10 — the card a SHIPPED probe would advertise (mixed-hubs) ===");
    println!("   (2.5 GB Fast-slot probe extrapolated to each candidate; β=1 is the linear");
    println!("    assumption `throughput_factor` already makes, so it must be an identity)");
    println!("   Latency alone cannot read this table: §4.1.1 established that sending");
    println!("   knowledge turns to small fast models looks like a large latency win and");
    println!("   is a quality regression. `downgrades` is the column that tells them apart.");
    println!(
        "  {:<14} {:>9} {:>11} {:>8} {:>11} {:>9}",
        "β (size→rate)", "shipped", "+rate-card", "Δ", "downgrades", "declined"
    );

    let control = {
        let (mut before, mut after) = (0.0f64, 0.0f64);
        for seed in seeds {
            let sc = scenario::mixed_hubs(SEED);
            before += mean_total_ms(&run(&sc, Arm::BlindShipped, seed)) / 1000.0;
            after += mean_total_ms(&run(&sc, Arm::BlindPeerRamp, seed)) / 1000.0;
        }
        let n = seeds.len() as f64;
        (before / n, after / n)
    };

    for beta in [1.0_f32, 0.9, 0.7, 0.5] {
        let (mut before, mut after) = (0.0f64, 0.0f64);
        let (mut down, mut declined) = (0usize, 0usize);
        for seed in seeds {
            let sc = scenario::mixed_hubs(SEED);
            let cfg = SimConfig {
                probe_baseline_size_gb: Some(FAST_SLOT_GB),
                probe_sublinearity: beta,
                ..SimConfig::default()
            };
            before += mean_total_ms(&run_with(&sc, Arm::BlindShipped, seed, cfg.clone())) / 1000.0;
            let wired = run_with(&sc, Arm::BlindPeerRamp, seed, cfg);
            after += mean_total_ms(&wired) / 1000.0;
            let scored = score(&wired, GOSSIP_WINDOW_MS, None);
            down += scored.tier.downgrades;
            declined += scored.tier.declined_upgrades;
        }
        let n = seeds.len() as f64;
        let (b, a) = (before / n, after / n);
        println!(
            "  {beta:<14.1} {b:>8.1}s {a:>10.1}s {:>7.0}% {:>11.1} {:>9.1}",
            100.0 * (a - b) / b.max(0.001),
            down as f64 / n,
            declined as f64 / n,
        );
        if (beta - 1.0).abs() < 1e-6 {
            assert!(
                (a - control.1).abs() < 0.05 && (b - control.0).abs() < 0.05,
                "β=1 must reproduce the un-probed rate card exactly \
                 ({:.2}s/{:.2}s vs control {:.2}s/{:.2}s). `throughput_factor` scales \
                 linearly on the size ratio, so measuring a smaller model and scaling \
                 back up is an identity under a linear law — if it is not, this knob \
                 is perturbing something besides the extrapolation and no row in this \
                 table can be attributed to it",
                b,
                a,
                control.0,
                control.1
            );
        }
    }
}
