// SPDX-License-Identifier: AGPL-3.0-or-later
use super::{B3Outcome, Clustering, EntityOutcome};
use std::collections::{BTreeMap, BTreeSet};

/// Nonstandard B³ recovery: average precision over all placed members and
/// recall over all gold members, using full clusters on both sides. Missing
/// members contribute zero recall; extra placements contribute zero precision
/// and remain in predicted cluster sizes. Empty denominators return zero.
pub fn recovery_b_cubed(predicted: &Clustering, gold: &Clustering) -> B3Outcome {
    let mut pc: BTreeMap<&String, BTreeSet<&String>> = BTreeMap::new();
    let mut gc: BTreeMap<&String, BTreeSet<&String>> = BTreeMap::new();
    for (member, cluster) in predicted {
        pc.entry(cluster).or_default().insert(member);
    }
    for (member, cluster) in gold {
        gc.entry(cluster).or_default().insert(member);
    }
    let pk: BTreeSet<_> = predicted.keys().collect();
    let gk: BTreeSet<_> = gold.keys().collect();
    let mut p_sum = 0.0;
    let mut r_sum = 0.0;
    let mut n_aligned = 0;
    for member in pk.intersection(&gk) {
        let p = &pc[&predicted[*member]];
        let g = &gc[&gold[*member]];
        let overlap = p.intersection(g).count() as f64;
        p_sum += overlap / p.len() as f64;
        r_sum += overlap / g.len() as f64;
        n_aligned += 1;
    }
    let precision = if predicted.is_empty() {
        0.0
    } else {
        p_sum / predicted.len() as f64
    };
    let recall = if gold.is_empty() {
        0.0
    } else {
        r_sum / gold.len() as f64
    };
    tracing::debug!(
        predicted = predicted.len(),
        gold = gold.len(),
        n_aligned,
        precision,
        recall,
        "entity_resolution_score: recovery includes missing members and extra placements"
    );
    B3Outcome {
        precision,
        recall,
        f1: EntityOutcome::of(precision, recall).f1,
        n_aligned,
        unmatched_predicted: pk.difference(&gk).map(|s| (*s).clone()).collect(),
        unmatched_gold: gk.difference(&pk).map(|s| (*s).clone()).collect(),
    }
}
