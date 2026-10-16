// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the window reader is shown of each document's body (`window.rs`).

use tracing::debug;

use super::super::line_classes::LineClasses;
use crate::enrichment::atlas::SourceDocument;

/// A document's own lines, as the passes reader asks them: no line it carries
/// from a document dated earlier, no header line, no line of a message it
/// embeds (`line_classes.rs`). A reply's quoted history is the earlier
/// message's, which the window holds under its own date or another window
/// does; read here, it states the earlier stage under the later date.
pub(super) fn own_text(doc: &SourceDocument, classes: &LineClasses) -> String {
    let raw = doc.raw_body();
    let lines = super::super::passes::lines(&raw);
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
        "(no line of its own: every line is carried, a header or an embedded message)".to_string()
    } else {
        own.join("\n")
    }
}
