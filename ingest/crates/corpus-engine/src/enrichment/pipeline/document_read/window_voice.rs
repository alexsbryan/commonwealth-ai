// SPDX-License-Identifier: AGPL-3.0-or-later
//! Who asserts a statement and how its kind grades it, as a window read's
//! claim gives them (`said_by`, `grade`; `window_prompt.md`). Who says a
//! statement is a fact the reader reports, never a condition of writing it: a
//! voice its document does not hold is dropped and the claim kept, and the
//! document's own voice (a narrator, a report) names no speaker.

use serde_json::{Map, Value};
use tracing::debug;

use super::super::validation::speaker_in_body;
use super::particulars::names_of;
use super::text;
use crate::enrichment::atlas::SourceDocument;
use crate::enrichment::ontology::OntologyTypeDecl;
use crate::enrichment::pipeline::pipelines::parse_policy::{validated_choice, ATTR_GRADE};

/// The answer's `said_by` for a statement its document makes in its own voice.
pub(super) const OWN_VOICE: &str = "the document";

/// What became of a claim's `said_by`, counted per window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Voice {
    /// A particular's name or the document's words for who says it, kept.
    Named,
    /// The document says it in its own voice.
    Own,
    /// The answer gave none.
    Unstated,
    /// Words, or every name of a particular, this document does not hold.
    Dropped,
}

/// The speaker a claim keeps for `said_by` in `document`, and which voice it
/// was. The answer offers no particular to cite (offered one, a mail reader
/// minted a person per sender, note 2dde9c77), but a label written anyway
/// reads as the first of its names the document holds; other words are kept
/// as written when it holds them: the check
/// validation makes of every speaker (`speaker_in_body`).
pub(super) fn said_by(
    said_by: &Value,
    document: &SourceDocument,
    particulars: &[Value],
) -> (Option<String>, Voice) {
    let words = text(said_by);
    if words.is_empty() {
        return (None, Voice::Unstated);
    }
    if words.eq_ignore_ascii_case(OWN_VOICE) {
        return (None, Voice::Own);
    }
    let names = particulars
        .iter()
        .find(|p| text(&p["ref"]) == words)
        .map_or_else(|| vec![words.clone()], names_of);
    match names.into_iter().find(|n| speaker_in_body(document, n)) {
        Some(name) => (Some(name), Voice::Named),
        None => {
            debug!(document = %document.key(), said_by = %words, "document_read/window: a voice this document does not hold is dropped; its claim is kept");
            (None, Voice::Dropped)
        }
    }
}

/// The grade a claim keeps: one of its kind's declared grades, as declared,
/// by the general extractor's rule (`validated_choice`); none when the
/// answer gives none or one the kind does not declare.
pub(super) fn grade(claim: &Value, kind: &OntologyTypeDecl) -> Option<String> {
    let given = claim.get(ATTR_GRADE).filter(|v| !v.is_null())?;
    let mut raw = Map::from_iter([(ATTR_GRADE.to_string(), given.clone())]);
    let mut kept = Map::new();
    validated_choice(&mut raw, &mut kept, ATTR_GRADE, &kind.grades, &kind.name);
    kept.remove(ATTR_GRADE)
        .and_then(|v| v.as_str().map(str::to_string))
}
