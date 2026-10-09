// SPDX-License-Identifier: AGPL-3.0-or-later
//! Evidential document fields weighed in code (ONTOLOGY_METHOD.md §Identity,
//! invariant 2): a field the type lists in `identity_evidential` names the
//! one record from an earlier document that holds the statement's value, at
//! the precision declared for it. Held by two or more records, the field names
//! nothing. Ring 1a of `research/ontology-apps/resolve-prereg.md`: shown
//! "(same thread)", the model chose another thread's words, so the field is
//! weighed, not shown. Since Ring 2 a field is one vote among the sources
//! (`weigh.rs`); no field decides alone by being the most precise.

use std::collections::{BTreeSet, HashMap};

use tracing::debug;

use super::weigh::{weigh, Vote, Zone};
use super::{Criterion, Document, Plan};
use crate::enrichment::ontology::DocumentStamp;

/// A field's say for every statement of one document.
#[derive(Debug, Clone)]
pub(super) struct FieldVote {
    pub record: usize,
    pub stamp: DocumentStamp,
    pub precision: f64,
}

/// Every declared field of `doc` whose value exactly one earlier record holds.
pub(super) fn votes(
    criterion: &Criterion,
    doc: Document<'_>,
    by_field: &HashMap<(DocumentStamp, String), BTreeSet<usize>>,
) -> Vec<FieldVote> {
    let mut out = Vec::new();
    for &(stamp, precision) in &criterion.evidential {
        let field = stamp.attr();
        let Some(value) = doc.stamp(stamp) else {
            debug!(
                document = doc.id,
                field, "atlas/resolve: document lacks the field"
            );
            continue;
        };
        match by_field.get(&(stamp, value.to_string())) {
            None => debug!(
                document = doc.id,
                field, value, "atlas/resolve: no record holds the value"
            ),
            Some(held) if held.len() > 1 => debug!(
                document = doc.id,
                field,
                value,
                records = held.len(),
                "atlas/resolve: value held by several records; the field names nothing"
            ),
            Some(held) => {
                if let Some(&record) = held.first() {
                    debug!(
                        document = doc.id,
                        field, value, record, precision, "atlas/resolve: a field names a record"
                    );
                    out.push(FieldVote {
                        record,
                        stamp,
                        precision,
                    });
                }
            }
        }
    }
    out
}

/// The fields alone, weighed (`weigh.rs`) over the records they name and
/// none: the plan they make for every statement of `doc` no key settled when
/// the answerer is a partition (`Answerer::Model`, `Answerer::Proposed`),
/// which weighs no other source. `ids` names records for the trace.
pub(super) fn settle(
    criterion: &Criterion,
    doc: Document<'_>,
    votes: &[FieldVote],
    ids: &dyn Fn(usize) -> String,
) -> Option<Plan> {
    if votes.is_empty() {
        return None;
    }
    let Some(bar) = criterion.bar else {
        debug!(
            document = doc.id,
            "atlas/resolve: evidential fields declared without a bar; none links"
        );
        return None;
    };
    let mut named: Vec<usize> = Vec::new();
    for v in votes {
        if !named.contains(&v.record) {
            named.push(v.record);
        }
    }
    let at = |r: usize| named.iter().position(|&n| n == r).unwrap_or(0);
    let weighed: Vec<(usize, f64)> = votes.iter().map(|v| (at(v.record), v.precision)).collect();
    let Zone::Link(i, posterior) = weigh(named.len(), &weighed, bar).zone else {
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
        votes: votes
            .iter()
            .filter(|v| v.record == record)
            .map(|v| Vote {
                source: v.stamp.attr(),
                record: ids(record),
                precision: v.precision,
            })
            .collect(),
    })
}
