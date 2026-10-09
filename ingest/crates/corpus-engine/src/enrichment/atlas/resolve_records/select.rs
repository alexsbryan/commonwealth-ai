// SPDX-License-Identifier: AGPL-3.0-or-later
//! RESOLVE as forced choices: one question per statement, "which of these
//! records is it about, or none?", answered in one forward pass as a
//! distribution over single-token labels (`oicp_types::forced_choice`), never
//! generated. Statements are asked in document order, and a record a statement
//! of this document opened is a candidate for the ones after it, so grouping
//! within a document needs no second question.
//!
//! Ring 0 of the identity plan (`research/ontology-apps/resolve-prereg.md`):
//! unmeasured, the most probable label decides, so the information the choice
//! carries can be measured. Measured, its argmax is one source the decider
//! weighs with the rest (Ring 2, `weigh.rs`); the full distribution is kept on
//! every outcome.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;

use oicp_types::forced_choice;
use tracing::{debug, warn};

use super::answer::{describe, describe_opened, reasons, Proposed, ProposedVerdict};
use super::fields::FieldVote;
use super::weigh::{weigh, Vote, Zone};
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
enum Candidate<'p> {
    Record(usize, &'p Proposal),
    Opened(usize),
}

/// The plans for the asked statements, the choice behind each, and the calls made.
pub(super) struct Chosen {
    pub plans: Vec<Plan>,
    pub choices: Vec<Option<Choice>>,
    pub calls: u32,
    /// Candidates not offered because a necessary value differs.
    pub vetoed: u32,
}

/// Two value sets are compatible when every necessary attribute evidenced on
/// both sides has exactly one shared value. Missing evidence is permissive;
/// contaminated multi-value sets match neither of their members.
pub(super) fn necessary_compatible(
    criterion: &Criterion,
    left: &BTreeMap<String, BTreeSet<String>>,
    right: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    criterion.necessary.iter().all(|attr| {
        let Some(left) = left.get(&attr.name).filter(|values| !values.is_empty()) else {
            return true;
        };
        let Some(right) = right.get(&attr.name).filter(|values| !values.is_empty()) else {
            return true;
        };
        left.len() == 1 && right.len() == 1 && left.first() == right.first()
    })
}

/// Prior records matching a statement's declared sufficient keys.
pub(super) fn key_hits(
    by_key: &HashMap<(String, String), usize>,
    keys: &[Vec<(String, String)>],
) -> Vec<Vec<(usize, String, String)>> {
    keys.iter()
        .map(|keys| {
            keys.iter()
                .filter_map(|(key, value)| {
                    by_key
                        .get(&(key.clone(), value.clone()))
                        .map(|&record| (record, key.clone(), value.clone()))
                })
                .collect()
        })
        .collect()
}

/// A sufficient key settles only when its record's necessary values agree.
pub(super) fn key_plan(
    criterion: &Criterion,
    statement: &Statement,
    hits: &[(usize, String, String)],
    read: &BTreeMap<String, BTreeSet<String>>,
    records: &[Record],
    document: &str,
    vetoed: &mut u32,
) -> Option<Plan> {
    let held: BTreeSet<usize> = hits.iter().map(|(record, _, _)| *record).collect();
    match (held.len(), hits.first()) {
        (0, _) | (_, None) => None,
        (1, Some((record, key, value))) => {
            if necessary_compatible(criterion, read, &records[*record].fields) {
                Some(Plan::Key {
                    record: *record,
                    key: key.clone(),
                    value: value.clone(),
                })
            } else {
                *vetoed += 1;
                debug!(
                    document,
                    statement = %statement.id,
                    candidate = %records[*record].id,
                    route = "sufficient_key",
                    "atlas/resolve: necessary-field conflict vetoed the key join"
                );
                Some(Plan::Refuse(Refusal::Contradiction {
                    targets: vec![records[*record].id.clone()],
                }))
            }
        }
        _ => Some(Plan::Refuse(Refusal::Contradiction {
            targets: held
                .iter()
                .map(|&record| records[record].id.clone())
                .collect(),
        })),
    }
}

/// Apply the fields' plan (`fields::settle`) only where necessary values are compatible.
pub(super) fn settle_field(
    criterion: &Criterion,
    statements: &[Statement],
    read: &[BTreeMap<String, BTreeSet<String>>],
    records: &[Record],
    plans: &mut [Option<Plan>],
    field: Option<Plan>,
    document: &str,
    vetoed: &mut u32,
) {
    let Some(Plan::Weighed { record, .. }) = field.as_ref() else {
        return;
    };
    let record = *record;
    for (i, plan) in plans.iter_mut().enumerate() {
        if plan.is_some() {
            continue;
        }
        if necessary_compatible(criterion, &read[i], &records[record].fields) {
            *plan = field.clone();
        } else {
            *vetoed += 1;
            debug!(
                document,
                statement = %statements[i].id,
                candidate = %records[record].id,
                route = "evidential_field",
                "atlas/resolve: necessary-field conflict vetoed the field join"
            );
        }
    }
}

/// Recheck every answerer join and every same-document opened group before fold.
pub(super) fn gate_plans(
    criterion: &Criterion,
    statements: &[Statement],
    asked: &[usize],
    read: &[BTreeMap<String, BTreeSet<String>>],
    records: &[Record],
    decided: Vec<Plan>,
    document: &str,
    vetoed: &mut u32,
) -> Vec<Plan> {
    let mut open_groups: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut gated = Vec::with_capacity(decided.len());
    for (j, mut plan) in decided.into_iter().enumerate() {
        let i = asked[j];
        let target = match &plan {
            Plan::Key { record, .. }
            | Plan::Join { record, .. }
            | Plan::Selected { record, .. }
            | Plan::Weighed { record, .. } => Some(*record),
            Plan::Open { .. } | Plan::Held(_) | Plan::Refuse(_) => None,
        };
        if let Some(record) = target {
            if !necessary_compatible(criterion, &read[i], &records[record].fields) {
                *vetoed += 1;
                debug!(
                    document,
                    statement = %statements[i].id,
                    candidate = %records[record].id,
                    "atlas/resolve: necessary-field conflict vetoed the answerer join"
                );
                plan = Plan::Refuse(Refusal::Contradiction {
                    targets: vec![records[record].id.clone()],
                });
            }
        }
        let open_group = match &plan {
            Plan::Open { group, .. } => Some(*group),
            _ => None,
        };
        if let Some(group) = open_group {
            let conflict = open_groups.get(&group).and_then(|prior| {
                prior
                    .iter()
                    .copied()
                    .find(|&prior| !necessary_compatible(criterion, &read[i], &read[prior]))
            });
            if let Some(prior) = conflict {
                *vetoed += 1;
                debug!(
                    document,
                    statement = %statements[i].id,
                    candidate = %statements[prior].id,
                    "atlas/resolve: necessary-field conflict vetoed the within-document join"
                );
                plan = Plan::Refuse(Refusal::Contradiction {
                    targets: vec![statements[prior].id.clone()],
                });
            } else {
                open_groups.entry(group).or_default().push(i);
            }
        }
        gated.push(plan);
    }
    gated
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
#[derive(Clone, Copy, PartialEq, Debug)]
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

/// Where an earlier asked statement's plan put it; `None` when held or refused.
fn alt_of(plan: &Plan) -> Option<Alt> {
    match plan {
        Plan::Key { record, .. }
        | Plan::Join { record, .. }
        | Plan::Selected { record, .. }
        | Plan::Weighed { record, .. } => Some(Alt::Record(*record)),
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

/// Ask each asked statement, in document order, which candidate it is about,
/// and decide it with every source (Ring 2, `weigh.rs`). A candidate whose
/// necessary value differs from the statement's, both supplied or READ
/// (`read.rs`), is not offered. Each source names one alternative or is
/// silent: a field (`fields.rs`), the proposed answer, the model's most
/// probable candidate (its "none" is silent: no precision is measured for
/// it). Their posterior against the type's bar links, opens a record (no
/// source raised a candidate), or holds the statement unsettled. The model is
/// asked as before Ring 2: where its measured precision clears the bar, or
/// unmeasured (Ring 0, where its argmax decides so it can be measured), and
/// not where the fields alone link.
#[allow(clippy::too_many_arguments)]
pub(super) async fn choose(
    criterion: &Criterion,
    doc: Document<'_>,
    statements: &[Statement],
    asked: &[usize],
    shown: &[(usize, &Proposal)],
    records: &[Record],
    key_edges: &[(usize, usize)],
    read: &[BTreeMap<String, BTreeSet<String>>],
    proposed: Option<&Proposed>,
    fields: &[FieldVote],
    reason: bool,
    infer: &InferenceFn,
) -> Chosen {
    let mut plans: Vec<Plan> = Vec::with_capacity(asked.len());
    let mut choices: Vec<Option<Choice>> = Vec::with_capacity(asked.len());
    let mut opened: Vec<usize> = Vec::new();
    let mut calls = 0;
    let mut vetoed = 0;
    let clears = |p: f64| criterion.bar.is_some_and(|b| p >= b);
    // The choice weighed is the one asked: after reasoning, or in one pass.
    let (measured, model_source) = if reason {
        (criterion.reasoned_choice, "reasoned_choice")
    } else {
        (criterion.model_choice, "model_choice")
    };
    let ask_model = measured.is_none_or(clears);
    if !ask_model {
        debug!(document = doc.id, reason, ?measured, bar = ?criterion.bar, "atlas/resolve: the model's choice is below the bar; it is not asked");
    }
    let id_of = |a: Alt| match a {
        Alt::Record(r) => records[r].id.clone(),
        Alt::Opened(g) => statements[asked[g]].id.clone(),
    };
    for (j, &i) in asked.iter().enumerate() {
        // A declared key shared with an earlier statement of this document
        // settles it: the same plan, no question.
        if let Some(&(a, _)) = key_edges.iter().find(|&&(a, b)| b == j && a < j) {
            plans.push(plans[a].clone());
            choices.push(None);
            continue;
        }
        let mut candidates: Vec<Candidate> = opened.iter().map(|&g| Candidate::Opened(g)).collect();
        candidates.extend(shown.iter().map(|&(r, p)| Candidate::Record(r, p)));
        let offered = candidates.len();
        candidates.retain(|c| match *c {
            Candidate::Record(r, _) => {
                necessary_compatible(criterion, &read[j], &records[r].fields)
            }
            Candidate::Opened(g) => necessary_compatible(criterion, &read[j], &read[g]),
        });
        if candidates.len() < offered {
            let n = offered - candidates.len();
            vetoed += n as u32;
            debug!(document = doc.id, statement = %statements[i].id, vetoed = n, "atlas/resolve: candidates whose necessary value differs are not offered");
        }
        candidates.truncate(LABELS.len());
        let mut alts: Vec<Alt> = candidates.iter().map(Candidate::alt).collect();
        // (position in `alts`, precision, source)
        let mut votes: Vec<(usize, f64, &'static str)> = Vec::new();
        for f in fields {
            if necessary_compatible(criterion, &read[j], &records[f.record].fields) {
                votes.push((
                    place(&mut alts, Alt::Record(f.record)),
                    f.precision,
                    f.stamp.attr(),
                ));
            } else {
                vetoed += 1;
                debug!(
                    document = doc.id,
                    statement = %statements[i].id,
                    candidate = %records[f.record].id,
                    route = "evidential_field",
                    "atlas/resolve: necessary-field conflict vetoed the field's vote"
                );
            }
        }
        let weighed = |votes: &[(usize, f64, &'static str)]| -> Vec<(usize, f64)> {
            votes.iter().map(|&(at, p, _)| (at, p)).collect()
        };
        let fields_link = !votes.is_empty()
            && criterion.bar.is_some_and(|bar| {
                matches!(
                    weigh(alts.len(), &weighed(&votes), bar).zone,
                    Zone::Link(..)
                )
            });
        // The proposed answer: an offered record, or where an earlier
        // statement of its wording went, its necessary values agreeing.
        if let (Some(pr), Some(pa)) = (proposed, criterion.proposed_answer) {
            let named = match pr.verdict(j) {
                Some(ProposedVerdict::Record(r))
                    if candidates
                        .iter()
                        .any(|c| matches!(c, Candidate::Record(x, _) if *x == r)) =>
                {
                    Some(Alt::Record(r))
                }
                Some(ProposedVerdict::Earlier(f))
                    if necessary_compatible(criterion, &read[j], &read[f]) =>
                {
                    alt_of(&plans[f])
                }
                _ => None,
            };
            if let Some(a) = named {
                votes.push((place(&mut alts, a), pa, "proposed_answer"));
            }
        }
        let mut argmax: Option<(Alt, f64)> = None;
        let mut choice: Option<Choice> = None;
        if ask_model && !candidates.is_empty() && !fields_link {
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
            let (best, probability) = candidates
                .iter()
                .zip(LABELS)
                .map(|(c, l)| (Some(c.alt()), p(l)))
                .chain([(None, p(NONE))])
                .fold(
                    (None, f64::NEG_INFINITY),
                    |a, b| if b.1 >= a.1 { b } else { a },
                );
            argmax = best.map(|a| (a, probability));
        }
        let open = |opened: &mut Vec<usize>| {
            opened.push(j);
            Plan::Open {
                group: j,
                cite: None,
            }
        };
        let decided = match (measured, criterion.bar) {
            // Unmeasured, the argmax decides, so it can be measured (Ring 0).
            (None, _) if !fields_link => match argmax {
                Some((Alt::Record(record), probability)) => Plan::Selected {
                    record,
                    probability,
                },
                Some((Alt::Opened(group), _)) => Plan::Open { group, cite: None },
                None => open(&mut opened),
            },
            (_, None) => {
                debug!(document = doc.id, statement = %statements[i].id, "atlas/resolve: no bar is declared; no source links");
                open(&mut opened)
            }
            (_, Some(bar)) => {
                if let (Some(mc), Some((a, _))) = (measured, argmax) {
                    votes.push((place(&mut alts, a), mc, model_source));
                }
                let w = weigh(alts.len(), &weighed(&votes), bar);
                let vote = |&(at, precision, source): &(usize, f64, &'static str)| Vote {
                    source,
                    record: id_of(alts[at]),
                    precision,
                };
                debug!(document = doc.id, statement = %statements[i].id, alternatives = ?alts, ?votes, zone = ?w.zone, "atlas/resolve: the sources weighed");
                match w.zone {
                    Zone::Link(at, posterior) => match alts[at] {
                        Alt::Record(record) => Plan::Weighed {
                            record,
                            posterior,
                            votes: votes.iter().filter(|v| v.0 == at).map(vote).collect(),
                        },
                        Alt::Opened(group) => Plan::Open { group, cite: None },
                    },
                    Zone::NoLink => open(&mut opened),
                    Zone::Unsettled => Plan::Held(Held {
                        alternatives: alts
                            .iter()
                            .map(|&a| id_of(a))
                            .zip(w.posterior.iter().copied())
                            .collect(),
                        none: w.none,
                        sources: votes.iter().map(vote).collect(),
                    }),
                }
            }
        };
        plans.push(decided);
        choices.push(choice);
    }
    Chosen {
        plans,
        choices,
        calls,
        vetoed,
    }
}

/// The one-statement question. The type, criterion and document come first
/// and are the same for every statement of the document, so the engine can
/// keep them prefilled; the candidates and the statement follow.
#[allow(clippy::too_many_arguments)]
fn question(
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
    for (c, label) in candidates.iter().zip(LABELS) {
        let line = match *c {
            Candidate::Record(r, p) => describe(&records[r], &reasons(p)),
            Candidate::Opened(g) => {
                let o = &statements[asked[g]];
                // Asked spans were checked readable in `resolve_document`.
                describe_opened(
                    &doc.body[o.start..o.end],
                    &context(doc.body, o.start, o.end),
                )
            }
        };
        u.push_str(&format!("{label} {line}"));
    }
    u.push_str(&format!(
        "\nStatement, its words in [[ ]]: \"…{}…\"\n\n",
        marked_context(doc.body, s.start, s.end)
    ));
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
enum Ask<'a> {
    Choose,
    Reason,
    ChooseAfter(&'a str),
}

/// The reasoning budget: a cost knob, it decides nothing. Ward's reasoned
/// answers ran to about 900 tokens; a cut answer is still read as a choice.
const REASON_TOKENS: u32 = 2000;

/// The reasoning before a reasoned choice: one bounded generation, traced
/// and counted by the caller. Empty or failed, the statement is refused.
async fn reason_call(
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
            debug!(
                document,
                statement,
                ms,
                chars = text.len(),
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
