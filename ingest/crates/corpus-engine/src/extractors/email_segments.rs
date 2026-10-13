// SPDX-License-Identifier: AGPL-3.0-or-later
//! The messages a mail body embeds: a reply's quoted original, a forward, a
//! run of `>`-marked lines. Mail knowledge, so it lives with the mail
//! extractor and nowhere else: the reader sees only the spans written into
//! the document's metadata (`segments`) and never asks a line in one as the
//! author's own (ONTOLOGY_METHOD §Reading; a segment's own header is the
//! only thing that may date or author what it says).
//!
//! A segment opens at one of the conventions mail clients write:
//!
//! - a separator line: dashes or underscores around words ("Original
//!   Message", "Forwarded by …", "Forwarded Message"), or "Begin forwarded
//!   message:";
//! - a header run: two or more `Field: value` lines of From, To, Cc, Bcc,
//!   Sent, Date or Subject (wrapped values included), with the one or two
//!   lines directly above it when they name a sender (an `@`) or a date, as
//!   a Notes client writes "Name@ORG" and "03/28/2001 05:11 PM" there;
//! - an attribution line ending in "wrote:", or a run of `>`-marked lines.
//!
//! A separator or header run opens a segment that runs to the end of the
//! body, as a client puts the original under the reply. An attribution or a
//! marked run covers itself and the marked lines after it, because authors
//! answer between quoted lines in that style.

use serde::Serialize;

/// One embedded message, in byte offsets of the body it was found in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmbeddedSegment {
    pub start: usize,
    pub end: usize,
    /// Where its own header block ends (`start` when it has none).
    pub header_end: usize,
    pub kind: SegmentKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentKind {
    Reply,
    Forward,
    Quote,
}

const HEADER_FIELDS: [&str; 7] = ["from", "to", "cc", "bcc", "sent", "date", "subject"];

/// A body's lines with their byte ranges (end excludes the newline).
fn lines(body: &str) -> Vec<(usize, usize, &str)> {
    let mut out = Vec::new();
    let mut at = 0;
    for raw in body.split('\n') {
        out.push((at, at + raw.len(), raw));
        at += raw.len() + 1;
    }
    out
}

fn is_field(line: &str) -> bool {
    let t = line.trim_start();
    t.split_once(':').is_some_and(|(name, _)| {
        let name = name.trim().to_ascii_lowercase();
        HEADER_FIELDS.contains(&name.as_str())
    })
}

fn is_separator(line: &str) -> Option<SegmentKind> {
    let t = line.trim();
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("begin forwarded message") {
        return Some(SegmentKind::Forward);
    }
    let fenced = |c: char| t.starts_with(c) && t.chars().take_while(|&x| x == c).count() >= 3;
    if !(fenced('-') || fenced('_')) {
        return None;
    }
    let words = lower.trim_matches(|c: char| c == '-' || c == '_' || c.is_whitespace());
    if words.contains("forward") {
        Some(SegmentKind::Forward)
    } else if words.contains("original message") || words.contains("reply") {
        Some(SegmentKind::Reply)
    } else {
        None
    }
}

/// Whether a line above a header run names who sent it or when.
fn names_sender_or_date(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    let has_date = t
        .split(|c: char| c.is_whitespace() || c == ',')
        .any(|w| {
            let parts: Vec<&str> = w.split('/').collect();
            parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        });
    has_date || t.contains('@')
}

/// The header run starting at line `i`: the index past its last line, when it
/// holds two or more field lines.
fn header_run(ls: &[(usize, usize, &str)], i: usize) -> Option<usize> {
    if !is_field(ls[i].2) {
        return None;
    }
    let (mut j, mut fields) = (i, 0);
    while j < ls.len() && !ls[j].2.trim().is_empty() {
        if is_field(ls[j].2) {
            fields += 1;
        }
        j += 1;
    }
    (fields >= 2).then_some(j)
}

/// The messages `body` embeds, in order, none overlapping.
pub fn embedded_segments(body: &str) -> Vec<EmbeddedSegment> {
    let ls = lines(body);
    let mut out: Vec<EmbeddedSegment> = Vec::new();
    let mut i = 0;
    while i < ls.len() {
        let (start, _, text) = ls[i];
        // Separator, with the header run under it when there is one.
        if let Some(kind) = is_separator(text) {
            let below = (i + 1..ls.len()).find(|&k| !ls[k].2.trim().is_empty());
            let header_end = below
                .and_then(|k| header_run(&ls, k).map(|end| ls[end - 1].1))
                .unwrap_or(ls[i].1);
            out.push(EmbeddedSegment { start, end: body.len(), header_end, kind });
            break;
        }
        if let Some(end) = header_run(&ls, i) {
            let mut first = i;
            for _ in 0..2 {
                if first > 0 && names_sender_or_date(ls[first - 1].2) {
                    first -= 1;
                } else {
                    break;
                }
            }
            let kind = if ls[first..end].iter().any(|l| l.2.to_ascii_lowercase().contains("forward")) {
                SegmentKind::Forward
            } else {
                SegmentKind::Reply
            };
            out.push(EmbeddedSegment {
                start: ls[first].0,
                end: body.len(),
                header_end: ls[end - 1].1,
                kind,
            });
            break;
        }
        let marked = |k: usize| ls[k].2.trim_start().starts_with('>');
        let attribution = text.trim_end().ends_with("wrote:");
        if attribution || marked(i) {
            let mut j = i + 1;
            while j < ls.len() && marked(j) {
                j += 1;
            }
            if attribution && j == i + 1 {
                i += 1;
                continue;
            }
            out.push(EmbeddedSegment {
                start,
                end: ls[j - 1].1,
                header_end: if attribution { ls[i].1 } else { start },
                kind: SegmentKind::Quote,
            });
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

#[cfg(test)]
#[path = "email_segments_tests.rs"]
mod tests;
