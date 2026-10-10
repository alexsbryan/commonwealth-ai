// SPDX-License-Identifier: AGPL-3.0-or-later
//! What each identity source is worth on the corpus being read, estimated
//! with no labels (campaign ontology-layer E2): Fellegi-Sunter agreement
//! weights fitted by EM over the candidate pairs RESOLVE weighs.
//!
//! A pair is a statement and one alternative it was weighed over. Each source
//! that spoke on it agreed (named that alternative, or holds the same value)
//! or disagreed; a silent source is absent. The pair is a match (one
//! particular) or not, unobserved. Under the model, sources err independently
//! given that, so a source is two rates: `m`, how often it agrees on a match,
//! and `u`, how often on a non-match. Agreeing adds ln(m/u) to the pair's log
//! odds, disagreeing ln((1-m)/(1-u)). EM finds the rates that make the
//! observed agreement patterns most likely. The model's choice and the
//! proposed answer both read the text, so they are not independent in fact;
//! what that costs is read against the labelled ratios (layer-estimator).

use std::collections::BTreeMap;

use serde::Serialize;
use tracing::debug;

use super::weigh::Carried;

/// Each source that spoke on one pair: `true` agreed, `false` disagreed.
pub type Comparison = BTreeMap<String, bool>;

/// The pairs seen so far, kept as counts per agreement pattern: EM only reads
/// how often each pattern occurred, so the fit costs patterns, not pairs.
#[derive(Debug, Default, Clone)]
pub struct Pairs {
    patterns: BTreeMap<Vec<(String, bool)>, u32>,
    total: u32,
}

impl Pairs {
    /// Count one pair. A pair no source spoke on says nothing and is not counted.
    pub fn add(&mut self, c: &Comparison) {
        if c.is_empty() {
            return;
        }
        let key: Vec<(String, bool)> = c.iter().map(|(s, &a)| (s.clone(), a)).collect();
        *self.patterns.entry(key).or_default() += 1;
        self.total += 1;
    }

    pub fn len(&self) -> u32 {
        self.total
    }
}

/// One source's fitted rates and what they make of its say.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceWeight {
    /// P(agrees | one particular).
    pub m: f64,
    /// P(agrees | two particulars).
    pub u: f64,
    /// ln(m/u): what agreeing adds to a pair's log odds.
    pub agree: f64,
    /// ln((1-m)/(1-u)): what disagreeing adds.
    pub disagree: f64,
    /// P(one particular | it agrees): the precision of its links on this
    /// corpus, the number the labelled ratios are read against (C3).
    pub precision: f64,
    /// Pairs it spoke on.
    pub spoke: u32,
}

/// The weights estimated from `Pairs`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Estimate {
    pub pairs: u32,
    /// P(one particular) for a pair before any source speaks.
    pub prior: f64,
    pub sources: BTreeMap<String, SourceWeight>,
    pub iterations: u32,
}

/// Where EM starts. Only which optimum it climbs to depends on these, never
/// the fitted values: sources start more often agreeing on matches than on
/// non-matches, which names the class "match"; matches start rare.
const START_PRIOR: f64 = 0.1;
const START_M: f64 = 0.9;
const START_U: f64 = 0.1;
/// EM stops when no rate moves by more than this, or after `MAX_ITERATIONS`.
const TOLERANCE: f64 = 1e-9;
const MAX_ITERATIONS: u32 = 500;

/// Rates are read as open-interval probabilities.
const EDGE: f64 = 1e-6;

impl Estimate {
    /// Fit by EM. Each rate is a posterior mean under a uniform prior (one
    /// pseudo-agreement and one pseudo-disagreement), so a source seen on few
    /// pairs, or none, weighs close to nothing rather than without bound.
    pub fn fit(pairs: &Pairs) -> Self {
        let mut sources: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
        for pattern in pairs.patterns.keys() {
            for (s, _) in pattern {
                sources.entry(s.as_str()).or_insert((START_M, START_U));
            }
        }
        let mut prior = START_PRIOR;
        let mut iterations = 0;
        if pairs.total > 0 {
            while iterations < MAX_ITERATIONS {
                iterations += 1;
                // E: each pattern's probability of being a match.
                let mut matched_total = 0.0;
                let mut seen: BTreeMap<&str, [f64; 4]> = BTreeMap::new(); // [g·agree, g, (1-g)·agree, 1-g]
                for (pattern, &n) in &pairs.patterns {
                    let n = n as f64;
                    let (mut lm, mut lu) = (prior.ln(), (1.0 - prior).ln());
                    for (s, agree) in pattern {
                        let (m, u) = sources[s.as_str()];
                        if *agree {
                            lm += m.ln();
                            lu += u.ln();
                        } else {
                            lm += (1.0 - m).ln();
                            lu += (1.0 - u).ln();
                        }
                    }
                    let g = 1.0 / (1.0 + (lu - lm).exp());
                    matched_total += n * g;
                    for (s, agree) in pattern {
                        let e = seen.entry(s.as_str()).or_default();
                        let a = if *agree { 1.0 } else { 0.0 };
                        e[0] += n * g * a;
                        e[1] += n * g;
                        e[2] += n * (1.0 - g) * a;
                        e[3] += n * (1.0 - g);
                    }
                }
                // M: the rates the expected counts give.
                let mut moved: f64 = 0.0;
                let next_prior = clamp((matched_total + 1.0) / (pairs.total as f64 + 2.0));
                moved = moved.max((next_prior - prior).abs());
                prior = next_prior;
                for (s, rates) in sources.iter_mut() {
                    let e = seen.get(s).copied().unwrap_or_default();
                    let m = clamp((e[0] + 1.0) / (e[1] + 2.0));
                    let u = clamp((e[2] + 1.0) / (e[3] + 2.0));
                    moved = moved.max((m - rates.0).abs()).max((u - rates.1).abs());
                    *rates = (m, u);
                }
                if moved < TOLERANCE {
                    break;
                }
            }
            // EM is symmetric in its two classes: "match" is the one the
            // sources agree on more. If it converged the other way round,
            // swap the names.
            let lean: f64 = sources.values().map(|(m, u)| m - u).sum();
            if lean < 0.0 {
                prior = 1.0 - prior;
                for rates in sources.values_mut() {
                    *rates = (rates.1, rates.0);
                }
                debug!("atlas/resolve estimate: classes swapped so the match class is the one sources agree on");
            }
        } else {
            // No pair: nothing is known, so nothing weighs.
            prior = 0.5;
            for rates in sources.values_mut() {
                *rates = (0.5, 0.5);
            }
        }
        let mut spoke: BTreeMap<&str, u32> = BTreeMap::new();
        for (pattern, &n) in &pairs.patterns {
            for (s, _) in pattern {
                *spoke.entry(s.as_str()).or_default() += n;
            }
        }
        let sources = sources
            .into_iter()
            .map(|(s, (m, u))| {
                let w = SourceWeight {
                    m,
                    u,
                    agree: (m / u).ln(),
                    disagree: ((1.0 - m) / (1.0 - u)).ln(),
                    precision: prior * m / (prior * m + (1.0 - prior) * u),
                    spoke: spoke.get(s).copied().unwrap_or(0),
                };
                (s.to_string(), w)
            })
            .collect();
        let estimate = Self {
            pairs: pairs.total,
            prior,
            sources,
            iterations,
        };
        debug!(pairs = estimate.pairs, prior = estimate.prior, iterations, sources = ?estimate.sources, "atlas/resolve estimate: agreement weights fitted");
        estimate
    }

    /// The fit of no pairs: every weight 0, prior odds even.
    pub fn none() -> Self {
        Self::fit(&Pairs::default())
    }

    /// The log-likelihood ratio a pair's comparison carries: each source's
    /// agree or disagree weight. A source with no estimate yet weighs 0.
    pub fn evidence(&self, c: &Comparison) -> f64 {
        c.iter()
            .filter_map(|(s, &agree)| {
                self.sources
                    .get(s)
                    .map(|w| if agree { w.agree } else { w.disagree })
            })
            .sum()
    }

    /// Whether a source agreed in `c` at a weight above 0: what lets an
    /// alternative link (`weigh`).
    pub fn supports(&self, c: &Comparison) -> bool {
        c.iter()
            .any(|(s, &agree)| agree && self.sources.get(s).is_some_and(|w| w.agree > 0.0))
    }

    /// ln(prior / (1 - prior)).
    pub fn prior_log_odds(&self) -> f64 {
        (self.prior / (1.0 - self.prior)).ln()
    }

    /// The source's estimated precision, if it has spoken on any pair.
    pub fn precision(&self, source: &str) -> Option<f64> {
        self.sources
            .get(source)
            .filter(|w| w.spoke > 0)
            .map(|w| w.precision)
    }
}

/// The resolver's view of its estimate (D2).
impl super::Resolver {
    /// A resolver that has already weighed `pairs`: its estimate starts from
    /// them. Tests weigh at weights they chose (`estimate::expected_pairs`).
    #[cfg(test)]
    pub(crate) fn seeded(pairs: Pairs) -> Self {
        Self {
            estimate: Estimate::fit(&pairs),
            pairs,
            ..Self::default()
        }
    }

    /// The weights estimated from every pair weighed so far.
    pub fn estimate(&self) -> &Estimate {
        &self.estimate
    }

    /// Per source, the links and vetoes it carried so far.
    pub fn carried(&self) -> &BTreeMap<String, Carried> {
        &self.carried
    }

    /// One line per source: its estimated weights and what it carried. The
    /// run's summary of the estimate (D2).
    pub fn sources_summary(&self) -> Vec<String> {
        let e = &self.estimate;
        let mut lines = vec![format!(
            "estimated on {} pair(s): prior {:.3}",
            e.pairs, e.prior
        )];
        for (source, w) in &e.sources {
            let c = self.carried.get(source).copied().unwrap_or_default();
            lines.push(format!(
                "{source}: precision {:.3} (m {:.3}, u {:.3}; agree {:+.2}, disagree {:+.2}) on {} pair(s); \
                 {} link(s) agreed, {} turned, {} veto(es)",
                w.precision, w.m, w.u, w.agree, w.disagree, w.spoke, c.links, c.pivotal_links, c.vetoes
            ));
        }
        lines
    }
}

/// Pairs whose agreement patterns occur exactly as often as `prior` and
/// each source's `(name, m, u)` make them, out of 10,000: a corpus EM fits
/// back to those rates, so a test can weigh at weights it chose. Three or
/// more sources identify the model.
#[cfg(test)]
pub fn expected_pairs(prior: f64, sources: &[(&str, f64, f64)]) -> Pairs {
    let mut pairs = Pairs::default();
    for bits in 0..(1u32 << sources.len()) {
        let c: Comparison = sources
            .iter()
            .enumerate()
            .map(|(k, (s, _, _))| (s.to_string(), bits & (1 << k) != 0))
            .collect();
        let p = |matched: bool| -> f64 {
            sources
                .iter()
                .enumerate()
                .map(|(k, &(_, m, u))| {
                    let r = if matched { m } else { u };
                    if bits & (1 << k) != 0 {
                        r
                    } else {
                        1.0 - r
                    }
                })
                .product()
        };
        let n = (10_000.0 * (prior * p(true) + (1.0 - prior) * p(false))).round() as u32;
        if n > 0 {
            *pairs
                .patterns
                .entry(c.iter().map(|(s, &a)| (s.clone(), a)).collect())
                .or_default() += n;
            pairs.total += n;
        }
    }
    pairs
}

impl Default for Estimate {
    fn default() -> Self {
        Self::none()
    }
}

fn clamp(p: f64) -> f64 {
    p.clamp(EDGE, 1.0 - EDGE)
}

#[cfg(test)]
#[path = "estimate_tests.rs"]
mod tests;
