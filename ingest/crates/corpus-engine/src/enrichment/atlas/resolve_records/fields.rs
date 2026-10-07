// SPDX-License-Identifier: AGPL-3.0-or-later
//! Evidential document fields weighed in code (ONTOLOGY_METHOD.md §Identity,
//! invariant 2): a field the type lists in `identity_evidential` links a
//! statement on its own only where the precision measured for it clears the
//! type's `identity_bar`, and only to the one record from an earlier document
//! that holds the statement's value. Held by two or more records, the field
//! settles nothing and the statement goes to the answerer. Ring 1a of
//! `research/ontology-apps/resolve-prereg.md`: shown "(same thread)", the
//! model chose another thread's words, so the field is weighed, not shown.

use std::collections::{BTreeSet, HashMap};

use tracing::debug;

use super::{Criterion, Document, Plan};
use crate::enrichment::ontology::DocumentStamp;

/// The plan an evidential field makes for every statement of `doc` no key
/// settled, or `None`. Of several fields that settle, the most precise wins.
pub(super) fn settle(
    criterion: &Criterion,
    doc: Document<'_>,
    by_field: &HashMap<(DocumentStamp, String), BTreeSet<usize>>,
) -> Option<Plan> {
    if criterion.evidential.is_empty() {
        return None;
    }
    let Some(bar) = criterion.bar else {
        debug!(
            document = doc.id,
            "atlas/resolve: evidential fields declared without a bar; none links"
        );
        return None;
    };
    let mut best: Option<(usize, DocumentStamp, &str, f64)> = None;
    for &(stamp, precision) in &criterion.evidential {
        let field = stamp.attr();
        let Some(value) = doc.stamp(stamp) else {
            debug!(
                document = doc.id,
                field, "atlas/resolve: document lacks the field"
            );
            continue;
        };
        if precision < bar {
            debug!(
                document = doc.id,
                field, precision, bar, "atlas/resolve: field below the bar"
            );
            continue;
        }
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
                "atlas/resolve: value held by several records; the field settles nothing"
            ),
            Some(held) => {
                if let Some(&record) = held.first() {
                    if best.is_none_or(|(_, _, _, p)| precision > p) {
                        best = Some((record, stamp, value, precision));
                    }
                }
            }
        }
    }
    let (record, stamp, value, precision) = best?;
    debug!(
        document = doc.id,
        field = stamp.attr(),
        value,
        record,
        precision,
        bar,
        "atlas/resolve: an evidential field settles the document"
    );
    Some(Plan::Field {
        record,
        stamp,
        value: value.to_string(),
        precision,
    })
}
