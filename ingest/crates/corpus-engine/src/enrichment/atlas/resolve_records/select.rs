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

use std::collections::BTreeMap;
use std::time::Instant;

use oicp_types::forced_choice;
use tracing::{debug, warn};

use super::answer::{describe, reasons};
use super::{
    context, marked_context, Choice, Criterion, Document, Evidence, Plan, Proposal, Record,
    Refusal, Statement,
};
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::InferenceFn;

const SYSTEM: &str = include_str!("../resolve_select_prompt.md");

/// Candidate labels, single tokens on the tokenizers in use; `NONE` answers
/// "none of them". At most `LABELS.len()` candidates are shown, a cost cap.
const LABELS: [&str; 25] = [
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S",
    "T", "U", "V", "W", "X", "Y",
];
const NONE: &str = "0";

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
#[allow(clippy::too_many_arguments)]
pub(super) async fn choose(
    criterion: &Criterion,
    doc: Document<'_>,
    statements: &[Statement],
    asked: &[usize],
    shown: &[(usize, &Proposal)],
    records: &[Record],
    key_edges: &[(usize, usize)],
    infer: &InferenceFn,
) -> Chosen {
    let mut plans: Vec<Plan> = Vec::with_capacity(asked.len());
    let mut choices: Vec<Option<Choice>> = Vec::with_capacity(asked.len());
    let mut opened: Vec<usize> = Vec::new();
    let mut calls = 0;
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
        candidates.truncate(LABELS.len());
        if candidates.is_empty() {
            opened.push(j);
            plans.push(Plan::Open {
                group: j,
                cite: None,
            });
            choices.push(None);
            continue;
        }
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
        let prompt = question(
            criterion,
            same_when,
            doc,
            s,
            &candidates,
            &labels,
            statements,
            asked,
            records,
        );
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
        let choice = Choice {
            candidates: candidates
                .iter()
                .zip(LABELS)
                .map(|(c, l)| (id_of(c), p(l)))
                .collect(),
            none: p(NONE),
        };
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
        plans.push(match best {
            Some(Candidate::Record(record, _)) => Plan::Selected {
                record,
                probability,
            },
            Some(Candidate::Opened(group)) => Plan::Open { group, cite: None },
            None => {
                opened.push(j);
                Plan::Open {
                    group: j,
                    cite: None,
                }
            }
        });
        choices.push(Some(choice));
    }
    Chosen {
        plans,
        choices,
        calls,
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
        "\nStatement, its words in [[ ]]: \"…{}…\"\n\nWhich record is the statement about? Answer with its letter, or {NONE} if none of them.",
        marked_context(doc.body, s.start, s.end)
    ));
    ChatPrompt::new(SYSTEM, u)
        .with_response_schema("select", forced_choice::schema(labels))
        .with_phase_id("resolve_select")
        .with_temperature(0.0)
}
