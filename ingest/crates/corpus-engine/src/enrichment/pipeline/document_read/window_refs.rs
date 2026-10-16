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
/// Written with several names (`names_in`), it is found by the first that
/// resolves, and a mention or held words keep every name the reader wrote, so
/// what RESOLVE shows carries the other name too; a listed record answers its
/// own identity, which is what lands the reading on it.
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
    let names = names_in(&words);
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
            .map(|c| (*name, c))
    });
    let held = || {
        names
            .iter()
            .find(|n| cite.is_some() || field_evidence_exists(document, n))
    };
    match (candidate, held()) {
        (Some((name, c @ Candidate::Record { .. })), _) => {
            if name != words {
                debug!(document = %document.key(), %words, name, "document_read/window: a reference written with several names read by one");
            }
            let (value, scalar) = c.answer();
            supported(Value::String(value), &scalar)
        }
        (Some((name, c)), _) => {
            let (text, scalar) = c.answer();
            supported(Value::String(written(&words, name, text, document)), &scalar)
        }
        (None, Some(&name)) => supported(
            Value::String(written(&words, name, name.to_string(), document)),
            cite.unwrap_or(name),
        ),
        (None, None) => unknown(format!(
            "{words:?} is neither a {of} this document's fields name nor words it holds"
        )),
    }
}

pub(super) use super::super::names_in;

/// The value of a reading found by `name`, one of the names the reader wrote
/// as `words`: `found` when the reader wrote one name, else every name it
/// wrote, so the reading keeps the other name for RESOLVE to show (code lands
/// it on a record by any of them, `resolution_derived/read_fields.rs`).
fn written(words: &str, name: &str, found: String, document: &SourceDocument) -> String {
    if name == words {
        return found;
    }
    debug!(document = %document.key(), %words, name, "document_read/window: a reference written with several names, found by one, keeps them all");
    words.to_string()
}
