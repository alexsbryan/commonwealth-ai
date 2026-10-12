// SPDX-License-Identifier: AGPL-3.0-or-later
//! The compiled reader's jobs (`waves.rs`): each a small state machine over
//! closed questions (enumerate what a document holds of one target; read one
//! field of a particular or claim; read an act and cite it). [`run`] asks
//! every job's next questions together, one forced-choice wave per round, so
//! a document costs as many requests as its deepest job has steps.

use std::collections::BTreeMap;
use std::ops::Range;

use tracing::{debug, info};

use super::about::{count_of, Count, Particular, MAX_PARTICULARS};
use super::act::Unit;
use super::passes::{argmax, excerpt, render, Line};
use super::pick::Candidates;
use super::point::words;
use super::waves_forms::{self as forms, Target, Who, COUNT_LABELS};
use super::waves_readings::{ActRead, Field};
use crate::enrichment::atlas::resolve_records::{decision_wave, LABELS, NONE};
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::InferenceFn;

/// A bound on the rounds one level of one document may take: what a document
/// may cost, not a number measured on any corpus. A job steps forward on
/// every answer, so a level ends long before it on any document read so far.
const MAX_ROUNDS: u32 = 64;

/// One document as the jobs ask about it.
pub(super) struct Doc<'a> {
    pub(super) infer: &'a InferenceFn,
    pub(super) document: &'a str,
    /// What every question opens with: the declared facts and the numbered
    /// document (`ask::opening`).
    pub(super) prefix: String,
    pub(super) body: &'a str,
    pub(super) lines: &'a [Line],
    /// The lines a question may point at: those no line class keeps out.
    pub(super) asked: Vec<&'a Line>,
    /// Sentence units of the asked lines, what a cite chooses among.
    pub(super) units: Vec<Unit>,
    /// Per referenced type, what a Pick offers; filled once mentions are known.
    pub(super) candidates: BTreeMap<String, Candidates>,
}

impl Doc<'_> {
    pub(super) fn ask(&self, suffix: String, labels: Vec<&'static str>, item: String) -> Q {
        Q {
            prompt: forms::question(&self.prefix, &suffix, &labels),
            labels,
            item,
        }
    }

    /// One line question per window of the asked lines.
    pub(super) fn line_windows(
        &self,
        render: impl Fn(&[(usize, &str)], &[&str]) -> String,
        item: &str,
    ) -> Vec<Q> {
        self.asked
            .chunks(LABELS.len())
            .enumerate()
            .map(|(w, chunk)| {
                let labels = with_none(chunk.len());
                let shown = forms::window(self.body, chunk);
                self.ask(
                    render(&shown, &labels),
                    labels,
                    format!("{item} lines {}", w + 1),
                )
            })
            .collect()
    }

    /// The asked line the earliest window chose, if any chose one.
    pub(super) fn chosen_line(
        &self,
        answers: &[Answer],
        item: &str,
    ) -> Result<Option<&Line>, String> {
        for (w, (chunk, answer)) in self.asked.chunks(LABELS.len()).zip(answers).enumerate() {
            let labels = with_none(chunk.len());
            let dist = answer.as_ref().map_err(Clone::clone)?;
            let (best, p) = argmax(&labels, dist);
            debug!(document = self.document, item, window = w, best, p, dist = %render(dist), "document_read/waves: line");
            if best < chunk.len() {
                return Ok(Some(chunk[best]));
            }
        }
        Ok(None)
    }
}

/// One question: its prompt, its labels, and what the trace names it.
pub(super) struct Q {
    pub(super) prompt: ChatPrompt,
    pub(super) labels: Vec<&'static str>,
    pub(super) item: String,
}

pub(super) type Answer = Result<BTreeMap<String, f64>, String>;

pub(super) fn with_none(n: usize) -> Vec<&'static str> {
    LABELS[..n].iter().copied().chain([NONE]).collect()
}

/// One step of word pointing: the start among windows of the words, then the
/// end among the words from there.
#[derive(Debug, Clone)]
pub(super) enum Words {
    Start,
    End { start: usize },
}

/// What one step of word pointing decided.
pub(super) enum Pointed {
    Asking(Words),
    Span(Range<usize>),
    None,
}

pub(super) fn word_questions(
    d: &Doc<'_>,
    step: &Words,
    head: &str,
    shown: &str,
    text: &str,
    what: &str,
    item: &str,
) -> Vec<Q> {
    let all = words(text);
    match step {
        Words::Start => all
            .chunks(LABELS.len())
            .enumerate()
            .map(|(w, chunk)| {
                let labels = with_none(chunk.len());
                let ws: Vec<&str> = chunk.iter().map(|r| &text[r.clone()]).collect();
                d.ask(
                    forms::word_start(head, shown, &ws, what, &labels),
                    labels,
                    format!("{item} start {}", w + 1),
                )
            })
            .collect(),
        Words::End { start } => {
            let tail = &all[*start..(*start + LABELS.len()).min(all.len())];
            let labels: Vec<&'static str> = LABELS[..tail.len()].to_vec();
            let ws: Vec<&str> = tail.iter().map(|r| &text[r.clone()]).collect();
            vec![d.ask(
                forms::word_end(head, shown, &ws, what, &labels),
                labels,
                format!("{item} end"),
            )]
        }
    }
}

pub(super) fn word_answers(
    d: &Doc<'_>,
    step: &Words,
    text: &str,
    answers: &[Answer],
    item: &str,
) -> Result<Pointed, String> {
    let all = words(text);
    match step {
        Words::Start => {
            for (w, (chunk, answer)) in all.chunks(LABELS.len()).zip(answers).enumerate() {
                let labels = with_none(chunk.len());
                let dist = answer.as_ref().map_err(Clone::clone)?;
                let (best, p) = argmax(&labels, dist);
                debug!(document = d.document, item, window = w, best, p, dist = %render(dist), "document_read/waves: start word");
                if best < chunk.len() {
                    let start = w * LABELS.len() + best;
                    if start + 1 == all.len() {
                        return Ok(Pointed::Span(all[start].clone()));
                    }
                    return Ok(Pointed::Asking(Words::End { start }));
                }
            }
            Ok(Pointed::None)
        }
        Words::End { start } => {
            let tail = &all[*start..(*start + LABELS.len()).min(all.len())];
            let labels: Vec<&str> = LABELS[..tail.len()].to_vec();
            let dist = answers
                .first()
                .ok_or("no answer")?
                .as_ref()
                .map_err(Clone::clone)?;
            let (best, p) = argmax(&labels, dist);
            debug!(document = d.document, item, best, p, dist = %render(dist), "document_read/waves: end word");
            Ok(Pointed::Span(all[*start].start..all[start + best].end))
        }
    }
}

// ---------------------------------------------------------------- enumerate

#[derive(Debug, Clone)]
enum Enumerating {
    Count,
    Lines,
    Words { line: usize, step: Words },
    Done,
}

/// What a document holds of one target: a count, then each one pointed at
/// where it is first named, described or stated, at most [`MAX_PARTICULARS`].
/// A claim is its line; a particular is a line and then its words there.
pub(super) struct Enumerate<'a> {
    pub(super) target: Target<'a>,
    /// The particular a kind's claims are counted about, when per particular.
    pub(super) within: Option<Who<'a>>,
    pub(super) count: Option<Count>,
    pub(super) found: Vec<Particular>,
    /// Whether any question it asked was answered.
    pub(super) answered: bool,
    /// Whether pointing stopped where the document said it did: false when it
    /// reached the cap or was refused (`about::About::complete`).
    pub(super) complete: bool,
    cap: usize,
    state: Enumerating,
    item: String,
}

impl<'a> Enumerate<'a> {
    /// Counted first at the document; per particular (`within`), pointed at
    /// until no line is chosen.
    pub(super) fn new(target: Target<'a>, within: Option<Who<'a>>, item: String) -> Self {
        let state = if within.is_some() {
            Enumerating::Lines
        } else {
            Enumerating::Count
        };
        Self {
            target,
            within,
            count: None,
            found: Vec::new(),
            answered: false,
            complete: false,
            cap: MAX_PARTICULARS,
            state,
            item,
        }
    }

    fn found_words(&self) -> Vec<(usize, String)> {
        self.found
            .iter()
            .map(|p| (p.line, p.words.clone()))
            .collect()
    }

    fn ask(&self, d: &Doc<'_>) -> Vec<Q> {
        match &self.state {
            Enumerating::Count => vec![d.ask(
                forms::count(self.target),
                COUNT_LABELS.to_vec(),
                format!("{} count", self.item),
            )],
            Enumerating::Lines => {
                let found = self.found_words();
                let item = format!("{} {}", self.item, self.found.len() + 1);
                d.line_windows(
                    |w, l| forms::line(self.target, self.within.as_ref(), &found, w, l),
                    &item,
                )
            }
            Enumerating::Words { line, step } => {
                let l = &d.lines[*line];
                let text = &d.body[l.start..l.end];
                word_questions(
                    d,
                    step,
                    &forms::naming_head(self.target),
                    &forms::shown_line(l.n, text),
                    text,
                    &forms::naming_what(self.target),
                    &format!("{} {}", self.item, self.found.len() + 1),
                )
            }
            Enumerating::Done => Vec::new(),
        }
    }

    fn take(&mut self, d: &Doc<'_>, answers: Vec<Answer>) {
        let item = format!("{} {}", self.item, self.found.len() + 1);
        self.answered |= answers.iter().any(Result::is_ok);
        self.state = match std::mem::replace(&mut self.state, Enumerating::Done) {
            Enumerating::Count => {
                let count = match answers.first() {
                    Some(Ok(dist)) => {
                        let (count, p) = count_of(dist);
                        debug!(document = d.document, item = %self.item, ?count, p, dist = %render(dist), "document_read/waves: count");
                        count
                    }
                    other => {
                        // A refused count is unjudged and pointed at as for several: whether it holds
                        // any is then the pointing's to say ("none of these lines"), as About's is.
                        debug!(document = d.document, item = %self.item, refusal = ?other.map(|a| a.as_ref().err()), "document_read/waves: count refused; pointed at as for several");
                        Count::Unjudged
                    }
                };
                self.count = Some(count);
                self.cap = match count {
                    Count::NotAbout => 0,
                    Count::One => 1,
                    _ => MAX_PARTICULARS,
                };
                if self.cap == 0 {
                    self.complete = true;
                    Enumerating::Done
                } else {
                    Enumerating::Lines
                }
            }
            Enumerating::Lines => match d.chosen_line(&answers, &item) {
                Err(refusal) => {
                    debug!(document = d.document, item = %item, %refusal, "document_read/waves: pointing refused");
                    Enumerating::Done
                }
                Ok(None) => {
                    self.complete = true;
                    Enumerating::Done
                }
                Ok(Some(line))
                    if self.found.iter().any(|p| p.line == line.n)
                        && matches!(self.target, Target::Kind(_)) =>
                {
                    debug!(document = d.document, item = %item, line = line.n, "document_read/waves: pointed at a line already found; no more");
                    Enumerating::Done
                }
                Ok(Some(line)) => match self.target {
                    Target::Kind(_) => {
                        let text = &d.body[line.start..line.end];
                        debug!(document = d.document, item = %item, line = line.n, text = %excerpt(text), "document_read/waves: claim");
                        self.found.push(Particular {
                            line: line.n,
                            span: line.start..line.end,
                            words: text.to_string(),
                        });
                        self.next()
                    }
                    _ => Enumerating::Words {
                        line: line.n - 1,
                        step: Words::Start,
                    },
                },
            },
            Enumerating::Words { line, step } => {
                let l = &d.lines[line];
                let text = &d.body[l.start..l.end];
                match word_answers(d, &step, text, &answers, &item) {
                    Err(refusal) => {
                        debug!(document = d.document, item = %item, %refusal, "document_read/waves: word pointing refused");
                        Enumerating::Done
                    }
                    Ok(Pointed::Asking(step)) => Enumerating::Words { line, step },
                    Ok(Pointed::None) => {
                        debug!(document = d.document, item = %item, line = l.n, "document_read/waves: no words pointed at on the chosen line");
                        Enumerating::Done
                    }
                    Ok(Pointed::Span(span)) => {
                        let span = l.start + span.start..l.start + span.end;
                        if self.found.iter().any(|p| p.span == span) {
                            debug!(document = d.document, item = %item, line = l.n, "document_read/waves: pointed at one already found; no more");
                            Enumerating::Done
                        } else {
                            debug!(document = d.document, item = %item, line = l.n, words = %excerpt(&d.body[span.clone()]), "document_read/waves: particular");
                            self.found.push(Particular {
                                line: l.n,
                                words: d.body[span.clone()].to_string(),
                                span,
                            });
                            self.next()
                        }
                    }
                }
            }
            Enumerating::Done => Enumerating::Done,
        };
    }

    fn next(&self) -> Enumerating {
        if self.found.len() >= self.cap {
            Enumerating::Done
        } else {
            Enumerating::Lines
        }
    }
}

// ---------------------------------------------------------------- rounds

/// Any job of the compiled reader.
pub(super) enum Job<'a> {
    Enumerate(Enumerate<'a>),
    Field(Field<'a>),
    Act(ActRead<'a>),
}

impl Job<'_> {
    fn ask(&mut self, d: &Doc<'_>) -> Vec<Q> {
        match self {
            Job::Enumerate(e) => e.ask(d),
            Job::Field(f) => f.ask(d),
            Job::Act(a) => a.ask(d),
        }
    }

    fn take(&mut self, d: &Doc<'_>, answers: Vec<Answer>) {
        match self {
            Job::Enumerate(e) => e.take(d, answers),
            Job::Field(f) => f.take(d, answers),
            Job::Act(a) => a.take(d, answers),
        }
    }
}

/// What one level of one document cost.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Spent {
    pub(super) questions: u32,
    pub(super) rounds: u32,
}

/// Ask `jobs` until none has a question: each round, every job's next
/// questions as one wave (`decision_wave` composes them, at most 64 to a
/// request), each job given its own answers back in order.
pub(super) async fn run(d: &Doc<'_>, jobs: &mut [Job<'_>], level: &str, spent: &mut Spent) {
    let mut rounds = 0u32;
    loop {
        let mut asked: Vec<(usize, usize)> = Vec::new();
        let mut questions: Vec<Q> = Vec::new();
        for (j, job) in jobs.iter_mut().enumerate() {
            let q = job.ask(d);
            if !q.is_empty() {
                asked.push((j, q.len()));
                questions.extend(q);
            }
        }
        if questions.is_empty() {
            break;
        }
        if rounds == MAX_ROUNDS {
            info!(document = d.document, level, rounds, pending = questions.len(), "document_read/waves: the level reached its round bound; its pending jobs end unasked");
            break;
        }
        rounds += 1;
        spent.questions += questions.len() as u32;
        let prompts: Vec<ChatPrompt> = questions.iter().map(|q| q.prompt.clone()).collect();
        let labels: Vec<Vec<&str>> = questions.iter().map(|q| q.labels.clone()).collect();
        let items: Vec<String> = questions.iter().map(|q| q.item.clone()).collect();
        debug!(
            document = d.document,
            level,
            round = rounds,
            questions = questions.len(),
            jobs = asked.len(),
            "document_read/waves: round"
        );
        let mut answers = decision_wave(d.infer, &prompts, &labels, d.document, &items)
            .await
            .into_iter()
            .map(|a| a.map_err(|r| format!("{r:?}")));
        for (j, n) in asked {
            let mine: Vec<Answer> = answers.by_ref().take(n).collect();
            jobs[j].take(d, mine);
        }
    }
    spent.rounds += rounds;
}
