// SPDX-License-Identifier: AGPL-3.0-or-later
//! A window read's reference value (`window.rs::reading`, `AttrFamily::Ref`).

use serde_json::Value;

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
    let wanted = fold_identity_value(&words);
    let candidate = named
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
        });
    match candidate {
        Some(c) => {
            let (value, scalar) = c.answer();
            supported(Value::String(value), &scalar)
        }
        None if cite.is_some() || field_evidence_exists(document, &words) => {
            supported(Value::String(words.clone()), cite.unwrap_or(&words))
        }
        None => unknown(format!(
            "{words:?} is neither a {of} this document's fields name nor words it holds"
        )),
    }
}
