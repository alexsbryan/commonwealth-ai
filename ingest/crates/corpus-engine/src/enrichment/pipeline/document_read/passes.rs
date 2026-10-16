// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reader of a declaration (svrn/docs/specs/ONTOLOGY_METHOD.md §Reading):
//! a fixed plan of small closed questions generated from the contract, each
//! answered as a distribution over single-token labels in one forward pass
//! through ingest's census funnel (`decision_call`). It replaced a one-shot
//! reader that asked the local model to find, label, name and cite at once and
//! failed at it (stage label .43, party .24-.44; crm-proof loops 7-12).
//!
//! Every question about a document opens with its declared facts
//! (`prefill.rs`). **Locate** asks every line no line class keeps out
//! (`line_classes.rs`), off the document with its numbered lines, which
//! declared claim kind it states; consecutive lines of one kind are one
//! statement of at most [`MAX_STATEMENT_LINES`]. A kind a protocol rule names
//! is not located: it is asked of the whole document (**Act**, `act.rs`). **Mention** asks the same
//! lines which entity type a read reference targets they name, and points at
//! the words (`mention.rs`). Then per statement, each field of the claim and
//! of its subject by its declared family (`ask.rs`): **Choose** a closed value
//! (the one-attribute question RESOLVE's READ asks, `choice_question`),
//! **Point** at an open one (`point.rs`), **Pick** a reference among the
//! candidates code proposes (`pick.rs`). The answers assemble into the
//! `{"documents": [...]}` envelope that parsing, validation and the section
//! cache read.
//!
//! No field's read precision is declared yet, so its argmax decides, as
//! RESOLVE's Ring 0 does, so it can be measured; "none of them" or a refused
//! call leaves the field unknown with its distribution in the reason.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde_json::{json, Value};
use tracing::{debug, info};

use super::ask::Ask;
use super::folded::Folded;
use super::line_classes::{LineClass, LineClasses};
use super::locate::{locate, locate_question, statements, MAX_STATEMENT_LINES};
use super::{about, act, mention, pick, point, prefill};
use super::{DocumentReadClaim, DocumentReadOutcome, DocumentReadStatus};
use crate::enrichment::atlas::resolution_records::decides;
use crate::enrichment::atlas::resolve_records::{choice_question, ClosedAttr, LABELS, NONE};
use crate::enrichment::atlas::SourceDocument;
use crate::enrichment::ontology::{
    AttrDecl, AttrFamily, OntologyPolicies, OntologyTypeDecl, SourceDecl, TypeIndex,
};
use crate::enrichment::pipeline::types::ChapterInput;
use crate::error::{Error, Result};
use crate::InferenceFn;

pub(super) const CHOOSE_PHASE: &str = "document_passes_choose";

/// Read every supplied document of `chapter` by the plan the contract
/// generates, returning the envelope `parse_response` reads.
pub async fn read(
    chapter: &ChapterInput,
    policies: &OntologyPolicies,
    classes: &LineClasses,
    infer: &InferenceFn,
) -> Result<String> {
    match super::reader() {
        super::Reader::Window => super::window::read(chapter, policies, classes, infer).await,
        super::Reader::Waves => super::waves::read(chapter, policies, classes, infer).await,
        super::Reader::Passes => by_plan(chapter, policies, classes, infer).await,
    }
}

/// The passes reader itself, whatever `SOVEREIGN_READER` chooses.
pub(super) async fn by_plan(
    chapter: &ChapterInput,
    policies: &OntologyPolicies,
    classes: &LineClasses,
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
        let (outcome, n) = read_document(document, &plan, policies, classes, infer).await;
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
    /// Every type a read reference (a Pick field) targets, each once.
    pub(super) picked: Vec<&'a OntologyTypeDecl>,
    /// Every type whose identity RESOLVE decides (`resolution_records::
    /// decides`), each once, a statement kind about it or not: what About
    /// asks for. A kind's subjects first, in the kinds' order, then the rest
    /// in declaration order.
    pub(super) decided: Vec<&'a OntologyTypeDecl>,
    /// Per decided type, the fields that identify a particular of it
    /// (`schema::subject_read_attributes`), read of each one About finds.
    identifying: BTreeMap<&'a str, Vec<FieldPlan<'a>>>,
}

pub(super) struct KindPlan<'a> {
    pub(super) decl: &'a OntologyTypeDecl,
    pub(super) subject: &'a OntologyTypeDecl,
    pub(super) fields: Vec<FieldPlan<'a>>,
    pub(super) subject_fields: Vec<FieldPlan<'a>>,
    /// The fields asked of the whole document (`act::keys`); empty for a kind
    /// Locate asks line by line.
    pub(super) keys: Vec<String>,
}

/// One non-derived field, asked by the pass its declared family names.
pub(super) enum FieldPlan<'a> {
    /// A closed set of 1..=25 values: one forced choice over them.
    Choose(ClosedAttr),
    /// Open text, a quantity or a time: a span of the statement's words.
    /// Owned when composed rather than declared (a state's value,
    /// `schema::statement_attributes`).
    Point(Cow<'a, AttrDecl>),
    /// A reference: one choice among the candidates code proposes (owned for
    /// a relation's `to`).
    Pick(Cow<'a, AttrDecl>, &'a str),
    /// The subject of a claim kind (the first), when its type (the second)
    /// has a metadata source: one Pick among the records that source reads
    /// from the document, written as the readings of the identity fields (the
    /// third), which the statement's words rarely hold.
    Subject(&'a OntologyTypeDecl, &'a str, Vec<&'a AttrDecl>),
    /// A closed set wider than one forced choice shows: not asked, said why.
    Unasked(Cow<'a, AttrDecl>),
}

impl<'a> FieldPlan<'a> {
    fn of(attr: Cow<'a, AttrDecl>, policies: &'a OntologyPolicies) -> Self {
        if let Some(closed) = ClosedAttr::of(&attr) {
            return FieldPlan::Choose(closed);
        }
        match &attr.family {
            // The target's name, borrowed from its declaration (an owned
            // attribute cannot lend it): a reference to an undeclared type
            // is refused at parse, so the declaration is there.
            AttrFamily::Ref { of } => match policies.type_decl(of) {
                Some(target) => FieldPlan::Pick(attr, target.name.as_str()),
                None => FieldPlan::Unasked(attr),
            },
            AttrFamily::Text { values } if !values.is_empty() => FieldPlan::Unasked(attr),
            AttrFamily::Text { .. } | AttrFamily::Quantity { .. } | AttrFamily::Time { .. } => {
                FieldPlan::Point(attr)
            }
        }
    }

    /// The pass that asks it, as `recipe validate` names it.
    pub(super) fn pass(&self) -> Option<&'static str> {
        match self {
            FieldPlan::Choose(_) => Some("Choose"),
            FieldPlan::Point(_) => Some("Point"),
            FieldPlan::Pick(..) | FieldPlan::Subject(..) => Some("Pick"),
            FieldPlan::Unasked(_) => None,
        }
    }

    /// The fields it reads: one, or a subject's identity fields.
    pub(super) fn names(&self) -> Vec<&str> {
        match self {
            FieldPlan::Choose(c) => vec![&c.name],
            FieldPlan::Point(a) | FieldPlan::Pick(a, _) | FieldPlan::Unasked(a) => vec![&a.name],
            FieldPlan::Subject(_, _, keys) => keys.iter().map(|a| a.name.as_str()).collect(),
        }
    }
}

impl<'a> Plan<'a> {
    pub(super) fn of(policies: &'a OntologyPolicies) -> Self {
        let index = TypeIndex::from_policies(policies);
        let fields = |attrs: Vec<Cow<'a, AttrDecl>>| -> Vec<FieldPlan<'a>> {
            attrs
                .into_iter()
                .filter(|attr| attr.derived.is_none())
                .map(|attr| FieldPlan::of(attr, policies))
                .collect()
        };
        let kinds = policies
            .shape
            .types
            .iter()
            .filter(|ty| super::schema::read_kind(ty, policies))
            .filter_map(|decl| {
                let subject = policies.type_decl(decl.read_subject()?)?;
                let keys = super::schema::subject_read_attributes(policies, &index, &subject.name);
                let subject_fields = match &subject.source {
                    Some(SourceDecl::Metadata(_)) if !keys.is_empty() => {
                        vec![FieldPlan::Subject(decl, subject.name.as_str(), keys)]
                    }
                    _ => fields(keys.into_iter().map(Cow::Borrowed).collect()),
                };
                let fields = fields(super::schema::statement_attributes(&index, decl));
                let keys = act::keys(policies, &index, decl, subject, &fields);
                Some(KindPlan {
                    decl,
                    subject,
                    fields,
                    subject_fields,
                    keys,
                })
            })
            .collect::<Vec<KindPlan<'a>>>();
        let mut picked: Vec<&'a OntologyTypeDecl> = Vec::new();
        for kind in &kinds {
            for field in kind.fields.iter().chain(&kind.subject_fields) {
                if let FieldPlan::Pick(_, of) = field {
                    if let Some(target) = policies.type_decl(of) {
                        if !picked.iter().any(|m| m.name == target.name) {
                            picked.push(target);
                        }
                    }
                }
            }
        }
        let mut decided: Vec<&'a OntologyTypeDecl> = Vec::new();
        for t in kinds
            .iter()
            .map(|k| k.subject)
            .chain(policies.shape.types.iter())
        {
            if decides(&index, &t.name) && !decided.iter().any(|d| d.name == t.name) {
                decided.push(t);
            }
        }
        let identifying = decided
            .iter()
            .map(|t| {
                let keys = super::schema::subject_read_attributes(policies, &index, &t.name);
                (
                    t.name.as_str(),
                    fields(keys.into_iter().map(Cow::Borrowed).collect()),
                )
            })
            .collect();
        Self {
            kinds,
            picked,
            decided,
            identifying,
        }
    }

    /// The fields that identify a particular of the decided type `of`. Every
    /// type in `decided` has an entry, built from it in `of`; any other is a
    /// caller's error, never "no fields".
    pub(super) fn identifying(&self, of: &str) -> &[FieldPlan<'a>] {
        &self.identifying[of]
    }

    /// The kinds Locate asks line by line: every kind but an act's.
    pub(super) fn located(&self) -> Vec<usize> {
        (0..self.kinds.len())
            .filter(|&k| self.kinds[k].keys.is_empty())
            .collect()
    }

    /// The subject types statements of `kinds` pick (`FieldPlan::Subject`),
    /// each once.
    pub(super) fn subjects_for(
        &self,
        kinds: &[usize],
        policies: &'a OntologyPolicies,
    ) -> Vec<&'a OntologyTypeDecl> {
        let mut out: Vec<&'a OntologyTypeDecl> = Vec::new();
        for k in kinds {
            for f in &self.kinds[*k].subject_fields {
                if let FieldPlan::Subject(_, of, _) = f {
                    if let Some(t) = policies.type_decl(of) {
                        if !out.iter().any(|o| o.name == t.name) {
                            out.push(t);
                        }
                    }
                }
            }
        }
        out
    }

    /// The types the Pick fields of statements of `kinds` target.
    pub(super) fn picked_for(&self, kinds: &[usize]) -> Vec<&'a OntologyTypeDecl> {
        self.picked
            .iter()
            .copied()
            .filter(|t| {
                kinds.iter().any(|k| {
                    let kind = &self.kinds[*k];
                    kind.fields
                        .iter()
                        .chain(&kind.subject_fields)
                        .any(|f| matches!(f, FieldPlan::Pick(_, of) if *of == t.name))
                })
            })
            .collect()
    }
}

/// Whether Mention asks for `target`: a table source holds a type whole, so
/// the text names none it lacks; a metadata source names only who took part
/// (ONTOLOGY_METHOD §Reading: "no exhaustive source covers").
pub(super) fn mentioned(target: &OntologyTypeDecl) -> bool {
    !matches!(target.source, Some(SourceDecl::Table(_)))
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

/// The document with its lines numbered: the prefix every line question of a
/// document shares (Locate's, Mention's).
pub(super) fn numbered(body: &str, lines: &[Line]) -> String {
    let mut u = String::from("Document, its lines numbered:\n<<<\n");
    for l in lines {
        u.push_str(&format!(
            "{} {}\n",
            l.n,
            body[l.raw_start..l.end].trim_end()
        ));
    }
    u.push_str(">>>");
    u
}

/// The most probable label's index and its probability. The census funnel
/// refuses a distribution that leaves an asked label out, so every label is in
/// `dist`.
pub(super) fn argmax(labels: &[&str], dist: &BTreeMap<String, f64>) -> (usize, f64) {
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
pub(super) fn excerpt(text: &str) -> &str {
    text.char_indices()
        .nth(160)
        .map_or(text, |(at, _)| &text[..at])
}

pub(super) fn render(dist: &BTreeMap<String, f64>) -> String {
    dist.iter()
        .map(|(label, p)| format!("{label} {p:.2}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Locate, then Choose, for one document. Returns its outcome and the calls made.
async fn read_document(
    document: &SourceDocument,
    plan: &Plan<'_>,
    policies: &OntologyPolicies,
    classes: &LineClasses,
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
        particulars: Vec::new(),
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
    let facts = prefill::facts(document, policies);
    let mut calls = 0u32;
    let mut classed: BTreeMap<LineClass, usize> = BTreeMap::new();
    let mut asked_lines = vec![true; lines.len()];
    let mut headers = vec![false; lines.len()];
    let mut asking: Vec<usize> = Vec::with_capacity(lines.len());
    for (i, (line, carried)) in lines
        .iter()
        .zip(classes.classes(&id, &raw, &lines))
        .enumerate()
    {
        let text = &raw[line.start..line.end];
        if let Some(carried) = carried {
            debug!(document = %id, line = line.n, class = carried.class.label(), from = carried.from, text = %excerpt(text), "document_read/passes: line not asked");
            *classed.entry(carried.class).or_default() += 1;
            asked_lines[line.n - 1] = false;
            headers[i] = carried.class == LineClass::Field;
            continue;
        }
        asking.push(i);
    }
    let located = locate((infer, &facts, &id, &raw), &lines, &asking, plan).await;
    calls += located.asked as u32;
    let refused = located.refused;
    if located.asked > 0 && refused == located.asked {
        return (
            outcome(
                DocumentReadStatus::CouldNotJudge,
                format!("every Locate call ({refused}) was refused"),
            ),
            calls,
        );
    }
    let found = statements(&located.kinds);
    debug!(document = %id, lines = lines.len(), refused, classed = ?classed, statements = found.len(), "document_read/passes: statements");
    let folded = Folded::of(&raw);
    let view = numbered(&raw, &lines);
    let doc = about::Doc {
        infer,
        facts: &facts,
        document: &id,
        body: &raw,
        lines: &lines,
        headers: &headers,
        view: &view,
    };
    let abouts = about::about(&doc, &plan.decided, &mut calls).await;
    let acts = act::acts(&doc, plan, &abouts, &asked_lines, &mut calls).await;
    if found.is_empty() && acts.is_empty() && abouts.iter().all(|a| a.particulars.is_empty()) {
        return (
            outcome(
                DocumentReadStatus::NothingApplicable,
                format!(
                    "no line of {} was located as a declared claim kind and no act was read \
                     ({refused} refused, {} not asked: {classed:?})",
                    lines.len(),
                    classed.values().sum::<usize>()
                ),
            ),
            calls,
        );
    }
    // The kinds located or read as acts, and those about a type a particular
    // was found of: its identifying fields are asked as theirs are, from the
    // same candidates.
    let mut kinds: Vec<usize> = found.iter().map(|(k, _)| *k).collect();
    kinds.extend(acts.iter().map(|a| a.kind));
    for (of, a) in plan.decided.iter().zip(&abouts) {
        if !a.particulars.is_empty() {
            kinds.extend((0..plan.kinds.len()).filter(|&k| plan.kinds[k].subject.name == of.name));
        }
    }
    let picked = plan.picked_for(&kinds);
    let targets: Vec<&OntologyTypeDecl> = picked.iter().copied().filter(|t| mentioned(t)).collect();
    let mentions = mention::mentions(
        infer,
        &facts,
        &id,
        &raw,
        &lines,
        &asked_lines,
        &targets,
        &mut calls,
    )
    .await;
    let mut candidates = pick::per_type(document, policies, &picked, &mentions);
    // A picked subject is one of the records its source reads from the
    // document; a mention of it names no record, so none is asked for it.
    for t in plan.subjects_for(&kinds, policies) {
        candidates
            .entry(t.name.clone())
            .or_insert_with(|| pick::candidates(document, policies, t, &[]));
    }
    let said: Vec<(&str, &str, (usize, usize))> = found
        .iter()
        .map(|(kind, span)| {
            let (first, last) = (&lines[span.start], &lines[span.end - 1]);
            (
                plan.kinds[*kind].subject.name.as_str(),
                &folded.text[folded.span(first.start..last.end)],
                (first.n, last.n),
            )
        })
        .collect();
    let particular_of = about::particular_of(&doc, &plan.decided, &abouts, &said, &mut calls).await;
    let identities = about::identities(&doc, plan, &abouts, &folded, &candidates, &mut calls).await;
    let mut claims = Vec::with_capacity(found.len());
    for ((kind, span), &particular) in found.iter().zip(&particular_of) {
        let kind = &plan.kinds[*kind];
        let (first, last) = (&lines[span.start], &lines[span.end - 1]);
        let evidence = raw[first.start..last.end].to_string();
        // The particular the statement is about, by where it is first named;
        // a statement about none is its own lines, never its place among what
        // was located. RESOLVE keys a statement by its document, span and
        // this reference, and joins one reference's spans (`local_subjects`).
        let local_ref = match particular {
            Some((t, p)) => abouts[t].particulars[p].local_ref(),
            None => format!("l{}-{}", first.n, last.n),
        };
        let at = folded.span(first.start..last.end);
        let ask = Ask {
            infer,
            facts: &facts,
            document: &id,
            statement: &local_ref,
            body: &folded.text,
            view: &view,
            lines: (first.n, last.n),
            at,
            evidence: &evidence,
            candidates: &candidates,
        };
        let mut fields = BTreeMap::new();
        for field in &kind.fields {
            fields.extend(ask.field(kind.decl, field, &mut calls).await);
        }
        let subject_fields = match particular {
            Some(key) => identities[&key].clone(),
            None => ask.subject_fields(kind, &mut calls).await,
        };
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
    claims.extend(
        act::claims(
            &doc,
            plan,
            &abouts,
            &acts,
            &folded,
            &candidates,
            &identities,
            &mut calls,
        )
        .await,
    );
    let stated: Vec<Option<(usize, usize)>> = particular_of
        .iter()
        .copied()
        .chain(acts.iter().map(|a| a.particular))
        .collect();
    let mut particulars = about::unstated(&doc, &plan.decided, &abouts, &stated, identities);
    particulars.extend(mention::particulars(
        document, policies, &raw, &lines, &targets, &mentions,
    ));
    (
        DocumentReadOutcome {
            document_id: id,
            status: DocumentReadStatus::Read,
            reason: None,
            claims,
            refused: Vec::new(),
            particulars,
        },
        calls,
    )
}

/// Every field the plan asks, as `(claim kind, type, attribute, pass)`: the
/// claim's own fields under its own name, its subject's under the subject's,
/// each with the pass that asks it (`Choose`, `Point`, `Pick`). `None` when
/// the declaration is not read by this plan at all
/// (`OntologyPolicies::reads_documents`). `recipe validate`'s fill analysis
/// reads it, so what validate says is asked is what the reader asks.
pub fn read_fields(
    policies: &OntologyPolicies,
) -> Option<Vec<(String, String, String, &'static str)>> {
    if !policies.reads_documents() {
        return None;
    }
    let plan = Plan::of(policies);
    let mut out = Vec::new();
    for kind in &plan.kinds {
        let sides = [
            (kind.decl, &kind.fields),
            (kind.subject, &kind.subject_fields),
        ];
        for (owner, fields) in sides {
            for field in fields.iter() {
                if let Some(pass) = field.pass() {
                    for name in field.names() {
                        // A key of an act is read with the act, not by Choose.
                        let keyed =
                            owner.name == kind.decl.name && kind.keys.iter().any(|k| k == name);
                        let pass = if keyed { "Act" } else { pass };
                        out.push((
                            kind.decl.name.clone(),
                            owner.name.clone(),
                            name.to_string(),
                            pass,
                        ));
                    }
                }
            }
        }
    }
    Some(out)
}

/// What the plan asks, rendered on a fixed sample document: a change to any
/// question's wording changes this value, and so the contract fingerprint
/// that keys cached reads (`schema::contract_value`).
pub(super) fn contract_value(policies: &OntologyPolicies) -> Value {
    match super::reader() {
        super::Reader::Window => super::window::contract_value(policies),
        super::Reader::Waves => super::waves::contract_value(policies),
        super::Reader::Passes => plan_contract_value(policies),
    }
}

/// The passes reader's contract, whatever `SOVEREIGN_READER` chooses.
pub(super) fn plan_contract_value(policies: &OntologyPolicies) -> Value {
    let plan = Plan::of(policies);
    let body = "first line\nsecond line";
    let lines = lines(body);
    let labels: Vec<&str> = LABELS[..plan.located().len().min(LABELS.len())]
        .iter()
        .copied()
        .chain([NONE])
        .collect();
    let locate = locate_question(
        prefill::FACTS_HEADER,
        body,
        &lines,
        &lines[0],
        &plan,
        &labels,
    );
    let probe = ClosedAttr {
        name: "attribute".into(),
        description: String::new(),
        values: vec!["value".into()],
    };
    let choose = prefill::carrying(
        choice_question(
            "type",
            "",
            &probe,
            &["A", NONE],
            &super::ask::statement_words(&body[0..10], (1, 1)),
            CHOOSE_PHASE,
        ),
        &super::ask::opening(prefill::FACTS_HEADER, &numbered(body, &lines)),
    );
    json!({
        "reader": "passes",
        "max_statement_lines": MAX_STATEMENT_LINES,
        "locate": [locate.system, locate.user],
        "choose": [choose.system, choose.user],
        "mention": mention::contract_sample(body, &lines),
        "point": point::contract_sample(),
        "pick": pick::contract_sample(),
        "about": about::contract_sample(body, &lines),
        "act": act::contract_sample(body, &lines),
    })
}

#[cfg(test)]
#[path = "passes_tests.rs"]
mod tests;
