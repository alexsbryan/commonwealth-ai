// SPDX-License-Identifier: AGPL-3.0-or-later
//! The compiled reader's level-2 and level-3 jobs (`waves.rs`): one declared
//! field of a particular or claim, and one referent's act with its cited
//! sentence. Split from `waves_jobs.rs`, which runs them.

use std::collections::BTreeMap;
use std::ops::Range;

use serde_json::Value;
use tracing::debug;

use super::about::Particular;
use super::act::{self, Reading, Unit};
use super::passes::{argmax, excerpt, render, FieldPlan};
use super::pick::Candidate;
use super::point::{family_words, normalise};
use super::waves_forms::{self as forms, Who};
use super::waves_jobs::{with_none, word_answers, word_questions, Answer, Doc, Pointed, Words, Q};
use super::DocumentReadField;
use crate::enrichment::atlas::precision::{Precision, SourcePrecision};
use crate::enrichment::atlas::resolve_records::{ClosedAttr, LABELS};
use crate::enrichment::ontology::OntologyTypeDecl;

// ---------------------------------------------------------------- one field

#[derive(Debug, Clone)]
enum Reading1 {
    Ask,
    Lines,
    Words {
        text: Range<usize>,
        n: (usize, usize),
        step: Words,
    },
    Done,
}

/// One declared field of a particular or a claim, asked by its family: a
/// closed value chosen, a reference picked, an open value pointed at (for a
/// particular, the line that states it and then its words; for a claim,
/// among its own words).
pub(super) struct Field<'a> {
    pub(super) plan: &'a FieldPlan<'a>,
    pub(super) who: Who<'a>,
    /// The raw span that cites what `who` is (a particular's words, a claim's
    /// lines): a chosen value's evidence, and a claim's words to point among.
    pub(super) at: Range<usize>,
    pub(super) read: Vec<(String, DocumentReadField)>,
    state: Reading1,
    item: String,
}

fn unknown(reason: impl Into<String>) -> DocumentReadField {
    DocumentReadField::Unknown {
        reason: reason.into(),
    }
}

fn supported(value: Value, evidence: &str, source: &'static str) -> DocumentReadField {
    DocumentReadField::Supported {
        value,
        evidence: evidence.to_string(),
        by: Some(SourcePrecision::new(source, Precision::Unmeasured)),
    }
}

impl<'a> Field<'a> {
    pub(super) fn new(
        plan: &'a FieldPlan<'a>,
        who: Who<'a>,
        at: Range<usize>,
        item: String,
    ) -> Self {
        let mut f = Self {
            plan,
            who,
            at,
            read: Vec::new(),
            state: Reading1::Ask,
            item,
        };
        if let FieldPlan::Unasked(attr) = plan {
            f.read = vec![(
                attr.name.clone(),
                unknown(format!(
                    "not asked: `{}` declares more values than one forced choice shows ({})",
                    attr.name,
                    LABELS.len()
                )),
            )];
            f.state = Reading1::Done;
        } else if let FieldPlan::Point(_) = plan {
            f.state = match &f.who {
                Who::Claim { lines, .. } => Reading1::Words {
                    text: f.at.clone(),
                    n: *lines,
                    step: Words::Start,
                },
                _ => Reading1::Lines,
            };
        }
        f
    }

    pub(super) fn ask(&mut self, d: &Doc<'_>) -> Vec<Q> {
        match (&self.state, self.plan) {
            (Reading1::Done, _) | (_, FieldPlan::Unasked(_)) => Vec::new(),
            (Reading1::Ask, FieldPlan::Choose(attr)) => {
                let labels = with_none(attr.values.len());
                vec![d.ask(
                    forms::choose(&self.who, attr, &labels),
                    labels,
                    format!("{} {}", self.item, attr.name),
                )]
            }
            (Reading1::Ask, FieldPlan::Pick(attr, of)) => {
                match d.candidates.get(*of).filter(|c| !c.shown.is_empty()) {
                    Some(c) => {
                        let labels = with_none(c.shown.len());
                        vec![d.ask(
                            forms::pick(&self.who, Some(attr), of, &c.shown, &labels),
                            labels,
                            format!("{} {}", self.item, attr.name),
                        )]
                    }
                    None => {
                        self.read = vec![(
                            attr.name.clone(),
                            unknown(format!(
                                "not asked: the document offers no `{of}` candidate"
                            )),
                        )];
                        self.state = Reading1::Done;
                        Vec::new()
                    }
                }
            }
            (Reading1::Ask, FieldPlan::Subject(_, of, keys)) => {
                match d.candidates.get(*of).filter(|c| !c.shown.is_empty()) {
                    Some(c) => {
                        let labels = with_none(c.shown.len());
                        vec![d.ask(
                            forms::pick(&self.who, None, of, &c.shown, &labels),
                            labels,
                            format!("{} subject", self.item),
                        )]
                    }
                    None => {
                        let why = format!("not asked: the document offers no `{of}` candidate");
                        self.read = keys
                            .iter()
                            .map(|k| (k.name.clone(), unknown(why.clone())))
                            .collect();
                        self.state = Reading1::Done;
                        Vec::new()
                    }
                }
            }
            (Reading1::Lines, FieldPlan::Point(attr)) => {
                let family = family_words(&attr.family);
                d.line_windows(
                    |w, l| forms::field_line(&self.who, attr, &family, w, l),
                    &format!("{} {}", self.item, attr.name),
                )
            }
            (Reading1::Words { text, n, step }, FieldPlan::Point(attr)) => {
                let family = family_words(&attr.family);
                let t = &d.body[text.clone()];
                let shown = if n.0 == n.1 && !matches!(self.who, Who::Claim { .. }) {
                    forms::shown_line(n.0, t)
                } else {
                    forms::shown_claim(*n, t)
                };
                word_questions(
                    d,
                    step,
                    &forms::field_head(&self.who, attr, &family),
                    &shown,
                    t,
                    &format!("{}'s {}", self.who_name(), attr.name),
                    &format!("{} {}", self.item, attr.name),
                )
            }
            _ => Vec::new(),
        }
    }

    fn who_name(&self) -> String {
        match &self.who {
            Who::Particular { of, .. } | Who::It { of } => format!("this {}", of.name),
            Who::Claim { kind, .. } => format!("this {}", kind.name),
        }
    }

    pub(super) fn take(&mut self, d: &Doc<'_>, answers: Vec<Answer>) {
        let item = self.item.clone();
        let evidence = d.body[self.at.clone()].to_string();
        match (
            std::mem::replace(&mut self.state, Reading1::Done),
            self.plan,
        ) {
            (Reading1::Ask, FieldPlan::Choose(attr)) => {
                let labels = with_none(attr.values.len());
                let field = match answers.first() {
                    Some(Ok(dist)) => match argmax(&labels, dist) {
                        (best, p) if best < attr.values.len() => {
                            debug!(document = d.document, item = %item, field = %attr.name, value = %attr.values[best], p, dist = %render(dist), "document_read/waves: chose");
                            supported(
                                Value::String(attr.values[best].clone()),
                                &evidence,
                                "reader_choose",
                            )
                        }
                        (_, p) => {
                            debug!(document = d.document, item = %item, field = %attr.name, p, dist = %render(dist), "document_read/waves: chose none of the values");
                            unknown(format!(
                                "the reader chose none of the values: {}",
                                render(dist)
                            ))
                        }
                    },
                    other => unknown(format!("refused: {:?}", other.map(|a| a.as_ref().err()))),
                };
                self.read = vec![(attr.name.clone(), field)];
            }
            (Reading1::Ask, FieldPlan::Pick(attr, of)) => {
                let field = match self.picked(d, of, &answers) {
                    Ok(c) => {
                        let (value, cites) = c.answer();
                        supported(Value::String(value), &cites, "reader_pick")
                    }
                    Err(why) => unknown(why),
                };
                self.read = vec![(attr.name.clone(), field)];
            }
            (Reading1::Ask, FieldPlan::Subject(_, of, keys)) => {
                let each = |read: &dyn Fn(usize) -> DocumentReadField| -> Vec<(String, DocumentReadField)> {
                    keys.iter().enumerate().map(|(i, k)| (k.name.clone(), read(i))).collect()
                };
                self.read = match self.picked(d, of, &answers) {
                    Ok(Candidate::Record {
                        identity, scalar, ..
                    }) if identity.len() == keys.len() => each(&|i| {
                        supported(Value::String(identity[i].clone()), scalar, "reader_pick")
                    }),
                    Ok(Candidate::Mention { text, .. }) if keys.len() == 1 => {
                        each(&|_| supported(Value::String(text.clone()), text, "reader_pick"))
                    }
                    Ok(other) => {
                        let why = format!(
                            "the picked subject ({}) fills no identity field",
                            other.shown(of)
                        );
                        debug!(document = d.document, item = %item, %why, "document_read/waves: subject");
                        each(&|_| unknown(why.clone()))
                    }
                    Err(why) => each(&|_| unknown(why.clone())),
                };
            }
            (Reading1::Lines, FieldPlan::Point(attr)) => match d.chosen_line(&answers, &item) {
                Err(refusal) => {
                    self.read = vec![(attr.name.clone(), unknown(format!("refused: {refusal}")))]
                }
                Ok(None) => {
                    debug!(document = d.document, item = %item, field = %attr.name, "document_read/waves: no line states it");
                    self.read = vec![(
                        attr.name.clone(),
                        unknown("not stated: the reader chose none of the document's lines"),
                    )];
                }
                Ok(Some(line)) => {
                    self.state = Reading1::Words {
                        text: line.start..line.end,
                        n: (line.n, line.n),
                        step: Words::Start,
                    }
                }
            },
            (Reading1::Words { text, n, step }, FieldPlan::Point(attr)) => {
                let t = &d.body[text.clone()];
                match word_answers(d, &step, t, &answers, &item) {
                    Err(refusal) => {
                        self.read =
                            vec![(attr.name.clone(), unknown(format!("refused: {refusal}")))]
                    }
                    Ok(Pointed::Asking(step)) => self.state = Reading1::Words { text, n, step },
                    Ok(Pointed::None) => {
                        self.read = vec![(
                            attr.name.clone(),
                            unknown("not stated: the reader pointed at none of the words"),
                        )]
                    }
                    Ok(Pointed::Span(span)) => {
                        let w = &t[span];
                        self.read = vec![(
                            attr.name.clone(),
                            match normalise(&attr.family, w) {
                                Ok(value) => {
                                    debug!(document = d.document, item = %item, field = %attr.name, words = %w, %value, "document_read/waves: pointed");
                                    supported(value, w, "reader_point")
                                }
                                Err(why) => unknown(format!("refused: {why}")),
                            },
                        )];
                    }
                }
            }
            (state, _) => self.state = state,
        }
    }

    fn picked<'c>(
        &self,
        d: &'c Doc<'_>,
        of: &str,
        answers: &[Answer],
    ) -> Result<&'c Candidate, String> {
        let offered = d
            .candidates
            .get(of)
            .ok_or_else(|| format!("not asked: the document offers no `{of}` candidate"))?;
        let labels = with_none(offered.shown.len());
        let dist = answers
            .first()
            .ok_or("no answer")?
            .as_ref()
            .map_err(|r| format!("refused: {r}"))?;
        match argmax(&labels, dist) {
            (best, p) if best < offered.shown.len() => {
                debug!(document = d.document, item = %self.item, of, picked = %offered.shown[best].shown(of), p, dist = %render(dist), "document_read/waves: picked");
                Ok(&offered.shown[best])
            }
            (_, p) => {
                debug!(document = d.document, item = %self.item, of, p, dist = %render(dist), "document_read/waves: picked none");
                Err(format!(
                    "the reader picked none of the candidates: {}",
                    render(dist)
                ))
            }
        }
    }
}

// ---------------------------------------------------------------- one act

enum Acting {
    Ask,
    Cite { round: Vec<usize>, fact: String },
    Done,
}

/// One referent's act of one kind: each protocol key asked in two wordings
/// (agreement decides, a difference is held), then, if any key agreed, the
/// sentence that shows it chosen among the document's units, a page of
/// [`LABELS`] at a time until one is left (`act.rs`'s rule).
pub(super) struct ActRead<'a> {
    pub(super) kind: usize,
    pub(super) particular: Option<(usize, usize)>,
    pub(super) named: Option<Particular>,
    pub(super) who: Who<'a>,
    pub(super) keys: Vec<&'a ClosedAttr>,
    pub(super) subject: &'a OntologyTypeDecl,
    pub(super) read: BTreeMap<String, Reading>,
    /// The cited unit's line indices.
    pub(super) cited: Option<Range<usize>>,
    state: Acting,
    item: String,
}

impl<'a> ActRead<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        kind: usize,
        particular: Option<(usize, usize)>,
        named: Option<Particular>,
        who: Who<'a>,
        keys: Vec<&'a ClosedAttr>,
        subject: &'a OntologyTypeDecl,
        item: String,
    ) -> Self {
        Self {
            kind,
            particular,
            named,
            who,
            keys,
            subject,
            read: BTreeMap::new(),
            cited: None,
            state: Acting::Ask,
            item,
        }
    }

    pub(super) fn ask(&self, d: &Doc<'_>) -> Vec<Q> {
        match &self.state {
            Acting::Ask => self
                .keys
                .iter()
                .flat_map(|key| {
                    (0..forms::ACT_WORDINGS).map(move |w| {
                        let labels = with_none(key.values.len());
                        d.ask(
                            forms::act(&self.who, key, w, &labels),
                            labels,
                            format!("{} {} w{}", self.item, key.name, w + 1),
                        )
                    })
                })
                .collect(),
            Acting::Cite { round, fact } => round
                .chunks(LABELS.len())
                .filter(|page| page.len() > 1)
                .enumerate()
                .map(|(p, page)| {
                    let labels: Vec<&'static str> = LABELS[..page.len()].to_vec();
                    let shown: Vec<&Unit> = page.iter().map(|&u| &d.units[u]).collect();
                    d.ask(
                        forms::cite(fact, &shown, &labels),
                        labels,
                        format!("{} cite of {} page {}", self.item, round.len(), p + 1),
                    )
                })
                .collect(),
            Acting::Done => Vec::new(),
        }
    }

    pub(super) fn take(&mut self, d: &Doc<'_>, answers: Vec<Answer>) {
        self.state = match std::mem::replace(&mut self.state, Acting::Done) {
            Acting::Ask => {
                for (key, pair) in self.keys.iter().zip(answers.chunks(forms::ACT_WORDINGS)) {
                    let labels = with_none(key.values.len());
                    let reading = act::reading(key, &labels, pair);
                    let dists: Vec<String> = pair
                        .iter()
                        .map(|a| a.as_ref().map_or_else(Clone::clone, render))
                        .collect();
                    debug!(document = d.document, item = %self.item, key = %key.name, ?reading, dists = %dists.join(" | "), "document_read/waves: act read");
                    self.read.insert(key.name.clone(), reading);
                }
                let agreed: Vec<(&ClosedAttr, &str)> = self
                    .keys
                    .iter()
                    .filter_map(|key| match self.read.get(&key.name) {
                        Some(Reading::Agreed(v)) => Some((*key, v.as_str())),
                        _ => None,
                    })
                    .collect();
                if agreed.is_empty() {
                    debug!(document = d.document, item = %self.item, "document_read/waves: no key agreed; no statement");
                    Acting::Done
                } else {
                    let fact = act::fact(self.subject, &agreed, self.named.as_ref());
                    let round: Vec<usize> = (0..d.units.len()).collect();
                    self.settle(d, round, fact)
                }
            }
            Acting::Cite { round, fact } => {
                let mut answers = answers.into_iter();
                let mut picks = Vec::new();
                for page in round.chunks(LABELS.len()) {
                    if page.len() == 1 {
                        picks.push(page[0]);
                        continue;
                    }
                    let labels: Vec<&str> = LABELS[..page.len()].to_vec();
                    match answers.next() {
                        Some(Ok(dist)) => {
                            let (best, p) = argmax(&labels, &dist);
                            debug!(document = d.document, item = %self.item, of = round.len(), sentence = %excerpt(&d.units[page[best]].text), p, dist = %render(&dist), "document_read/waves: cite");
                            picks.push(page[best]);
                        }
                        other => {
                            debug!(document = d.document, item = %self.item, refusal = ?other.map(|a| a.err()), "document_read/waves: cite refused; no statement");
                            return;
                        }
                    }
                }
                self.settle(d, picks, fact)
            }
            Acting::Done => Acting::Done,
        };
    }

    /// The tournament's next round, or its winner cited.
    fn settle(&mut self, d: &Doc<'_>, round: Vec<usize>, fact: String) -> Acting {
        match round.len() {
            0 => {
                debug!(document = d.document, item = %self.item, "document_read/waves: no sentence to cite; no statement");
                Acting::Done
            }
            1 => {
                let unit = &d.units[round[0]];
                debug!(document = d.document, item = %self.item, sentence = %excerpt(&unit.text), "document_read/waves: cited");
                self.cited = Some(unit.lines.clone());
                Acting::Done
            }
            _ => Acting::Cite { round, fact },
        }
    }
}
