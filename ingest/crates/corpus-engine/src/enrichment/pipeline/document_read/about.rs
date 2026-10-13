// SPDX-License-Identifier: AGPL-3.0-or-later
//! About (ONTOLOGY_METHOD §Reading): per document, the particulars it is about
//! of each type a claim kind is about and RESOLVE decides
//! (`resolution_records::decides`). One forced choice says whether the
//! document is about none, one or more than one; each particular is then
//! pointed at where it is first named or described, a line and then its words
//! (`point::point_span`), at most [`MAX_PARTICULARS`]; with more than one, each
//! statement of the type is asked which of them it is about. A statement's
//! local reference is its particular's, so statements about one particular
//! reach RESOLVE as one (`resolution_records/local_subjects.rs`), and a
//! particular's identifying fields are read once, at its words. Whether a
//! document is about a particular is asked apart from whether any line states
//! a claim kind: an issue that opens its thread is about a case before any
//! line says where the case stands (uv #1401, note 0f8f8c6c).

use std::collections::BTreeMap;
use std::ops::Range;

use oicp_types::forced_choice;
use serde_json::{json, Value};
use tracing::debug;

use super::ask::{opening, statement_words, Ask};
use super::folded::Folded;
use super::passes::{argmax, excerpt, numbered, render, Line, Plan};
use super::pick::Candidates;
use super::point::{point_span, Pointing};
use super::{DocumentReadField, DocumentReadParticular};
use crate::enrichment::atlas::resolve_records::{decision_call, decision_wave, LABELS, NONE};
use crate::enrichment::ontology::OntologyTypeDecl;
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::InferenceFn;

const ABOUT_SYSTEM: &str = include_str!("passes_about_prompt.md");
pub(super) const ABOUT_PHASE: &str = "document_passes_about";

/// How many particulars of one type a document is pointed at for: a bound on
/// what one document may cost, not a number measured on any corpus
/// (research/ontology-apps/particulars-prereg.md H1).
pub(super) const MAX_PARTICULARS: usize = 5;

/// The count question's answers: one, more than one, none.
const COUNT_LABELS: [&str; 3] = ["A", "B", NONE];

/// What a document says it is about, of one type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Count {
    NotAbout,
    One,
    Several,
    /// The count question was refused: not "about none". Its particulars are
    /// pointed at as for several, until none or one already found is chosen.
    Unjudged,
}

/// One particular a document is about: where it is first named or described,
/// as a line and that line's words.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Particular {
    pub(super) line: usize,
    /// The words' byte span in the raw body.
    pub(super) span: Range<usize>,
    pub(super) words: String,
}

impl Particular {
    /// Its reference local to the document: where it is first named, never
    /// its place among the particulars found.
    pub(super) fn local_ref(&self) -> String {
        local_ref(self.line, &self.words)
    }
}

/// A particular's reference local to its document: the line that first names
/// it and its words there. About's particulars and Mention's share it.
pub(super) fn local_ref(line: usize, words: &str) -> String {
    format!("l{line}:{words}")
}

/// What one document is about, of one type.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct About {
    pub(super) count: Count,
    pub(super) particulars: Vec<Particular>,
    /// Whether pointing stopped where the document said it did: false when it
    /// reached [`MAX_PARTICULARS`] with none chosen yet, or was refused, so
    /// the document may be about particulars no statement can be put on.
    pub(super) complete: bool,
}

/// One document being asked what it is about.
pub(super) struct Doc<'a> {
    pub(super) infer: &'a InferenceFn,
    pub(super) facts: &'a str,
    pub(super) document: &'a str,
    pub(super) body: &'a str,
    pub(super) lines: &'a [Line],
    /// Per line, whether it is a header line (`LineClass::Field`): shown in
    /// the view, never offered as where a particular is named, since what a
    /// header states is declared and its records are the source's. Empty, or
    /// shorter than `lines`, when no header line is known past its end.
    pub(super) headers: &'a [bool],
    /// The document with its lines numbered (`passes::numbered`).
    pub(super) view: &'a str,
}

/// The declared kind as every About question shows it, in RESOLVE's words
/// for the criterion (`resolve_records/select.rs`).
fn kind(of: &OntologyTypeDecl) -> String {
    let mut s = format!("Kind of thing: {}", of.name);
    if !of.description.is_empty() {
        s.push_str(&format!(" ({})", of.description));
    }
    if let Some(criterion) = &of.identity_criterion {
        s.push_str(&format!("\nSame particular when: {criterion}"));
    }
    s.push('\n');
    s
}

fn question(user: String, labels: &[&str]) -> ChatPrompt {
    ChatPrompt::new(ABOUT_SYSTEM, user)
        .with_response_schema("read", forced_choice::schema(labels))
        .with_phase_id(ABOUT_PHASE)
        .with_temperature(0.0)
}

/// Whether the document is about a particular of `of`: one, several, none.
pub(super) fn count_question(facts: &str, view: &str, of: &OntologyTypeDecl) -> ChatPrompt {
    let name = &of.name;
    let user = format!(
        "{}{}\nIs the document about a particular {name}?\n{} yes, one {name}\n{} yes, more \
         than one {name}\n{NONE} no\nAnswer with its letter.",
        opening(facts, view),
        kind(of),
        COUNT_LABELS[0],
        COUNT_LABELS[1],
    );
    question(user, &COUNT_LABELS)
}

/// What the count question's answer says. Its first decision is whether the
/// document is about a particular at all, and two labels answer yes ("one",
/// "more than one"), so it is read over yes against no, their mass summed:
/// an argmax over three labels lets the two yeses split the mass and "no"
/// win with less than half of it (2026-10-12, a bhp message with four
/// located stage statements: no .48 against one .24 and several .29). Then
/// one against several. A tie is "not about", the reading before About.
pub(super) fn count_of(dist: &BTreeMap<String, f64>) -> (Count, f64) {
    let (one, several, no) = (
        dist[COUNT_LABELS[0]],
        dist[COUNT_LABELS[1]],
        dist[COUNT_LABELS[2]],
    );
    if no >= one + several {
        (Count::NotAbout, no)
    } else if one >= several {
        (Count::One, one + several)
    } else {
        (Count::Several, one + several)
    }
}

/// On which of `window`'s lines the next particular of `of` is first named
/// or described, `found` being those already pointed at.
pub(super) fn line_question(
    (facts, view, body): (&str, &str, &str),
    of: &OntologyTypeDecl,
    found: &[Particular],
    window: &[Line],
    labels: &[&str],
) -> ChatPrompt {
    let mut user = format!("{}{}", opening(facts, view), kind(of));
    let which = if found.is_empty() {
        format!("the {}", of.name)
    } else {
        user.push_str("Already found, each where it is first named or described:\n");
        for p in found {
            user.push_str(&format!("- line {}: \"{}\"\n", p.line, p.words));
        }
        format!(
            "another {}, not the same particular as any already found,",
            of.name
        )
    };
    user.push_str(&format!(
        "\nOn which line is {which} the document is about first named or described?\n"
    ));
    for (line, label) in window.iter().zip(labels) {
        user.push_str(&format!(
            "{label} line {}: {}\n",
            line.n,
            excerpt(&body[line.start..line.end])
        ));
    }
    user.push_str(&format!(
        "{NONE} none of these lines\nAnswer with its letter."
    ));
    question(user, labels)
}

/// Which of `particulars` the statement `words` (on lines `at`) is about.
pub(super) fn assign_question(
    (facts, view): (&str, &str),
    of: &OntologyTypeDecl,
    particulars: &[Particular],
    words: &str,
    at: (usize, usize),
    labels: &[&str],
) -> ChatPrompt {
    let mut user = format!(
        "{}{}\n{}\nWhich {} is the statement about?\n",
        opening(facts, view),
        statement_words(words, at),
        kind(of),
        of.name
    );
    for (p, label) in particulars.iter().zip(labels) {
        user.push_str(&format!("{label} {} (line {})\n", p.words, p.line));
    }
    user.push_str(&format!("{NONE} none of them\nAnswer with its letter."));
    question(user, labels)
}

/// What the document is about of each of `types`: the count questions as one
/// wave, then each type's particulars pointed at in turn.
pub(super) async fn about(d: &Doc<'_>, types: &[&OntologyTypeDecl], calls: &mut u32) -> Vec<About> {
    let prompts: Vec<ChatPrompt> = types
        .iter()
        .map(|of| count_question(d.facts, d.view, of))
        .collect();
    let items: Vec<String> = types
        .iter()
        .map(|of| format!("count {}", of.name))
        .collect();
    *calls += prompts.len() as u32;
    let answers = decision_wave(
        d.infer,
        &prompts,
        &vec![COUNT_LABELS.to_vec(); prompts.len()],
        d.document,
        &items,
    )
    .await;
    let mut out = Vec::with_capacity(types.len());
    for (of, answer) in types.iter().zip(answers) {
        let count = match answer {
            Ok(dist) => {
                let (count, p) = count_of(&dist);
                debug!(document = d.document, of = %of.name, ?count, p, dist = %render(&dist), "document_read/about: count");
                count
            }
            Err(refusal) => {
                debug!(document = d.document, of = %of.name, ?refusal, "document_read/about: count refused; unjudged, its particulars pointed at as for several");
                Count::Unjudged
            }
        };
        let most = match count {
            Count::NotAbout => 0,
            Count::One => 1,
            Count::Several | Count::Unjudged => MAX_PARTICULARS,
        };
        let mut particulars: Vec<Particular> = Vec::new();
        let mut complete = true;
        while particulars.len() < most {
            match next(d, of, &particulars, calls).await {
                Ok(Some(p)) if particulars.iter().any(|f| f.span == p.span) => {
                    debug!(document = d.document, of = %of.name, line = p.line, words = %excerpt(&p.words), "document_read/about: pointed at one already found; no more");
                    break;
                }
                Ok(Some(p)) => {
                    debug!(document = d.document, of = %of.name, line = p.line, words = %excerpt(&p.words), "document_read/about: particular");
                    particulars.push(p);
                }
                Ok(None) => break,
                Err(refusal) => {
                    debug!(document = d.document, of = %of.name, %refusal, "document_read/about: pointing refused; the particulars found are incomplete");
                    complete = false;
                    break;
                }
            }
        }
        if most == MAX_PARTICULARS && particulars.len() == most {
            debug!(document = d.document, of = %of.name, most, "document_read/about: pointing stopped at its bound with none chosen yet; the particulars found are incomplete");
            complete = false;
        }
        debug!(document = d.document, of = %of.name, ?count, particulars = particulars.len(), complete, "document_read/about: document");
        out.push(About {
            count,
            particulars,
            complete,
        });
    }
    out
}

/// The next particular of `of`: the line it is first named on, a window of
/// lines at a time ("none of these lines" moving to the next), then its words
/// on that line. `Ok(None)` when no line or no word is chosen.
async fn next(
    d: &Doc<'_>,
    of: &OntologyTypeDecl,
    found: &[Particular],
    calls: &mut u32,
) -> Result<Option<Particular>, String> {
    let k = found.len() + 1;
    let item = format!("{} {k}", of.name);
    let mut chosen = None;
    let nameable: Vec<Line> = d
        .lines
        .iter()
        .enumerate()
        .filter(|(i, _)| !d.headers.get(*i).copied().unwrap_or(false))
        .map(|(_, l)| l.clone())
        .collect();
    for window in nameable.chunks(LABELS.len()) {
        let labels: Vec<&str> = LABELS[..window.len()]
            .iter()
            .copied()
            .chain([NONE])
            .collect();
        let prompt = line_question((d.facts, d.view, d.body), of, found, window, &labels);
        *calls += 1;
        let dist = decision_call(d.infer, &prompt, &labels, d.document, &item)
            .await
            .map_err(|r| format!("{r:?}"))?;
        let (best, p) = argmax(&labels, &dist);
        debug!(document = d.document, item = %item, first = window[0].n, best, p, dist = %render(&dist), "document_read/about: line");
        if best < window.len() {
            chosen = Some(&window[best]);
            break;
        }
    }
    let Some(line) = chosen else {
        return Ok(None);
    };
    let text = &d.body[line.start..line.end];
    let pointing = Pointing {
        infer: d.infer,
        facts: d.facts,
        head: format!("Line {}: \"{text}\"\n{}", line.n, kind(of).trim_end()),
        what: format!("the words that name or describe the {}", of.name),
        phase: ABOUT_PHASE,
        document: d.document,
        item: &item,
    };
    match point_span(&pointing, text, calls).await? {
        Some(span) => Ok(Some(Particular {
            line: line.n,
            span: line.start + span.start..line.start + span.end,
            words: text[span].to_string(),
        })),
        None => {
            debug!(document = d.document, item = %item, line = line.n, "document_read/about: no words pointed at on the chosen line");
            Ok(None)
        }
    }
}

/// Which particular of `about` each statement (its words and lines) is
/// about: the one, when the document is about one; asked, when it is about
/// several or its count was refused; none, when it is about none or none was
/// pointed at. A statement asked whose answer is "none of them" or refused is
/// about none.
pub(super) async fn assign(
    d: &Doc<'_>,
    of: &OntologyTypeDecl,
    about: &About,
    statements: &[(&str, (usize, usize))],
    calls: &mut u32,
) -> Vec<Option<usize>> {
    match (about.count, about.particulars.len()) {
        (_, 0) | (Count::NotAbout, _) => return vec![None; statements.len()],
        (Count::One, _) => return vec![Some(0); statements.len()],
        (Count::Several | Count::Unjudged, _) => {}
    }
    let labels: Vec<&str> = LABELS[..about.particulars.len()]
        .iter()
        .copied()
        .chain([NONE])
        .collect();
    let prompts: Vec<ChatPrompt> = statements
        .iter()
        .map(|(words, at)| {
            assign_question(
                (d.facts, d.view),
                of,
                &about.particulars,
                words,
                *at,
                &labels,
            )
        })
        .collect();
    let items: Vec<String> = statements
        .iter()
        .map(|(_, (a, b))| format!("{} l{a}-{b}", of.name))
        .collect();
    *calls += prompts.len() as u32;
    let answers = decision_wave(
        d.infer,
        &prompts,
        &vec![labels.clone(); prompts.len()],
        d.document,
        &items,
    )
    .await;
    items
        .iter()
        .zip(answers)
        .map(|(item, answer)| match answer {
            Ok(dist) => {
                let (best, p) = argmax(&labels, &dist);
                let to = (best < about.particulars.len()).then_some(best);
                debug!(document = d.document, item = %item, particular = ?to.map(|i| about.particulars[i].local_ref()), p, dist = %render(&dist), "document_read/about: statement assigned");
                to
            }
            Err(refusal) => {
                debug!(document = d.document, item = %item, ?refusal, "document_read/about: assignment refused");
                None
            }
        })
        .collect()
}

/// Which particular each statement (its subject type, words and lines) is
/// about, as (index in `types`, index among that type's particulars); `None`
/// for a statement of a type About does not ask, or about none.
pub(super) async fn particular_of(
    d: &Doc<'_>,
    types: &[&OntologyTypeDecl],
    abouts: &[About],
    said: &[(&str, &str, (usize, usize))],
    calls: &mut u32,
) -> Vec<Option<(usize, usize)>> {
    let mut out = vec![None; said.len()];
    for (t, (of, about)) in types.iter().zip(abouts).enumerate() {
        let of_type: Vec<usize> = (0..said.len()).filter(|&s| said[s].0 == of.name).collect();
        if of_type.is_empty() {
            continue;
        }
        match (about.count, about.complete) {
            (Count::NotAbout, _) => {
                debug!(document = d.document, of = %of.name, statements = of_type.len(), "document_read/about: not about one, yet statements of it were located; each stays its own");
            }
            (Count::Unjudged, _) => {
                debug!(document = d.document, of = %of.name, statements = of_type.len(), particulars = about.particulars.len(), "document_read/about: the count was unjudged; each statement is asked which particular pointed at it is about");
            }
            (_, false) => {
                debug!(document = d.document, of = %of.name, statements = of_type.len(), particulars = about.particulars.len(), "document_read/about: the particulars found are incomplete; a statement about none of them stays its own");
            }
            _ => {}
        }
        let statements: Vec<(&str, (usize, usize))> =
            of_type.iter().map(|&s| (said[s].1, said[s].2)).collect();
        let to = assign(d, of, about, &statements, calls).await;
        for (&s, p) in of_type.iter().zip(to) {
            out[s] = p.map(|p| (t, p));
        }
    }
    out
}

/// Each particular's identifying fields, read once at the words that first
/// name it: the subject fields the first kind about its type declares, each
/// asked as a statement's are, the particular's line and words in its place.
pub(super) async fn identities(
    d: &Doc<'_>,
    plan: &Plan<'_>,
    abouts: &[About],
    folded: &Folded,
    candidates: &BTreeMap<String, Candidates>,
    calls: &mut u32,
) -> BTreeMap<(usize, usize), BTreeMap<String, DocumentReadField>> {
    let mut out = BTreeMap::new();
    for (t, (of, about)) in plan.decided.iter().zip(abouts).enumerate() {
        let Some(kind) = plan.kinds.iter().find(|k| k.subject.name == of.name) else {
            continue;
        };
        for (p, named) in about.particulars.iter().enumerate() {
            let local_ref = named.local_ref();
            let ask = Ask {
                infer: d.infer,
                facts: d.facts,
                document: d.document,
                statement: &local_ref,
                body: &folded.text,
                view: d.view,
                lines: (named.line, named.line),
                at: folded.span(named.span.clone()),
                evidence: &d.body[named.span.clone()],
                candidates,
            };
            out.insert((t, p), ask.subject_fields(kind, calls).await);
        }
    }
    out
}

/// The particulars no statement is about (`stated`: what each located
/// statement and each act is about), each with its identifying fields: what
/// the document is about although it states no claim kind of it. Each cites
/// the whole line that first names it, as every reading cites whole lines;
/// its words stay in its reference.
pub(super) fn unstated(
    d: &Doc<'_>,
    types: &[&OntologyTypeDecl],
    abouts: &[About],
    stated: &[Option<(usize, usize)>],
    mut identities: BTreeMap<(usize, usize), BTreeMap<String, DocumentReadField>>,
) -> Vec<DocumentReadParticular> {
    let mut out = Vec::new();
    for (t, (of, about)) in types.iter().zip(abouts).enumerate() {
        for (p, named) in about.particulars.iter().enumerate() {
            if stated.contains(&Some((t, p))) {
                continue;
            }
            debug!(document = d.document, of = %of.name, particular = %named.local_ref(), "document_read/about: a particular no statement is about; it reaches RESOLVE on its own");
            let Some(subject_fields) = identities.remove(&(t, p)) else {
                continue;
            };
            let local_ref = named.local_ref();
            let line = &d.lines[named.line - 1];
            out.push(DocumentReadParticular {
                subject_type: of.name.clone(),
                subject_name: format!("{} {local_ref}", of.name),
                subject_local_ref: local_ref,
                evidence: d.body[line.start..line.end].to_string(),
                subject_fields,
                words: None,
            });
        }
    }
    out
}

/// The About questions on a fixed sample, for the read contract's fingerprint.
pub(super) fn contract_sample(body: &str, lines: &[Line]) -> Value {
    let probe = OntologyTypeDecl {
        name: "type".into(),
        identity_criterion: Some("criterion".into()),
        ..Default::default()
    };
    let view = numbered(body, lines);
    let found = [Particular {
        line: 1,
        span: 0..5,
        words: "first".into(),
    }];
    let count = count_question("", &view, &probe);
    let line = line_question(("", &view, body), &probe, &found, &lines[..1], &["A", NONE]);
    let assign = assign_question(("", &view), &probe, &found, "second", (2, 2), &["A", NONE]);
    json!([count.system, count.user, line.user, assign.user])
}

#[cfg(test)]
#[path = "about_tests.rs"]
mod tests;
