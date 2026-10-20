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
//!   lines above it that name a sender (an `@`) or a date, as a Notes client
//!   writes "Name@ORG" and "03/28/2001 05:11 PM" there, over a line of
//!   whitespace between them and the fields;
//! - an attribution line ending in "wrote:", or a run of `>`-marked lines.
//!
//! A separator or header run opens a segment that runs to the next one, else
//! to the end of the body, as a client puts each original under the reply to
//! it: one segment per message of the history (`email_embedded` reads each as
//! a message of its own). An attribution or a marked run before the first
//! covers itself and the marked lines after it, because authors answer
//! between quoted lines in that style; under a header, marked lines are that
//! message's quotes.

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

/// A header field a mail client writes as `Field: value`. Closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Field {
    From,
    To,
    Cc,
    Bcc,
    Sent,
    /// Notes writes "Sent by:" under the sender and date lines.
    SentBy,
    Date,
    Subject,
}

impl Field {
    const NAMES: [(&'static str, Field); 8] = [
        ("from", Field::From),
        ("to", Field::To),
        ("cc", Field::Cc),
        ("bcc", Field::Bcc),
        ("sent", Field::Sent),
        ("sent by", Field::SentBy),
        ("date", Field::Date),
        ("subject", Field::Subject),
    ];

    /// The field a line writes, and its value.
    pub(super) fn of(line: &str) -> Option<(Self, &str)> {
        let (name, value) = line.trim_start().split_once(':')?;
        let name = name.trim().to_ascii_lowercase();
        Self::NAMES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|&(_, field)| (field, value))
    }
}

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

pub(super) fn is_field(line: &str) -> bool {
    Field::of(line).is_some()
}

pub(super) fn is_separator(line: &str) -> Option<SegmentKind> {
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

/// Whether a line ends in a separator's closing dashes, as the second line of
/// a separator a client wrapped ("12:05 PM ---------------------------").
pub(super) fn fenced_end(line: &str) -> bool {
    let t = line.trim_end();
    t.ends_with("---") || t.ends_with("___")
}

/// Whether a line holds a `d/d/d` date.
pub(super) fn names_date(line: &str) -> bool {
    line.split(|c: char| c.is_whitespace() || c == ',')
        .any(|w| {
            let parts: Vec<&str> = w.split('/').collect();
            parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
}

/// Whether a line above a header run names who sent it or when.
pub(super) fn names_sender_or_date(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && (names_date(t) || t.contains('@'))
}

/// The header run starting at line `i`: the index past its last line, when it
/// holds two or more field lines. Its fields go on across whitespace lines,
/// as Notes writes "cc:", a blank line, then "Subject:".
fn header_run(ls: &[(usize, usize, &str)], i: usize) -> Option<usize> {
    if !is_field(ls[i].2) {
        return None;
    }
    let (mut j, mut end, mut fields) = (i, i, 0);
    while j < ls.len() {
        if ls[j].2.trim().is_empty() {
            match (j..ls.len()).find(|&k| !ls[k].2.trim().is_empty()) {
                Some(k) if is_field(ls[k].2) => j = k,
                _ => break,
            }
        }
        if is_field(ls[j].2) {
            fields += 1;
        }
        j += 1;
        end = j;
    }
    (fields >= 2).then_some(end)
}

/// Where the header block of the field run at line `i` begins: the line above
/// it naming a sender or a date, passing over whitespace lines between them,
/// and the line above that when it names a sender too or the first is a date
/// (Notes writes "Kim Ward" over "01/02/2001 08:13 AM"); else the run itself.
fn header_block_start(ls: &[(usize, usize, &str)], i: usize) -> usize {
    let mut k = i;
    while k > 0 && ls[k - 1].2.trim().is_empty() {
        k -= 1;
    }
    if k == 0 || !names_sender_or_date(ls[k - 1].2) {
        return i;
    }
    let first = k - 1;
    let above = first
        .checked_sub(1)
        .map(|a| ls[a].2.trim())
        .filter(|t| !t.is_empty() && !is_field(t));
    match above {
        Some(t) if names_sender_or_date(t) || names_date(ls[first].2) => first - 1,
        _ => first,
    }
}

/// The messages `body` embeds, in order, none overlapping.
pub fn embedded_segments(body: &str) -> Vec<EmbeddedSegment> {
    let ls = lines(body);
    let mut out: Vec<EmbeddedSegment> = Vec::new();
    // Each separator or header run opens a message the one before it embeds:
    // that one ends where it opens.
    let open = |out: &mut Vec<EmbeddedSegment>, segment: EmbeddedSegment| {
        if let Some(last) = out.last_mut().filter(|s| s.kind != SegmentKind::Quote) {
            last.end = segment.start;
        }
        out.push(segment);
    };
    let mut headed = false;
    let mut i = 0;
    while i < ls.len() {
        let (start, _, text) = ls[i];
        // Separator, with the header run under it when there is one.
        if let Some(kind) = is_separator(text) {
            // A separator wrapped onto the next line, which its dashes close.
            let last = if !fenced_end(text) && ls.get(i + 1).is_some_and(|l| fenced_end(l.2)) {
                i + 1
            } else {
                i
            };
            let below = (last + 1..ls.len()).find(|&k| !ls[k].2.trim().is_empty());
            let run = below.and_then(|k| header_run(&ls, k));
            open(
                &mut out,
                EmbeddedSegment {
                    start,
                    end: body.len(),
                    header_end: run.map_or(ls[last].1, |end| ls[end - 1].1),
                    kind,
                },
            );
            headed = true;
            i = run.unwrap_or(last + 1);
            continue;
        }
        if let Some(end) = header_run(&ls, i) {
            let first = header_block_start(&ls, i);
            // A separator with only its message's sender and date lines under
            // it, above these fields: the fields are that message's header.
            if let Some(last) = out
                .last_mut()
                .filter(|s| s.kind != SegmentKind::Quote)
                .filter(|s| body[s.header_end..ls[first].0].trim().is_empty())
            {
                last.header_end = ls[end - 1].1;
                i = end;
                continue;
            }
            let kind = if ls[first..end]
                .iter()
                .any(|l| l.2.to_ascii_lowercase().contains("forward"))
            {
                SegmentKind::Forward
            } else {
                SegmentKind::Reply
            };
            open(
                &mut out,
                EmbeddedSegment {
                    start: ls[first].0,
                    end: body.len(),
                    header_end: ls[end - 1].1,
                    kind,
                },
            );
            headed = true;
            i = end;
            continue;
        }
        if headed {
            i += 1;
            continue;
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
