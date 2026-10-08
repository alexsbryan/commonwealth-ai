//! The reader's document-local subject reference, carried into statements.
//!
//! The typed reading contract gives every claim a subject type and a
//! reference local to its document. Within one document and type, claims that
//! share a reference are the reader's statement that they concern one
//! particular, wherever their passages fall; they become one statement located
//! at the earliest span. Across documents a reference carries nothing: RESOLVE
//! alone decides there. Claims of one reference that disagree on a supplied
//! identity value are not joined; each span stays its own statement and the
//! disagreement is reported.

use std::collections::{BTreeMap, BTreeSet};

use tracing::debug;

use super::{failure, PhaseFailure, PhaseFailureKind};

/// One claim located in a placed document, before statements are formed.
pub(super) struct Spot {
    pub placed: usize,
    pub start: usize,
    pub end: usize,
    pub local_ref: Option<String>,
    pub keys: BTreeMap<String, String>,
}

/// The statement a spot belongs to: its id and the span it is located at.
pub(super) struct Assigned {
    pub id: String,
    pub start: usize,
    pub end: usize,
}

fn span_id(document: &str, start: usize, end: usize, local_ref: Option<&str>) -> String {
    match local_ref {
        Some(local_ref) => format!(
            "{document}@{start}..{end}#{}",
            serde_json::to_string(local_ref).expect("local references are serializable")
        ),
        None => format!("{document}@{start}..{end}"),
    }
}

/// Each spot's statement. `document_of[k]` is the key of placed document `k`.
pub(super) fn assign(
    document_of: &[&str],
    spots: &[Spot],
    failures: &mut Vec<PhaseFailure>,
) -> Vec<Assigned> {
    let mut assigned: Vec<Assigned> = spots
        .iter()
        .map(|s| Assigned {
            id: span_id(
                document_of[s.placed],
                s.start,
                s.end,
                s.local_ref.as_deref(),
            ),
            start: s.start,
            end: s.end,
        })
        .collect();
    let mut groups: BTreeMap<(usize, &str), Vec<usize>> = BTreeMap::new();
    for (j, s) in spots.iter().enumerate() {
        if let Some(local_ref) = s.local_ref.as_deref() {
            groups.entry((s.placed, local_ref)).or_default().push(j);
        }
    }
    for ((k, local_ref), members) in groups {
        let spans: BTreeSet<(usize, usize)> = members
            .iter()
            .map(|&j| (spots[j].start, spots[j].end))
            .collect();
        if spans.len() < 2 {
            continue;
        }
        let mut values: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for &j in &members {
            for (key, value) in &spots[j].keys {
                values.entry(key).or_default().insert(value);
            }
        }
        if let Some((key, _)) = values.iter().find(|(_, seen)| seen.len() > 1) {
            debug!(
                document = document_of[k],
                local_ref,
                spans = spans.len(),
                field = key,
                "atlas/resolve: local subject's spans disagree on an identity value; not joined"
            );
            failures.push(failure(
                format!("document:{}", document_of[k]),
                PhaseFailureKind::Other,
                format!(
                    "claims for local subject `{local_ref}` at {} spans conflict on identity field `{key}`; kept as separate statements",
                    spans.len()
                ),
            ));
            continue;
        }
        let (start, end) = *spans.first().expect("two or more spans");
        let id = span_id(document_of[k], start, end, Some(local_ref));
        debug!(document = document_of[k], local_ref, spans = spans.len(), statement = %id, "atlas/resolve: one local subject at several spans is one statement");
        for &j in &members {
            assigned[j] = Assigned {
                id: id.clone(),
                start,
                end,
            };
        }
    }
    assigned
}
