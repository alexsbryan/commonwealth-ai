// SPDX-License-Identifier: AGPL-3.0-or-later
//! Point (ONTOLOGY_METHOD §Reading): an open-valued field of a statement
//! (text, quantity, time) is answered as a span of the statement's own words,
//! or "not stated". The model points; it never writes the value. Each question
//! is a forced choice over words shown under single-token labels: at which
//! word the value starts (a window of words at a time, "none of these" moving
//! to the next), then at which it ends. So every span is the statement's own
//! text by construction, and code then reads the value out of it by the
//! field's declared family: text as written, a quantity as the one number its
//! words hold, a time as ISO 8601 when the words are a date code can read and
//! as written otherwise. A quantity whose words hold no number, or more than
//! one, is refused for that field alone, never guessed.
//!
//! [`point_span`] is the one pointing question; Mention (`mention.rs`) points
//! with it too.

use std::ops::Range;

use oicp_types::forced_choice;
use serde_json::{json, Value};
use tracing::debug;

use super::ask::Ask;
use super::passes::{argmax, render};
use super::DocumentReadField;
use crate::enrichment::atlas::precision::{Precision, SourcePrecision};
use crate::enrichment::atlas::resolve_records::{decision_call, marked_context, LABELS, NONE};
use crate::enrichment::ontology::{metadata_date, AttrDecl, AttrFamily, OntologyTypeDecl};
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::InferenceFn;

const POINT_SYSTEM: &str = include_str!("passes_point_prompt.md");
pub(super) const POINT_PHASE: &str = "document_passes_point";

/// The byte span of each whitespace-separated word of `text`.
pub(super) fn words(text: &str) -> Vec<Range<usize>> {
    text.split_whitespace()
        .map(|w| {
            // A word is a subslice of `text`: its offset is exact.
            let start = w.as_ptr() as usize - text.as_ptr() as usize;
            start..start + w.len()
        })
        .collect()
}

/// What one pointing question is about.
pub(super) struct Pointing<'a> {
    pub(super) infer: &'a InferenceFn,
    pub(super) facts: &'a str,
    /// What the question shows above the words (the attribute, the passage).
    pub(super) head: String,
    /// What is pointed at, as the question names it (`the price`).
    pub(super) what: String,
    pub(super) phase: &'static str,
    pub(super) document: &'a str,
    pub(super) item: &'a str,
}

fn question(
    facts: &str,
    head: &str,
    phase: &str,
    shown: &str,
    ask: &str,
    labels: &[&str],
) -> ChatPrompt {
    let user = format!("{facts}{head}\n\n{shown}{ask}\nAnswer with its letter.");
    ChatPrompt::new(POINT_SYSTEM, user)
        .with_response_schema("read", forced_choice::schema(labels))
        .with_phase_id(phase)
        .with_temperature(0.0)
}

/// Point at a run of `text`'s words: the start word, then the end word.
/// `Ok(None)` is "not stated": no word shown was chosen as the start. A call
/// that answers no distribution refuses the whole span.
pub(super) async fn point_span(
    p: &Pointing<'_>,
    text: &str,
    calls: &mut u32,
) -> Result<Option<Range<usize>>, String> {
    let all = words(text);
    let window = LABELS.len();
    let mut start = None;
    for (w, chunk) in all.chunks(window).enumerate() {
        let labels: Vec<&str> = LABELS[..chunk.len()]
            .iter()
            .copied()
            .chain([NONE])
            .collect();
        let mut shown = String::from("The words, each under a letter:\n");
        for (word, label) in chunk.iter().zip(&labels) {
            shown.push_str(&format!("{label} {}\n", &text[word.clone()]));
        }
        shown.push_str(&format!("{NONE} none of these words\n"));
        let prompt = question(
            p.facts,
            &p.head,
            p.phase,
            &shown,
            &format!("At which word does {} start?", p.what),
            &labels,
        );
        *calls += 1;
        let dist = decision_call(p.infer, &prompt, &labels, p.document, p.item)
            .await
            .map_err(|r| format!("{r:?}"))?;
        let (best, prob) = argmax(&labels, &dist);
        debug!(document = p.document, item = p.item, phase = p.phase, window = w, best, prob, dist = %render(&dist), "document_read/point: start");
        if best < chunk.len() {
            start = Some(w * window + best);
            break;
        }
    }
    let Some(start) = start else {
        return Ok(None);
    };
    let tail = &all[start..(start + window).min(all.len())];
    let end = if tail.len() == 1 {
        start
    } else {
        let labels: Vec<&str> = LABELS[..tail.len()].to_vec();
        let mut shown = format!(
            "It starts at the word \"{}\". The words from there, each under a letter:\n",
            &text[all[start].clone()]
        );
        for (word, label) in tail.iter().zip(&labels) {
            shown.push_str(&format!("{label} {}\n", &text[word.clone()]));
        }
        let prompt = question(
            p.facts,
            &p.head,
            p.phase,
            &shown,
            &format!("At which word does {} end?", p.what),
            &labels,
        );
        *calls += 1;
        let dist = decision_call(p.infer, &prompt, &labels, p.document, p.item)
            .await
            .map_err(|r| format!("{r:?}"))?;
        let (best, prob) = argmax(&labels, &dist);
        debug!(document = p.document, item = p.item, phase = p.phase, best, prob, dist = %render(&dist), "document_read/point: end");
        start + best
    };
    Ok(Some(all[start].start..all[end].end))
}

/// How a family's value is named in the question.
fn family_words(family: &AttrFamily) -> String {
    match family {
        AttrFamily::Text { .. } => "words of the statement".into(),
        AttrFamily::Quantity { unit: Some(u) } => format!("a number, in {u}"),
        AttrFamily::Quantity { unit: None } => "a number".into(),
        AttrFamily::Time { range: false } => "a point in time".into(),
        AttrFamily::Time { range: true } => "a span of time".into(),
        AttrFamily::Ref { of } => format!("one {of}"),
    }
}

/// The value the pointed words state, by the field's declared family, or why
/// code cannot read one.
pub(super) fn normalise(family: &AttrFamily, words: &str) -> Result<Value, String> {
    match family {
        AttrFamily::Text { .. } | AttrFamily::Ref { .. } => Ok(Value::String(words.to_string())),
        AttrFamily::Time { .. } => Ok(Value::String(
            metadata_date(words).unwrap_or_else(|| words.to_string()),
        )),
        AttrFamily::Quantity { .. } => {
            let numbers: Vec<f64> = words.split_whitespace().filter_map(number).collect();
            match numbers.as_slice() {
                [n] => serde_json::Number::from_f64(*n)
                    .map(Value::Number)
                    .ok_or_else(|| format!("{n} is not a finite number")),
                [] => Err(format!("the words {words:?} hold no number")),
                many => Err(format!(
                    "the words {words:?} hold {} numbers, so which is meant is not read",
                    many.len()
                )),
            }
        }
    }
}

/// The number one word writes in digits: an optional sign, digits grouped by
/// commas in threes, an optional decimal point; anything around it that is
/// neither a letter nor a digit (a currency sign, punctuation) is not part of
/// it. A word with a letter beside its digits writes no number code reads.
fn number(word: &str) -> Option<f64> {
    let core = word.trim_matches(|c: char| !c.is_alphanumeric());
    if core.is_empty()
        || !core
            .chars()
            .all(|c| c.is_ascii_digit() || c == ',' || c == '.')
    {
        return None;
    }
    let negative = word[..word.find(core)?].ends_with('-');
    let (whole, fraction) = match core.split_once('.') {
        Some((w, f)) if !f.contains('.') && !f.contains(',') && !f.is_empty() => (w, Some(f)),
        Some(_) => return None,
        None => (core, None),
    };
    let groups: Vec<&str> = whole.split(',').collect();
    let grouped = groups.len() == 1
        || (!groups[0].is_empty()
            && groups[0].len() <= 3
            && groups[1..].iter().all(|g| g.len() == 3));
    if !grouped || whole.is_empty() {
        return None;
    }
    let digits = format!(
        "{}{}",
        groups.concat(),
        fraction.map_or(String::new(), |f| format!(".{f}"))
    );
    let n: f64 = digits.parse().ok()?;
    Some(if negative { -n } else { n })
}

/// Point at `attr` in the statement `ask` holds.
pub(super) async fn point(
    ask: &Ask<'_>,
    owner: &OntologyTypeDecl,
    attr: &AttrDecl,
    calls: &mut u32,
) -> DocumentReadField {
    let described = |name: &str, d: &str| {
        if d.is_empty() {
            name.to_string()
        } else {
            format!("{name} ({d})")
        }
    };
    let head = format!(
        "Type: {}\nAttribute: {}, {}\n\nStatement, its words in [[ ]]: \"…{}…\"",
        described(&owner.name, &owner.description),
        described(&attr.name, &attr.description),
        family_words(&attr.family),
        marked_context(ask.body, ask.at.start, ask.at.end)
    );
    let pointing = Pointing {
        infer: ask.infer,
        facts: ask.facts,
        head,
        what: format!("the statement's {}", attr.name),
        phase: POINT_PHASE,
        document: ask.document,
        item: ask.statement,
    };
    let span = match point_span(&pointing, ask.evidence, calls).await {
        Err(refusal) => {
            return DocumentReadField::Unknown {
                reason: format!("refused: {refusal}"),
            }
        }
        Ok(None) => {
            debug!(document = ask.document, statement = ask.statement, field = %attr.name, "document_read/point: not stated");
            return DocumentReadField::Unknown {
                reason: "not stated: the reader pointed at none of the statement's words".into(),
            };
        }
        Ok(Some(span)) => span,
    };
    let words = &ask.evidence[span];
    match normalise(&attr.family, words) {
        Ok(value) => {
            debug!(document = ask.document, statement = ask.statement, field = %attr.name, %words, %value, "document_read/point: pointed");
            DocumentReadField::Supported {
                value,
                evidence: words.to_string(),
                by: Some(SourcePrecision::new("reader_point", Precision::Unmeasured)),
            }
        }
        Err(why) => {
            debug!(document = ask.document, statement = ask.statement, field = %attr.name, %words, %why, "document_read/point: refused");
            DocumentReadField::Unknown {
                reason: format!("refused: {why}"),
            }
        }
    }
}

/// The Point question on a fixed sample, for the read contract's fingerprint.
pub(super) fn contract_sample() -> Value {
    let start = question(
        "",
        "Type: type\nAttribute: attribute, words of the statement",
        POINT_PHASE,
        "The words, each under a letter:\nA word\n0 none of these words\n",
        "At which word does the statement's attribute start?",
        &["A", NONE],
    );
    json!([
        start.system,
        start.user,
        family_words(&AttrFamily::Quantity { unit: None }),
        family_words(&AttrFamily::Time { range: false }),
    ])
}

#[cfg(test)]
#[path = "point_tests.rs"]
mod tests;
