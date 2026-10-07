// SPDX-License-Identifier: AGPL-3.0-or-later
//! RESOLVE as forced choices: one question per statement, "which of these
//! records is it about, or none?", answered in one forward pass as a
//! distribution over single-token labels (`oicp_types::forced_choice`), never
//! generated. Statements are asked in document order, and a record a statement
//! of this document opened is a candidate for the ones after it, so grouping
//! within a document needs no second question.
//!
//! Ring 0 of the identity plan (`research/ontology-apps/resolve-prereg.md`):
//! the most probable label decides, so the information the choice carries can
//! be measured against the generated partition's. The full distribution is
//! kept on every outcome for the decider that replaces the argmax.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;

use oicp_types::forced_choice;
use tracing::{debug, warn};

use super::answer::{describe, reasons, Proposed, ProposedVerdict};
use super::{
    context, marked_context, Choice, Criterion, Document, Evidence, Plan, Proposal, Record,
    Refusal, Statement,
};
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::InferenceFn;

const SYSTEM: &str = include_str!("../resolve_select_prompt.md");

/// Candidate labels, single tokens on the tokenizers in use; `NONE` answers
/// "none of them". At most `LABELS.len()` candidates are shown, a cost cap.
pub(super) const LABELS: [&str; 25] = [
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S",
    "T", "U", "V", "W", "X", "Y",
];
pub(super) const NONE: &str = "0";

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
    criterion.necessary.iter().all(|(attr, _)| {
        let Some(left) = left.get(attr).filter(|values| !values.is_empty()) else {
            return true;
        };
        let Some(right) = right.get(attr).filter(|values| !values.is_empty()) else {
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

/// Apply an evidential-field match only where necessary values are compatible.
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
    let Some(Plan::Field { record, .. }) = field.as_ref() else {
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
            | Plan::Field { record, .. }
            | Plan::Proposed { record, .. } => Some(*record),
            Plan::Open { .. } | Plan::Refuse(_) => None,
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
pub(super) async fn decision_call(
    infer: &InferenceFn,
    prompt: &ChatPrompt,
    labels: &[&str],
    document: &str,
    statement: &str,
) -> Result<BTreeMap<String, f64>, Refusal> {
    let started = Instant::now();
    let out = infer(prompt, None).await;
    let ms = started.elapsed().as_millis() as u64;
    match out {
        Ok(raw) => match forced_choice::parse(&raw) {
            Some(d) if labels.iter().all(|l| d.contains_key(*l)) => {
                debug!(document, statement, ms, distribution = ?d, "atlas/resolve: decision call");
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
                warn!(document, statement, ms, %head, "atlas/resolve: decision call answered no distribution");
                Err(Refusal::NoAnswer {
                    reason: format!("not a forced-choice distribution: {head:?}"),
                })
            }
        },
        Err(e) => {
            warn!(document, statement, ms, error = %e, "atlas/resolve: decision call failed");
            Err(Refusal::NoAnswer {
                reason: format!("call failed: {e:#}"),
            })
        }
    }
}

/// Ask each asked statement, in document order, which candidate it is about.
/// A candidate whose necessary value differs from the statement's, both
/// supplied or READ (`read.rs`), is not offered. Where the model's choice is
/// measured (`model_choice`) it is weighed beside the proposed answer
/// (`proposed_answer`): of those that name a candidate, the more precise that
/// clears the type's bar decides, and a choice below the bar is never asked.
/// Unmeasured, the argmax decides (Ring 0), so the choice can be measured.
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
    let measured = if reason {
        criterion.reasoned_choice
    } else {
        criterion.model_choice
    };
    let ask_model = measured.is_none_or(clears);
    if !ask_model {
        debug!(document = doc.id, reason, ?measured, bar = ?criterion.bar, "atlas/resolve: the model's choice is below the bar; it is not asked");
    }
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
        // The proposed answer, where it is weighed and clears the bar: an
        // offered record, or the plan of an earlier statement of its wording
        // whose necessary values agree.
        let proposal: Option<(Plan, f64)> = match (proposed, criterion.proposed_answer) {
            (Some(pr), Some(pa)) if clears(pa) => match pr.verdict(j) {
                Some(ProposedVerdict::Record(r))
                    if candidates
                        .iter()
                        .any(|c| matches!(c, Candidate::Record(x, _) if *x == r)) =>
                {
                    Some((
                        Plan::Proposed {
                            record: r,
                            precision: pa,
                        },
                        pa,
                    ))
                }
                Some(ProposedVerdict::Earlier(f))
                    if necessary_compatible(criterion, &read[j], &read[f]) =>
                {
                    Some((plans[f].clone(), pa))
                }
                _ => None,
            },
            _ => None,
        };
        let mut model: Option<Plan> = None;
        let mut choice: Option<Choice> = None;
        if ask_model && !candidates.is_empty() {
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
            let id_of = |c: &Candidate| match *c {
                Candidate::Record(r, _) => records[r].id.clone(),
                Candidate::Opened(g) => statements[asked[g]].id.clone(),
            };
            // decision_call refused any answer that left a label out.
            let p = |label: &str| dist[label];
            choice = Some(Choice {
                candidates: candidates
                    .iter()
                    .zip(LABELS)
                    .map(|(c, l)| (id_of(c), p(l)))
                    .collect(),
                none: p(NONE),
                reasoning,
            });
            // The most probable label; a tie goes to the later one, so to none.
            let (best, probability) = candidates
                .iter()
                .zip(LABELS)
                .map(|(c, l)| (Some(*c), p(l)))
                .chain([(None, p(NONE))])
                .fold(
                    (None, f64::NEG_INFINITY),
                    |a, b| if b.1 >= a.1 { b } else { a },
                );
            model = match best {
                Some(Candidate::Record(record, _)) => Some(Plan::Selected {
                    record,
                    probability,
                }),
                Some(Candidate::Opened(group)) => Some(Plan::Open { group, cite: None }),
                None => None,
            };
        }
        let decided = match measured {
            // Unmeasured: the argmax decides, so it can be measured (Ring 0).
            None => model,
            // Measured (and asked only if it clears the bar): the more precise
            // of the model's choice and the proposed answer; a tie goes to the
            // proposed answer, which costs no call to reproduce.
            Some(mc) => match (model, proposal) {
                (Some(m), Some((_, qp))) if mc > qp => Some(m),
                (_, Some((q, _))) => Some(q),
                (m, None) => m,
            },
        };
        plans.push(decided.unwrap_or_else(|| {
            opened.push(j);
            Plan::Open {
                group: j,
                cite: None,
            }
        }));
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
                let surface = &doc.body[o.start..o.end];
                let opened = Record {
                    id: o.id.clone(),
                    handle: String::new(),
                    statements: vec![],
                    keys: Default::default(),
                    fields: Default::default(),
                    evidence: vec![Evidence {
                        document: doc.id.to_string(),
                        title: None,
                        surface: surface.to_string(),
                        cite: None,
                        context: context(doc.body, o.start, o.end),
                    }],
                };
                describe(&opened, "opened earlier in this document")
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
