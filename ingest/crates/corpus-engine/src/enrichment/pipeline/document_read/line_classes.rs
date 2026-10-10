// SPDX-License-Identifier: AGPL-3.0-or-later
//! Lines code can classify by structure alone, kept out of Locate
//! (ONTOLOGY_METHOD §Reading). Two classes, each a fact about where else a
//! line's text is stored, never about its words:
//!
//! - a **quote**: the line's text is a line of another document dated earlier
//!   by the declared clock (`change.document.date`), as a reply carries the
//!   message it answers;
//! - **boilerplate**: the line's text is a line of another document by the
//!   same author, by the declared author field (`change.document.author`), as
//!   a signature is.
//!
//! A line's text is compared from its first to its last letter or digit, with
//! its whitespace folded, so a marker a client puts before a quoted line is
//! not part of it; no marker is named. "Earlier" is the declared clock, never
//! the order documents were stored in, so the classes (and the records) are
//! the same in any document order (C4). A document with no readable date
//! quotes nothing and is quoted by nothing; with no author field declared,
//! nothing is boilerplate. Both are counted where the reader traces a
//! document.

use std::collections::{BTreeSet, HashMap};

use crate::enrichment::atlas::resolution_documents::read_stamp;
use crate::enrichment::atlas::SourceDocument;
use crate::enrichment::ontology::{DocumentStamp, OntologyPolicies};
use crate::enrichment::reconciliation::identity_signals::fold_identity_value;

/// Why a line is not asked. Closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum LineClass {
    /// Its text is a line of a document by the same author.
    Boilerplate,
    /// Its text is a line of a document dated earlier.
    Quote,
}

impl LineClass {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Boilerplate => "boilerplate",
            Self::Quote => "quote",
        }
    }
}

/// The corpus's lines, by where each text is stored: built once over every
/// document a run reads, before any is read.
#[derive(Debug, Default)]
pub struct LineClasses {
    /// Document key → (its date by the declared clock, its author).
    docs: HashMap<String, (Option<String>, Option<String>)>,
    /// A line's text → the documents holding it as a line.
    holders: HashMap<String, BTreeSet<String>>,
}

/// A line's text as compared: from its first to its last letter or digit,
/// whitespace folded. `None` for a line with neither.
pub(super) fn line_key(text: &str) -> Option<String> {
    let start = text.find(char::is_alphanumeric)?;
    let end = text
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(i, c)| i + c.len_utf8())?;
    Some(
        text[start..end]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    )
}

impl LineClasses {
    /// Index `documents` (each once, by key) under `policies`' declared date
    /// and author fields.
    pub fn of<'a>(
        documents: impl IntoIterator<Item = &'a SourceDocument>,
        policies: &OntologyPolicies,
    ) -> Self {
        let declared = policies.change.document.as_ref();
        let date_field = declared.and_then(|d| d.date.as_deref());
        let author_field = declared.and_then(|d| d.author.as_deref());
        let mut out = Self::default();
        for d in documents {
            if out.docs.contains_key(d.key()) {
                continue;
            }
            let date =
                date_field.and_then(|f| read_stamp(d.metadata(), DocumentStamp::Date, f).ok());
            // An author is compared after the identity fold, as a source's
            // identity values are.
            let author = author_field
                .and_then(|f| read_stamp(d.metadata(), DocumentStamp::Id, f).ok())
                .and_then(|a| fold_identity_value(&a));
            out.docs.insert(d.key().to_string(), (date, author));
            for line in d.raw_body().lines() {
                if let Some(k) = line_key(line) {
                    out.holders
                        .entry(k)
                        .or_default()
                        .insert(d.key().to_string());
                }
            }
        }
        tracing::debug!(
            documents = out.docs.len(),
            texts = out.holders.len(),
            dated = out.docs.values().filter(|(d, _)| d.is_some()).count(),
            authored = out.docs.values().filter(|(_, a)| a.is_some()).count(),
            "document_read/line_classes: indexed"
        );
        out
    }

    /// The class of the line `text` of `document`, if it has one. Boilerplate
    /// first: a signature its author repeats is boilerplate wherever it is.
    pub(super) fn class(&self, document: &str, text: &str) -> Option<LineClass> {
        let key = line_key(text)?;
        let (date, author) = self.docs.get(document)?;
        let others = self.holders.get(&key)?.iter().filter(|k| *k != document);
        let mut quote = false;
        for other in others {
            let Some((other_date, other_author)) = self.docs.get(other) else {
                continue;
            };
            if author.is_some() && other_author == author {
                return Some(LineClass::Boilerplate);
            }
            quote |= matches!((date, other_date), (Some(mine), Some(theirs)) if theirs < mine);
        }
        quote.then_some(LineClass::Quote)
    }

    /// What keys a cached read of `documents`: each one's classified lines,
    /// so a read is redone when another document changes what it quotes.
    pub fn digest<'a>(&self, documents: impl IntoIterator<Item = &'a SourceDocument>) -> String {
        let mut out = String::new();
        for d in documents {
            out.push_str(d.key());
            for (n, line) in d.raw_body().lines().enumerate() {
                if let Some(class) = self.class(d.key(), line) {
                    out.push_str(&format!(" {n}:{}", class.label()));
                }
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
#[path = "line_classes_tests.rs"]
mod tests;
