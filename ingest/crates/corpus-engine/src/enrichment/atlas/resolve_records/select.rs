// SPDX-License-Identifier: AGPL-3.0-or-later
//! RESOLVE as forced choices: one question per statement, "which of these
//! records is it about, or none?", answered in one forward pass as a
//! distribution over single-token labels (`oicp_types::forced_choice`), never
//! generated. Statements are asked in document order, and a record a statement
//! of this document opened is a candidate for the ones after it, so grouping
//! within a document needs no second question.
//!
//! The model is asked whenever a record is offered, and its most probable
//! label decides: a record it picks over every other and over none
//! (`Decision::Chosen`), or none, which opens one. The sources `weigh.rs`
//! weighs at their corpus-estimated weights are kept beside the choice, never
//! over it: a statement no candidate was offered for is the only one they
//! decide. Operator delivery review, 2026-10-12: on Ward the weighed decider
//! overrode 36 of the model's 39 picks of an offered company and linked 42
//! statements no model was asked about, so it earns no authority it has not
//! shown in the workflow. The full distribution is kept on every outcome.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;

use oicp_types::forced_choice;
use tracing::{debug, warn};

use super::answer::{describe, describe_opened, reasons, Proposed, ProposedVerdict};
use super::apart::{
    beside_in_document, chapters_compatible, necessary_compatible, necessary_conflict, offerable,
    read_beside,
};
use super::estimate::{Comparison, Estimate};
use super::fields::{self, compare_necessary};
use super::weigh::{tally, weigh, Carried, Zone};
use super::{
    context, marked_context, Choice, Criterion, Document, Held, Plan, Proposal, Record, Refusal,
    Statement,
};
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::InferenceFn;

const SYSTEM: &str = include_str!("../resolve_select_prompt.md");

/// Candidate labels, single tokens on the tokenizers in use; `NONE` answers
/// "none of them". At most `LABELS.len()` candidates are shown, a cost cap.
pub(crate) const LABELS: [&str; 25] = [
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S",
    "T", "U", "V", "W", "X", "Y",
];
pub(crate) const NONE: &str = "0";

/// What a candidate is: an open record a proposer offered, or a record an
/// earlier statement of this document opened (its position in `asked`).
#[derive(Clone, Copy)]
pub(super) enum Candidate<'p> {
    Record(usize, &'p Proposal),
    Opened(usize),
}

/// The plans for the asked statements, the choice behind each, and the calls made.
pub(super) struct Chosen {
    pub plans: Vec<Plan>,
    pub choices: Vec<Option<Choice>>,
    pub calls: u32,
    /// Candidates not offered because a supplied necessary value differs.
    pub vetoed: u32,
    /// Every (statement, alternative) comparison weighed, for the estimate.
    pub seen: Vec<Comparison>,
    /// Per source, what it carried (`Carried`).
    pub carried: BTreeMap<String, Carried>,
}

/// RESOLVE's census funnel: every forced-choice call RESOLVE makes is issued
/// here, timed and traced with its whole distribution, and counted by the
/// caller in `DocumentResolution::calls`. An answer that is not a
/// distribution over every asked label is a refusal, never a default: a label
/// left out is not read as probability 0 (`cargo xtask judge-funnel-gate`).
pub(crate) async fn decision_call(
    infer: &InferenceFn,
    prompt: &ChatPrompt,
    labels: &[&str],
    document: &str,
    statement: &str,
) -> Result<BTreeMap<String, f64>, Refusal> {
    let started = Instant::now();
    let out = infer(prompt, None).await;
    let ms = started.elapsed().as_millis() as u64;
    let phase = &prompt.phase_id;
    match out {
        Ok(raw) => match forced_choice::parse(&raw) {
            Some(d) if labels.iter().all(|l| d.contains_key(*l)) => {
                debug!(document, statement, ?phase, ms, distribution = ?d, "atlas/resolve: decision call");
                Ok(d)
            }
            Some(d) => {
                let missing: Vec<&str> = labels
                    .iter()
                    .copied()
                    .filter(|l| !d.contains_key(*l))
                    .collect();
                warn!(
                    document,
                    statement,
                    ?phase,
                    ms,
                    ?missing,
                    "atlas/resolve: decision call left labels out"
                );
                Err(Refusal::NoAnswer {
                    reason: format!("distribution lacks label(s) {missing:?}"),
                })
            }
            None => {
                let head: String = raw.chars().take(120).collect();
                warn!(document, statement, ?phase, ms, %head, "atlas/resolve: decision call answered no distribution");
                Err(Refusal::NoAnswer {
                    reason: format!("not a forced-choice distribution: {head:?}"),
                })
            }
        },
        Err(e) => {
            warn!(document, statement, ?phase, ms, error = %e, "atlas/resolve: decision call failed");
            Err(Refusal::NoAnswer {
                reason: format!("call failed: {e:#}"),
            })
        }
    }
}

/// An alternative a statement is weighed over: an offered record, or a record
/// an earlier statement of this document opened (its position in `asked`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Alt {
    Record(usize),
    Opened(usize),
}

impl Candidate<'_> {
    fn alt(&self) -> Alt {
        match *self {
            Candidate::Record(r, _) => Alt::Record(r),
            Candidate::Opened(g) => Alt::Opened(g),
        }
    }
}

/// Keep what a plan joins, a record or one this document opened (its
/// opener included), by the statement's window reference, for the statements
/// of its document after it: both are weighed by everything they hold.
fn note_join(joined: &mut HashMap<Alt, BTreeSet<String>>, plan: &Plan, mine: Option<&BTreeSet<String>>) {
    if let (Some(a), Some(mine)) = (alt_of(plan), mine) {
        joined.entry(a).or_default().extend(mine.iter().cloned());
    }
}

/// Where an earlier asked statement's plan put it; `None` when held or refused.
fn alt_of(plan: &Plan) -> Option<Alt> {
    match plan {
        Plan::Key { record, .. }
        | Plan::Join { record, .. }
        | Plan::Weighed { record, .. }
        | Plan::Chosen { record, .. } => Some(Alt::Record(*record)),
        Plan::Open { group, .. } => Some(Alt::Opened(*group)),
        Plan::Held(_) | Plan::Refuse(_) => None,
    }
}

/// `a`'s position among `alts`, added at the end when no candidate offered it.
fn place(alts: &mut Vec<Alt>, a: Alt) -> usize {
    match alts.iter().position(|&x| x == a) {
        Some(at) => at,
        None => {
            alts.push(a);
            alts.len() - 1
        }
    }
}

/// What the proposed answer says of one statement.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Say {
    Names(Alt),
    /// It proposes none of the alternatives: it disagrees with each.
    NoneOf,
    /// It names an earlier statement that was held or refused: silent.
    Silent,
}

/// The inputs every statement of one document is weighed with.
pub(super) struct Weighing<'a> {
    pub asked: &'a [usize],
    pub shown: &'a [(usize, &'a Proposal)],
    pub records: &'a [Record],
    pub key_edges: &'a [(usize, usize)],
    /// Per asked statement: necessary values supplied or READ, and supplied alone.
    pub read: &'a [BTreeMap<String, BTreeSet<String>>],
    pub supplied: &'a [BTreeMap<String, BTreeSet<String>>],
    pub proposed: &'a Proposed,
    /// Records holding one of the document's declared field values (`fields::named`).
    pub field_named: &'a [usize],
    pub estimate: &'a Estimate,
}

/// Ask each asked statement, in document order, which candidate it is about,
/// and decide it with every source (`weigh.rs`) at the weights estimated on
/// this corpus (`estimate.rs`). A candidate whose SUPPLIED necessary value
/// differs is not offered; a READ value that differs is one more source,
/// weighed. Each source speaks on each alternative or is silent: the declared
/// document fields (`fields.rs`), each necessary value, the proposed answer,
/// and the model's most probable candidate (agreeing with it, disagreeing
/// with every other shown, all of them when it chose none). The model is
/// asked only where the other sources do not already link. The posterior
/// against the type's bar links, opens a record, or holds the statement.
pub(super) async fn choose(
    criterion: &Criterion,
    doc: Document<'_>,
    statements: &[Statement],
    w: &Weighing<'_>,
    reason: bool,
    infer: &InferenceFn,
) -> Chosen {
    let (asked, records, estimate) = (w.asked, w.records, w.estimate);
    let mut plans: Vec<Plan> = Vec::with_capacity(asked.len());
    let mut choices: Vec<Option<Choice>> = Vec::with_capacity(asked.len());
    let mut opened: Vec<usize> = Vec::new();
    let mut seen: Vec<Comparison> = Vec::new();
    let mut carried: BTreeMap<String, Carried> = BTreeMap::new();
    let mut calls = 0;
    let mut vetoed = 0;
    let model_source = if reason {
        "reasoned_choice"
    } else {
        "model_choice"
    };
    let prior = estimate.prior_log_odds();
    let id_of = |a: Alt| match a {
        Alt::Record(r) => records[r].id.clone(),
        Alt::Opened(g) => statements[asked[g]].id.clone(),
    };
    // The window particulars this document's statements are about, and the
    // records its earlier statements joined, by their references: folded
    // only after the document, so kept here as it is decided (`read_beside`).
    let in_document: BTreeSet<String> = statements
        .iter()
        .filter_map(|s| s.keys.get(super::CHAPTER_SUBJECT_KEY).cloned())
        .collect();
    let mut joined: HashMap<Alt, BTreeSet<String>> = HashMap::new();
    for (j, &i) in asked.iter().enumerate() {
        let mine = w.supplied[j].get(super::CHAPTER_SUBJECT_KEY);
        // A declared key shared with an earlier statement of this document
        // settles it: the same plan, no question.
        if let Some(&(a, _)) = w.key_edges.iter().find(|&&(a, b)| b == j && a < j) {
            plans.push(plans[a].clone());
            choices.push(None);
            note_join(&mut joined, &plans[j], mine);
            continue;
        }
        let beside = |r: usize| {
            let mut held = records[r]
                .supplied
                .get(super::CHAPTER_SUBJECT_KEY)
                .cloned()
                .unwrap_or_default();
            held.extend(joined.get(&Alt::Record(r)).into_iter().flatten().cloned());
            read_beside(mine, &held, &in_document).cloned()
        };
        // A record this document opened holds its opener's values and the
        // window references of every statement that has joined it since, as
        // the fold will (`gate_plans`): weighed by its opener's alone, a
        // particular's window-mate was offered it, chose it and was refused.
        let opened_holds = |g: usize| {
            let mut holds = w.supplied[g].clone();
            if let Some(joiners) = joined.get(&Alt::Opened(g)) {
                holds
                    .entry(super::CHAPTER_SUBJECT_KEY.to_string())
                    .or_default()
                    .extend(joiners.iter().cloned());
            }
            holds
        };
        let compatible = |a: Alt| match a {
            Alt::Record(r) => {
                offerable(criterion, &w.supplied[j], &records[r].supplied) && beside(r).is_none()
            }
            Alt::Opened(g) => necessary_compatible(criterion, &w.supplied[j], &opened_holds(g)),
        };
        let mut candidates: Vec<Candidate> = opened.iter().map(|&g| Candidate::Opened(g)).collect();
        candidates.extend(w.shown.iter().map(|&(r, p)| Candidate::Record(r, p)));
        let offered = candidates.len();
        for &g in &opened {
            if let Some(conflict) = necessary_conflict(criterion, &w.supplied[j], &opened_holds(g)) {
                debug!(document = doc.id, statement = %statements[i].id, candidate = %statements[asked[g]].id, %conflict, "atlas/resolve: a record this document opened holds what keeps this statement out; not offered");
            }
        }
        for &(r, _) in w.shown {
            if let Some(other) = beside(r) {
                debug!(document = doc.id, statement = %statements[i].id, candidate = %records[r].id, beside = %other, "atlas/resolve: the record holds a particular the reader read beside this one in its document; not offered");
            }
        }
        candidates.retain(|c| compatible(c.alt()));
        if candidates.len() < offered {
            let n = offered - candidates.len();
            vetoed += n as u32;
            debug!(document = doc.id, statement = %statements[i].id, vetoed = n, "atlas/resolve: candidates whose supplied necessary value differs are not offered");
        }
        candidates.truncate(LABELS.len());
        let mut alts: Vec<Alt> = candidates.iter().map(Candidate::alt).collect();
        for &r in w.field_named {
            if compatible(Alt::Record(r)) {
                place(&mut alts, Alt::Record(r));
            } else {
                vetoed += 1;
                debug!(document = doc.id, statement = %statements[i].id, candidate = %records[r].id, route = "evidential_field", beside = ?beside(r), "atlas/resolve: a supplied necessary value differs, or the record holds a particular read beside this one; the field's record is not weighed");
            }
        }
        // The proposed answer: an offered record, or where an earlier
        // statement of its wording went.
        let say = match w.proposed.verdict(j) {
            Some(ProposedVerdict::Record(r)) if alts.contains(&Alt::Record(r)) => {
                Say::Names(Alt::Record(r))
            }
            Some(ProposedVerdict::Record(_)) => Say::NoneOf,
            Some(ProposedVerdict::Earlier(f)) => match alt_of(&plans[f]) {
                Some(a) if compatible(a) => Say::Names(a),
                _ => Say::Silent,
            },
            None => Say::NoneOf,
        };
        if let Say::Names(a) = say {
            place(&mut alts, a);
        }
        let mut comps: Vec<Comparison> = alts
            .iter()
            .map(|&a| {
                let mut c = Comparison::new();
                match a {
                    Alt::Record(r) => {
                        fields::compare(doc, &records[r], &mut c);
                        compare_necessary(criterion, &w.read[j], &records[r].fields, &mut c);
                    }
                    // One document: its fields say nothing about two of its statements.
                    Alt::Opened(g) => compare_necessary(criterion, &w.read[j], &w.read[g], &mut c),
                }
                match say {
                    Say::Names(n) => {
                        c.insert("proposed_answer".into(), n == a);
                    }
                    Say::NoneOf => {
                        c.insert("proposed_answer".into(), false);
                    }
                    Say::Silent => {}
                }
                c
            })
            .collect();
        let evidence = |comps: &[Comparison]| -> Vec<f64> {
            comps.iter().map(|c| estimate.evidence(c)).collect()
        };
        let raised = |comps: &[Comparison]| -> Vec<bool> {
            comps.iter().map(|c| estimate.supports(c)).collect()
        };
        let mut choice: Option<Choice> = None;
        // The model is asked whenever a record is offered, so no statement
        // links on fitted weights alone; what it picks is `picked`.
        let mut picked: Option<Option<Alt>> = None;
        if !candidates.is_empty() {
            let Some(same_when) = criterion.same_when.as_deref() else {
                plans.push(Plan::Refuse(Refusal::NoCriterion));
                choices.push(None);
                continue;
            };
            let s = &statements[i];
            let labels: Vec<&str> = LABELS[..candidates.len()]
                .iter()
                .copied()
                .chain([NONE])
                .collect();
            let ask = |a: Ask<'_>| {
                question(
                    criterion,
                    same_when,
                    doc,
                    s,
                    &candidates,
                    &labels,
                    statements,
                    asked,
                    records,
                    a,
                )
            };
            let reasoning = if reason {
                calls += 1;
                match reason_call(infer, &ask(Ask::Reason), doc.id, &s.id).await {
                    Ok(text) => Some(text),
                    Err(refusal) => {
                        plans.push(Plan::Refuse(refusal));
                        choices.push(None);
                        continue;
                    }
                }
            } else {
                None
            };
            let prompt = match &reasoning {
                Some(text) => ask(Ask::ChooseAfter(text)),
                None => ask(Ask::Choose),
            };
            calls += 1;
            let dist = match decision_call(infer, &prompt, &labels, doc.id, &s.id).await {
                Ok(dist) => dist,
                Err(refusal) => {
                    plans.push(Plan::Refuse(refusal));
                    choices.push(None);
                    continue;
                }
            };
            // decision_call refused any answer that left a label out.
            let p = |label: &str| dist[label];
            choice = Some(Choice {
                candidates: candidates
                    .iter()
                    .zip(LABELS)
                    .map(|(c, l)| (id_of(c.alt()), p(l)))
                    .collect(),
                none: p(NONE),
                reasoning,
            });
            // The most probable label; a tie goes to the later one, so to none.
            let (best, _) = candidates
                .iter()
                .zip(LABELS)
                .map(|(c, l)| (Some(c.alt()), p(l)))
                .chain([(None, p(NONE))])
                .fold(
                    (None, f64::NEG_INFINITY),
                    |a, b| if b.1 >= a.1 { b } else { a },
                );
            // The model spoke on the candidates it was shown, the first
            // `candidates.len()` alternatives; the rest it never saw.
            for (k, c) in comps.iter_mut().enumerate().take(candidates.len()) {
                c.insert(model_source.into(), best == Some(alts[k]));
            }
            picked = Some(best);
        }
        let weighed = weigh(&evidence(&comps), &raised(&comps), prior, criterion.bar);
        // The model's forced choice decides an asked statement, its pick or
        // none (operator delivery review, 2026-10-12: fitted weights are not
        // an authority; on Ward they overrode 36 of the model's 39 picks of an
        // offered company). The weighed posterior is kept beside it.
        let zone = match picked {
            Some(Some(alt)) => {
                let at = alts
                    .iter()
                    .position(|&a| a == alt)
                    .expect("the model picks among the alternatives it was shown");
                Zone::Link(at, weighed.posterior[at])
            }
            Some(None) => Zone::NoLink,
            None => weighed.zone,
        };
        tally(&mut carried, &comps, estimate, prior, criterion.bar, zone);
        // Each alternative by its record id beside its comparison, so the
        // estimate can be read against gold pair by pair (layer-estimator).
        let named: Vec<String> = alts.iter().map(|&a| id_of(a)).collect();
        debug!(document = doc.id, statement = %statements[i].id, alternatives = ?named, comparisons = ?comps, zone = ?weighed.zone, "atlas/resolve: the sources weighed");
        if picked.is_some() {
            debug!(document = doc.id, statement = %statements[i].id, decided = ?zone, weighed = ?weighed.zone, "atlas/resolve: the model's choice decides; the weighed zone is kept beside it");
        }
        let decided = match zone {
            Zone::Link(at, posterior) => match alts[at] {
                Alt::Record(record) if picked.is_some() => Plan::Chosen {
                    record,
                    posterior,
                    votes: fields::votes_for(&comps[at], &records[record].id, estimate),
                },
                Alt::Record(record) => Plan::Weighed {
                    record,
                    posterior,
                    votes: fields::votes_for(&comps[at], &records[record].id, estimate),
                },
                Alt::Opened(group) => Plan::Open { group, cite: None },
            },
            Zone::NoLink => {
                opened.push(j);
                Plan::Open {
                    group: j,
                    cite: None,
                }
            }
            Zone::Unsettled => Plan::Held(Held {
                alternatives: alts
                    .iter()
                    .map(|&a| id_of(a))
                    .zip(weighed.posterior.iter().copied())
                    .collect(),
                none: weighed.none,
                sources: alts
                    .iter()
                    .zip(&comps)
                    .flat_map(|(&a, c)| fields::votes_for(c, &id_of(a), estimate))
                    .collect(),
                read: w.read[j].clone(),
                supplied: w.supplied[j].clone(),
            }),
        };
        seen.extend(comps);
        plans.push(decided);
        choices.push(choice);
        note_join(&mut joined, &plans[j], mine);
    }
    Chosen {
        plans,
        choices,
        calls,
        vetoed,
        seen,
        carried,
    }
}

/// The one-statement question. The type, criterion and document come first
/// and are the same for every statement of the document, so the engine can
/// keep them prefilled; the candidates and the statement follow.
#[allow(clippy::too_many_arguments)]
pub(super) fn question(
    criterion: &Criterion,
    same_when: &str,
    doc: Document<'_>,
    s: &Statement,
    candidates: &[Candidate],
    labels: &[&str],
    statements: &[Statement],
    asked: &[usize],
    records: &[Record],
    ask: Ask<'_>,
) -> ChatPrompt {
    let mut u = format!("Type: {}", criterion.type_name);
    if !criterion.description.is_empty() {
        u.push_str(&format!(" ({})", criterion.description));
    }
    u.push_str(&format!(
        "\nSame particular when: {}\n\nDocument{}:\n<<<\n{}\n>>>\n\nCandidate records:\n",
        same_when,
        match doc.title {
            Some(t) => format!(" titled {t:?}"),
            None => String::new(),
        },
        doc.body
    ));
    let mut offered = Vec::with_capacity(candidates.len());
    let mine: Option<BTreeSet<String>> = s
        .keys
        .get(super::CHAPTER_SUBJECT_KEY)
        .map(|v| BTreeSet::from([v.clone()]));
    for (c, label) in candidates.iter().zip(LABELS) {
        let line = match *c {
            Candidate::Record(r, p) => {
                let mut why = reasons(p);
                if !chapters_compatible(
                    mine.as_ref(),
                    records[r].supplied.get(super::CHAPTER_SUBJECT_KEY),
                ) {
                    why.push_str(
                        "; the window's reader read this record's particular as another than this statement's",
                    );
                }
                describe(&records[r], &why, doc.id)
            }
            Candidate::Opened(g) => {
                let o = &statements[asked[g]];
                // Asked spans were checked readable in `resolve_document`.
                describe_opened(
                    &doc.body[o.start..o.end],
                    &context(doc.body, o.start, o.end),
                    &o.described.render(),
                )
            }
        };
        u.push_str(&format!("{label} {line}"));
        offered.push(format!("{label} {}", line.trim_end()));
    }
    let shown = marked_context(doc.body, s.start, s.end);
    let read_as = s.described.render();
    debug!(document = doc.id, statement = %s.id, offered = ?offered, shown = %shown, read_as = %read_as, "atlas/resolve: the records a question offers");
    u.push_str(&format!("\nStatement, its words in [[ ]]: \"…{shown}…\"\n"));
    if !read_as.is_empty() {
        u.push_str(&format!("Its reader read what it is about as: {read_as}\n"));
    }
    // Its window's other particulars in this document, as read (absence shown
    // as absence): the first of two siblings asked can leave a record that
    // fits the other, which `read_beside` then keeps from it.
    for (reference, described) in beside_in_document(s, statements) {
        let read = described.render();
        if !read.is_empty() {
            debug!(document = doc.id, statement = %s.id, beside = reference, read = %read, "atlas/resolve: the question shows a particular read beside the statement");
            u.push_str(&format!("Read in this document, as another particular: {read}. The two are different particulars; a record is at most one of them.\n"));
        }
    }
    u.push('\n');
    match ask {
        Ask::Reason => {
            u.push_str(&format!("Which record is the statement about? Reason it through under the rule, then end with its letter, or {NONE} if none of them."));
            return ChatPrompt::new(SYSTEM, u)
                .with_phase_id("resolve_reason")
                .with_max_output_tokens(REASON_TOKENS)
                .with_temperature(0.0);
        }
        Ask::ChooseAfter(reasoning) => u.push_str(&format!("Your reasoning:\n{reasoning}\n\n")),
        Ask::Choose => {}
    }
    u.push_str(&format!(
        "Which record is the statement about? Answer with its letter, or {NONE} if none of them."
    ));
    ChatPrompt::new(SYSTEM, u)
        .with_response_schema("select", forced_choice::schema(labels))
        .with_phase_id("resolve_select")
        .with_temperature(0.0)
}

/// How a question closes: one forced choice, the model's reasoning about it
/// (generated, bounded by `REASON_TOKENS`), or one forced choice read after
/// that reasoning (`reasoned_choice`).
#[derive(Clone, Copy)]
pub(super) enum Ask<'a> {
    Choose,
    Reason,
    ChooseAfter(&'a str),
}

/// The reasoning budget: a cost knob, it decides nothing. Ward's reasoned
/// answers ran to about 900 tokens; a cut answer is still read as a choice.
pub(crate) const REASON_TOKENS: u32 = 2000;

/// The reasoning before a reasoned choice: one bounded generation, traced
/// and counted by the caller. Empty or failed, the statement is refused.
pub(super) async fn reason_call(
    infer: &InferenceFn,
    prompt: &ChatPrompt,
    document: &str,
    statement: &str,
) -> Result<String, Refusal> {
    let started = Instant::now();
    let out = infer(prompt, Some(REASON_TOKENS)).await;
    let ms = started.elapsed().as_millis() as u64;
    match out {
        Ok(text) if !text.trim().is_empty() => {
            // The reasoning itself, escaped onto one line: why a statement was
            // joined or opened is read from it, not inferred from the choice.
            debug!(
                document,
                statement,
                ms,
                chars = text.len(),
                reasoning = ?text,
                "atlas/resolve: reasoning call"
            );
            Ok(text)
        }
        Ok(_) => {
            warn!(
                document,
                statement, ms, "atlas/resolve: reasoning call answered nothing"
            );
            Err(Refusal::NoAnswer {
                reason: "the reasoning was empty".into(),
            })
        }
        Err(e) => {
            warn!(document, statement, ms, error = %e, "atlas/resolve: reasoning call failed");
            Err(Refusal::NoAnswer {
                reason: format!("reasoning call failed: {e:#}"),
            })
        }
    }
}
