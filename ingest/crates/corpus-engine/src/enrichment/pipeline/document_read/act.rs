// SPDX-License-Identifier: AGPL-3.0-or-later
//! Act (ONTOLOGY_METHOD §Reading; research/ontology-apps/REVIEW_BRIEF.md §6
//! step 1). A claim kind a protocol rule names, about a type RESOLVE decides,
//! is something a document does to a particular (a deal's stage moved, a
//! case's state reached), and it is asked of the whole document, per
//! particular, never of each line. Asked of a physical line, the local model
//! answered "none" at .84-.99 on the lines that carry it; asked of the whole
//! message it read Ward 18-22 of 27 and uv 96-98 of 125, against 10 and 76
//! line by line (REVIEW_BRIEF §4.2, note a4b85978).
//!
//! An act's keys are the fields its rules qualify on (`when`) that one forced
//! choice reads. Each is asked in two wordings off one wave, under a system
//! prompt naming the declared type and field (an abstract one read 8 of 25
//! where this framing read 18, §4.3); when the wordings' answers differ the
//! reading is held, traced and never settled. A value both agree on is then
//! cited by one forced choice over the document's asked lines, marked a page
//! at a time, with a final choice among the pages' picks when more than one
//! picks (cite-probe/PREREG.md part 2, M2's line arm). The statement is the
//! cited line; its keys are the act's readings, and its other fields are
//! asked as every statement's are.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use oicp_types::forced_choice;
use serde_json::{json, Value};
use tracing::debug;

use super::about::{About, Doc, Particular};
use super::ask::{opening, Ask};
use super::folded::Folded;
use super::passes::{argmax, excerpt, numbered, render, FieldPlan, Line, Plan};
use super::pick::Candidates;
use super::{DocumentReadClaim, DocumentReadField};
use crate::enrichment::atlas::precision::{Precision, SourcePrecision};
use crate::enrichment::atlas::resolution_records::decides;
use crate::enrichment::atlas::resolve_records::{decision_wave, ClosedAttr, LABELS, NONE};
use crate::enrichment::ontology::derived::FoldBy;
use crate::enrichment::ontology::{OntologyPolicies, OntologyTypeDecl, TypeIndex};
use crate::enrichment::pipeline::types::ChatPrompt;

const ACT_SYSTEM: &str = include_str!("passes_act_prompt.md");
const CITE_SYSTEM: &str = include_str!("passes_cite_prompt.md");
pub(super) const ACT_PHASE: &str = "document_passes_act";
pub(super) const CITE_PHASE: &str = "document_passes_cite";

/// The act question's two wordings, the second a paraphrase of the first:
/// the question, how it names the particular when About named none, and the
/// answer that is none of the values. Only these differ between the two, so
/// both ride one wave off one prefix.
const WORDINGS: [(&str, &str, &str); 2] = [
    (
        "Where does this document show {which} standing?",
        "the {type} it is about",
        "it shows no {type}'s {field}",
    ),
    (
        "According to this document, where does {which} stand?",
        "the {type} it concerns",
        "the document shows no {type}'s {field}",
    ),
];

/// The fields of claim kind `decl` (`fields` its read fields) asked of the
/// whole document: those a protocol rule naming the kind qualifies on, each
/// read by one forced choice, when RESOLVE decides its subject `subject`.
/// Empty for every other kind, which Locate asks line by line. A pure
/// function of the declaration.
pub(super) fn keys(
    policies: &OntologyPolicies,
    index: &TypeIndex<'_>,
    decl: &OntologyTypeDecl,
    subject: &OntologyTypeDecl,
    fields: &[FieldPlan<'_>],
) -> Vec<String> {
    if !decides(index, &subject.name) {
        return Vec::new();
    }
    let qualified: BTreeSet<&str> = policies
        .derivation
        .derived
        .folds
        .iter()
        .filter(|f| f.by == FoldBy::Protocol)
        .filter_map(|f| f.protocol.as_ref())
        .flat_map(|p| &p.rules)
        .filter(|r| index.is_a(&decl.name, &r.claim_kind))
        .flat_map(|r| r.when.keys().map(String::as_str))
        .collect();
    fields
        .iter()
        .filter_map(|f| match f {
            FieldPlan::Choose(c) if qualified.contains(c.name.as_str()) => Some(c.name.clone()),
            _ => None,
        })
        .collect()
}

/// How an act question names the particular: by where About found it first
/// named, or, when About named at most one, as the one the document is about.
fn which(of: &str, wording: usize, named: Option<&Particular>) -> String {
    match named {
        Some(p) => format!(
            "the {of} first named or described on line {} (\"{}\")",
            p.line,
            excerpt(&p.words)
        ),
        None => WORDINGS[wording].1.replace("{type}", of),
    }
}

/// The act question: the document, the declared type and the key with their
/// descriptions, then where the document shows the particular standing,
/// among the key's values.
pub(super) fn act_question(
    (facts, view): (&str, &str),
    of: &OntologyTypeDecl,
    key: &ClosedAttr,
    named: Option<&Particular>,
    wording: usize,
    labels: &[&str],
) -> ChatPrompt {
    let (ask, _, none) = WORDINGS[wording];
    let fill = |s: &str| s.replace("{type}", &of.name).replace("{field}", &key.name);
    let mut user = format!("{}Kind of thing: {}", opening(facts, view), of.name);
    if !of.description.is_empty() {
        user.push_str(&format!(" ({})", of.description));
    }
    user.push_str(&format!("\nAttribute: {}", key.name));
    if !key.description.is_empty() {
        user.push_str(&format!(" ({})", key.description));
    }
    user.push_str("\n\n");
    user.push_str(&ask.replace("{which}", &which(&of.name, wording, named)));
    user.push('\n');
    for (value, label) in key.values.iter().zip(labels) {
        user.push_str(&format!("{label} {value}\n"));
    }
    user.push_str(&format!("{NONE} {}\nAnswer with its letter.", fill(none)));
    ChatPrompt::new(fill(ACT_SYSTEM), user)
        .with_response_schema("read", forced_choice::schema(labels))
        .with_phase_id(ACT_PHASE)
        .with_temperature(0.0)
}

/// What a citation asks to be shown: each key with its description, then the
/// particular holding the agreed values.
pub(super) fn fact(
    of: &OntologyTypeDecl,
    keys: &[(&ClosedAttr, &str)],
    named: Option<&Particular>,
) -> String {
    let mut s = String::new();
    for (key, _) in keys {
        s.push_str(&format!("Attribute: {}", key.name));
        if !key.description.is_empty() {
            s.push_str(&format!(" ({})", key.description));
        }
        s.push('\n');
    }
    let who = match named {
        Some(p) => format!("the {} \"{}\"", of.name, excerpt(&p.words)),
        None => format!("the {} this document is about", of.name),
    };
    let has: Vec<String> = keys
        .iter()
        .map(|(key, value)| format!("{} '{value}'", key.name))
        .collect();
    s.push_str(&format!("Fact: {who} has {}.", has.join(" and ")));
    s
}

/// One sentence unit of a document: a paragraph's lines (a run with no blank
/// line between, all asked or all carried, so a cited sentence never takes in
/// a line a line class keeps out) joined with a space and split after '.', '?' or '!' before
/// whitespace, `chunkers/sentence.rs`'s boundary; with the lines it touches
/// (indices into the document's lines). Cited over these, the gold unit was
/// the model's first on Ward 13 of 32 in both wordings, over physical lines
/// 8 and 12 (cite-probe M2, 6177909c6).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Unit {
    pub(super) text: String,
    pub(super) lines: Range<usize>,
}

/// The document's sentence units, in order; `asked[i]` is whether line `i`
/// is asked at all.
pub(super) fn units(body: &str, lines: &[Line], asked: &[bool]) -> Vec<Unit> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let mut j = i + 1;
        while j < lines.len()
            && asked[j] == asked[i]
            && body[lines[j - 1].end..lines[j].start].matches('\n').count() == 1
        {
            j += 1;
        }
        // The paragraph joined, each byte's line beside it; a joining space is
        // the next line's.
        let (mut joined, mut owner) = (String::new(), Vec::new());
        for (k, line) in lines.iter().enumerate().take(j).skip(i) {
            if !joined.is_empty() {
                joined.push(' ');
                owner.push(k);
            }
            joined.push_str(&body[line.start..line.end]);
            owner.resize(joined.len(), k);
        }
        let mut cuts: Vec<usize> = joined
            .char_indices()
            .zip(joined.chars().skip(1))
            .filter(|((_, c), next)| matches!(c, '.' | '?' | '!') && next.is_whitespace())
            .map(|((at, _), _)| at + 1)
            .collect();
        cuts.push(joined.len());
        let mut start = 0;
        for end in cuts {
            let piece = &joined[start..end];
            let text = piece.trim();
            if !text.is_empty() {
                let a = start + (piece.len() - piece.trim_start().len());
                out.push(Unit {
                    text: text.to_string(),
                    lines: owner[a]..owner[a + text.len() - 1] + 1,
                });
            }
            start = end;
        }
        i = j;
    }
    out
}

/// The citation question: the document's sentence units, `marked` (indices
/// into `units`) lettered in order and the rest shown unmarked, then the
/// fact. It offers no "none": the act's presence is decided before it is
/// cited, and offered "none" the model re-judged presence and answered it on
/// 26-30 of 32 Ward items, one-sentence uv documents whose sentence is the act
/// among them (cite-probe M2, 6177909c6).
pub(super) fn cite_question(
    facts: &str,
    units: &[Unit],
    marked: &[usize],
    fact: &str,
    labels: &[&str],
) -> ChatPrompt {
    let mut user = format!("{facts}Document:\n<<<\n");
    for (i, unit) in units.iter().enumerate() {
        match marked.iter().position(|&m| m == i) {
            Some(k) => user.push_str(&format!("[{}] {}\n", labels[k], unit.text)),
            None => user.push_str(&format!("{}\n", unit.text)),
        }
    }
    user.push_str(&format!(
        ">>>\n\n{fact}\n\nWhich marked sentence shows this fact? Answer with its letter."
    ));
    ChatPrompt::new(CITE_SYSTEM, user)
        .with_response_schema("read", forced_choice::schema(labels))
        .with_phase_id(CITE_PHASE)
        .with_temperature(0.0)
}

/// One key's reading, from its two wordings' answers.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Reading {
    /// Both wordings chose this value.
    Agreed(String),
    /// Both chose none of the values.
    Neither,
    /// The wordings chose differently, or one was refused: held, never settled.
    Held(String),
}

impl Reading {
    /// The field a statement carries from this reading, citing `evidence`.
    pub(super) fn field(&self, evidence: &str) -> DocumentReadField {
        match self {
            // No read precision is declared yet: the agreed value decides so
            // that it can be measured (`passes.rs` module doc).
            Reading::Agreed(value) => DocumentReadField::Supported {
                value: Value::String(value.clone()),
                evidence: evidence.to_string(),
                by: Some(SourcePrecision::new("reader_act", Precision::Unmeasured)),
            },
            Reading::Neither => DocumentReadField::Unknown {
                reason: "both wordings of the act chose none of the values".into(),
            },
            Reading::Held(why) => DocumentReadField::Unknown {
                reason: format!("held: {why}"),
            },
        }
    }
}

/// What `key`'s two wordings say, `labels` its values' then none.
pub(super) fn reading<R: std::fmt::Debug>(
    key: &ClosedAttr,
    labels: &[&str],
    answers: &[Result<BTreeMap<String, f64>, R>],
) -> Reading {
    let mut chosen = Vec::with_capacity(answers.len());
    for (w, answer) in answers.iter().enumerate() {
        match answer {
            Ok(dist) => chosen.push(argmax(labels, dist)),
            Err(refusal) => {
                return Reading::Held(format!("wording {} refused: {refusal:?}", w + 1))
            }
        }
    }
    let value = |best: usize| key.values.get(best).map_or("none", String::as_str);
    let first = chosen[0].0;
    if chosen.iter().any(|(best, _)| *best != first) {
        let said: Vec<String> = chosen
            .iter()
            .map(|(best, p)| format!("{} {p:.2}", value(*best)))
            .collect();
        return Reading::Held(format!("the wordings differ: {}", said.join(" / ")));
    }
    match key.values.get(first) {
        Some(v) => Reading::Agreed(v.clone()),
        None => Reading::Neither,
    }
}

/// One act a document was read to make: the kind (index in `Plan::kinds`),
/// the particular it is about (index in `Plan::decided`, index among that
/// type's particulars) or none, each key's reading, and the lines the cited
/// sentence touches (indices into the document's lines).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Act {
    pub(super) kind: usize,
    pub(super) particular: Option<(usize, usize)>,
    pub(super) read: BTreeMap<String, Reading>,
    pub(super) lines: Range<usize>,
}

/// Every act of `plan` the document makes. Per act kind, its particulars of
/// the subject type are asked about by where About found them first named,
/// when it found more than one; otherwise once, as the one the document is
/// about, whether About found it or not, so About never gates what is read.
/// `asked[i]` is whether line `i` is asked at all (no line class keeps it).
pub(super) async fn acts(
    d: &Doc<'_>,
    plan: &Plan<'_>,
    abouts: &[About],
    asked: &[bool],
    calls: &mut u32,
) -> Vec<Act> {
    let mut out = Vec::new();
    let units = units(d.body, d.lines, asked);
    for (k, kind) in plan.kinds.iter().enumerate() {
        if kind.keys.is_empty() {
            continue;
        }
        let Some(t) = plan
            .decided
            .iter()
            .position(|o| o.name == kind.subject.name)
        else {
            debug!(document = d.document, kind = %kind.decl.name, "document_read/act: its subject is not one About asks of; not asked");
            continue;
        };
        let found = &abouts[t].particulars;
        let referents: Vec<(Option<(usize, usize)>, Option<&Particular>)> = if found.len() > 1 {
            (0..found.len())
                .map(|p| (Some((t, p)), Some(&found[p])))
                .collect()
        } else {
            vec![(found.first().map(|_| (t, 0)), None)]
        };
        let keys: Vec<&ClosedAttr> = kind
            .fields
            .iter()
            .filter_map(|f| match f {
                FieldPlan::Choose(c) if kind.keys.contains(&c.name) => Some(c),
                _ => None,
            })
            .collect();
        let mut read: Vec<BTreeMap<String, Reading>> = vec![BTreeMap::new(); referents.len()];
        for key in &keys {
            let labels: Vec<&str> = LABELS[..key.values.len()]
                .iter()
                .copied()
                .chain([NONE])
                .collect();
            let mut prompts = Vec::with_capacity(referents.len() * WORDINGS.len());
            let mut items = Vec::with_capacity(prompts.capacity());
            for (r, (_, named)) in referents.iter().enumerate() {
                for w in 0..WORDINGS.len() {
                    prompts.push(act_question(
                        (d.facts, d.view),
                        kind.subject,
                        key,
                        *named,
                        w,
                        &labels,
                    ));
                    items.push(format!("{} {} r{r} w{}", kind.decl.name, key.name, w + 1));
                }
            }
            *calls += prompts.len() as u32;
            let answers = decision_wave(
                d.infer,
                &prompts,
                &vec![labels.clone(); prompts.len()],
                d.document,
                &items,
            )
            .await;
            for (r, pair) in answers.chunks(WORDINGS.len()).enumerate() {
                let reading = reading(key, &labels, pair);
                let dists: Vec<String> = pair
                    .iter()
                    .map(|a| a.as_ref().map_or_else(|e| format!("{e:?}"), render))
                    .collect();
                debug!(document = d.document, kind = %kind.decl.name, key = %key.name, particular = ?referents[r].1.map(Particular::local_ref), ?reading, dists = %dists.join(" | "), "document_read/act: read");
                read[r].insert(key.name.clone(), reading);
            }
        }
        for ((particular, named), read) in referents.into_iter().zip(read) {
            let agreed: Vec<(&ClosedAttr, &str)> = keys
                .iter()
                .filter_map(|key| match read.get(&key.name) {
                    Some(Reading::Agreed(v)) => Some((*key, v.as_str())),
                    _ => None,
                })
                .collect();
            if agreed.is_empty() {
                debug!(document = d.document, kind = %kind.decl.name, particular = ?named.map(Particular::local_ref), "document_read/act: no key agreed; no statement");
                continue;
            }
            let item = format!(
                "{} {}",
                kind.decl.name,
                named.map_or_else(|| "it".into(), Particular::local_ref)
            );
            let fact = fact(kind.subject, &agreed, named);
            match cite(d, &units, &fact, asked, &item, calls).await {
                Some(u) => {
                    let lines = units[u].lines.clone();
                    debug!(document = d.document, item = %item, first = d.lines[lines.start].n, last = d.lines[lines.end - 1].n, sentence = %excerpt(&units[u].text), "document_read/act: cited");
                    out.push(Act {
                        kind: k,
                        particular,
                        read,
                        lines,
                    });
                }
                None => {
                    debug!(document = d.document, item = %item, "document_read/act: read, but no sentence cited; no statement");
                }
            }
        }
    }
    out
}

/// The sentence that shows `fact` (index into `units`), among those touching
/// an asked line: cited by code when there is one, otherwise by one forced
/// choice per page of candidates, then among the pages' picks until one is
/// left (a page of one is its own pick). `None` when no sentence touches an
/// asked line, or a question is refused.
async fn cite(
    d: &Doc<'_>,
    units: &[Unit],
    fact: &str,
    asked: &[bool],
    item: &str,
    calls: &mut u32,
) -> Option<usize> {
    let mut round: Vec<usize> = (0..units.len())
        .filter(|&u| asked[units[u].lines.start])
        .collect();
    if round.len() < 2 {
        debug!(
            document = d.document,
            item,
            candidates = round.len(),
            "document_read/act: cited with no question"
        );
    }
    while round.len() > 1 {
        let pages: Vec<&[usize]> = round.chunks(LABELS.len()).collect();
        let asking: Vec<&[usize]> = pages.iter().copied().filter(|p| p.len() > 1).collect();
        let labels: Vec<Vec<&str>> = asking.iter().map(|p| LABELS[..p.len()].to_vec()).collect();
        let prompts: Vec<ChatPrompt> = asking
            .iter()
            .zip(&labels)
            .map(|(page, labels)| cite_question(d.facts, units, page, fact, labels))
            .collect();
        let items: Vec<String> = (0..asking.len())
            .map(|p| format!("{item} of {} page {}", round.len(), p + 1))
            .collect();
        *calls += prompts.len() as u32;
        let mut answers = decision_wave(d.infer, &prompts, &labels, d.document, &items)
            .await
            .into_iter()
            .zip(labels.iter());
        let mut picks = Vec::with_capacity(pages.len());
        for page in pages {
            if page.len() == 1 {
                picks.push(page[0]);
                continue;
            }
            match answers.next() {
                Some((Ok(dist), labels)) => {
                    let (best, p) = argmax(labels, &dist);
                    debug!(document = d.document, item, of = round.len(), sentence = %excerpt(&units[page[best]].text), p, dist = %render(&dist), "document_read/act: cite");
                    picks.push(page[best]);
                }
                Some((Err(refusal), _)) => {
                    debug!(
                        document = d.document,
                        item,
                        ?refusal,
                        "document_read/act: cite refused"
                    );
                    return None;
                }
                None => return None,
            }
        }
        round = picks;
    }
    round.first().copied()
}

/// Each act as a statement: the lines its cited sentence touches; its keys'
/// readings; its other
/// fields asked as every statement's are; its subject's identifying fields
/// its particular's (`about::identities`), or asked at the line when it is
/// about none.
#[allow(clippy::too_many_arguments)]
pub(super) async fn claims(
    d: &Doc<'_>,
    plan: &Plan<'_>,
    abouts: &[About],
    acts: &[Act],
    folded: &Folded,
    candidates: &BTreeMap<String, Candidates>,
    identities: &BTreeMap<(usize, usize), BTreeMap<String, DocumentReadField>>,
    calls: &mut u32,
) -> Vec<DocumentReadClaim> {
    let mut out = Vec::with_capacity(acts.len());
    for act in acts {
        let kind = &plan.kinds[act.kind];
        let (first, last) = (&d.lines[act.lines.start], &d.lines[act.lines.end - 1]);
        let evidence = d.body[first.start..last.end].to_string();
        let local_ref = match act.particular {
            Some((t, p)) => abouts[t].particulars[p].local_ref(),
            None => format!("l{}-{}", first.n, last.n),
        };
        let at = folded.span(first.start..last.end);
        let ask = Ask {
            infer: d.infer,
            facts: d.facts,
            document: d.document,
            statement: &local_ref,
            body: &folded.text,
            view: d.view,
            lines: (first.n, last.n),
            at: at.clone(),
            evidence: &evidence,
            candidates,
        };
        let mut fields = BTreeMap::new();
        for field in &kind.fields {
            match field {
                FieldPlan::Choose(c) if act.read.contains_key(&c.name) => {
                    fields.insert(c.name.clone(), act.read[&c.name].field(&evidence));
                }
                _ => fields.extend(ask.field(kind.decl, field, calls).await),
            }
        }
        let subject_fields = match act.particular.and_then(|key| identities.get(&key)) {
            Some(read) => read.clone(),
            None => ask.subject_fields(kind, calls).await,
        };
        out.push(DocumentReadClaim {
            kind: kind.decl.name.clone(),
            content: folded.text[at].to_string(),
            subject_type: kind.subject.name.clone(),
            subject_name: format!("{} {local_ref}", kind.subject.name),
            subject_local_ref: local_ref,
            speaker: None,
            evidence,
            fields,
            subject_fields,
            other_names: Default::default(),
        });
    }
    out
}

/// The act and citation questions on a fixed sample, for the read contract's
/// fingerprint.
pub(super) fn contract_sample(body: &str, lines: &[Line]) -> Value {
    let probe = OntologyTypeDecl {
        name: "type".into(),
        description: "description".into(),
        ..Default::default()
    };
    let key = ClosedAttr {
        name: "attribute".into(),
        description: "meaning".into(),
        values: vec!["value".into()],
    };
    let named = Particular {
        line: 1,
        span: 0..5,
        words: "first".into(),
    };
    let view = numbered(body, lines);
    let labels = ["A", NONE];
    let it = act_question(("", &view), &probe, &key, None, 0, &labels);
    let other = act_question(("", &view), &probe, &key, Some(&named), 1, &labels);
    let cite = cite_question(
        "",
        &units(body, lines, &vec![true; lines.len()]),
        &[0, 1],
        &fact(&probe, &[(&key, "value")], Some(&named)),
        &["A", "B"],
    );
    json!([it.system, it.user, other.user, cite.system, cite.user])
}

#[cfg(test)]
#[path = "act_tests.rs"]
mod tests;
