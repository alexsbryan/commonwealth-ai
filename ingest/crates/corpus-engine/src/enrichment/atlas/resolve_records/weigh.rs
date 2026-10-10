// SPDX-License-Identifier: AGPL-3.0-or-later
//! The decider (ONTOLOGY_METHOD.md §Identity, Ring 2 of
//! `research/ontology-apps/resolve-prereg.md`): every source that spoke on a
//! statement's alternatives adds its evidence, weighed at what it is
//! estimated to be worth on this corpus (`estimate.rs`), and the type's
//! declared bar reads three zones off the posterior: link, no link,
//! unsettled.
//!
//! Each alternative's evidence is the sum of the agreement weights of the
//! sources that spoke on it (Fellegi-Sunter); with the pair prior it is the
//! alternative's log odds of being the particular against none. At most one
//! alternative is the particular, so the posterior over the alternatives and
//! none is their odds normalised, none at even odds.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use tracing::debug;

use super::estimate::{Comparison, Estimate};

/// One source's say for a statement: the alternative it agreed with, and
/// that source's precision as estimated on this corpus.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Vote {
    /// The source: a document stamp's attribute, `proposed_answer`,
    /// `model_choice`, `reasoned_choice` or `necessary:<attribute>`.
    pub source: String,
    /// The record it named; inside one document, the statement that opened it.
    pub record: String,
    /// P(one particular | it agrees), estimated on this corpus.
    pub precision: f64,
}

/// Where the posterior falls against the bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Zone {
    /// The alternative at this position reached the bar, strictly ahead of every other.
    Link(usize, f64),
    /// No source raised any alternative's odds, or none itself reached the bar: novelty.
    NoLink,
    /// Some alternative rose, and neither it nor none reached the bar.
    Unsettled,
}

/// The posterior over the alternatives and none, and its zone.
#[derive(Debug, Clone)]
pub(super) struct Weighed {
    pub posterior: Vec<f64>,
    pub none: f64,
    pub zone: Zone,
}

/// Weigh each alternative's summed `evidence` (log-likelihood ratios) at
/// `prior_log_odds`, against `bar`. `raised[k]`: some source agreed with
/// alternative k at a weight above 0; only such an alternative can link,
/// since one that rose only because every other fell has nothing for it.
/// With no bar declared the most probable of the alternatives and none
/// decides (the Bayes choice): there is no unsettled zone.
pub(super) fn weigh(
    evidence: &[f64],
    raised: &[bool],
    prior_log_odds: f64,
    bar: Option<f64>,
) -> Weighed {
    let score: Vec<f64> = evidence.iter().map(|e| e + prior_log_odds).collect();
    // Log-sum-exp with none at 0.
    let top = score.iter().copied().fold(0.0_f64, f64::max);
    let total = (-top).exp() + score.iter().map(|s| (s - top).exp()).sum::<f64>();
    let posterior: Vec<f64> = score.iter().map(|s| (s - top).exp() / total).collect();
    let none = (-top).exp() / total;
    let any_raised = raised.iter().any(|&r| r);
    let best = posterior
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(&a.0)));
    let strict = |at: usize, p: f64| {
        raised[at]
            && p > none + 1e-12
            && posterior
                .iter()
                .enumerate()
                .all(|(i, &q)| i == at || q + 1e-12 < p)
    };
    let zone = match (best, bar) {
        _ if !any_raised => Zone::NoLink,
        (Some((at, &p)), Some(bar)) if p >= bar && strict(at, p) => Zone::Link(at, p),
        (_, Some(bar)) if none >= bar => Zone::NoLink,
        (_, Some(_)) => Zone::Unsettled,
        (Some((at, &p)), None) if strict(at, p) => Zone::Link(at, p),
        (_, None) => Zone::NoLink,
    };
    debug!(
        alternatives = evidence.len(),
        ?evidence,
        prior_log_odds,
        ?posterior,
        none,
        ?bar,
        ?zone,
        "atlas/resolve weigh: posterior over the alternatives"
    );
    Weighed {
        posterior,
        none,
        zone,
    }
}

/// What one source carried over a run: links it agreed with, and decisions
/// it turned: a link that would not have been made without it, or one that
/// would have been made but for it (its disagreement, a veto weighed).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Carried {
    pub links: u32,
    pub pivotal_links: u32,
    pub vetoes: u32,
}

/// Count, per source, the links it agreed with and the decisions it turned:
/// weighed again without it, a link it made would not be, or a link it
/// prevented would be (a weighed veto).
pub(super) fn tally(
    carried: &mut BTreeMap<String, Carried>,
    comps: &[Comparison],
    estimate: &Estimate,
    prior: f64,
    bar: Option<f64>,
    zone: Zone,
) {
    let sources: BTreeSet<&String> = comps.iter().flat_map(|c| c.keys()).collect();
    for source in sources {
        let without: Vec<Comparison> = comps
            .iter()
            .map(|c| {
                let mut c = c.clone();
                c.remove(source.as_str());
                c
            })
            .collect();
        let other = weigh(
            &without
                .iter()
                .map(|c| estimate.evidence(c))
                .collect::<Vec<_>>(),
            &without
                .iter()
                .map(|c| estimate.supports(c))
                .collect::<Vec<_>>(),
            prior,
            bar,
        )
        .zone;
        let t = carried.entry(source.clone()).or_default();
        match (zone, other) {
            (Zone::Link(at, _), Zone::Link(was, _)) if at == was => {}
            (Zone::Link(..), _) => t.pivotal_links += 1,
            (_, Zone::Link(..)) => t.vetoes += 1,
            _ => {}
        }
        if let Zone::Link(at, _) = zone {
            if comps[at].get(source.as_str()) == Some(&true) {
                t.links += 1;
            }
        }
    }
}

#[cfg(test)]
#[path = "weigh_tests.rs"]
mod tests;
