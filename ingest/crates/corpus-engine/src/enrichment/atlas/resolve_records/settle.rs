// SPDX-License-Identifier: AGPL-3.0-or-later
//! E3 (campaign ontology-layer; ONTOLOGY_METHOD §Identity, Ring 2): the
//! statements the decider held, settled once every document is seen. In a
//! document a statement is weighed against the records the documents before
//! it made, at weights those documents estimate; one it held there is weighed
//! again here, against the records it was held between as they stand after
//! the last document, at the weights the whole corpus estimates, with the
//! model asked once more, over those records only, as one more weighed source
//! (`model_choice`, or `reasoned_choice` when the run reasons). A record the
//! posterior puts at the type's bar takes it; one where no source raises any
//! record opens its own; and where it is still unsettled the most probable of
//! the records and none decides, as with no bar declared, so nothing is left
//! held after the last document. Statements settle in the documents' clock
//! order, each document's in text order, so any input order settles alike.

use std::collections::BTreeMap;

use tracing::{debug, info};

use super::fields::{self, compare_necessary, declared_keys};
use super::select::{
    decision_call, necessary_compatible, question, reason_call, Ask, Candidate, LABELS, NONE,
};
use super::weigh::{weigh, Zone};
use super::{
    Answerer, Choice, Criterion, Decision, Document, DocumentResolution, Held, Outcome, Proposal,
    Record, Refusal, Resolver, Statement, StatementOutcome,
};
use crate::InferenceFn;

/// One statement held in a document: the document's position on the clock,
/// the statement, and what it was held with.
pub(crate) struct HeldStatement {
    pub(crate) document: usize,
    pub(crate) statement: String,
    pub(crate) held: Held,
}

impl Resolver {
    /// Settle every statement in `held` (E3), returning one resolution per
    /// document that had any, its outcomes replacing the held ones.
    pub(crate) async fn settle<'a>(
        &mut self,
        criterion: &Criterion,
        documents: &[(Document<'a>, &'a [Statement])],
        held: Vec<HeldStatement>,
        answerer: Answerer<'_>,
    ) -> Vec<(usize, DocumentResolution)> {
        let before = held.len();
        let mut out: Vec<(usize, DocumentResolution)> = Vec::new();
        for h in held {
            let (doc, statements) = documents[h.document];
            let Some(s) = statements.iter().find(|s| s.id == h.statement) else {
                continue;
            };
            let (outcome, choice, calls, candidates) =
                self.settle_one(criterion, doc, s, &h.held, answerer).await;
            debug!(document = doc.id, statement = %s.id, decision = outcome.label(), record = outcome.record().unwrap_or("-"), "atlas/resolve settle: settled after the last document");
            let resolution = match out.last_mut() {
                Some((k, r)) if *k == h.document => r,
                _ => {
                    out.push((
                        h.document,
                        DocumentResolution {
                            document: doc.id.to_string(),
                            candidates: Vec::new(),
                            calls: 0,
                            vetoed: 0,
                            unread: 0,
                            necessary: BTreeMap::new(),
                            outcomes: Vec::new(),
                            settles: true,
                        },
                    ));
                    &mut out.last_mut().expect("just pushed").1
                }
            };
            resolution.calls += calls;
            for c in candidates {
                if !resolution.candidates.contains(&c) {
                    resolution.candidates.push(c);
                }
            }
            resolution.outcomes.push(StatementOutcome {
                statement: s.id.clone(),
                by: outcome.by(),
                outcome,
                choice,
            });
        }
        let after = out
            .iter()
            .flat_map(|(_, r)| &r.outcomes)
            .filter(|o| matches!(o.outcome, Outcome::Held(_)))
            .count();
        info!(r#type = %criterion.type_name, held = before, still_held = after, documents = out.len(), "atlas/resolve settle: held statements settled after the last document");
        out
    }

    async fn settle_one(
        &mut self,
        criterion: &Criterion,
        doc: Document<'_>,
        s: &Statement,
        held: &Held,
        answerer: Answerer<'_>,
    ) -> (Outcome, Option<Choice>, u32, Vec<String>) {
        // The records it was held between, as they stand now; one whose
        // supplied necessary value differs is never offered.
        let mut alts: Vec<usize> = Vec::new();
        for (id, _) in &held.alternatives {
            if let Some(&r) = self.position.get(id) {
                if !alts.contains(&r)
                    && necessary_compatible(criterion, &held.supplied, &self.records[r].supplied)
                {
                    alts.push(r);
                }
            }
        }
        alts.truncate(LABELS.len());
        let candidates: Vec<String> = alts.iter().map(|&r| self.records[r].id.clone()).collect();
        let mut comps: Vec<_> = alts
            .iter()
            .map(|&r| {
                let mut c = BTreeMap::new();
                fields::compare(doc, &self.records[r], &mut c);
                compare_necessary(criterion, &held.read, &self.records[r].fields, &mut c);
                c
            })
            .collect();
        let (mut calls, mut choice) = (0, None);
        let model = match answerer {
            Answerer::Select(infer) => Some((infer, false)),
            Answerer::Reason(infer) => Some((infer, true)),
            Answerer::Model(_) | Answerer::Proposed => None,
        };
        if let (Some((infer, reason)), false) = (model, alts.is_empty()) {
            let shown: Vec<Proposal> = candidates
                .iter()
                .map(|id| Proposal {
                    record: id.clone(),
                    reasons: Vec::new(),
                })
                .collect();
            let offered: Vec<(usize, &Proposal)> = alts.iter().copied().zip(&shown).collect();
            match choice_among(
                criterion,
                doc,
                s,
                &offered,
                &self.records,
                reason,
                infer,
                &mut calls,
            )
            .await
            {
                Ok((best, c)) => {
                    let source = if reason {
                        "reasoned_choice"
                    } else {
                        "model_choice"
                    };
                    for (k, comp) in comps.iter_mut().enumerate() {
                        comp.insert(source.into(), best == Some(k));
                    }
                    choice = Some(c);
                }
                Err(refusal) => return (Outcome::Refused(refusal), None, calls, candidates),
            }
        }
        let estimate = &self.estimate;
        let evidence: Vec<f64> = comps.iter().map(|c| estimate.evidence(c)).collect();
        let raised: Vec<bool> = comps.iter().map(|c| estimate.supports(c)).collect();
        let prior = estimate.prior_log_odds();
        let mut weighed = weigh(&evidence, &raised, prior, criterion.bar);
        if weighed.zone == Zone::Unsettled {
            // Every document is seen: no later evidence can come, so the most
            // probable of the records and none decides.
            weighed = weigh(&evidence, &raised, prior, None);
            debug!(document = doc.id, statement = %s.id, zone = ?weighed.zone, "atlas/resolve settle: unsettled at the bar; the most probable decides");
        }
        let surface = doc.body.get(s.start..s.end).unwrap_or("");
        let keys = declared_keys(criterion, s);
        let outcome = match weighed.zone {
            Zone::Link(at, posterior) => {
                let r = alts[at];
                let votes = fields::votes_for(&comps[at], &self.records[r].id, estimate);
                self.fold(r, doc, s, surface, &keys, &held.read, &held.supplied, None);
                Outcome::Decided(Decision::Weighed {
                    record: self.records[r].id.clone(),
                    posterior,
                    sources: votes,
                })
            }
            Zone::NoLink | Zone::Unsettled => {
                let r = match self.position.get(&s.id) {
                    Some(&r) => r,
                    None => self.open(&s.id),
                };
                self.fold(r, doc, s, surface, &keys, &held.read, &held.supplied, None);
                Outcome::Decided(Decision::Opened {
                    record: self.records[r].id.clone(),
                    cite: None,
                })
            }
        };
        (outcome, choice, calls, candidates)
    }
}

/// The model's choice among `alts`, records the statement was held between:
/// the same question a statement is asked in its document, with or without
/// its own reasoning first. Returns the chosen position (`None`: none of
/// them) and the whole choice.
#[allow(clippy::too_many_arguments)]
async fn choice_among(
    criterion: &Criterion,
    doc: Document<'_>,
    s: &Statement,
    alts: &[(usize, &Proposal)],
    records: &[Record],
    reason: bool,
    infer: &InferenceFn,
    calls: &mut u32,
) -> Result<(Option<usize>, Choice), Refusal> {
    let same_when = criterion.same_when.as_deref().ok_or(Refusal::NoCriterion)?;
    let candidates: Vec<Candidate> = alts.iter().map(|&(r, p)| Candidate::Record(r, p)).collect();
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
            &[],
            &[],
            records,
            a,
        )
    };
    let reasoning = if reason {
        *calls += 1;
        Some(reason_call(infer, &ask(Ask::Reason), doc.id, &s.id).await?)
    } else {
        None
    };
    let prompt = match &reasoning {
        Some(text) => ask(Ask::ChooseAfter(text)),
        None => ask(Ask::Choose),
    };
    *calls += 1;
    let dist = decision_call(infer, &prompt, &labels, doc.id, &s.id).await?;
    let p = |label: &str| dist[label];
    // The most probable label; a tie goes to the later one, so to none.
    let (best, _) = (0..alts.len())
        .map(|k| (Some(k), p(LABELS[k])))
        .chain([(None, p(NONE))])
        .fold(
            (None, f64::NEG_INFINITY),
            |a, b| if b.1 >= a.1 { b } else { a },
        );
    let choice = Choice {
        candidates: alts
            .iter()
            .zip(LABELS)
            .map(|(&(r, _), l)| (records[r].id.clone(), p(l)))
            .collect(),
        none: p(NONE),
        reasoning,
    };
    Ok((best, choice))
}

/// Which held statements, by document position, a run's resolutions leave.
pub(crate) fn held_in(k: usize, r: &DocumentResolution, into: &mut Vec<HeldStatement>) {
    for o in &r.outcomes {
        if let Outcome::Held(h) = &o.outcome {
            into.push(HeldStatement {
                document: k,
                statement: o.statement.clone(),
                held: h.clone(),
            });
        }
    }
}
