// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the window reader is shown of each document's body (`window.rs`).

use std::collections::HashMap;

use tracing::debug;

use super::super::line_classes::{LineClass, LineClasses};
use crate::enrichment::atlas::SourceDocument;

const NO_OWN_LINE: &str =
    "(no line of its own: every line is carried, a header or an embedded message)";

/// A document's own lines: no line it carries
/// from a document dated earlier, no header line, no line of a message it
/// embeds (`line_classes.rs`). A reply's quoted history is the earlier
/// message's, which the window holds under its own date or another window
/// does; read here, it states the earlier stage under the later date. What a
/// claim cites and its sentence is widened in (`sentence_of`).
pub(super) fn own_text(doc: &SourceDocument, classes: &LineClasses) -> String {
    let raw = doc.raw_body();
    let lines = super::super::line_classes::lines(&raw);
    let mut held = 0usize;
    let own: Vec<&str> = lines
        .iter()
        .zip(classes.classes(doc.key(), &raw, &lines))
        .filter_map(|(line, carried)| match carried {
            Some(_) => {
                held += 1;
                None
            }
            None => Some(&raw[line.start..line.end]),
        })
        .collect();
    debug!(document = %doc.key(), lines = lines.len(), held, "document_read/window: carried, header and embedded lines not shown");
    if own.is_empty() {
        NO_OWN_LINE.to_string()
    } else {
        own.join("\n")
    }
}

/// The lines a document quotes from an earlier one or embeds from a message
/// it carries: the text a claim must not cite as the document's own
/// (`envelope`). Header lines are not among them: a header is cited through a
/// declared identity field, as validation allows.
pub(super) fn quoted_text(doc: &SourceDocument, classes: &LineClasses) -> String {
    let raw = doc.raw_body();
    let lines = super::super::line_classes::lines(&raw);
    lines
        .iter()
        .zip(classes.classes(doc.key(), &raw, &lines))
        .filter(|(_, carried)| {
            carried
                .as_ref()
                .is_some_and(|c| c.class != LineClass::Field)
        })
        .map(|(line, _)| &raw[line.start..line.end])
        .collect::<Vec<_>>()
        .join("\n")
}

/// Each document of the window by key, its label (`d1`...): what a pointer
/// names when the document it quotes is in the window.
pub(super) fn labels<'a>(
    documents: &'a [SourceDocument],
    labels: &'a [String],
) -> HashMap<&'a str, &'a str> {
    documents
        .iter()
        .zip(labels)
        .map(|(doc, label)| (doc.key(), label.as_str()))
        .collect()
}

/// What the reader is shown of a document: its own lines (`own_text`'s), and
/// in place of each run of lines it quotes from an earlier document, one
/// pointer to that document: its label in the window (`[quoted from d3]`),
/// else its date on the declared clock. The pointer says what it is, so no
/// prompt line explains it; the quoted text stays unshown, and code refuses a
/// claim that cites it (`quoted_text`, `envelope`).
/// A header line, and embedded text no earlier document holds, leave none.
pub(super) fn shown_text(
    doc: &SourceDocument,
    classes: &LineClasses,
    labels: &HashMap<&str, &str>,
) -> String {
    let raw = doc.raw_body();
    let lines = super::super::line_classes::lines(&raw);
    let sources = classes.sources(doc.key(), &raw, &lines);
    let (mut shown, mut own, mut pointers) = (Vec::new(), 0usize, 0usize);
    let mut last: Option<&str> = None;
    let carried = classes.classes(doc.key(), &raw, &lines);
    for ((line, carried), source) in lines.iter().zip(carried).zip(sources) {
        let text = &raw[line.start..line.end];
        match (carried, source) {
            (None, _) => {
                own += 1;
                if !text.trim().is_empty() {
                    last = None;
                }
                shown.push(text.to_string());
            }
            (Some(_), Some(from)) if last != Some(from) => {
                last = Some(from);
                pointers += 1;
                shown.push(match (labels.get(from), classes.date_of(from)) {
                    (Some(label), _) => format!("[quoted from {label}]"),
                    (None, Some(date)) => {
                        format!("[quoted from a document of {date}, outside this window]")
                    }
                    (None, None) => "[quoted from a document outside this window]".to_string(),
                });
            }
            _ => {}
        }
    }
    debug!(document = %doc.key(), own, pointers, "document_read/window: quoted lines shown as pointers to their documents");
    if own == 0 {
        shown.insert(0, NO_OWN_LINE.to_string());
    }
    shown.join("\n")
}
