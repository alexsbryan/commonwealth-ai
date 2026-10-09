// SPDX-License-Identifier: AGPL-3.0-or-later
//! The decider (ONTOLOGY_METHOD.md §Identity, Ring 2 of
//! `research/ontology-apps/resolve-prereg.md`): every source that names an
//! alternative for a statement adds its evidence, and the type's declared bar
//! reads three zones off the posterior: link, no link, unsettled.
//!
//! Independence, stated because the posterior rests on it: the sources err
//! independently given the truth, and a wrong source names any of the other
//! K-1 alternatives alike, under a uniform prior over the K alternatives (the
//! candidates and none). A source of precision p naming alternative a then
//! multiplies a's odds against each other alternative by p(K-1)/(1-p), so a
//! lone source's posterior is its own precision and every single-source link
//! made before Ring 2 is kept. The model's choice and the similarity behind
//! the proposed answer both read the text, so they are not independent in
//! fact; what that costs is measured, not assumed away.

use serde::Serialize;
use tracing::debug;

/// One source's say for a statement: the alternative it named, and the
/// precision it is weighed at.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Vote {
    /// The declared source: a document stamp's attribute, `proposed_answer`,
    /// `model_choice` or `reasoned_choice`.
    pub source: &'static str,
    /// The record it named; inside one document, the statement that opened it.
    pub record: String,
    pub precision: f64,
}

/// Where the posterior falls against the bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Zone {
    /// The candidate at this position reached the bar, strictly ahead of every other.
    Link(usize, f64),
    /// No source raised any candidate's odds against none: novelty.
    NoLink,
    /// Some candidate rose, none reached the bar alone at the top.
    Unsettled,
}

/// The posterior over `candidates` alternatives and none, and its zone.
#[derive(Debug, Clone)]
pub(super) struct Weighed {
    pub posterior: Vec<f64>,
    pub none: f64,
    pub zone: Zone,
}

/// Precisions are read as open-interval probabilities; a declared 0 or 1
/// would make one source overrule every other without bound.
const EDGE: f64 = 1e-9;

/// Weigh `votes`, each (candidate position, precision), over `candidates`
/// alternatives plus none, against `bar`.
pub(super) fn weigh(candidates: usize, votes: &[(usize, f64)], bar: f64) -> Weighed {
    let k = candidates as f64 + 1.0;
    let mut score = vec![0.0_f64; candidates];
    for &(at, p) in votes {
        let p = p.clamp(EDGE, 1.0 - EDGE);
        score[at] += (p * (k - 1.0) / (1.0 - p)).ln();
    }
    // Log-sum-exp with none at 0.
    let top = score.iter().copied().fold(0.0_f64, f64::max);
    let total = (-top).exp() + score.iter().map(|s| (s - top).exp()).sum::<f64>();
    let posterior: Vec<f64> = score.iter().map(|s| (s - top).exp() / total).collect();
    let none = (-top).exp() / total;
    // Raised: some source moved a candidate's odds against none up. A source
    // below 1/K lowers what it names, which lifts the others only by
    // renormalising, and that is no evidence for them.
    let raised = score.iter().any(|&s| s > 1e-12);
    let best = posterior
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(&a.0)));
    let zone = match best {
        _ if !raised => Zone::NoLink,
        Some((at, &p))
            if p >= bar
                && posterior
                    .iter()
                    .enumerate()
                    .all(|(i, &q)| i == at || q + 1e-12 < p) =>
        {
            Zone::Link(at, p)
        }
        _ => Zone::Unsettled,
    };
    debug!(
        candidates,
        votes = votes.len(),
        ?posterior,
        none,
        bar,
        ?zone,
        "atlas/resolve weigh: posterior over the alternatives"
    );
    Weighed {
        posterior,
        none,
        zone,
    }
}

#[cfg(test)]
#[path = "weigh_tests.rs"]
mod tests;
