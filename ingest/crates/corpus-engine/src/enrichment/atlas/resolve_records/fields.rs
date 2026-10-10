// SPDX-License-Identifier: AGPL-3.0-or-later
//! Declared document fields as identity sources (ONTOLOGY_METHOD.md
//! §Identity, invariant 2): every stamp `change.document` declares is
//! compared between a statement's document and a record, and weighed at what
//! its agreement is estimated to be worth on this corpus (`estimate.rs`). A
//! record holding the document's value is an alternative even when no
//! proposer offered it. Ring 1a of `research/ontology-apps/resolve-prereg.md`:
//! shown "(same thread)", the model chose another thread's words, so a field
//! is weighed in code, never shown.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use tracing::debug;

use super::estimate::{Comparison, Estimate};
use super::weigh::{weigh, Vote, Zone};
use super::{Criterion, Document, Plan, Record, Statement};
use crate::enrichment::ontology::DocumentStamp;
use crate::enrichment::reconciliation::identity_signals::fold_identity_value;

/// The records holding one of `doc`'s stamp values, in record order.
pub(super) fn named(
    doc: Document<'_>,
    by_field: &HashMap<(DocumentStamp, String), BTreeSet<usize>>,
) -> Vec<usize> {
    let mut out = BTreeSet::new();
    for (stamp, value) in doc.stamps {
        match by_field.get(&(*stamp, value.clone())) {
            Some(held) => {
                debug!(
                    document = doc.id,
                    field = stamp.attr(),
                    value,
                    records = held.len(),
                    "atlas/resolve: records hold the document's field value"
                );
                out.extend(held.iter().copied());
            }
            None => debug!(
                document = doc.id,
                field = stamp.attr(),
                value,
                "atlas/resolve: no record holds the value"
            ),
        }
    }
    out.into_iter().collect()
}

/// Each stamp of `doc` against `record`: agrees when the record holds the
/// document's value, disagrees when it holds the field with other values,
/// silent when either side lacks the field.
pub(super) fn compare(doc: Document<'_>, record: &Record, into: &mut Comparison) {
    for (stamp, value) in doc.stamps {
        if let Some(held) = record.fields.get(stamp.attr()) {
            into.insert(stamp.attr().to_string(), held.contains(value));
        }
    }
}

/// The sources of `c` that agreed at a weight above 0, each at its
/// estimated precision: what carried a link to `record` (C3).
pub(super) fn votes_for(c: &Comparison, record: &str, estimate: &Estimate) -> Vec<Vote> {
    c.iter()
        .filter(|(_, &agree)| agree)
        .filter(|(source, _)| estimate.sources.get(*source).is_some_and(|w| w.agree > 0.0))
        .filter_map(|(source, _)| {
            Some(Vote {
                source: source.clone(),
                record: record.to_string(),
                precision: estimate.precision(source)?,
            })
        })
        .collect()
}

/// The fields alone, weighed over the records they name and none: the plan
/// they make for every statement of `doc` no key settled when the answerer is
/// a partition (`Answerer::Model`, `Answerer::Proposed`), which weighs no
/// other source. The comparisons it weighed are pushed to `seen`, so the
/// estimate learns from them. `ids` names records for the trace.
pub(super) fn settle(
    criterion: &Criterion,
    doc: Document<'_>,
    records: &[Record],
    by_field: &HashMap<(DocumentStamp, String), BTreeSet<usize>>,
    estimate: &Estimate,
    seen: &mut Vec<Comparison>,
) -> Option<Plan> {
    let named = named(doc, by_field);
    if named.is_empty() {
        return None;
    }
    let comparisons: Vec<Comparison> = named
        .iter()
        .map(|&r| {
            let mut c = Comparison::new();
            compare(doc, &records[r], &mut c);
            c
        })
        .collect();
    let evidence: Vec<f64> = comparisons.iter().map(|c| estimate.evidence(c)).collect();
    let raised: Vec<bool> = comparisons.iter().map(|c| estimate.supports(c)).collect();
    seen.extend(comparisons.iter().cloned());
    let Zone::Link(i, posterior) =
        weigh(&evidence, &raised, estimate.prior_log_odds(), criterion.bar).zone
    else {
        debug!(
            document = doc.id,
            "atlas/resolve: the fields settle nothing"
        );
        return None;
    };
    let record = named[i];
    Some(Plan::Weighed {
        record,
        posterior,
        votes: votes_for(&comparisons[i], &records[record].id, estimate),
    })
}

/// Each necessary attribute as a source: agrees when the statement's one
/// value is the other side's one value, disagrees when the other side does
/// not hold it, silent when either side has none or the other holds several
/// including it. Supplied values that differ never get here: they forbid
/// (`necessary_compatible`).
pub(super) fn compare_necessary(
    criterion: &Criterion,
    statement: &BTreeMap<String, BTreeSet<String>>,
    other: &BTreeMap<String, BTreeSet<String>>,
    into: &mut Comparison,
) {
    for attr in &criterion.necessary {
        let Some(mine) = statement.get(&attr.name).filter(|v| v.len() == 1) else {
            continue;
        };
        let Some(theirs) = other.get(&attr.name).filter(|v| !v.is_empty()) else {
            continue;
        };
        let value = mine.first().expect("one value");
        let source = format!("necessary:{}", attr.name);
        match (theirs.contains(value), theirs.len()) {
            (true, 1) => {
                into.insert(source, true);
            }
            (false, _) => {
                into.insert(source, false);
            }
            (true, _) => {}
        }
    }
}

/// The declared keys a statement carries, folded the way every identity
/// comparison folds them (`fold_identity_value`, one decider with the reconciler).
pub(super) fn declared_keys(criterion: &Criterion, s: &Statement) -> Vec<(String, String)> {
    criterion
        .keys
        .iter()
        .filter_map(|k| Some((k.clone(), fold_identity_value(s.keys.get(k)?)?)))
        .collect()
}

/// Pairs of asked statements (positions in `asked`) that share a declared key value.
pub(super) fn key_edges(asked: &[usize], keys: &[Vec<(String, String)>]) -> Vec<(usize, usize)> {
    let mut first: HashMap<&(String, String), usize> = HashMap::new();
    let mut edges = Vec::new();
    for (j, &i) in asked.iter().enumerate() {
        for kv in &keys[i] {
            match first.get(kv) {
                Some(&f) => edges.push((f, j)),
                None => {
                    first.insert(kv, j);
                }
            }
        }
    }
    edges
}
