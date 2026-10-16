// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mention (ONTOLOGY_METHOD §Reading): per document, the particulars of each
//! entity type a read reference field targets and no exhaustive declared
//! source covers (a metadata source is never exhaustive: a header names who
//! wrote, not everyone the text names; a table source would be). Each asked
//! line is one forced choice over those types and "none"; a line that names
//! one is then pointed at (`point::point_span`), so a mention's words are the
//! line's own by construction. A mention is a proposal for Pick and never a
//! record by itself. Of a type RESOLVE decides the mentions of
//! (`resolution_records::decides_mentions`), a mention naming no record the
//! document's fields list is also a particular (`particulars`): it reaches
//! RESOLVE as a statement, which links it to a record or opens one.

use std::collections::BTreeMap;

use oicp_types::forced_choice;
use serde_json::{json, Value};
use tracing::debug;

use super::about::local_ref;
use super::passes::{argmax, excerpt, numbered, render, Line};
use super::pick::unlisted_mentions;
use super::point::{point_span, Pointing};
use super::schema::subject_read_attributes;
use super::{DocumentReadField, DocumentReadParticular};
use crate::enrichment::atlas::resolution_records::decides_mentions;
use crate::enrichment::atlas::resolve_records::{decision_wave, LABELS, NONE};
use crate::enrichment::atlas::SourceDocument;
use crate::enrichment::ontology::{OntologyPolicies, OntologyTypeDecl, TypeIndex};
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::InferenceFn;

const MENTION_SYSTEM: &str = include_str!("passes_mention_prompt.md");
pub(super) const MENTION_PHASE: &str = "document_passes_mention";

/// One particular a line names: its declared type, the line, and the words.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Mention {
    pub(super) of: String,
    pub(super) line: usize,
    pub(super) text: String,
}

/// The Mention question for `line`: the document with its lines numbered,
/// the line, and the types, each with its description.
pub(super) fn mention_question(
    facts: &str,
    body: &str,
    lines: &[Line],
    line: &Line,
    targets: &[&OntologyTypeDecl],
    labels: &[&str],
) -> ChatPrompt {
    let mut u = format!("{facts}{}", numbered(body, lines));
    u.push_str(&format!(
        "\n\nLine {}: \"{}\"\n\nWhich kind of thing does line {} name a particular one of?\n",
        line.n,
        &body[line.start..line.end],
        line.n
    ));
    for (t, label) in targets.iter().zip(labels) {
        u.push_str(&format!("{label} {}", t.name));
        if !t.description.is_empty() {
            u.push_str(&format!(" ({})", t.description));
        }
        u.push('\n');
    }
    u.push_str(&format!("{NONE} none of them\nAnswer with its letter."));
    ChatPrompt::new(MENTION_SYSTEM, u)
        .with_response_schema("read", forced_choice::schema(labels))
        .with_phase_id(MENTION_PHASE)
        .with_temperature(0.0)
}

/// What one document's asked lines mention of `targets`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn mentions(
    infer: &InferenceFn,
    facts: &str,
    document: &str,
    body: &str,
    lines: &[Line],
    asked: &[bool],
    targets: &[&OntologyTypeDecl],
    calls: &mut u32,
) -> Vec<Mention> {
    let mut out: Vec<Mention> = Vec::new();
    if targets.is_empty() {
        return out;
    }
    let labels: Vec<&str> = LABELS[..targets.len().min(LABELS.len())]
        .iter()
        .copied()
        .chain([NONE])
        .collect();
    let mut refused = 0usize;
    // Which type each line names needs no other line's answer: one wave. The
    // pointing that follows a named line depends on that answer, so it is
    // asked after, line by line.
    let asking: Vec<&Line> = lines
        .iter()
        .zip(asked)
        .filter(|(_, a)| **a)
        .map(|(l, _)| l)
        .collect();
    let prompts: Vec<_> = asking
        .iter()
        .map(|line| mention_question(facts, body, lines, line, targets, &labels))
        .collect();
    let items: Vec<String> = asking
        .iter()
        .map(|line| format!("line {}", line.n))
        .collect();
    *calls += prompts.len() as u32;
    let answers = decision_wave(
        infer,
        &prompts,
        &vec![labels.clone(); prompts.len()],
        document,
        &items,
    )
    .await;
    for ((line, item), answer) in asking.into_iter().zip(&items).zip(answers) {
        let dist = match answer {
            Ok(d) => d,
            Err(refusal) => {
                debug!(
                    document,
                    line = line.n,
                    ?refusal,
                    "document_read/mention: refused"
                );
                refused += 1;
                continue;
            }
        };
        let (best, p) = argmax(&labels, &dist);
        let Some(target) = targets.get(best).filter(|_| best < labels.len() - 1) else {
            continue;
        };
        let text = &body[line.start..line.end];
        let pointing = Pointing {
            infer,
            facts,
            head: format!(
                "Line {}: \"{}\"\nKind of thing: {}{}",
                line.n,
                text,
                target.name,
                if target.description.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", target.description)
                }
            ),
            what: format!("the name of the {}", target.name),
            phase: MENTION_PHASE,
            document,
            item,
        };
        match point_span(&pointing, text, calls).await {
            Ok(Some(span)) => {
                let words = text[span].to_string();
                debug!(document, line = line.n, of = %target.name, p, dist = %render(&dist), words = %excerpt(&words), "document_read/mention: named");
                let m = Mention {
                    of: target.name.clone(),
                    line: line.n,
                    text: words,
                };
                if !out.iter().any(|o| o.of == m.of && o.text == m.text) {
                    out.push(m);
                }
            }
            Ok(None) => {
                debug!(document, line = line.n, of = %target.name, "document_read/mention: no words pointed at");
            }
            Err(refusal) => {
                debug!(document, line = line.n, %refusal, "document_read/mention: pointing refused");
                refused += 1;
            }
        }
    }
    debug!(
        document,
        mentions = out.len(),
        refused,
        "document_read/mention: document"
    );
    out
}

/// The particulars `mentions` give each of `targets` whose mentions RESOLVE
/// decides: one per mention a reference would read by its words alone, naming
/// no record the document's fields list or exclude (`pick::unlisted_mentions`,
/// Pick's own rule). Its evidence is the whole line, its statement the words
/// on it, and it reads no field: each the type's particulars name is unknown,
/// so identity its text does not state stays absent.
pub(super) fn particulars(
    document: &SourceDocument,
    policies: &OntologyPolicies,
    body: &str,
    lines: &[Line],
    targets: &[&OntologyTypeDecl],
    mentions: &[Mention],
) -> Vec<DocumentReadParticular> {
    let index = TypeIndex::from_policies(policies);
    let mut out = Vec::new();
    for t in targets.iter().filter(|t| decides_mentions(&index, &t.name)) {
        for (n, words) in unlisted_mentions(document, policies, t, mentions) {
            let line = &lines[n - 1];
            let reference = local_ref(n, &words);
            debug!(document = %document.key(), of = %t.name, particular = %reference, "document_read/mention: a mention reaches RESOLVE as a particular");
            let subject_fields: BTreeMap<String, DocumentReadField> =
                subject_read_attributes(policies, &index, &t.name)
                    .into_iter()
                    .map(|a| {
                        let reason = "a mention is its words; no field of it is read".into();
                        (a.name.clone(), DocumentReadField::Unknown { reason })
                    })
                    .collect();
            out.push(DocumentReadParticular {
                subject_type: t.name.clone(),
                subject_name: format!("{} {reference}", t.name),
                subject_local_ref: reference,
                evidence: body[line.start..line.end].to_string(),
                subject_fields,
                words: Some(words),
            });
        }
    }
    out
}

/// The Mention question on a fixed sample, for the read contract's fingerprint.
pub(super) fn contract_sample(body: &str, lines: &[Line]) -> Value {
    let probe = OntologyTypeDecl {
        name: "type".into(),
        ..Default::default()
    };
    let q = mention_question("", body, lines, &lines[0], &[&probe], &["A", NONE]);
    json!([q.system, q.user])
}

#[cfg(test)]
#[path = "mention_tests.rs"]
mod tests;
