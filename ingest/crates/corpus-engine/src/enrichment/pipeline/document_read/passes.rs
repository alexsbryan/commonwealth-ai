// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reader of a declaration (svrn/docs/specs/ONTOLOGY_METHOD.md §Reading):
//! a fixed plan of small closed questions generated from the contract, each
//! answered as a distribution over single-token labels in one forward pass
//! through ingest's census funnel (`decision_call`). It replaced a one-shot
//! reader that asked the local model to find, label, name and cite at once and
//! failed at it (stage label .43, party .24-.44; crm-proof loops 7-12).
//!
//! **Locate** asks every non-empty line, off the document prefilled with its
//! numbered lines, which declared claim kind it states; consecutive lines of
//! one kind are one statement of at most [`MAX_STATEMENT_LINES`]. **Choose**
//! asks each closed-valued field of the claim and of its subject the
//! one-attribute question RESOLVE's READ asks (`choice_question`), over the
//! folded body it was measured on. A field this plan cannot ask yet (a
//! reference, a quantity, a time, open text) is unknown with that reason,
//! never guessed. The answers assemble into the `{"documents": [...]}`
//! envelope that parsing, validation and the section cache read.
//!
//! No field's read precision is declared yet, so its argmax decides, as
//! RESOLVE's Ring 0 does, so it can be measured; "none of them" or a refused
//! call leaves the field unknown with its distribution in the reason.

use std::collections::BTreeMap;
use std::ops::Range;

use oicp_types::forced_choice;
use serde_json::{json, Value};
use tracing::{debug, info};

use super::{DocumentReadClaim, DocumentReadField, DocumentReadOutcome, DocumentReadStatus};
use crate::enrichment::atlas::resolve_records::{
    choice_question, decision_call, ClosedAttr, LABELS, NONE,
};
use crate::enrichment::atlas::SourceDocument;
use crate::enrichment::ontology::{AttrDecl, OntologyPolicies, OntologyTypeDecl, TypeIndex};
use crate::enrichment::pipeline::types::{ChapterInput, ChatPrompt};
use crate::error::{Error, Result};
use crate::InferenceFn;

const LOCATE_SYSTEM: &str = include_str!("passes_locate_prompt.md");

/// A statement is one kind and at most this many verified lines
/// (ONTOLOGY_METHOD §Reading).
pub(super) const MAX_STATEMENT_LINES: usize = 3;

const LOCATE_PHASE: &str = "document_passes_locate";
const CHOOSE_PHASE: &str = "document_passes_choose";

/// Read every supplied document of `chapter` by the plan the contract
/// generates, returning the envelope `parse_response` reads.
pub async fn read(
    chapter: &ChapterInput,
    policies: &OntologyPolicies,
    infer: &InferenceFn,
) -> Result<String> {
    let plan = Plan::of(policies);
    if plan.kinds.is_empty() {
        return Err(Error::InvalidInput(
            "the passes reader found no eligible declared claim kind to locate".into(),
        ));
    }
    let mut documents = Vec::with_capacity(chapter.source_documents.len());
    let mut calls = 0u32;
    for document in &chapter.source_documents {
        let (outcome, n) = read_document(document, &plan, infer).await;
        calls += n;
        documents.push(outcome);
    }
    info!(
        chapter = %chapter.chapter_id,
        documents = documents.len(),
        claims = documents.iter().map(|d| d.claims.len()).sum::<usize>(),
        calls,
        "document_read/passes: chapter read"
    );
    serde_json::to_string(&json!({ "documents": documents })).map_err(|error| {
        Error::Serialization(format!(
            "passes reader envelope cannot be serialized: {error}"
        ))
    })
}

/// What the reader asks, generated from the contract alone: a pure function
/// of it, so renaming every type and attribute renames the questions and
/// changes nothing else (ONTOLOGY_METHOD invariant 1).
pub(super) struct Plan<'a> {
    pub(super) kinds: Vec<KindPlan<'a>>,
}

pub(super) struct KindPlan<'a> {
    pub(super) decl: &'a OntologyTypeDecl,
    pub(super) subject: &'a OntologyTypeDecl,
    pub(super) fields: Vec<FieldPlan<'a>>,
    pub(super) subject_fields: Vec<FieldPlan<'a>>,
}

/// One non-derived field: asked as one forced choice, or unknown for a
/// reason this plan names.
pub(super) enum FieldPlan<'a> {
    Choose(ClosedAttr),
    Unasked(&'a AttrDecl),
}

impl<'a> Plan<'a> {
    pub(super) fn of(policies: &'a OntologyPolicies) -> Self {
        let index = TypeIndex::from_policies(policies);
        let fields = |attrs: Vec<&'a AttrDecl>| -> Vec<FieldPlan<'a>> {
            attrs
                .into_iter()
                .filter(|attr| attr.derived.is_none())
                .map(|attr| match ClosedAttr::of(attr) {
                    Some(closed) => FieldPlan::Choose(closed),
                    None => FieldPlan::Unasked(attr),
                })
                .collect()
        };
        let kinds = policies
            .shape
            .types
            .iter()
            .filter(|ty| super::schema::eligible_claim(ty, policies))
            .filter_map(|decl| {
                let subject = policies.type_decl(decl.subject.as_deref()?)?;
                Some(KindPlan {
                    decl,
                    subject,
                    fields: fields(index.extracted_attributes(&decl.name)),
                    subject_fields: fields(super::schema::subject_read_attributes(
                        policies,
                        &index,
                        &subject.name,
                    )),
                })
            })
            .collect();
        Self { kinds }
    }
}

/// One non-empty line of a document body: its number from 1, where the raw
/// line starts, and the byte span of its trimmed text.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Line {
    pub(super) n: usize,
    raw_start: usize,
    pub(super) start: usize,
    pub(super) end: usize,
}

pub(super) fn lines(body: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let mut offset = 0;
    for raw in body.split('\n') {
        let text = raw.trim();
        if !text.is_empty() {
            let start = offset + (raw.len() - raw.trim_start().len());
            out.push(Line {
                n: out.len() + 1,
                raw_start: offset,
                start,
                end: start + text.len(),
            });
        }
        offset += raw.len() + 1;
    }
    out
}

/// The Locate question for `line`: the whole document with its lines
/// numbered (a prefix every line of the document shares), then the line and
/// the declared kinds, each with its description.
pub(super) fn locate_question(
    body: &str,
    lines: &[Line],
    line: &Line,
    plan: &Plan<'_>,
    labels: &[&str],
) -> ChatPrompt {
    let mut u = String::from("Document, its lines numbered:\n<<<\n");
    for l in lines {
        u.push_str(&format!(
            "{} {}\n",
            l.n,
            body[l.raw_start..l.end].trim_end()
        ));
    }
    u.push_str(&format!(
        ">>>\n\nLine {}: \"{}\"\n\nWhich kind of claim does line {} state?\n",
        line.n,
        &body[line.start..line.end],
        line.n
    ));
    for (kind, label) in plan.kinds.iter().zip(labels) {
        u.push_str(&format!("{label} {}", kind.decl.name));
        if !kind.decl.description.is_empty() {
            u.push_str(&format!(" ({})", kind.decl.description));
        }
        u.push('\n');
    }
    u.push_str(&format!("{NONE} none of them\nAnswer with its letter."));
    ChatPrompt::new(LOCATE_SYSTEM, u)
        .with_response_schema("read", forced_choice::schema(labels))
        .with_phase_id(LOCATE_PHASE)
        .with_temperature(0.0)
}

/// Runs of consecutive located lines of one kind, each cut into statements
/// of at most `MAX_STATEMENT_LINES`: (kind index, line indices).
pub(super) fn statements(located: &[Option<usize>]) -> Vec<(usize, Range<usize>)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < located.len() {
        let Some(kind) = located[i] else {
            i += 1;
            continue;
        };
        let mut j = i;
        while j < located.len() && located[j] == Some(kind) && j - i < MAX_STATEMENT_LINES {
            j += 1;
        }
        out.push((kind, i..j));
        i = j;
    }
    out
}

/// The most probable label's index and its probability. The census funnel
/// refuses a distribution that leaves an asked label out, so every label is in
/// `dist`.
fn argmax(labels: &[&str], dist: &BTreeMap<String, f64>) -> (usize, f64) {
    labels
        .iter()
        .enumerate()
        .fold((0, f64::NEG_INFINITY), |best, (i, label)| {
            let p = dist[*label];
            if p > best.1 {
                (i, p)
            } else {
                best
            }
        })
}

/// A line as a trace shows it: at most 160 characters, cut at a character.
fn excerpt(text: &str) -> &str {
    text.char_indices()
        .nth(160)
        .map_or(text, |(at, _)| &text[..at])
}

fn render(dist: &BTreeMap<String, f64>) -> String {
    dist.iter()
        .map(|(label, p)| format!("{label} {p:.2}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Locate, then Choose, for one document. Returns its outcome and the calls made.
async fn read_document(
    document: &SourceDocument,
    plan: &Plan<'_>,
    infer: &InferenceFn,
) -> (DocumentReadOutcome, u32) {
    let id = document.key().to_string();
    let raw = document.raw_body();
    let lines = lines(&raw);
    let outcome = |status, reason: String| DocumentReadOutcome {
        document_id: id.clone(),
        status,
        reason: Some(reason),
        claims: Vec::new(),
        refused: Vec::new(),
    };
    if lines.is_empty() {
        return (
            outcome(
                DocumentReadStatus::NothingApplicable,
                "the document has no text".into(),
            ),
            0,
        );
    }
    let labels: Vec<&str> = LABELS[..plan.kinds.len()]
        .iter()
        .copied()
        .chain([NONE])
        .collect();
    let mut calls = 0u32;
    let mut refused = 0usize;
    let mut located = Vec::with_capacity(lines.len());
    for line in &lines {
        let prompt = locate_question(&raw, &lines, line, plan, &labels);
        calls += 1;
        let item = format!("line {}", line.n);
        match decision_call(infer, &prompt, &labels, &id, &item).await {
            Ok(dist) => {
                let (best, p) = argmax(&labels, &dist);
                let kind = (best < plan.kinds.len()).then_some(best);
                // Every line's answer, located or not, so a missed statement
                // shows how near it came (crm-proof baseline, 2026-10-09).
                debug!(
                    document = %id,
                    line = line.n,
                    kind = kind.map_or("none", |k| plan.kinds[k].decl.name.as_str()),
                    p,
                    dist = %render(&dist),
                    text = %excerpt(&raw[line.start..line.end]),
                    "document_read/passes: located"
                );
                located.push(kind);
            }
            Err(refusal) => {
                debug!(document = %id, line = line.n, ?refusal, "document_read/passes: locate refused");
                refused += 1;
                located.push(None);
            }
        }
    }
    if refused == lines.len() {
        return (
            outcome(
                DocumentReadStatus::CouldNotJudge,
                format!("every Locate call ({refused}) was refused"),
            ),
            calls,
        );
    }
    let found = statements(&located);
    debug!(document = %id, lines = lines.len(), refused, statements = found.len(), "document_read/passes: statements");
    if found.is_empty() {
        return (
            outcome(
                DocumentReadStatus::NothingApplicable,
                format!(
                    "no line of {} was located as a declared claim kind ({refused} refused)",
                    lines.len()
                ),
            ),
            calls,
        );
    }
    let folded = Folded::of(&raw);
    let mut claims = Vec::with_capacity(found.len());
    for (kind, span) in &found {
        let kind = &plan.kinds[*kind];
        let (first, last) = (&lines[span.start], &lines[span.end - 1]);
        let evidence = raw[first.start..last.end].to_string();
        // The statement's own lines, never its place among what was located:
        // RESOLVE keys a statement by its document, span and this reference.
        let local_ref = format!("l{}-{}", first.n, last.n);
        let at = folded.span(first.start..last.end);
        let ask = Ask {
            infer,
            document: &id,
            statement: &local_ref,
            body: &folded.text,
            at,
            evidence: &evidence,
        };
        let mut fields = BTreeMap::new();
        for field in &kind.fields {
            let (name, read) = ask.field(kind.decl, field, &mut calls).await;
            fields.insert(name, read);
        }
        let mut subject_fields = BTreeMap::new();
        for field in &kind.subject_fields {
            let (name, read) = ask.field(kind.subject, field, &mut calls).await;
            subject_fields.insert(name, read);
        }
        claims.push(DocumentReadClaim {
            kind: kind.decl.name.clone(),
            content: folded.text[folded.span(first.start..last.end)].to_string(),
            subject_type: kind.subject.name.clone(),
            subject_name: format!("{} {local_ref}", kind.subject.name),
            subject_local_ref: local_ref,
            speaker: None,
            evidence,
            fields,
            subject_fields,
        });
    }
    (
        DocumentReadOutcome {
            document_id: id,
            status: DocumentReadStatus::Read,
            reason: None,
            claims,
            refused: Vec::new(),
        },
        calls,
    )
}

/// One statement being asked: where it is in the folded body, and the raw
/// lines that are its evidence.
struct Ask<'q> {
    infer: &'q InferenceFn,
    document: &'q str,
    statement: &'q str,
    body: &'q str,
    at: Range<usize>,
    evidence: &'q str,
}

impl Ask<'_> {
    async fn field(
        &self,
        owner: &OntologyTypeDecl,
        field: &FieldPlan<'_>,
        calls: &mut u32,
    ) -> (String, DocumentReadField) {
        let attr = match field {
            FieldPlan::Unasked(attr) => {
                return (
                    attr.name.clone(),
                    DocumentReadField::Unknown {
                        reason: format!(
                        "not asked: the passes reader asks closed-valued fields only (`{}` is {})",
                        attr.name,
                        attr.family.key()
                    ),
                    },
                )
            }
            FieldPlan::Choose(attr) => attr,
        };
        let labels: Vec<&str> = LABELS[..attr.values.len()]
            .iter()
            .copied()
            .chain([NONE])
            .collect();
        let prompt = choice_question(
            &owner.name,
            &owner.description,
            attr,
            &labels,
            self.body,
            self.at.clone(),
            CHOOSE_PHASE,
        );
        *calls += 1;
        let read = match decision_call(self.infer, &prompt, &labels, self.document, self.statement)
            .await
        {
            Err(refusal) => DocumentReadField::Unknown {
                reason: format!("refused: {refusal:?}"),
            },
            Ok(dist) => match argmax(&labels, &dist) {
                (best, p) if best < attr.values.len() => {
                    let value = &attr.values[best];
                    debug!(document = self.document, statement = self.statement, field = %attr.name, %value, p, dist = %render(&dist), "document_read/passes: chose");
                    DocumentReadField::Supported {
                        value: Value::String(value.clone()),
                        evidence: self.evidence.to_string(),
                    }
                }
                (_, p) => {
                    debug!(document = self.document, statement = self.statement, field = %attr.name, p, dist = %render(&dist), "document_read/passes: chose none of the values");
                    DocumentReadField::Unknown {
                        reason: format!("the reader chose none of the values: {}", render(&dist)),
                    }
                }
            },
        };
        (attr.name.clone(), read)
    }
}

/// The body whitespace-folded as RESOLVE reads it (`SourceDocument::body`),
/// with each word's raw span, so a statement located on raw lines is asked
/// over the folded text its question was measured on.
struct Folded {
    text: String,
    /// (raw start, raw end, folded start) per word.
    words: Vec<(usize, usize, usize)>,
}

impl Folded {
    fn of(raw: &str) -> Self {
        let mut text = String::with_capacity(raw.len());
        let mut words = Vec::new();
        for word in raw.split_whitespace() {
            // A word is a subslice of `raw`: its offset is exact, never searched for.
            let start = word.as_ptr() as usize - raw.as_ptr() as usize;
            if !text.is_empty() {
                text.push(' ');
            }
            words.push((start, start + word.len(), text.len()));
            text.push_str(word);
        }
        Self { text, words }
    }

    /// The folded span of the words a raw span covers.
    fn span(&self, raw: Range<usize>) -> Range<usize> {
        let inside: Vec<&(usize, usize, usize)> = self
            .words
            .iter()
            .filter(|(s, e, _)| *s >= raw.start && *e <= raw.end)
            .collect();
        match (inside.first(), inside.last()) {
            (Some(first), Some(last)) => first.2..last.2 + (last.1 - last.0),
            _ => 0..0,
        }
    }
}

/// What the plan asks, rendered on a fixed sample document: a change to any
/// question's wording changes this value, and so the contract fingerprint
/// that keys cached reads (`schema::contract_value`).
pub(super) fn contract_value(policies: &OntologyPolicies) -> Value {
    let plan = Plan::of(policies);
    let body = "first line\nsecond line";
    let lines = lines(body);
    let labels: Vec<&str> = LABELS[..plan.kinds.len().min(LABELS.len())]
        .iter()
        .copied()
        .chain([NONE])
        .collect();
    let locate = locate_question(body, &lines, &lines[0], &plan, &labels);
    let probe = ClosedAttr {
        name: "attribute".into(),
        description: String::new(),
        values: vec!["value".into()],
    };
    let choose = choice_question("type", "", &probe, &["A", NONE], body, 0..10, CHOOSE_PHASE);
    json!({
        "reader": "passes",
        "max_statement_lines": MAX_STATEMENT_LINES,
        "locate": [locate.system, locate.user],
        "choose": [choose.system, choose.user],
    })
}

#[cfg(test)]
#[path = "passes_tests.rs"]
mod tests;
