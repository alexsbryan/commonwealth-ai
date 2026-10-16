// SPDX-License-Identifier: AGPL-3.0-or-later
//! A window read's reference value (`window.rs::reading`, `AttrFamily::Ref`).

use serde_json::Value;
use tracing::debug;

use super::super::pick::Candidate;
use super::super::validation::field_evidence_exists;
use super::super::DocumentReadField;
use super::{particulars, supported, unknown};
use crate::enrichment::atlas::SourceDocument;
use crate::enrichment::ontology::TypeIndex;
use crate::enrichment::reconciliation::identity_signals::fold_identity_value;

/// A reference to an `of` the window read as `words`: a candidate the
/// document's fields name, words it holds, or a `particulars` label read as
/// its name, cited by `cited` (`label_name`); nothing else is supported.
/// When `words` names a particular the answer lists other names for
/// (`names_for`), it is found by the first of its names that resolves, and
/// the reading is that name; the statement carries the other names
/// (`OTHER_NAMES_ATTRIBUTE`, `envelope`) for RESOLVE to show. A listed record
/// answers its own identity, which is what lands the reading on it.
pub(super) fn reference(
    of: &str,
    words: &str,
    cited: Option<&str>,
    document: &SourceDocument,
    named: &[(String, Vec<Candidate>)],
    particulars: &[Value],
    index: &TypeIndex<'_>,
) -> DocumentReadField {
    let label = particulars::label_name(particulars, index, of, words, document);
    let cite = cited.filter(|_| label.is_some());
    let words = label.unwrap_or_else(|| words.to_string());
    let names = particulars::names_for(particulars, index, of, &words);
    let candidate = names.iter().find_map(|name| {
        let wanted = fold_identity_value(name);
        named
            .iter()
            .filter(|(t, _)| t == of)
            .flat_map(|(_, cs)| cs)
            .find(|c| match c {
                Candidate::Record {
                    identity, names, ..
                } => names
                    .iter()
                    .chain([identity.join(" ")].iter())
                    .any(|n| fold_identity_value(n) == wanted),
                Candidate::Mention { text, .. } => fold_identity_value(text) == wanted,
            })
            .map(|c| (name.as_str(), c))
    });
    let held = || {
        names
            .iter()
            .find(|n| cite.is_some() || field_evidence_exists(document, n))
    };
    let by_other = |name: &str| {
        if name != words {
            debug!(document = %document.key(), %words, name, "document_read/window: a reference found by another name its particular has");
        }
    };
    match (candidate, held()) {
        (Some((name, c)), _) => {
            by_other(name);
            let (value, scalar) = c.answer();
            supported(Value::String(value), &scalar)
        }
        (None, Some(name)) => {
            by_other(name);
            supported(Value::String(name.clone()), cite.unwrap_or(name.as_str()))
        }
        (None, None) => unknown(format!(
            "{words:?} is neither a {of} this document's fields name nor words it holds"
        )),
    }
}
