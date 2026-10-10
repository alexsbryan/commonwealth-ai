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
enum Candidate<'p> {
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

/// Two value sets are compatible when every necessary attribute evidenced on
/// both sides has exactly one shared value. Missing evidence is permissive;
/// contaminated multi-value sets match neither of their members. Called on
/// SUPPLIED values only (a declared field's, never a model's read): those
/// forbid outright. A read value is a weighed source (`compare_necessary`).
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

/// A sufficient key settles only when its record's supplied necessary values agree.
pub(super) fn key_plan(
    criterion: &Criterion,
    statement: &Statement,
    hits: &[(usize, String, String)],
    supplied: &BTreeMap<String, BTreeSet<String>>,
    records: &[Record],
    document: &str,
    vetoed: &mut u32,
) -> Option<Plan> {
    let held: BTreeSet<usize> = hits.iter().map(|(record, _, _)| *record).collect();
    match (held.len(), hits.first()) {
        (0, _) | (_, None) => None,
        (1, Some((record, key, value))) => {
            if necessary_compatible(criterion, supplied, &records[*record].supplied) {
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

/// Apply the fields' plan (`fields::settle`) only where supplied necessary values are compatible.
pub(super) fn settle_field(
    criterion: &Criterion,
    statements: &[Statement],
    supplied: &[BTreeMap<String, BTreeSet<String>>],
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
        if necessary_compatible(criterion, &supplied[i], &records[record].supplied) {
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

/// Recheck every answerer join and every same-document opened group against
/// necessary values before fold: supplied ones when the answerer is a forced
/// choice, whose decider weighed every read value already; supplied and READ
/// ones (`values`, and a record's `fields`) under a partition answerer, which
/// weighs no source, so a read value can only be honoured as a constraint.
pub(super) fn gate_plans(
    criterion: &Criterion,
    statements: &[Statement],
    asked: &[usize],
    supplied: &[BTreeMap<String, BTreeSet<String>>],
    records: &[Record],
    reads_weighed: bool,
    decided: Vec<Plan>,
    document: &str,
    vetoed: &mut u32,
) -> Vec<Plan> {
    let mut open_groups: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut gated = Vec::with_capacity(decided.len());
    for (j, mut plan) in decided.into_iter().enumerate() {
        let i = asked[j];
        let target = match &plan {
            Plan::Key { record, .. } | Plan::Join { record, .. } | Plan::Weighed { record, .. } => {
                Some(*record)
            }
            Plan::Open { .. } | Plan::Held(_) | Plan::Refuse(_) => None,
        };
        if let Some(record) = target {
            let held = if reads_weighed {
                &records[record].supplied
            } else {
                &records[record].fields
            };
            if !necessary_compatible(criterion, &supplied[i], held) {
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
                    .find(|&prior| !necessary_compatible(criterion, &supplied[i], &supplied[prior]))
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
        Plan::Key { record, .. } | Plan::Join { record, .. } | Plan::Weighed { record, .. } => {
            Some(Alt::Record(*record))
        }
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
    for (j, &i) in asked.iter().enumerate() {
        // A declared key shared with an earlier statement of this document
        // settles it: the same plan, no question.
        if let Some(&(a, _)) = w.key_edges.iter().find(|&&(a, b)| b == j && a < j) {
            plans.push(plans[a].clone());
            choices.push(None);
            continue;
        }
        let compatible = |a: Alt| match a {
            Alt::Record(r) => necessary_compatible(criterion, &w.supplied[j], &records[r].supplied),
            Alt::Opened(g) => necessary_compatible(criterion, &w.supplied[j], &w.supplied[g]),
        };
        let mut candidates: Vec<Candidate> = opened.iter().map(|&g| Candidate::Opened(g)).collect();
        candidates.extend(w.shown.iter().map(|&(r, p)| Candidate::Record(r, p)));
        let offered = candidates.len();
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
                debug!(document = doc.id, statement = %statements[i].id, candidate = %records[r].id, route = "evidential_field", "atlas/resolve: a supplied necessary value differs; the field's record is not weighed");
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
        let before = weigh(&evidence(&comps), &raised(&comps), prior, criterion.bar);
        let mut choice: Option<Choice> = None;
        if !matches!(before.zone, Zone::Link(..)) && !candidates.is_empty() {
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
        }
        let weighed = weigh(&evidence(&comps), &raised(&comps), prior, criterion.bar);
        tally(
            &mut carried,
            &comps,
            estimate,
            prior,
            criterion.bar,
            weighed.zone,
        );
        // Each alternative by its record id beside its comparison, so the
        // estimate can be read against gold pair by pair (layer-estimator).
        let named: Vec<String> = alts.iter().map(|&a| id_of(a)).collect();
        debug!(document = doc.id, statement = %statements[i].id, alternatives = ?named, comparisons = ?comps, zone = ?weighed.zone, "atlas/resolve: the sources weighed");
        let decided = match weighed.zone {
            Zone::Link(at, posterior) => match alts[at] {
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
            }),
        };
        seen.extend(comps);
        plans.push(decided);
        choices.push(choice);
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
            Candidate::Record(r, p) => describe(&records[r], &reasons(p), doc.id),
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
