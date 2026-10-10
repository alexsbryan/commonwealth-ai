// SPDX-License-Identifier: AGPL-3.0-or-later
//! A statement's passage: the words around it in its own document, the one
//! document a question holds (split from `resolve_records.rs`, arch-gate's
//! 800-line band).

use super::super::resolution_documents::fold_ws;
use super::CONTEXT_BYTES;

/// `CONTEXT_BYTES` of `body` either side of a span, cut back to whitespace so
/// no word is split: the bounds of a statement's passage.
fn window(body: &str, start: usize, end: usize) -> (usize, usize) {
    let mut lo = start.saturating_sub(CONTEXT_BYTES);
    while !body.is_char_boundary(lo) {
        lo -= 1;
    }
    let mut hi = (end + CONTEXT_BYTES).min(body.len());
    while !body.is_char_boundary(hi) {
        hi += 1;
    }
    if lo > 0 {
        if let Some(cut) = body[lo..start].find(char::is_whitespace) {
            lo += cut;
        }
    }
    if hi < body.len() {
        if let Some(cut) = body[end..hi].rfind(char::is_whitespace) {
            hi = end + cut;
        }
    }
    (lo, hi)
}

/// A statement's passage, whitespace folded.
pub(super) fn context(body: &str, start: usize, end: usize) -> String {
    let (lo, hi) = window(body, start, end);
    fold_ws(&body[lo..hi])
}

/// A statement's passage with its own words in `[[` `]]`.
pub(super) fn marked_context(body: &str, start: usize, end: usize) -> String {
    let (lo, hi) = window(body, start, end);
    fold_ws(&format!(
        "{}[[{}]]{}",
        &body[lo..start],
        &body[start..end],
        &body[end..hi]
    ))
}

/// The whole lines a statement's span lies on, whitespace folded: the lines
/// it cites. What a later RESOLVE question quotes of its document (C2,
/// narrowed). A span narrower than a line (a gold mention) is widened to its
/// line. A body with no line break is the default path's folded text
/// (`resolution_documents::body`), where the span is the reader's cited lines
/// already, so it stands as it is.
pub(super) fn lines_of(body: &str, start: usize, end: usize) -> String {
    if !body.contains('\n') {
        return fold_ws(&body[start..end]);
    }
    let lo = body[..start].rfind('\n').map_or(0, |at| at + 1);
    let hi = body[end..].find('\n').map_or(body.len(), |at| end + at);
    fold_ws(&body[lo..hi])
}
