// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decision replay, self-tested against a simulated capture —
//! `SCHEDULER_QUALITY.md` Phase 1, step **S1**.
//!
//! S1's gate is decision-agreement between a replayed capture and the
//! live scheduler. This file establishes the *instrument* before any
//! hardware time is spent producing a capture to point it at, by
//! running it against a fixture whose answer is known:
//!
//! ```text
//! mesh_sim  →  DecisionEvent stream
//!           →  TracingDecisionSink::to_path   (the production writer)
//!           →  JSONL on disk
//!           →  SchedulerTrace::from_jsonl_path (the production loader)
//!           →  decision_replay::replay_trace
//! ```
//!
//! Every stage is the code a real capture goes through — the sim
//! contributes only the *content* of the records. Agreement must
//! therefore come out at exactly 1.000, and anything less is a bug in
//! the record, the writer, the loader or the replay rather than a
//! calibration finding. That is what makes this fixture worth having:
//! it has a right answer, so a wrong one is unambiguous.
//!
//! Note what is *not* asserted here. The gate uses only facts a
//! production capture carries — recorded inputs, recorded scores,
//! recorded verdicts. Nothing reads `RunReport::truth`, because the
//! same gate has to run unchanged against two daemons on real
//! hardware, where no such field exists.
//!
//! ```text
//! cargo test -p sovereign-mesh --features mesh-sim,treesitter \
//!     --test scheduler_replay_agreement -- --nocapture
//! ```
#![cfg(feature = "mesh-sim")]

use std::path::PathBuf;

use sovereign_mesh::decision_log::{DecisionEvent, DecisionSink, TracingDecisionSink};
use sovereign_mesh::decision_replay::{replay_decisions, replay_trace, ReplayReport, SkipReason};
use sovereign_mesh::decision_trace::SchedulerTrace;
use sovereign_mesh::mesh_sim::scenario::{self, Scenario};
use sovereign_mesh::mesh_sim::{run, Arm, RunReport};

const SEED: u64 = 20_260_726;

/// A temp directory that removes itself, so a failing run leaves no
/// litter in `/tmp` and a passing one leaves none either.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "sovereign-replay-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Write a run's records through the **production** JSONL sink and
/// read them back through the **production** loader.
///
/// Going out to disk rather than replaying `report.records` in memory
/// is the whole point of the round trip: it exercises serde on every
/// record shape and proves `f32` survives the format. An in-memory
/// replay would silently skip the one stage a real capture cannot.
fn capture_and_load(report: &RunReport, tag: &str) -> (SchedulerTrace, Scratch) {
    let scratch = Scratch::new(tag);
    let path = scratch.path("trace.jsonl");
    {
        let sink = TracingDecisionSink::to_path(&path).expect("open sink");
        for event in &report.records {
            sink.record(event.clone());
        }
    }
    let trace = SchedulerTrace::from_jsonl_path(&path).expect("load trace");
    (trace, scratch)
}

fn decisions_in(report: &RunReport) -> usize {
    report
        .records
        .iter()
        .filter(|e| matches!(e, DecisionEvent::Decision(_)))
        .count()
}

/// The S1 gate. Both halves at 1.000, over a non-trivial number of
/// decisions.
fn assert_agreement(report: &ReplayReport, context: &str) {
    assert!(
        report.replayed > 0,
        "{context}: nothing was replayed, so agreement is vacuous\n{report}"
    );
    assert!(
        report.candidates_checked > 0,
        "{context}: no candidate was scored\n{report}"
    );
    assert!(
        report.gaps.is_empty(),
        "{context}: the record could not be interpreted\n{report}"
    );
    assert_eq!(
        report.scorer_agreement(),
        1.0,
        "{context}: scorer disagreement\n{report}"
    );
    assert_eq!(
        report.policy_agreement(),
        1.0,
        "{context}: policy disagreement\n{report}"
    );
}

#[test]
fn a_captured_run_reproduces_its_own_scores_and_verdicts() {
    let s = scenario::household_evening_12(SEED);
    let run_report = run(&s, Arm::AsImplemented, SEED);
    let (trace, _scratch) = capture_and_load(&run_report, "household");
    let report = replay_trace(&trace);
    println!("household_evening_12 / as-implemented\n{report}");
    assert_agreement(&report, "household_evening_12");
    assert_eq!(
        report.decisions_seen,
        decisions_in(&run_report),
        "the loader dropped decisions on the way through JSONL"
    );
}

/// Every scenario and every arm that decides *under the objective
/// replay assumes*. Two arms are excluded on purpose, each with its own
/// test below:
///
///   - `Oracle` bypasses the scorer, so it has no scores to reproduce.
///   - `PredictedTime` records a verdict the product policy did not
///     produce, and a record cannot say which objective produced it.
#[test]
fn agreement_holds_across_every_scenario_and_arm() {
    let scenarios: Vec<Scenario> = vec![
        scenario::household_evening_12(SEED),
        scenario::pair(SEED),
        scenario::heterogeneous_fleet(SEED),
        scenario::twin_hubs(SEED),
        scenario::isolation(SEED),
    ];
    let arms = [
        Arm::AsImplemented,
        Arm::FreshSignals,
        Arm::TwoChoices,
        Arm::FreshTwoChoices,
        Arm::WarmStart,
        Arm::OutboundOnlyLoad,
    ];
    for s in &scenarios {
        for arm in arms {
            let run_report = run(s, arm, SEED);
            let (trace, _scratch) = capture_and_load(&run_report, "sweep");
            let report = replay_trace(&trace);
            println!(
                "{:<22} {:<18} scorer {:.3}  policy {:.3}  ({} replayed, {} candidates, {} exact)",
                s.name,
                arm.label(),
                report.scorer_agreement(),
                report.policy_agreement(),
                report.replayed,
                report.candidates_checked,
                report.candidates_exact,
            );
            assert_agreement(&report, &format!("{} / {}", s.name, arm.label()));
        }
    }
}

/// The affinity probe is the one place replay does arithmetic
/// production did not. If it costs precision, this is where it shows
/// — and on a fleet with no failures it costs none, so the whole
/// capture reproduces bit-for-bit rather than merely within tolerance.
#[test]
fn a_simulated_capture_reproduces_bit_for_bit_not_merely_within_tolerance() {
    let s = scenario::heterogeneous_fleet(SEED);
    let report = replay_trace(&capture_and_load(&run(&s, Arm::AsImplemented, SEED), "exact").0);
    assert_eq!(
        report.candidates_exact, report.candidates_checked,
        "{} of {} candidates reproduced only approximately; max Δ {:.3e}\n{report}",
        report.candidates_exact, report.candidates_checked, report.max_final_score_delta
    );
    assert_eq!(report.max_final_score_delta, 0.0, "{report}");
}

/// The `Oracle` arm is the sim's own denominator, not a policy: it
/// picks by perfect knowledge of every queue and never calls the
/// scorer. Its decisions carry no candidates, so replay must classify
/// them as out-of-domain rather than as agreement or disagreement.
/// Asserting this keeps a future scoreboard from quietly counting
/// oracle runs as perfect agreement.
#[test]
fn the_oracle_arm_is_out_of_domain_not_silently_perfect() {
    let s = scenario::household_evening_12(SEED);
    let run_report = run(&s, Arm::Oracle, SEED);
    let (trace, _scratch) = capture_and_load(&run_report, "oracle");
    let report = replay_trace(&trace);
    println!("oracle arm\n{report}");
    assert_eq!(report.replayed, 0);
    assert_eq!(report.scorer_agreement(), 0.0);
    assert_eq!(report.policy_agreement(), 0.0);
    assert!(report
        .skipped
        .iter()
        .any(|(r, _)| matches!(r, SkipReason::NoCandidates)));
}

/// **The record cannot say which objective decided.** `PredictedTime`
/// is left out of the sweep above, and the reason is a real gap rather
/// than a harness quirk: a `RoutingDecision` carries inputs, scores and
/// a verdict, but not the *objective* that mapped the scores to the
/// verdict. `decision_replay` therefore re-runs the product policy over
/// whatever capture it is handed, so a capture taken under
/// `SCHEDULER_QUALITY.md` §4.1's objective reads as policy
/// disagreement.
///
/// Asserting that disagreement instead of quietly omitting the arm does
/// two jobs. It pins the field the §4.1 landing has to add — an
/// objective tag on the record, which is a different and smaller change
/// than the `predicted_ms` column one might reach for first, since
/// `CandidateInputs` already carries every input the prediction needs.
/// And it is the arm's wiring check from replay's side: had
/// `PredictedTime` silently fallen through to the product objective,
/// policy agreement would come back at exactly 1.000.
///
/// The *scorer* half must still be perfect. The recorded scores are the
/// product's under either objective — §4.1 replaces the ranking and
/// touches nothing that is scored — so anything below 1.000 there would
/// be a genuine bug.
#[test]
fn a_predicted_time_capture_disagrees_with_the_product_policy_and_says_why() {
    let s = scenario::household_evening_12(SEED);
    let run_report = run(&s, Arm::PredictedTime, SEED);
    let (trace, _scratch) = capture_and_load(&run_report, "predicted");
    let report = replay_trace(&trace);
    println!("predicted-time capture, replayed under the product policy\n{report}");

    assert!(
        report.replayed > 0 && report.gaps.is_empty(),
        "the capture itself must still be interpretable — only the policy differs\n{report}"
    );
    assert_eq!(
        report.scorer_agreement(),
        1.0,
        "§4.1 replaces the ranking, not the scoring: every recorded score must still \
         follow from its recorded inputs\n{report}"
    );
    assert!(
        report.policy_agreement() < 1.0,
        "replaying a predicted-time capture under the product policy agreed perfectly — \
         either the arm is not wired through RankInputs, or the record has since grown \
         an objective tag and this test should now assert agreement instead\n{report}"
    );
}

/// Private and Fast requests are gated before scoring, so a real
/// capture is a mix of replayable and skipped decisions. The gate has
/// to hold on the mix — and the skipped half has to be *visible*, or
/// a capture that gated everything would report a perfect 1.000 over
/// nothing.
#[test]
fn gated_traffic_is_counted_apart_from_the_agreement_ratio() {
    let s = scenario::household_evening_12(SEED);
    let run_report = run(&s, Arm::AsImplemented, SEED);
    let (trace, _scratch) = capture_and_load(&run_report, "gated");
    let report = replay_trace(&trace);
    let gated: usize = report
        .skipped
        .iter()
        .filter(|(r, _)| matches!(r, SkipReason::Gated(_)))
        .map(|(_, n)| n)
        .sum();
    assert!(
        gated > 0,
        "the household scenario carries Private/Fast traffic that never scores\n{report}"
    );
    assert_eq!(report.replayed + gated, report.decisions_seen);
    assert_agreement(&report, "gated mix");
}

/// Replay must be able to *fail*, or the 1.000 above proves nothing.
/// Corrupt one recorded input in a real capture and the report has to
/// name the factor that no longer follows from it.
#[test]
fn a_corrupted_capture_is_detected_and_the_broken_factor_named() {
    let s = scenario::pair(SEED);
    let run_report = run(&s, Arm::AsImplemented, SEED);
    let (trace, _scratch) = capture_and_load(&run_report, "corrupt");

    let mut decisions: Vec<_> = trace.episodes.iter().map(|e| e.decision.clone()).collect();
    let victim = decisions
        .iter_mut()
        .find(|d| d.candidates.len() > 1)
        .expect("a scored decision with a peer");
    victim.candidates[1].inputs.samples = victim.candidates[1].inputs.samples.wrapping_add(97);

    let report = replay_decisions(decisions.iter());
    println!("corrupted capture\n{report}");
    assert!(report.scorer_agreement() < 1.0, "{report}");
    assert_eq!(report.scorer_disagreements_total, 1, "{report}");
    let factors = &report.scorer_disagreements[0].check.disagreeing_factors;
    assert!(
        factors.contains(&"cold_start_weight") || factors.contains(&"throughput_factor"),
        "a moved sample count must move the cold-start ramp or the throughput source; got {factors:?}"
    );
    // The policy half reads recorded scores, so it is untouched — the
    // two measurements are independent by construction.
    assert_eq!(report.policy_agreement(), 1.0, "{report}");
}

/// Determinism, end to end: the same (scenario, arm, seed) captured
/// twice must produce byte-identical JSONL apart from the decision
/// ids, which carry a per-process sequence and a random tail.
#[test]
fn two_captures_of_one_run_replay_identically() {
    let s = scenario::twin_hubs(SEED);
    let first = replay_trace(&capture_and_load(&run(&s, Arm::TwoChoices, SEED), "det-a").0);
    let second = replay_trace(&capture_and_load(&run(&s, Arm::TwoChoices, SEED), "det-b").0);
    assert_eq!(first.decisions_seen, second.decisions_seen);
    assert_eq!(first.replayed, second.replayed);
    assert_eq!(first.candidates_checked, second.candidates_checked);
    assert_eq!(first.candidates_exact, second.candidates_exact);
    assert_eq!(first.scorer_agreement(), second.scorer_agreement());
    assert_eq!(first.policy_agreement(), second.policy_agreement());
}
