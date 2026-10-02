// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tier-1 scoreboard — `SCHEDULER_QUALITY.md` Phase 1, step S0.
//!
//! Exit criterion for S0, verbatim: *"F1/F3/F5 reproduce against the
//! **real** scorer, or are retired as artifacts of my
//! transcription."* §3's numbers came from a 12-node model that
//! transcribed the scoring arithmetic by hand. These runs drive the
//! production decision function itself
//! (`sovereign_scheduler::scheduler_core::rank`) over a simulated fleet,
//! so a finding that survives here survives against the code.
//!
//! Run it and read the table:
//!
//! ```text
//! cargo test -p sovereign-serving-host --test main mesh_sim_scoreboard -- --nocapture
//! ```
//!
//! The assertions below are the *hard invariants* of §5 — the things
//! that are true or the mesh is broken. The rest of the scoreboard is
//! reported, not asserted: a metric with a threshold nobody agreed on
//! is a flaky test, and a scoreboard that fails the build every time
//! a number moves stops being read.
//!
//! It runs with serving-host's tests. It sat behind sovereign-mesh's `dst`
//! feature only to keep the simulator out of that crate's production build;
//! here the simulator lives under `tests/`, which no build links
//! (pb-mesh-dissolve).

use crate::mesh_sim::scenario::{self, RequestClass, Scenario};
use crate::mesh_sim::scoreboard::{render, score, ArmScore};
use crate::mesh_sim::{run, run_with, Arm, RunReport, SimConfig, ALL_ARMS};
use sovereign_scheduler::predicted_time::{self, PredictInputs, RequestShape};

// The tests, split by question; the helpers below stay here. Explicit
// `#[path]`: a child of a `#[path]`-loaded module resolves against this
// file's directory.
#[path = "mesh_sim_scoreboard/findings.rs"]
mod findings;
#[path = "mesh_sim_scoreboard/signals.rs"]
mod signals;
#[path = "mesh_sim_scoreboard/tier_floor.rs"]
mod tier_floor;
#[path = "mesh_sim_scoreboard/time_objective.rs"]
mod time_objective;

pub(crate) const SEED: u64 = 20_260_726;
const GOSSIP_WINDOW_MS: u64 = 10_000;

/// Run every arm over one scenario and score them against the
/// oracle's mean latency.
pub(crate) fn sweep(scenario: &Scenario) -> (Vec<RunReport>, Vec<ArmScore>) {
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
pub(crate) fn print_candidate_breakdown(report: &RunReport, n: usize) {
    use sovereign_scheduler::decision_log::DecisionEvent;
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
pub(crate) fn assert_hard_invariants(reports: &[RunReport], scores: &[ArmScore]) {
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
    let mut facts: Vec<&crate::mesh_sim::ServedFact> = report
        .truth
        .iter()
        .filter(|f| f.class == RequestClass::Knowledge)
        .collect();
    if facts.is_empty() {
        return None;
    }
    facts.sort_by_key(|f| f.dispatched_at_ms);
    let q = facts.len() / 4;
    let mean_wait = |slice: &[&crate::mesh_sim::ServedFact]| -> f64 {
        if slice.is_empty() {
            return 0.0;
        }
        slice.iter().map(|f| f.queue_wait_ms as f64).sum::<f64>() / slice.len() as f64 / 1000.0
    };
    let mean_service = |slice: &[&crate::mesh_sim::ServedFact]| -> f64 {
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

/// Every scored candidate record in a run, flattened.
fn candidates(report: &RunReport) -> Vec<&sovereign_scheduler::decision_log::CandidateRecord> {
    use sovereign_scheduler::decision_log::DecisionEvent;
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

/// The sample count a decider starts with, which `peer_samples` is
/// measured against.
fn seed_floor(arm: Arm) -> u32 {
    if arm.warm_start() {
        20
    } else {
        0
    }
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
    use sovereign_scheduler::decision_log::{CandidateKind, DecisionEvent};

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
