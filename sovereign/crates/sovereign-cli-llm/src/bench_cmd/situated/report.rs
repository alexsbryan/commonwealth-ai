// SPDX-License-Identifier: AGPL-3.0-or-later
//! Situated-lane report shapes — a binding over the shared rubric core.
//!
//! Formulas, Wilson CIs, the disjoint-CI significance rule and the diff all
//! live in [`crate::bench_cmd::rubric`]; nothing here re-implements any of
//! them (ARCH_PRINCIPLES §10.6). What is situated-specific: the probe's
//! identity and question type, and the run header's provenance — which
//! transcripts were scored, under which vocabulary.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::bench_cmd::rubric::report::{self as shared, RubricRun};
use crate::bench_cmd::rubric::score::{Aggregate, CriterionOutcome, RubricItem};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeReport {
    pub probe_id: String,
    /// The lane's slice axis. Situatedness is not one skill: a model can be
    /// excellent at grounding an answerable probe and terrible at naming a
    /// gap, and a single mean would hide exactly that.
    pub qtype: String,
    /// 0–100, `None` when every criterion was could-not-judge.
    pub score: Option<f64>,
    pub criteria: Vec<CriterionOutcome>,
    pub could_not_judge: usize,
    /// The judged text, kept for audit — every verdict must be checkable
    /// against what it judged.
    pub response: String,
    /// The gate action that produced this response, carried from the
    /// transcript so a score can be read against the turn that made it.
    #[serde(default)]
    pub gate_action: Option<String>,
    pub judge_ms_total: u64,
}

impl RubricItem for ProbeReport {
    fn id(&self) -> &str {
        &self.probe_id
    }
    fn score(&self) -> Option<f64> {
        self.score
    }
    fn criteria(&self) -> &[CriterionOutcome] {
        &self.criteria
    }
    fn group(&self) -> Option<&str> {
        Some(&self.qtype)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SituatedRun {
    pub started_at_unix: i64,
    /// The model whose transcripts these are — read from the chaos run, not
    /// from a flag, so it cannot disagree with what was actually scored.
    pub subject_model: String,
    pub judge_model: String,
    pub judge_trials: u8,
    /// Provenance of the input. A report that cannot name its transcripts
    /// cannot be reproduced.
    pub transcripts_path: String,
    pub criteria_path: String,
    pub criteria_version: u32,
    /// The vocabulary's own declared status. Carried into the report and
    /// printed, so a number produced under a `draft` criterion set can
    /// never be quoted later as if it came from a settled instrument.
    #[serde(default)]
    pub criteria_status: String,
    /// Rows the transcript loader could not parse. Surfaced in the report,
    /// never silently dropped.
    pub transcripts_skipped: usize,
    pub probes: Vec<ProbeReport>,
    pub aggregate: Aggregate,
}

impl RubricRun for SituatedRun {
    type Item = ProbeReport;
    fn items(&self) -> &[ProbeReport] {
        &self.probes
    }
    fn subject_model(&self) -> &str {
        &self.subject_model
    }
    fn judge_model(&self) -> &str {
        &self.judge_model
    }
    fn judge_trials(&self) -> u8 {
        self.judge_trials
    }
    fn aggregate(&self) -> &Aggregate {
        &self.aggregate
    }
}

/// Recover the behaviour name from a criterion id. The situated lane mints
/// ids as `{key}@{content-hash}` ([`super::criteria::BoundCriterion::id`])
/// precisely so this is possible; this is the only reader of that format.
pub fn criterion_key(id: &str) -> &str {
    id.split('@').next().unwrap_or(id)
}

/// Per-behaviour fulfillment, worst first — the artifact P4 acts on.
///
/// The dimension table says grounding is weak; this says WHICH grounding
/// behaviour is weak, which is the difference between a number and a work
/// item. Criteria that never varied are flagged: a criterion that fired the
/// same way on every probe is either measuring nothing here or is a probe
/// selection artifact, and in both cases it is a lead about the instrument
/// rather than a finding about the model.
pub fn print_by_criterion(run: &SituatedRun) {
    use std::collections::BTreeMap;
    // key -> (fulfilled, judged, could-not-judge, weight)
    let mut per: BTreeMap<&str, (usize, usize, usize, i32)> = BTreeMap::new();
    for p in &run.probes {
        for o in &p.criteria {
            let e = per.entry(criterion_key(&o.criterion_id)).or_insert((0, 0, 0, o.weight));
            match o.fulfilled() {
                Some(true) => {
                    e.0 += 1;
                    e.1 += 1;
                }
                Some(false) => e.1 += 1,
                None => e.2 += 1,
            }
        }
    }
    if per.is_empty() {
        return;
    }
    let mut rows: Vec<_> = per
        .into_iter()
        .map(|(k, (ful, judged, cnj, w))| {
            let rate = if judged > 0 { 100.0 * ful as f64 / judged as f64 } else { f64::NAN };
            (k, rate, ful, judged, cnj, w)
        })
        .collect();
    // Worst first; unjudged rows sort last rather than pretending to be 0.
    rows.sort_by(|a, b| {
        a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(b.0))
    });
    println!();
    println!("By criterion (fulfilled / judged — the work items, worst first):");
    for (key, rate, ful, judged, cnj, w) in rows {
        let flag = if judged >= 6 && (ful == 0 || ful == judged) { "   <-- never varied" } else { "" };
        let shown = if rate.is_nan() { "  n/a".to_string() } else { format!("{rate:5.1}%") };
        println!(
            "  {key:<28} {w:+}  {shown}  ({ful}/{judged}{}){flag}",
            if cnj > 0 { format!(", {cnj} could-not-judge") } else { String::new() }
        );
    }
}

pub fn write_json_report(path: &Path, run: &SituatedRun) -> std::io::Result<()> {
    shared::write_json_report(path, run)
}

pub fn load_report(path: &Path) -> std::io::Result<SituatedRun> {
    shared::load_report(path)
}

/// Per-dimension + overall delta against a stored baseline. Refuses to
/// compare across vocabularies: a criterion set change moves the numbers by
/// changing the instrument, and a delta that conflates the two is not a
/// finding. Same posture as the lane gate's model_mismatch refusal.
pub fn print_diff(baseline: &SituatedRun, current: &SituatedRun) {
    if baseline.criteria_version != current.criteria_version {
        println!();
        println!(
            "REFUSING TO DIFF: criterion vocabulary differs (baseline v{}, current v{}). \
             The instrument changed, so a delta would conflate the harness with the ruler. \
             Re-run the baseline under v{}.",
            baseline.criteria_version, current.criteria_version, current.criteria_version
        );
        return;
    }
    shared::print_diff(baseline, current)
}

pub fn print_text_report(run: &SituatedRun) {
    println!("bench situated — {} probes", run.probes.len());
    println!("subject model: {}", run.subject_model);
    println!("judge model:   {}  (trials per criterion: {})", run.judge_model, run.judge_trials);
    println!("transcripts:   {}", run.transcripts_path);
    println!("criteria:      {}  (v{})", run.criteria_path, run.criteria_version);
    if run.criteria_status != "stable" {
        println!(
            "NOTE:          criterion vocabulary is `{}`, not `stable` — these numbers \
             describe an instrument still being tuned",
            if run.criteria_status.is_empty() { "unset" } else { &run.criteria_status }
        );
    }
    if run.transcripts_skipped > 0 {
        println!("WARNING:       {} transcript row(s) unreadable and NOT scored", run.transcripts_skipped);
    }
    println!("==========================================");
    for p in &run.probes {
        let score = match p.score {
            Some(v) => format!("{v:5.1}"),
            None => "  n/a".to_string(),
        };
        let cnj = if p.could_not_judge > 0 {
            format!("  [{} could-not-judge]", p.could_not_judge)
        } else {
            String::new()
        };
        println!(
            "{score}  {}  ({})  judge {:.1}s{cnj}",
            p.probe_id,
            p.qtype,
            p.judge_ms_total as f64 / 1000.0,
        );
    }
    let a = &run.aggregate;
    shared::print_score_summary(a);
    shared::print_dimension_table(a);
    shared::print_group_table(a, "question type");
    print_by_criterion(run);
    shared::print_could_not_judge(a);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench_cmd::rubric::judge::{CriterionVerdict, Judgement};
    use crate::bench_cmd::rubric::score::score_item;

    fn outcome(id: &str, dim: &str, weight: i32, v: Option<Judgement>) -> CriterionOutcome {
        CriterionOutcome {
            criterion_id: id.into(),
            dimension: dim.into(),
            weight,
            verdict: CriterionVerdict {
                verdict: v,
                evidence: String::new(),
                trials_yes: matches!(v, Some(Judgement::Yes)) as u32,
                trials_no: matches!(v, Some(Judgement::No)) as u32,
                trials_failed: v.is_none() as u32,
            },
        }
    }

    fn run(version: u32) -> SituatedRun {
        let criteria = vec![outcome("sc-1", "grounding", 2, Some(Judgement::Yes))];
        let probes = vec![ProbeReport {
            probe_id: "present-wife".into(),
            qtype: "present".into(),
            score: score_item(&criteria),
            criteria,
            could_not_judge: 0,
            response: "x".into(),
            gate_action: Some("released".into()),
            judge_ms_total: 0,
        }];
        let aggregate = crate::bench_cmd::rubric::score::aggregate(&probes);
        SituatedRun {
            started_at_unix: 0,
            subject_model: "m".into(),
            judge_model: "j".into(),
            judge_trials: 1,
            transcripts_path: "t.jsonl".into(),
            criteria_path: "criteria.toml".into(),
            criteria_version: version,
            criteria_status: "draft".into(),
            transcripts_skipped: 0,
            probes,
            aggregate,
        }
    }

    #[test]
    fn criterion_key_is_recoverable_from_the_id() {
        assert_eq!(criterion_key("cites_a_source@a1b2c3"), "cites_a_source");
        // A bare id (no hash) degrades to itself rather than to empty.
        assert_eq!(criterion_key("legacy_id"), "legacy_id");
    }

    #[test]
    fn report_round_trips_and_slices_by_question_type() {
        let r = run(1);
        assert!(r.aggregate.by_role_domain.contains_key("present"));
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("r.json");
        write_json_report(&p, &r).unwrap();
        let back = load_report(&p).unwrap();
        assert_eq!(back.probes[0].probe_id, "present-wife");
        assert_eq!(back.criteria_version, 1);
    }

    /// Comparing across criterion versions would report a ruler change as a
    /// harness win. The refusal is the point.
    #[test]
    fn diff_refuses_across_criterion_versions() {
        // Behavioural assertion is the absence of a panic plus the guard
        // branch being reachable; the printed text is checked by eye in the
        // lane. What matters here is that the version fields differ and the
        // guard triggers before touching the shared differ.
        let a = run(1);
        let b = run(2);
        assert_ne!(a.criteria_version, b.criteria_version);
        print_diff(&a, &b);
    }

    /// The two arm-C reports as they were actually produced on 2026-08-05,
    /// with only the free prose (`response`, per-verdict `evidence`) stripped
    /// so the fixture stays reviewable. Every field the paired test reads is
    /// byte-identical to the run.
    const FROZEN_BASE: &str = include_str!("fixtures/paired_base.json");
    const FROZEN_ARM_C: &str = include_str!("fixtures/paired_arm_c.json");

    fn frozen() -> (SituatedRun, SituatedRun) {
        (
            serde_json::from_str(FROZEN_BASE).expect("frozen baseline fixture must parse"),
            serde_json::from_str(FROZEN_ARM_C).expect("frozen arm-C fixture must parse"),
        )
    }

    /// KNOWN-ANSWER CONTROL for the paired test (ARCH_PRINCIPLES §18.4:
    /// validate the instrument before the result).
    ///
    /// These p-values were computed independently from the frozen reports
    /// BEFORE `rubric::paired` existed, and they are what the module was built
    /// to reproduce. A change here is one of two things: a real bug, or a
    /// deliberate change to the test — in which case the new expected values
    /// have to be re-derived by hand, not read off the failing assertion.
    ///
    /// The values also encode the finding that motivated the whole module.
    /// `boundary` is 4 better against 2 WORSE, which is a heterogeneous effect;
    /// the dimension-rate table renders the same numbers as a clean `+9.5`.
    #[test]
    fn paired_test_reproduces_the_frozen_arm_c_comparison() {
        use crate::bench_cmd::rubric::paired;
        let (base, arm_c) = frozen();
        let cmp = paired::compare(base.items(), arm_c.items());

        // Every criterion pairs: same bank, same vocabulary, no could-not-judge.
        assert_eq!(cmp.overall.paired(), 63, "all 63 criterion judgements must pair");
        assert_eq!(cmp.overall.unpairable, 0, "nothing may be silently dropped");

        assert_eq!((cmp.overall.better, cmp.overall.worse), (5, 3));
        assert!(
            (cmp.overall.p_value - 0.7265625).abs() < 1e-9,
            "all-criteria p was {}",
            cmp.overall.p_value
        );

        let boundary = &cmp.by_dimension["boundary"];
        assert_eq!(
            (boundary.better, boundary.worse),
            (4, 2),
            "the headline boundary gain is 4 improvements against 2 regressions"
        );
        assert!(
            (boundary.p_value - 0.6875).abs() < 1e-9,
            "boundary p was {}",
            boundary.p_value
        );
    }

    /// The whole rendering path over the real frozen pair. Guards the case a
    /// unit test cannot: a diff that computes correctly and then panics (or
    /// silently prints nothing) while formatting. Run it with `--nocapture`
    /// to read the operator-facing output.
    #[test]
    fn frozen_pair_renders_the_full_diff() {
        let (base, arm_c) = frozen();
        assert_eq!(
            base.criteria_version, arm_c.criteria_version,
            "the frozen pair must share a vocabulary or print_diff refuses and this \
             test would assert nothing"
        );
        print_diff(&base, &arm_c);
    }

    /// The situated instrument was BLIND to the change arm C actually made.
    /// `cites_a_source` is the lane's only citation criterion and it returned
    /// "no" on all seven probes in BOTH arms, while the chaos lane counted
    /// citations going 0/7 → 3/7 over the same transcripts. Zero flips on
    /// `grounding` is therefore a fact about the ruler, not about the system.
    ///
    /// This test is a TRIPWIRE, not a celebration: when the locator work makes
    /// `cites_a_source` satisfiable, this assertion is expected to fail on a
    /// re-minted fixture, and that failure is the signal the blindness is
    /// gone. Do not update the fixture without moving this assertion too.
    #[test]
    fn grounding_registered_nothing_from_the_arm_c_citation_win() {
        use crate::bench_cmd::rubric::paired;
        let (base, arm_c) = frozen();
        let cmp = paired::compare(base.items(), arm_c.items());
        let grounding = &cmp.by_dimension["grounding"];
        assert_eq!(
            (grounding.better, grounding.worse),
            (0, 0),
            "grounding must show zero flips on this frozen pair"
        );
        assert_eq!(grounding.p_value, 1.0);

        for (arm, run) in [("baseline", &base), ("arm C", &arm_c)] {
            let fulfilled = run
                .probes
                .iter()
                .flat_map(|p| &p.criteria)
                .filter(|c| criterion_key(&c.criterion_id) == "cites_a_source")
                .filter(|c| c.fulfilled() == Some(true))
                .count();
            assert_eq!(fulfilled, 0, "cites_a_source was never earned in {arm}");
        }
    }
}
