// SPDX-License-Identifier: AGPL-3.0-or-later
//! Lines code can classify by structure alone, kept out of Locate
//! (ONTOLOGY_METHOD §Reading). One class, the quote: a line a document
//! carries from another dated earlier by the declared clock
//! (`change.document.date`), as a reply carries the message it answers.
//! Repetition is not that evidence (review 2026-10-10, note 2347f4c6): a
//! tracker writes the same act in the same words in every thread, an author
//! opens every message alike, and each of those lines is read. A line shows
//! it is carried in one of two ways, neither naming a format:
//!
//! - a **passage**: it and a line next to it are, in the same order, two
//!   neighbouring lines of one earlier document;
//! - a **mark**: it is a line of an earlier document, written under
//!   characters before its first letter or digit that end in the ones that
//!   line is written under and add some, as a client marks each line it
//!   quotes.
//!
//! A line's text is compared from its first to its last letter or digit, with
//! its whitespace folded; "next to" passes over lines with neither. "Earlier"
//! is the declared clock, never the order documents were stored in, so the
//! classes (and the records) are the same in any document order (C4). A
//! document with no readable date quotes nothing and is quoted by nothing.
//! Who wrote a line decides nothing: a passage an author sends twice, a
//! signature included, is read where it first appears. Each class is counted
//! where the reader traces a document.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::enrichment::atlas::resolution_documents::read_stamp;
use crate::enrichment::atlas::SourceDocument;
use crate::enrichment::ontology::{DocumentStamp, OntologyPolicies};

/// What shows a line is carried. Closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum LineClass {
    /// It and a line next to it are neighbouring lines, in that order, of a
    /// document dated earlier.
    Passage,
    /// It is a line of a document dated earlier, under a mark that line lacks.
    Marked,
}

impl LineClass {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Passage => "quoted passage",
            Self::Marked => "marked quote",
        }
    }
}

/// A line kept from Locate: what showed it carried, and the earliest other
/// document dated before it that holds it (ties by key), so a trace names
/// where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Carried<'a> {
    pub(super) class: LineClass,
    pub(super) from: &'a str,
}

/// The dated documents holding something, earliest first: (the rank of a
/// document's date, the document's index).
type Holders = BTreeSet<(u32, u32)>;

/// The corpus's lines, by where each text is stored: built once over every
/// document a run reads, before any is read.
#[derive(Debug, Default)]
pub struct LineClasses {
    /// Document key → its index (its place among the keys in order), and its
    /// date's rank among the corpus's distinct dates (equal dates, equal
    /// ranks); no rank without a date.
    docs: HashMap<String, (u32, Option<u32>)>,
    /// Each document's key, by index.
    keys: Vec<String>,
    /// A line's text → its id.
    texts: HashMap<String, u32>,
    /// A text's id → by the mark it is written under, the documents holding it.
    lines: HashMap<u32, BTreeMap<String, Holders>>,
    /// Two texts' ids, the second the next line after the first → the
    /// documents holding them so.
    passages: HashMap<(u32, u32), Holders>,
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

/// What a line is written under: its characters before its first letter or
/// digit, whitespace dropped.
pub(super) fn mark(text: &str) -> String {
    text.chars()
        .take_while(|c| !c.is_alphanumeric())
        .filter(|c| !c.is_whitespace())
        .collect()
}

impl LineClasses {
    /// Index `documents` (each once, by key) under `policies`' declared date
    /// field.
    pub fn of<'a>(
        documents: impl IntoIterator<Item = &'a SourceDocument>,
        policies: &OntologyPolicies,
    ) -> Self {
        let date_field = policies
            .change
            .document
            .as_ref()
            .and_then(|d| d.date.as_deref());
        let mut out = Self::default();
        // Each document once, by key, in key order: nothing below depends on
        // the order they were stored in (C4).
        let mut read: BTreeMap<&str, &SourceDocument> = BTreeMap::new();
        for d in documents {
            read.entry(d.key()).or_insert(d);
        }
        let read: Vec<(&SourceDocument, Option<String>)> = read
            .into_values()
            .map(|d| {
                let date = date_field.and_then(|f| {
                    read_stamp(d.metadata(), DocumentStamp::Date, f)
                        .map_err(|why| {
                            tracing::debug!(document = d.key(), %why, "document_read/line_classes: undated, so it quotes nothing and nothing quotes it");
                        })
                        .ok()
                });
                (d, date)
            })
            .collect();
        for (index, (d, _)) in read.iter().enumerate() {
            out.docs.insert(d.key().to_string(), (index as u32, None));
            out.keys.push(d.key().to_string());
        }
        let ranks: BTreeSet<&str> = read
            .iter()
            .filter_map(|(_, date)| date.as_deref())
            .collect();
        let ranks: HashMap<&str, u32> = ranks
            .into_iter()
            .enumerate()
            .map(|(rank, date)| (date, rank as u32))
            .collect();
        for (index, (d, date)) in read.iter().enumerate() {
            let Some(rank) = date.as_deref().map(|date| ranks[date]) else {
                continue;
            };
            if let Some(entry) = out.docs.get_mut(d.key()) {
                entry.1 = Some(rank);
            }
            let holder = (rank, index as u32);
            let mut previous = None;
            for line in d.raw_body().lines() {
                let Some(key) = line_key(line) else {
                    continue;
                };
                let next = out.texts.len() as u32;
                let id = *out.texts.entry(key).or_insert(next);
                out.lines
                    .entry(id)
                    .or_default()
                    .entry(mark(line))
                    .or_default()
                    .insert(holder);
                if let Some(previous) = previous {
                    out.passages
                        .entry((previous, id))
                        .or_default()
                        .insert(holder);
                }
                previous = Some(id);
            }
        }
        tracing::debug!(
            documents = out.docs.len(),
            dated = out.docs.values().filter(|(_, rank)| rank.is_some()).count(),
            texts = out.texts.len(),
            passages = out.passages.len(),
            "document_read/line_classes: indexed"
        );
        out
    }

    /// Each of `lines`, the lines of `document` in order, that is carried.
    pub(super) fn classes(&self, document: &str, lines: &[&str]) -> Vec<Option<Carried<'_>>> {
        let mut out = vec![None; lines.len()];
        let Some(&(me, Some(rank))) = self.docs.get(document) else {
            return out;
        };
        // Held earlier: the earliest document other than this one holding
        // it, when that one is dated before it.
        let earlier = |holders: &Holders| {
            holders
                .iter()
                .find(|(_, d)| *d != me)
                .filter(|(r, _)| *r < rank)
                .map(|(_, d)| *d)
        };
        let next_to = |a: Option<u32>, b: Option<u32>| match (a, b) {
            (Some(a), Some(b)) => self.passages.get(&(a, b)).and_then(earlier),
            _ => None,
        };
        let carried = |class, from: u32| Carried {
            class,
            from: &self.keys[from as usize],
        };
        // The lines with a key, in order: (index in `lines`, text id, mark).
        let keyed: Vec<(usize, Option<u32>, String)> = lines
            .iter()
            .enumerate()
            .filter_map(|(i, t)| line_key(t).map(|k| (i, self.texts.get(&k).copied(), mark(t))))
            .collect();
        for (j, (i, id, m)) in keyed.iter().enumerate() {
            let before = j.checked_sub(1).and_then(|p| keyed[p].1);
            let after = keyed.get(j + 1).and_then(|n| n.1);
            let passage = next_to(before, *id).or_else(|| next_to(*id, after));
            let marked = || {
                id.and_then(|id| self.lines.get(&id))?
                    .iter()
                    .filter(|(theirs, _)| m.len() > theirs.len() && m.ends_with(theirs.as_str()))
                    .find_map(|(_, holders)| earlier(holders))
            };
            out[*i] = passage
                .map(|from| carried(LineClass::Passage, from))
                .or_else(|| marked().map(|from| carried(LineClass::Marked, from)));
        }
        out
    }

    /// What keys a cached read of `documents`: each one's classified lines,
    /// numbered as the reader numbers them, so a read is redone when another
    /// document changes what it quotes.
    pub fn digest<'a>(&self, documents: impl IntoIterator<Item = &'a SourceDocument>) -> String {
        let mut out = String::new();
        for d in documents {
            out.push_str(d.key());
            let raw = d.raw_body();
            let lines = super::passes::lines(&raw);
            let texts: Vec<&str> = lines.iter().map(|l| &raw[l.start..l.end]).collect();
            for (line, carried) in lines.iter().zip(self.classes(d.key(), &texts)) {
                if let Some(carried) = carried {
                    out.push_str(&format!(" {}:{}", line.n, carried.class.label()));
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
