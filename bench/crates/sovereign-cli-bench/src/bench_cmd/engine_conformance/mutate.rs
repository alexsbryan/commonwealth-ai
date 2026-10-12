// SPDX-License-Identifier: AGPL-3.0-or-later
//! Instrument validation by mutation (the battery plan's "Mutations. One per
//! check, each watched failing"): for each check, take a cell that passed on
//! the run's own records, perturb on the target's record exactly the facet
//! the check reads, and judge it again. A check that still passes cannot fail
//! on the input it exists to catch, and is reported by name.
//!
//! Each mutation is the smallest one the check should see: a token id
//! flipped, a temperature moved by 0.01, an alternative added to a grammar,
//! the usage dropped, a logprob moved by one ulp past its tolerance, one
//! prefill token counted twice past the slack.

use serde::Serialize;
use serde_json::{json, Value};

use super::check::{CellVerdict, Check};
use super::inventory::Inventory;
use super::record::{CaseRecord, Outcome, ToolCall};

/// What one check's mutation showed.
#[derive(Debug, Clone, Serialize)]
pub struct Mutation {
    /// The row the check belongs to.
    pub row: String,
    /// The check, as written in the inventory.
    pub check: String,
    /// The case whose passing cell was mutated, when there was one.
    pub case: Option<String>,
    /// The target the mutated cell was judged against: the row's reference,
    /// or the subject's own A/A partner.
    pub reference: Option<String>,
    /// `caught`, `missed`, or `no-subject` when no cell passed to mutate.
    pub verdict: &'static str,
    /// The mutation, and what the check said of it.
    pub detail: String,
}

/// Mutate one passing cell per check of every row, over every target pair
/// the records hold.
pub fn run(inventory: &Inventory, records: &[CaseRecord]) -> Vec<Mutation> {
    let mut out = Vec::new();
    for row in &inventory.rows {
        for check in &row.judge {
            if matches!(check, Check::Scenario { .. }) {
                continue;
            }
            // A passing pair whose prompts or generations differ is a pass the
            // check cannot turn into a failure: the difference a mutation adds
            // is excused upstream. Such a pair is counted, not used.
            let mut unisolated = 0usize;
            let subject = records
                .iter()
                .filter(|t| t.rows.iter().any(|id| *id == row.id) && t.outcome == Outcome::Ok)
                .find_map(|t| {
                    if check.target_only() {
                        let r = reference_for(row, t, records);
                        return (check.judge(r, t) == CellVerdict::Passed).then_some((r, t));
                    }
                    candidates(row, t, records)
                        .find(|r| {
                            let passed = check.judge(Some(r), t) == CellVerdict::Passed;
                            let isolated = passed && check.isolated(r, t);
                            unisolated += usize::from(passed && !isolated);
                            isolated
                        })
                        .map(|r| (Some(r), t))
                });
            let Some((r, t)) = subject else {
                let detail = if unisolated > 0 {
                    format!(
                        "{unisolated} cell(s) passed, none on a pair whose prompts and \
                         generations agree, so none could fail"
                    )
                } else {
                    "no cell passed on these records to mutate".into()
                };
                out.push(Mutation {
                    row: row.id.clone(),
                    check: format!("{check:?}"),
                    case: None,
                    reference: None,
                    verdict: "no-subject",
                    detail,
                });
                continue;
            };
            let (mutated, what) = mutate(check, r, t);
            let seen = check.judge(r, &mutated);
            let (verdict, said) = match &seen {
                CellVerdict::Failed { detail, .. } => ("caught", detail.clone()),
                CellVerdict::Passed => ("missed", "still passed".to_string()),
                CellVerdict::CouldNotJudge(why) => ("missed", format!("could not judge: {why}")),
            };
            let reference = r.map(|r| r.target.clone());
            tracing::debug!(target: "engine_conformance", row = %row.id, ?check, case = %t.case_id, ?reference, verdict, %what, "mutation");
            out.push(Mutation {
                row: row.id.clone(),
                check: format!("{check:?}"),
                case: Some(t.case_id.clone()),
                reference,
                verdict,
                detail: format!("{what}: {said}"),
            });
        }
    }
    out
}

/// The record a row compares `t` against: the row's own pair, else the
/// embedded run of the same case. For the reference target itself, its A/A
/// partner: two runs of one engine that agree are a passing pair, so a check
/// whose cross-engine cell fails can still be watched catching a mutation.
fn reference_for<'a>(
    row: &super::inventory::Row,
    t: &CaseRecord,
    records: &'a [CaseRecord],
) -> Option<&'a CaseRecord> {
    let reference = row
        .pairs
        .iter()
        .find(|(_, target)| *target == t.target)
        .map(|(r, _)| r.as_str())
        .unwrap_or(super::verdict::REFERENCE);
    records
        .iter()
        .find(|r| r.case_id == t.case_id && r.target == reference && !std::ptr::eq(*r, t))
}

/// The records `t` can be judged against for a mutation, in order: the row's
/// reference, then another run of `t`'s own target. The second has the same
/// prompts wherever the engine renders deterministically, so it stays a pair
/// a projection check judges alone when the cross-engine prompts differ.
fn candidates<'a>(
    row: &super::inventory::Row,
    t: &'a CaseRecord,
    records: &'a [CaseRecord],
) -> impl Iterator<Item = &'a CaseRecord> {
    let partner = records
        .iter()
        .find(|r| r.case_id == t.case_id && r.target == t.target && !std::ptr::eq(*r, t));
    reference_for(row, t, records).into_iter().chain(partner)
}

/// A copy of `t` with the facet `check` reads perturbed, and what was done.
/// A check with a tolerance or a ratio compares `t` with the reference `r`,
/// so its mutation is placed just past the band around `r`'s value; moved
/// from `t`'s own value it could land back inside the band.
pub fn mutate(check: &Check, r: Option<&CaseRecord>, t: &CaseRecord) -> (CaseRecord, String) {
    let mut m = t.clone();
    let f = &mut m.facets;
    let what = match check {
        Check::PromptIds => {
            flip_last(f.prompt_ids.get_or_insert_with(Vec::new));
            "the last prompt token id flipped".to_string()
        }
        Check::Sampler { .. } => {
            let s = f.sampler.get_or_insert_with(Default::default);
            *s.params.entry("temperature".into()).or_insert(0.0) += 0.01;
            "temperature moved by 0.01".to_string()
        }
        Check::Grammar => {
            let g = f.grammar.get_or_insert_with(|| "start: \"a\"".into());
            g.push_str(" | \"mutation\"");
            "an alternative added to the grammar".to_string()
        }
        Check::GreedyTokens => {
            flip_last(f.greedy_tokens.get_or_insert_with(Vec::new));
            "the last greedy token flipped".to_string()
        }
        Check::Logprobs { tol } => {
            let steps = f.logprobs.get_or_insert_with(Vec::new);
            if steps.is_empty() {
                steps.push(vec![(0, 0.0)]);
            }
            if steps[0].is_empty() {
                steps[0].push((0, 0.0));
            }
            let base = r
                .and_then(|r| r.facets.logprobs.as_ref())
                .and_then(|s| s.first()?.first().map(|e| e.1))
                .unwrap_or(steps[0][0].1);
            steps[0][0].1 = past(base, *tol);
            format!("one logprob moved one ulp past tol {tol}")
        }
        Check::LabelProbs { tol } => {
            let probs = f.label_probs.get_or_insert_with(Default::default);
            match probs.iter_mut().next() {
                Some((label, p)) => {
                    let base = r
                        .and_then(|r| r.facets.label_probs.as_ref()?.get(label).copied())
                        .unwrap_or(*p);
                    *p = past(base, *tol);
                }
                None => {
                    probs.insert("mutation".into(), 1.0);
                }
            }
            format!("one label probability moved one ulp past tol {tol}")
        }
        Check::Embedding { .. } => {
            for v in f.embeddings.get_or_insert_with(Vec::new).iter_mut().take(1) {
                v.iter_mut().for_each(|x| *x = -*x);
            }
            "the first embedding negated".to_string()
        }
        Check::RerankOrder => {
            f.rerank_scores.get_or_insert_with(Vec::new).reverse();
            "the rerank scores reversed".to_string()
        }
        Check::TokenCount => {
            *f.token_count.get_or_insert(0) += 1;
            "count_tokens off by one".to_string()
        }
        Check::Text => {
            f.output.get_or_insert_with(Default::default).text.push('.');
            "one character appended to the text".to_string()
        }
        Check::ToolCalls => {
            f.output
                .get_or_insert_with(Default::default)
                .tool_calls
                .push(ToolCall {
                    name: "mutation".into(),
                    arguments: json!({}),
                });
            "a tool call added".to_string()
        }
        Check::Finish => {
            f.output.get_or_insert_with(Default::default).finish = Some("mutation".into());
            "the finish reason changed".to_string()
        }
        Check::Usage => {
            f.output.get_or_insert_with(Default::default).usage = None;
            "the usage dropped".to_string()
        }
        Check::StreamFrames { frames } => {
            let out = f.output.get_or_insert_with(Default::default);
            out.frames.retain(|k| !frames.contains(k));
            format!("the {frames:?} frames dropped")
        }
        Check::AllowlistHeld => {
            f.output
                .get_or_insert_with(Default::default)
                .text
                .push_str(" https://mutation.invalid/x [ev-T999999-mutation]");
            "a URL and an evidence id from outside the lists cited".to_string()
        }
        Check::ReasoningWithin { slack } => {
            let budget = t.request["think_budget"].as_u64().unwrap_or(0);
            f.reasoning_tokens = Some(budget + slack + 1);
            "one reasoning token past the budget and slack".to_string()
        }
        Check::Prefill { slack } => {
            let base = r.and_then(|r| r.facets.prefill_evaluated);
            let n = f.prefill_evaluated.get_or_insert(0);
            *n = base.unwrap_or(*n) + slack + 1;
            format!(
                "{} prefill tokens counted twice, one past the slack",
                slack + 1
            )
        }
        Check::Throughput { .. } => {
            f.tokens_per_s = Some(0.0);
            "throughput zeroed".to_string()
        }
        Check::ResidentBytes { ratio } => {
            let base = r.and_then(|r| r.facets.resident_bytes).unwrap_or(1).max(1);
            f.resident_bytes = Some((base as f64 * ratio) as u64 + 1);
            format!("resident bytes one past {ratio} of the reference's")
        }
        Check::HostTruth { keys } | Check::HostEqual { keys } => {
            if let Some(k) = keys.first() {
                f.host_reported
                    .insert(k.clone(), Value::String("mutation".into()));
            }
            format!("the reported {:?} changed", keys.first())
        }
        Check::Scenario { .. } => "nothing: a scenario reads no facet".to_string(),
    };
    (m, what)
}

fn flip_last(ids: &mut Vec<i64>) {
    match ids.last_mut() {
        Some(id) => *id ^= 1,
        None => ids.push(1),
    }
}

/// The nearest value more than `tol` above `x`: one ulp past the tolerance.
fn past(x: f64, tol: f64) -> f64 {
    let mut y = x + tol;
    while (y - x).abs() <= tol {
        y = f64::from_bits(if y >= 0.0 {
            y.to_bits() + 1
        } else {
            y.to_bits() - 1
        });
    }
    y
}

#[cfg(test)]
mod tests {
    use super::past;

    #[test]
    fn past_is_the_first_value_beyond_the_tolerance() {
        assert!(past(-1.25, 0.0) != -1.25);
        assert_eq!(past(-1.25, 0.0), f64::from_bits((-1.25f64).to_bits() - 1));
        let y = past(0.5, 0.01);
        assert!((y - 0.5).abs() > 0.01);
        assert!((y - 0.5) - 0.01 < 1e-15, "no further than it needs: {y}");
    }
}
