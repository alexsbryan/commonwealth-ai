//! Judge calibration — proves the FastInferenceJudge agrees with
//! hand-labeled ground truth before any harness result depends on
//! its verdicts.
//!
//! ## Why this exists
//!
//! The judge is just another model. A judge that hasn't been
//! validated against human ground truth is hallucinating verdicts
//! we have no reason to trust. The calibration bank is ~40
//! hand-labeled `(subject, assertion, ground_truth)` tuples that
//! probe every category of semantic predicate the harness uses,
//! plus adversarial near-misses that defeat naïve string matching.
//!
//! ## The contract
//!
//! `calibrate(...)` returns Ok only when **every category** clears
//! the ≥95% agreement threshold. A 96% aggregate that's 100% on
//! easy cases and 70% on adversarial fails — the per-category gate
//! is the load-bearing constraint.
//!
//! On success, `calibrate` returns a `CalibrationProof` zero-sized
//! type. Pass it to `CalibrationReceipt::from_passing_proof()` to
//! mint a trusted receipt. Production scorers should hold onto a
//! receipt and reuse it across the run — re-calibrating every
//! invocation is a slot-contention foot-gun.
//!
//! ## What this is NOT
//!
//! - Not a quality benchmark for the underlying judge model. It
//!   checks agreement with our hand-labeled bank; if the bank is
//!   wrong, this won't catch it.
//! - Not a substitute for fixture-level inspection. Use this as
//!   the gate, then watch transcripts to spot judge drift between
//!   calibration runs.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::gym_judge::{CalibrationProof, Judge};

/// Per-category agreement threshold. Below this the calibration
/// fails. The ≥95% bar is chosen because below ~90% the judge is
/// effectively coin-flipping on the hard cases — useless as a
/// gate. ≥95% leaves room for two adversarial misses out of 8 per
/// category before the gate trips.
pub const AGREEMENT_THRESHOLD: f32 = 0.95;

/// One hand-labeled calibration case. The `ground_truth_rationale`
/// is the human author's explanation — it lives in the bank so
/// reviewers can audit why the label is what it is.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub category: String,
    pub assertion: String,
    pub subject: String,
    /// "PASS" or "FAIL" — what a strict human evaluator would say.
    pub ground_truth: String,
    pub ground_truth_rationale: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseBank {
    #[serde(default, rename = "case")]
    cases: Vec<Case>,
}

impl CaseBank {
    pub fn load(path: &Path) -> Result<Self, String> {
        let body =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let bank: CaseBank =
            toml::from_str(&body).map_err(|e| format!("parse {}: {e}", path.display()))?;
        // Validate every case's ground_truth is PASS or FAIL.
        // Typos here would silently break the gate — fail loud.
        for c in &bank.cases {
            match c.ground_truth.as_str() {
                "PASS" | "FAIL" => {}
                other => {
                    return Err(format!(
                        "case {}: ground_truth must be PASS or FAIL, got {other:?}",
                        c.id
                    ))
                }
            }
        }
        // Detect duplicate ids — a copy-paste error in the bank
        // would silently double-count cases.
        let mut seen = std::collections::HashSet::new();
        for c in &bank.cases {
            if !seen.insert(&c.id) {
                return Err(format!("duplicate case id: {}", c.id));
            }
        }
        Ok(bank)
    }

    pub fn cases(&self) -> &[Case] {
        &self.cases
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseOutcome {
    pub id: String,
    pub category: String,
    pub agreed: bool,
    pub ground_truth: String,
    pub judge_verdict: String,
    pub judge_rationale: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryReport {
    pub category: String,
    pub total: usize,
    pub agreements: usize,
    pub agreement_rate: f32,
    pub disagreements: Vec<CaseOutcome>,
    pub passes_threshold: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CalibrationResult {
    pub model: String,
    pub threshold: f32,
    pub categories: Vec<CategoryReport>,
    pub overall_pass: bool,
    pub total_cases: usize,
    pub total_agreements: usize,
}

impl CalibrationResult {
    pub fn aggregate_rate(&self) -> f32 {
        if self.total_cases == 0 {
            0.0
        } else {
            self.total_agreements as f32 / self.total_cases as f32
        }
    }
}

/// Run every case through the judge, build a `CalibrationResult`,
/// and on a passing result mint a `CalibrationProof`. The judge
/// argument is `&dyn Judge` so the caller can use any concrete
/// implementation — typically `FastInferenceJudge::new(...,
/// CalibrationReceipt::untrusted())` since the receipt this run
/// produces is the upgrade path.
pub async fn calibrate(
    judge: &dyn Judge,
    bank: &CaseBank,
    model_label: &str,
) -> Result<(CalibrationResult, Option<CalibrationProof>), String> {
    let mut by_category: BTreeMap<String, Vec<CaseOutcome>> = BTreeMap::new();

    for case in bank.cases() {
        let verdict = judge.judge(&case.assertion, &case.subject).await?;
        let judge_label = if verdict.passes { "PASS" } else { "FAIL" };
        let agreed = judge_label == case.ground_truth;
        by_category
            .entry(case.category.clone())
            .or_default()
            .push(CaseOutcome {
                id: case.id.clone(),
                category: case.category.clone(),
                agreed,
                ground_truth: case.ground_truth.clone(),
                judge_verdict: judge_label.to_string(),
                judge_rationale: verdict.rationale,
            });
    }

    let mut categories: Vec<CategoryReport> = Vec::with_capacity(by_category.len());
    let mut total = 0usize;
    let mut total_agreements = 0usize;
    let mut all_categories_pass = true;

    for (category, outcomes) in by_category {
        let cat_total = outcomes.len();
        let agreements = outcomes.iter().filter(|o| o.agreed).count();
        let rate = if cat_total == 0 {
            0.0
        } else {
            agreements as f32 / cat_total as f32
        };
        let passes = rate >= AGREEMENT_THRESHOLD;
        if !passes {
            all_categories_pass = false;
        }
        let disagreements: Vec<CaseOutcome> = outcomes.into_iter().filter(|o| !o.agreed).collect();

        total += cat_total;
        total_agreements += agreements;
        categories.push(CategoryReport {
            category,
            total: cat_total,
            agreements,
            agreement_rate: rate,
            disagreements,
            passes_threshold: passes,
        });
    }

    // Require *every* category be represented. Missing categories
    // could mask a regression — if "decline" cases were silently
    // dropped, an aggregate ≥95% would still mint a receipt.
    let required = REQUIRED_CATEGORIES;
    for r in required {
        if !categories.iter().any(|c| c.category == *r) {
            return Err(format!(
                "calibration bank missing required category {r:?} — add cases or update REQUIRED_CATEGORIES"
            ));
        }
    }

    let result = CalibrationResult {
        model: model_label.to_string(),
        threshold: AGREEMENT_THRESHOLD,
        categories,
        overall_pass: all_categories_pass,
        total_cases: total,
        total_agreements,
    };

    let proof = if all_categories_pass {
        Some(CalibrationProof::new_from_passing_run())
    } else {
        None
    };
    Ok((result, proof))
}

/// Every category the production scorer relies on. Adding a new
/// semantic predicate type implies adding cases here. This list
/// is the structural defence against "calibration silently
/// stopped covering category X".
pub const REQUIRED_CATEGORIES: &[&str] = &[
    "decline",
    "zero_results",
    "citation",
    "reformulation",
    "context_aware",
];

/// Render the human-readable report. The format is intentionally
/// scannable — operator sees per-category rates and the worst
/// disagreements without leaving the terminal.
pub fn render_report(r: &CalibrationResult) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "== judge calibration ==\n\
         model:     {}\n\
         threshold: {:.0}% per category\n\n",
        r.model,
        r.threshold * 100.0
    ));
    out.push_str(&format!(
        "{:<16} {:>6} {:>7}   status\n",
        "category", "n", "rate"
    ));
    out.push_str(&format!("{:-<16} {:->6} {:->7}   {:-<7}\n", "", "", "", ""));
    for c in &r.categories {
        out.push_str(&format!(
            "{:<16} {:>6} {:>6.0}%   {}\n",
            c.category,
            c.total,
            c.agreement_rate * 100.0,
            if c.passes_threshold { "PASS" } else { "FAIL" }
        ));
    }
    out.push_str(&format!(
        "\naggregate: {}/{} ({:.0}%)\n",
        r.total_agreements,
        r.total_cases,
        r.aggregate_rate() * 100.0
    ));
    out.push_str(&format!(
        "OVERALL:   {}\n",
        if r.overall_pass {
            "PASS — receipt minted"
        } else {
            "FAIL — receipt NOT minted"
        }
    ));

    // Print the disagreements per category, capped so the report
    // stays usable for a 5-category bank but isn't a flood.
    let mut any_disagreement = false;
    for c in &r.categories {
        if c.disagreements.is_empty() {
            continue;
        }
        if !any_disagreement {
            out.push_str("\ndisagreements (judge vs ground truth):\n");
            any_disagreement = true;
        }
        out.push_str(&format!("\n  [{}]\n", c.category));
        for d in c.disagreements.iter().take(5) {
            out.push_str(&format!(
                "    {} — truth={} judge={}\n      rationale: {}\n",
                d.id,
                d.ground_truth,
                d.judge_verdict,
                d.judge_rationale.chars().take(160).collect::<String>()
            ));
        }
        if c.disagreements.len() > 5 {
            out.push_str(&format!(
                "    … and {} more (re-run with --json for the full list)\n",
                c.disagreements.len() - 5
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gym_judge::{ScriptedJudge, Verdict};
    use async_trait::async_trait;

    fn five_category_bank(everything: &str) -> CaseBank {
        // Build an in-memory bank with one PASS-truth case per
        // required category. The judge response is controlled per
        // test below; here we just construct the inputs.
        let body = format!(
            r#"
[[case]]
id = "decline_01"
category = "decline"
assertion = "asserts about decline"
subject = "{s}"
ground_truth = "PASS"
ground_truth_rationale = "fixture"

[[case]]
id = "zero_results_01"
category = "zero_results"
assertion = "asserts about zero results"
subject = "{s}"
ground_truth = "PASS"
ground_truth_rationale = "fixture"

[[case]]
id = "citation_01"
category = "citation"
assertion = "asserts about citations"
subject = "{s}"
ground_truth = "PASS"
ground_truth_rationale = "fixture"

[[case]]
id = "reformulation_01"
category = "reformulation"
assertion = "asserts about reformulation"
subject = "{s}"
ground_truth = "PASS"
ground_truth_rationale = "fixture"

[[case]]
id = "context_aware_01"
category = "context_aware"
assertion = "asserts about context"
subject = "{s}"
ground_truth = "PASS"
ground_truth_rationale = "fixture"
"#,
            s = everything
        );
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), body).unwrap();
        CaseBank::load(tmp.path()).unwrap()
    }

    #[tokio::test]
    async fn calibrate_passes_when_judge_always_agrees() {
        let bank = five_category_bank("subject text");
        let judge = ScriptedJudge {
            // Every assertion contains "asserts" — script returns PASS.
            script: vec![("asserts".into(), true, "agreed".into())],
        };
        let (result, proof) = calibrate(&judge, &bank, "test-judge").await.unwrap();
        assert!(result.overall_pass);
        assert_eq!(result.total_agreements, 5);
        assert!(proof.is_some(), "proof minted on passing run");
        for c in &result.categories {
            assert!(c.passes_threshold, "category {} should pass", c.category);
        }
    }

    #[tokio::test]
    async fn calibrate_fails_when_one_category_below_threshold() {
        // Manually build a bank with 8 cases in one category, 7 of
        // which the judge gets wrong. That category drops to
        // 12.5% — well below 95%.
        let mut cases = String::new();
        for i in 0..8 {
            cases.push_str(&format!(
                r#"
[[case]]
id = "decline_{i:02}"
category = "decline"
assertion = "DECLINE_KEY_{i}"
subject = "n/a"
ground_truth = "PASS"
ground_truth_rationale = "fixture"
"#
            ));
        }
        // Plus one case for every other required category, all easy.
        for other in ["zero_results", "citation", "reformulation", "context_aware"] {
            cases.push_str(&format!(
                r#"
[[case]]
id = "{other}_01"
category = "{other}"
assertion = "EASY_{other}"
subject = "n/a"
ground_truth = "PASS"
ground_truth_rationale = "fixture"
"#
            ));
        }
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &cases).unwrap();
        let bank = CaseBank::load(tmp.path()).unwrap();

        // Script: the first decline case returns PASS; the next 7
        // return FAIL. Easy cases pass.
        struct OneInEightJudge;
        #[async_trait]
        impl Judge for OneInEightJudge {
            async fn judge(&self, assertion: &str, _subject: &str) -> Result<Verdict, String> {
                if assertion == "DECLINE_KEY_0" {
                    Ok(Verdict {
                        passes: true,
                        rationale: "matched".into(),
                    })
                } else if assertion.starts_with("DECLINE_KEY_") {
                    Ok(Verdict {
                        passes: false,
                        rationale: "mismatched".into(),
                    })
                } else {
                    Ok(Verdict {
                        passes: true,
                        rationale: "easy".into(),
                    })
                }
            }
        }

        let (result, proof) = calibrate(&OneInEightJudge, &bank, "test").await.unwrap();
        assert!(!result.overall_pass);
        assert!(proof.is_none(), "no proof when a category fails");
        let decline = result
            .categories
            .iter()
            .find(|c| c.category == "decline")
            .unwrap();
        assert!(!decline.passes_threshold);
        assert_eq!(decline.disagreements.len(), 7);
    }

    #[tokio::test]
    async fn calibrate_errors_when_required_category_missing() {
        // Bank with only 4 of the 5 required categories.
        let body = r#"
[[case]]
id = "decline_01"
category = "decline"
assertion = "x"
subject = "y"
ground_truth = "PASS"
ground_truth_rationale = "fixture"

[[case]]
id = "zero_results_01"
category = "zero_results"
assertion = "x"
subject = "y"
ground_truth = "PASS"
ground_truth_rationale = "fixture"

[[case]]
id = "citation_01"
category = "citation"
assertion = "x"
subject = "y"
ground_truth = "PASS"
ground_truth_rationale = "fixture"

[[case]]
id = "reformulation_01"
category = "reformulation"
assertion = "x"
subject = "y"
ground_truth = "PASS"
ground_truth_rationale = "fixture"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), body).unwrap();
        let bank = CaseBank::load(tmp.path()).unwrap();
        let judge = ScriptedJudge {
            script: vec![("x".into(), true, "ok".into())],
        };
        let err = calibrate(&judge, &bank, "test").await.unwrap_err();
        assert!(err.contains("context_aware"), "err={err}");
    }

    #[test]
    fn case_bank_rejects_bad_ground_truth() {
        let body = r#"
[[case]]
id = "x"
category = "decline"
assertion = "x"
subject = "y"
ground_truth = "Maybe"
ground_truth_rationale = "z"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), body).unwrap();
        let err = CaseBank::load(tmp.path()).unwrap_err();
        assert!(err.contains("PASS or FAIL"), "err={err}");
    }

    #[test]
    fn case_bank_rejects_duplicate_ids() {
        let body = r#"
[[case]]
id = "x"
category = "decline"
assertion = "a"
subject = "b"
ground_truth = "PASS"
ground_truth_rationale = "z"

[[case]]
id = "x"
category = "decline"
assertion = "c"
subject = "d"
ground_truth = "PASS"
ground_truth_rationale = "z"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), body).unwrap();
        let err = CaseBank::load(tmp.path()).unwrap_err();
        assert!(err.contains("duplicate"), "err={err}");
    }

    #[test]
    fn case_bank_rejects_unknown_fields() {
        let body = r#"
[[case]]
id = "x"
category = "decline"
assertion = "a"
subject = "b"
ground_truth = "PASS"
ground_truth_rationale = "z"
extra = "oops"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), body).unwrap();
        let err = CaseBank::load(tmp.path()).unwrap_err();
        assert!(err.contains("extra"), "err={err}");
    }
}
