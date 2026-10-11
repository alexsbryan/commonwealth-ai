// SPDX-License-Identifier: AGPL-3.0-or-later
//! The checks a row can name, each a pure function of two records (the
//! reference target's and the target's) or of the target's record alone.

use serde::Deserialize;
use sovereign_contracts::decode_allowlist::{AllowlistLanguage, EVIDENCE_ID, URL};

use super::record::{CaseRecord, Outcome};

/// One check, as a row names it in `judge = [...]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Check {
    /// Prompt token ids are identical.
    PromptIds,
    /// Sampler parameters are identical, and their order unless
    /// `order = false`.
    Sampler {
        /// Compare the stage order as well as the values.
        #[serde(default = "yes")]
        order: bool,
    },
    /// The grammar text is identical once the `%llguidance` header is set
    /// aside.
    Grammar,
    /// Greedy tokens are identical.
    GreedyTokens,
    /// Top-k logprobs agree within `tol` at every position, same tokens.
    Logprobs {
        /// Largest absolute difference allowed.
        tol: f64,
    },
    /// Forced-choice label probabilities agree within `tol`.
    LabelProbs {
        /// Largest absolute difference allowed.
        tol: f64,
    },
    /// Embedding cosine is at least `min_cos`.
    Embedding {
        /// Smallest cosine allowed.
        min_cos: f64,
    },
    /// Rerank scores order the documents the same way.
    RerankOrder,
    /// `count_tokens` is identical.
    TokenCount,
    /// Answer text and reasoning are identical.
    Text,
    /// Parsed tool calls are identical.
    ToolCalls,
    /// Finish reason is identical.
    Finish,
    /// Usage counts are identical.
    Usage,
    /// Every listed frame kind the reference sent, the target sent too.
    StreamFrames {
        /// Frame kinds to compare.
        frames: Vec<String>,
    },
    /// Every URL and evidence id the target cited is in the request's
    /// allow-list. Target only.
    AllowlistHeld,
    /// The think block is no longer than the request's `think_budget` plus
    /// `slack`. Target only.
    ReasoningWithin {
        /// Tokens allowed past the budget.
        #[serde(default)]
        slack: u64,
    },
    /// The target evaluates no more prompt tokens than the reference plus
    /// `slack`.
    Prefill {
        /// Tokens allowed past the reference.
        slack: u64,
    },
    /// Target throughput is at least `ratio` of the reference's.
    Throughput {
        /// Smallest ratio allowed.
        ratio: f64,
    },
    /// Target resident bytes are at most `ratio` of the reference's.
    ResidentBytes {
        /// Largest ratio allowed.
        ratio: f64,
    },
    /// What the target reports about itself matches what its serving
    /// process says, for each key. Target only.
    HostTruth {
        /// Keys compared.
        keys: Vec<String>,
    },
    /// What the target reports matches what the reference reports.
    HostEqual {
        /// Keys compared.
        keys: Vec<String>,
    },
    /// Judged by a driver scenario that does not exist yet; always
    /// could-not-judge until it does.
    Scenario {
        /// What the scenario must establish.
        what: String,
    },
}

fn yes() -> bool {
    true
}

/// The verdict of one check on one case.
#[derive(Debug, Clone, PartialEq)]
pub enum CellVerdict {
    /// The check held.
    Passed,
    /// The check failed.
    Failed {
        /// Why, in the inventory's terms.
        cause: FailCause,
        /// What differed, for the report.
        detail: String,
    },
    /// The check could not be applied, and why.
    CouldNotJudge(String),
}

/// Why a check failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailCause {
    /// The target refused or errored where the reference served.
    Refused,
    /// The target served and differed.
    Differs,
}

fn differs(detail: impl Into<String>) -> CellVerdict {
    CellVerdict::Failed {
        cause: FailCause::Differs,
        detail: detail.into(),
    }
}

fn verdict(held: bool, detail: impl FnOnce() -> String) -> CellVerdict {
    if held {
        CellVerdict::Passed
    } else {
        differs(detail())
    }
}

/// A facet both sides must carry, or the check cannot be judged.
macro_rules! both {
    ($r:expr, $t:expr, $facet:ident) => {
        match ($r.facets.$facet.as_ref(), $t.facets.$facet.as_ref()) {
            (Some(r), Some(t)) => (r, t),
            (None, _) => return unobserved($r, stringify!($facet)),
            (_, None) => return unobserved($t, stringify!($facet)),
        }
    };
}

/// A facet the record does not carry, with the driver's reason when it gave
/// one.
fn unobserved(record: &CaseRecord, facet: &str) -> CellVerdict {
    let why = record
        .facets
        .unobserved
        .get(facet)
        .map(|w| format!(": {w}"))
        .unwrap_or_default();
    CellVerdict::CouldNotJudge(format!("{facet} not observed on {}{why}", record.target))
}

impl Check {
    /// Whether the check reads only the target's record.
    pub fn target_only(&self) -> bool {
        matches!(
            self,
            Check::AllowlistHeld | Check::ReasoningWithin { .. } | Check::HostTruth { .. }
        )
    }

    /// Judge one case. `reference` is `None` for a target-only check.
    pub fn judge(&self, reference: Option<&CaseRecord>, target: &CaseRecord) -> CellVerdict {
        if let Check::Scenario { what } = self {
            return CellVerdict::CouldNotJudge(format!("no driver scenario yet: {what}"));
        }
        if self.target_only() {
            return self.judge_alone(target);
        }
        let Some(reference) = reference else {
            return CellVerdict::CouldNotJudge(format!(
                "no reference record for case {}",
                target.case_id
            ));
        };
        match (&reference.outcome, &target.outcome) {
            (Outcome::Ok, Outcome::Ok) => self.judge_pair(reference, target),
            (Outcome::Ok, Outcome::Refused { message } | Outcome::Error { message }) => {
                CellVerdict::Failed {
                    cause: FailCause::Refused,
                    detail: format!(
                        "{} served, {} did not: {message}",
                        reference.target, target.target
                    ),
                }
            }
            (_, Outcome::Ok) => differs(format!(
                "{} did not serve, {} did",
                reference.target, target.target
            )),
            // Neither served: equal when both refused, or both errored.
            (r, t) => verdict(
                std::mem::discriminant(r) == std::mem::discriminant(t),
                || format!("{r:?} against {t:?}"),
            ),
        }
    }

    /// A projection check reads what the caller received, which also depends
    /// on what the model was given and generated. When either differs, the
    /// difference belongs to the prompt or decode rows, not this one.
    fn upstream_difference(&self, r: &CaseRecord, t: &CaseRecord) -> Option<String> {
        if !matches!(
            self,
            Check::Text | Check::ToolCalls | Check::Finish | Check::Usage
        ) {
            return None;
        }
        let (a, b) = (&r.facets, &t.facets);
        if matches!((&a.prompt_ids, &b.prompt_ids), (Some(x), Some(y)) if x != y) {
            return Some("the prompts differ, so the projection cannot be isolated".into());
        }
        if matches!((&a.greedy_tokens, &b.greedy_tokens), (Some(x), Some(y)) if x != y) {
            return Some(
                "the generated tokens differ, so the projection cannot be isolated".into(),
            );
        }
        None
    }

    fn judge_alone(&self, target: &CaseRecord) -> CellVerdict {
        if target.outcome != Outcome::Ok {
            return CellVerdict::CouldNotJudge(format!("{} did not serve", target.target));
        }
        let f = &target.facets;
        match self {
            Check::AllowlistHeld => {
                let Some(output) = &f.output else {
                    return unobserved(target, "output");
                };
                let outside: Vec<String> = [
                    (&URL, "url_allowlist"),
                    (&EVIDENCE_ID, "evidence_id_allowlist"),
                ]
                .into_iter()
                .filter_map(|(lang, key)| Some((lang, allowed(&target.request, key)?)))
                .flat_map(|(lang, list)| outside(lang, &output.text, &list))
                .collect();
                verdict(outside.is_empty(), || {
                    format!("cited outside the list: {outside:?}")
                })
            }
            Check::ReasoningWithin { slack } => {
                let Some(budget) = target.request.get("think_budget").and_then(|v| v.as_u64())
                else {
                    return CellVerdict::CouldNotJudge("request carries no think_budget".into());
                };
                let Some(spent) = f.reasoning_tokens else {
                    return unobserved(target, "reasoning_tokens");
                };
                verdict(spent <= budget + slack, || {
                    format!("{spent} reasoning tokens against a budget of {budget}")
                })
            }
            Check::HostTruth { keys } => {
                host_keys(keys, &f.host_reported, &f.host_observed, "observed")
            }
            _ => unreachable!("judge_alone is only called for target-only checks"),
        }
    }

    fn judge_pair(&self, r: &CaseRecord, t: &CaseRecord) -> CellVerdict {
        if let Some(upstream) = self.upstream_difference(r, t) {
            return CellVerdict::CouldNotJudge(upstream);
        }
        match self {
            Check::PromptIds => {
                let (a, b) = both!(r, t, prompt_ids);
                verdict(a == b, || first_divergence(a, b))
            }
            Check::Sampler { order } => {
                let (a, b) = both!(r, t, sampler);
                let mut diffs: Vec<String> = a
                    .params
                    .keys()
                    .chain(b.params.keys())
                    .filter(|k| match (a.params.get(*k), b.params.get(*k)) {
                        (Some(x), Some(y)) => (x - y).abs() > 1e-6,
                        _ => true,
                    })
                    .map(|k| format!("{k}: {:?} against {:?}", a.params.get(k), b.params.get(k)))
                    .collect();
                diffs.dedup();
                if *order && a.order != b.order {
                    diffs.push(format!("order {:?} against {:?}", a.order, b.order));
                }
                verdict(diffs.is_empty(), || diffs.join("; "))
            }
            Check::Grammar => {
                let header = |g: &str| g.strip_prefix("%llguidance {}\n").unwrap_or(g).to_string();
                match (r.facets.grammar.as_deref(), t.facets.grammar.as_deref()) {
                    (None, None) => CellVerdict::Passed,
                    (Some(a), Some(b)) => {
                        verdict(header(a) == header(b), || "grammar text differs".into())
                    }
                    (a, b) => differs(format!(
                        "grammar on {}: {}, on {}: {}",
                        r.target,
                        a.is_some(),
                        t.target,
                        b.is_some()
                    )),
                }
            }
            Check::GreedyTokens => {
                let (a, b) = both!(r, t, greedy_tokens);
                verdict(a == b, || first_divergence(a, b))
            }
            Check::Logprobs { tol } => {
                let (a, b) = both!(r, t, logprobs);
                if a.len() != b.len() {
                    return differs(format!("{} positions against {}", a.len(), b.len()));
                }
                for (pos, (x, y)) in a.iter().zip(b).enumerate() {
                    let tokens_x: Vec<i64> = x.iter().map(|p| p.0).collect();
                    let tokens_y: Vec<i64> = y.iter().map(|p| p.0).collect();
                    if tokens_x != tokens_y {
                        return differs(format!(
                            "position {pos}: top tokens {tokens_x:?} against {tokens_y:?}"
                        ));
                    }
                    if let Some((p, q)) = x.iter().zip(y).find(|(p, q)| (p.1 - q.1).abs() > *tol) {
                        return differs(format!(
                            "position {pos}, token {}: {} against {}",
                            p.0, p.1, q.1
                        ));
                    }
                }
                CellVerdict::Passed
            }
            Check::LabelProbs { tol } => {
                let (a, b) = both!(r, t, label_probs);
                // A label one side omits is a difference in its own right,
                // not a probability of zero.
                if a.keys().ne(b.keys()) {
                    return differs(format!(
                        "labels {:?} against {:?}",
                        a.keys().collect::<Vec<_>>(),
                        b.keys().collect::<Vec<_>>()
                    ));
                }
                let worst = a
                    .iter()
                    .map(|(k, p)| (k, (p - b[k]).abs()))
                    .max_by(|x, y| x.1.total_cmp(&y.1));
                match worst {
                    Some((k, d)) if d > *tol => differs(format!("label {k}: |Δ| = {d}")),
                    _ => CellVerdict::Passed,
                }
            }
            Check::Embedding { min_cos } => {
                let (a, b) = both!(r, t, embeddings);
                if a.len() != b.len() {
                    return differs(format!("{} vectors against {}", a.len(), b.len()));
                }
                let worst = a
                    .iter()
                    .zip(b)
                    .map(|(x, y)| cosine(x, y))
                    .enumerate()
                    .min_by(|x, y| x.1.total_cmp(&y.1));
                match worst {
                    Some((i, cos)) if cos < *min_cos => {
                        differs(format!("vector {i}: cosine {cos}"))
                    }
                    _ => CellVerdict::Passed,
                }
            }
            Check::RerankOrder => {
                let (a, b) = both!(r, t, rerank_scores);
                verdict(rank(a) == rank(b), || {
                    format!("order {:?} against {:?}", rank(a), rank(b))
                })
            }
            Check::TokenCount => {
                let (a, b) = both!(r, t, token_count);
                verdict(a == b, || format!("{a} against {b}"))
            }
            Check::Text => {
                let (a, b) = both!(r, t, output);
                verdict(a.text == b.text && a.reasoning == b.reasoning, || {
                    format!(
                        "text equal: {}, reasoning {:?} against {:?}",
                        a.text == b.text,
                        a.reasoning.as_ref().map(String::len),
                        b.reasoning.as_ref().map(String::len)
                    )
                })
            }
            Check::ToolCalls => {
                let (a, b) = both!(r, t, output);
                verdict(a.tool_calls == b.tool_calls, || {
                    format!("{:?} against {:?}", a.tool_calls, b.tool_calls)
                })
            }
            Check::Finish => {
                let (a, b) = both!(r, t, output);
                verdict(a.finish == b.finish, || {
                    format!("{:?} against {:?}", a.finish, b.finish)
                })
            }
            Check::Usage => {
                let (a, b) = both!(r, t, output);
                verdict(a.usage == b.usage, || {
                    format!("{:?} against {:?}", a.usage, b.usage)
                })
            }
            Check::StreamFrames { frames } => {
                let (a, b) = both!(r, t, output);
                let missing: Vec<&String> = frames
                    .iter()
                    .filter(|k| a.frames.contains(k) && !b.frames.contains(k))
                    .collect();
                verdict(missing.is_empty(), || {
                    format!("frames missing on {}: {missing:?}", t.target)
                })
            }
            Check::Prefill { slack } => {
                let (a, b) = both!(r, t, prefill_evaluated);
                verdict(*b <= a + slack, || format!("{b} evaluated against {a}"))
            }
            Check::Throughput { ratio } => {
                let (a, b) = both!(r, t, tokens_per_s);
                verdict(*b >= a * ratio, || format!("{b:.1} tok/s against {a:.1}"))
            }
            Check::ResidentBytes { ratio } => {
                let (a, b) = both!(r, t, resident_bytes);
                verdict((*b as f64) <= (*a as f64) * ratio, || {
                    format!("{b} bytes against {a}")
                })
            }
            Check::HostEqual { keys } => host_keys(
                keys,
                &r.facets.host_reported,
                &t.facets.host_reported,
                &r.target,
            ),
            Check::AllowlistHeld
            | Check::ReasoningWithin { .. }
            | Check::HostTruth { .. }
            | Check::Scenario { .. } => {
                unreachable!("handled before judge_pair")
            }
        }
    }
}

fn allowed(request: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    let list: Vec<String> = request
        .get(key)?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    (!list.is_empty()).then_some(list)
}

fn outside(lang: &AllowlistLanguage, text: &str, list: &[String]) -> Vec<String> {
    lang.entries_outside(text, list)
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn host_keys(
    keys: &[String],
    reported: &std::collections::BTreeMap<String, serde_json::Value>,
    truth: &std::collections::BTreeMap<String, serde_json::Value>,
    truth_name: &str,
) -> CellVerdict {
    let mut unjudged = Vec::new();
    let mut diffs = Vec::new();
    for k in keys {
        match (reported.get(k), truth.get(k)) {
            (Some(a), Some(b)) if a == b => {}
            (Some(a), Some(b)) => diffs.push(format!("{k}: reported {a}, {truth_name} {b}")),
            _ => unjudged.push(k.as_str()),
        }
    }
    if !diffs.is_empty() {
        differs(diffs.join("; "))
    } else if !unjudged.is_empty() {
        CellVerdict::CouldNotJudge(format!("keys not observed: {unjudged:?}"))
    } else {
        CellVerdict::Passed
    }
}

fn first_divergence(a: &[i64], b: &[i64]) -> String {
    let at = a
        .iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()));
    format!(
        "diverge at {at} of {} / {}: {:?} against {:?}",
        a.len(),
        b.len(),
        a.get(at),
        b.get(at)
    )
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return f64::NAN;
    }
    let dot: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum();
    let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    dot / (norm(a) * norm(b))
}

fn rank(scores: &[f32]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..scores.len()).collect();
    idx.sort_by(|&i, &j| scores[j].total_cmp(&scores[i]));
    idx
}
