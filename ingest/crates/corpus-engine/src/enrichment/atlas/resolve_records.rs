// SPDX-License-Identifier: AGPL-3.0-or-later
//! RESOLVE (svrn/docs/specs/ONTOLOGY_METHOD.md §The core): each statement READ
//! from one document goes to an open record, or to none, which opens one,
//! under its declared type's identity criterion and against the candidates a
//! proposer offered. GROUP and JOIN are one step; novelty is the "none" answer.
//!
//! Identity is decided two ways only (§Invariants 2): equality on a key the
//! type declares sufficient (`identity`), or a model answer whose cited
//! passage code finds in the document. Candidates only bound what the model
//! is shown. An answer code cannot verify refuses its statement, counted and
//! traced, never defaulted to a new record or to the nearest one (§4). The
//! statements of one document go to the model in ONE call.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::resolution_documents::fold_ws;
use crate::enrichment::ontology::{AttrFamily, DocumentStamp, OntologyTypeDecl};
use crate::enrichment::reconciliation::identity_signals::fold_identity_value;
use crate::InferenceFn;

/// Bytes of the document kept either side of a statement as its record's
/// context. A cost knob: it bounds the prompt, it decides nothing.
const CONTEXT_BYTES: usize = 200;

/// The most values a necessary attribute may declare: READ labels each with
/// one single-token letter (`select::LABELS`).
const MAX_READ_VALUES: usize = 25;

/// What RESOLVE reads from a declared type.
#[derive(Debug, Clone)]
pub struct Criterion {
    pub type_name: String,
    pub description: String,
    /// The author's words for when two mentions are one particular. Without
    /// it no question goes to the model: a question with no criterion is not
    /// RESOLVE (§The core, the spin-off read at precision .206).
    pub same_when: Option<String>,
    /// The keys that suffice, inherited ones included.
    pub keys: Vec<String>,
    /// The document fields whose agreement is evidence, each with its
    /// measured precision (`identity_evidential`).
    pub evidential: Vec<(DocumentStamp, f64)>,
    /// The precision a link decided by evidence alone must have.
    pub bar: Option<f64>,
    /// The measured precision of the forced choice's most probable candidate
    /// (`model_choice`). `None`: unmeasured, and the argmax decides (Ring 0),
    /// so it can be measured.
    pub model_choice: Option<f64>,
    /// The measured precision of the forced choice read after the model's own
    /// reasoning (`reasoned_choice`). `None`: unmeasured, its argmax decides.
    pub reasoned_choice: Option<f64>,
    /// The measured precision of the proposed answer (`proposed_answer`).
    /// `None`: it decides nothing in a forced-choice run.
    pub proposed_answer: Option<f64>,
    /// The attributes whose read values must agree, each with its values.
    pub necessary: Vec<(String, Vec<String>)>,
}

impl Criterion {
    /// `keys` is the type's effective identity, its parents' included. An
    /// evidential field that is no document stamp is refused, never skipped.
    pub fn of(decl: &OntologyTypeDecl, keys: Vec<String>) -> Result<Self, String> {
        let (mut evidential, mut model_choice, mut proposed_answer, mut reasoned_choice) =
            (Vec::new(), None, None, None);
        for e in &decl.identity_evidential {
            match (DocumentStamp::from_attr(&e.evidence), e.evidence.as_str()) {
                (Some(stamp), _) => evidential.push((stamp, e.precision())),
                (None, "model_choice") => model_choice = Some(e.precision()),
                (None, "proposed_answer") => proposed_answer = Some(e.precision()),
                (None, "reasoned_choice") => reasoned_choice = Some(e.precision()),
                (None, other) => {
                    return Err(format!(
                        "type `{}`: evidence `{other}` is no source",
                        decl.name
                    ))
                }
            }
        }
        let necessary = decl
            .identity_necessary
            .iter()
            .map(|n| {
                decl.attributes
                    .iter()
                    .find_map(|a| match &a.family {
                        AttrFamily::Text { values }
                            if a.name == *n
                                && !values.is_empty()
                                && values.len() <= MAX_READ_VALUES =>
                        {
                            Some((n.clone(), values.clone()))
                        }
                        _ => None,
                    })
                    .ok_or_else(|| {
                        format!(
                            "type `{}`: necessary attribute `{n}` declares no values, or more than \
                             {MAX_READ_VALUES} (one single-token label each)",
                            decl.name
                        )
                    })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            type_name: decl.name.clone(),
            description: decl.description.clone(),
            same_when: decl.identity_criterion.clone(),
            keys,
            evidential,
            bar: decl.identity_bar,
            model_choice,
            reasoned_choice,
            proposed_answer,
            necessary,
        })
    }
}

/// The document statements were read from. Citations are checked against `body`.
#[derive(Debug, Clone, Copy)]
pub struct Document<'a> {
    pub id: &'a str,
    pub title: Option<&'a str>,
    pub body: &'a str,
    /// The document's own fields the recipe declares (`change.document`), in
    /// the stamp's form (`read_stamp`); a stamp the document lacks is absent.
    pub stamps: &'a [(DocumentStamp, String)],
}

impl<'a> Document<'a> {
    pub fn stamp(&self, stamp: DocumentStamp) -> Option<&'a str> {
        self.stamps
            .iter()
            .find(|(s, _)| *s == stamp)
            .map(|(_, v)| v.as_str())
    }
}

/// One statement READ from a document: where it is, and the declared keys it carries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Statement {
    pub id: String,
    /// Byte offsets into the document body.
    pub start: usize,
    pub end: usize,
    #[serde(default)]
    pub keys: BTreeMap<String, String>,
}

/// An open record and what has been folded into it.
#[derive(Debug, Clone, Serialize)]
pub struct Record {
    /// The id of the statement that opened it: identity from what introduced
    /// the particular, never from when (ARCH principle 8).
    pub id: String,
    /// `r<n>`, the label one run shows the model. Run-local; never an identity.
    pub handle: String,
    pub statements: Vec<String>,
    /// Folded values of the declared keys its statements carried.
    pub keys: BTreeMap<String, BTreeSet<String>>,
    /// The document stamps of its statements' documents, by stamp attribute,
    /// and the values READ for its statements' necessary attributes.
    pub fields: BTreeMap<String, BTreeSet<String>>,
    /// One entry per folded statement; later calls are shown these.
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub document: String,
    pub title: Option<String>,
    pub surface: String,
    pub cite: Option<String>,
    /// The passage around the statement: what a later call compares under
    /// the criterion (who, when, where), since the cite alone may not say.
    pub context: String,
}

/// How a statement's record was decided. Closed: there is no other way.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// A declared sufficient key equals one the record holds.
    Key {
        record: String,
        key: String,
        value: String,
    },
    /// The model put the statement in a particular it said is the record,
    /// and the statement's cited passage was found.
    Cited { record: String, cite: String },
    /// A forced choice named the record as the statement's most probable
    /// candidate (`Answerer::Select`, Ring 0: the argmax decides, so what the
    /// choice carries can be measured). Its whole distribution is the
    /// outcome's `choice`.
    Selected { record: String, probability: f64 },
    /// An evidential document field the statement's document holds is held
    /// by exactly one record from an earlier document, and the field's
    /// measured precision clears the type's bar (`fields.rs`).
    Field {
        record: String,
        field: &'static str,
        value: String,
        precision: f64,
    },
    /// The proposed answer named the record, and its measured precision
    /// cleared the bar and outranked the model's choice (`select.rs`).
    Proposed { record: String, precision: f64 },
    /// None of the candidates: a record was opened. `cite` is `None` only for
    /// a statement alone in its document with no candidate, where no call is made.
    Opened {
        record: String,
        cite: Option<String>,
    },
}

/// Why a statement was left out of every record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "refusal", rename_all = "snake_case")]
pub enum Refusal {
    /// The span is not text of the document body.
    Unreadable { reason: String },
    /// The type declares no `identity_criterion`, and no key settled it.
    NoCriterion,
    /// The call failed, or its answer is not the schema's JSON.
    NoAnswer { reason: String },
    /// No particular of the answer holds this statement.
    Unanswered,
    /// More than one particular holds this statement.
    Duplicated,
    /// The cited passage is not in the document.
    CiteNotFound { cite: String },
    /// Its particular's `same_as` is neither a shown candidate nor "none".
    UnknownTarget { target: String },
    /// Its particular joined to another by a shared declared key, the two
    /// name two records, or a record and "none".
    Contradiction { targets: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Decided(Decision),
    Refused(Refusal),
}

impl Outcome {
    /// The constructor's name, for counting.
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Decided(Decision::Key { .. }) => "key",
            Outcome::Decided(Decision::Cited { .. }) => "cited",
            Outcome::Decided(Decision::Selected { .. }) => "selected",
            Outcome::Decided(Decision::Field { .. }) => "field",
            Outcome::Decided(Decision::Proposed { .. }) => "proposed",
            Outcome::Decided(Decision::Opened { .. }) => "opened",
            Outcome::Refused(r) => match r {
                Refusal::Unreadable { .. } => "refused:unreadable",
                Refusal::NoCriterion => "refused:no_criterion",
                Refusal::NoAnswer { .. } => "refused:no_answer",
                Refusal::Unanswered => "refused:unanswered",
                Refusal::Duplicated => "refused:duplicated",
                Refusal::CiteNotFound { .. } => "refused:cite_not_found",
                Refusal::UnknownTarget { .. } => "refused:unknown_target",
                Refusal::Contradiction { .. } => "refused:contradiction",
            },
        }
    }

    pub fn record(&self) -> Option<&str> {
        match self {
            Outcome::Decided(
                Decision::Key { record, .. }
                | Decision::Cited { record, .. }
                | Decision::Selected { record, .. }
                | Decision::Field { record, .. }
                | Decision::Proposed { record, .. }
                | Decision::Opened { record, .. },
            ) => Some(record),
            Outcome::Refused(_) => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StatementOutcome {
    pub statement: String,
    pub outcome: Outcome,
    /// What a forced choice answered for it, when one was asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choice: Option<Choice>,
}

/// A forced choice's whole answer for one statement: each candidate's record
/// id (for a record this document opened, the id of the statement that opened
/// it) with its probability, in the order shown, and the probability of none.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Choice {
    pub candidates: Vec<(String, f64)>,
    pub none: f64,
    /// The model's own reasoning the choice was read after, when it reasoned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

/// Why a record was offered. Closed: declared structure or generic,
/// domain-free retrieval, nothing else (§Invariants 3).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Reason {
    /// The record holds a statement of a document in this document's declared thread.
    SameThread,
    /// TF-IDF cosine between this document and the record's document the
    /// proposer matched it through.
    SimilarDocument { similarity: f32 },
}

/// A record a proposer offered, with every reason it was offered for.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Proposal {
    pub record: String,
    pub reasons: Vec<Reason>,
}

impl Proposal {
    /// The document similarity it was offered at; 0 when no similarity offered it.
    pub fn similarity(&self) -> f32 {
        self.reasons
            .iter()
            .filter_map(|r| match r {
                Reason::SimilarDocument { similarity } => Some(*similarity),
                Reason::SameThread => None,
            })
            .fold(0.0, f32::max)
    }

    pub fn same_thread(&self) -> bool {
        self.reasons.contains(&Reason::SameThread)
    }
}

/// Who answers the one question RESOLVE puts for a document.
#[derive(Clone, Copy)]
pub enum Answerer<'a> {
    /// The model, in one call.
    Model(&'a InferenceFn),
    /// No model: the proposed answer as given, each statement cited by its
    /// own words. The zero-model floor a model answer is held to, judged and
    /// folded by the same code; it is never the layer's decider.
    Proposed,
    /// The model, one forced choice per statement (`select.rs`).
    Select(&'a InferenceFn),
    /// The model, one forced choice per statement read after its own
    /// reasoning about it (`select.rs`, `reasoned_choice`).
    Reason(&'a InferenceFn),
}

/// One document's resolution: what was shown, what it cost, what was decided.
#[derive(Debug, Clone, Serialize)]
pub struct DocumentResolution {
    pub document: String,
    /// The candidate records the model was shown, in the proposer's order.
    pub candidates: Vec<String>,
    pub calls: u32,
    /// Candidates not offered because a necessary value READ differs.
    pub vetoed: u32,
    /// Necessary attributes READ as none of their values, or refused.
    pub unread: u32,
    pub outcomes: Vec<StatementOutcome>,
}

impl DocumentResolution {
    pub fn tally(&self) -> BTreeMap<&'static str, usize> {
        let mut t = BTreeMap::new();
        for o in &self.outcomes {
            *t.entry(o.outcome.label()).or_insert(0) += 1;
        }
        t
    }
}

/// A statement's fate before records are touched. `group` ties statements
/// that open ONE record together.
#[derive(Debug, Clone)]
enum Plan {
    Key {
        record: usize,
        key: String,
        value: String,
    },
    Join {
        record: usize,
        cite: String,
    },
    Selected {
        record: usize,
        probability: f64,
    },
    Field {
        record: usize,
        stamp: DocumentStamp,
        value: String,
        precision: f64,
    },
    Proposed {
        record: usize,
        precision: f64,
    },
    Open {
        group: usize,
        cite: Option<String>,
    },
    Refuse(Refusal),
}

/// The open records of one declared type, the declared keys they hold, and
/// the rule its proposed answers follow.
#[derive(Debug, Default)]
pub struct Resolver {
    rule: ProposalRule,
    records: Vec<Record>,
    /// Record id -> position in `records`.
    position: HashMap<String, usize>,
    by_key: HashMap<(String, String), usize>,
    /// (stamp, value) -> the records holding it, for evidential fields.
    by_field: HashMap<(DocumentStamp, String), BTreeSet<usize>>,
}

impl Resolver {
    pub fn with_rule(rule: ProposalRule) -> Self {
        Self {
            rule,
            ..Self::default()
        }
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// Resolve every statement of `doc`. `proposals` are the records the
    /// proposers offered; unknown ids are dropped, and only these are shown.
    pub async fn resolve_document(
        &mut self,
        criterion: &Criterion,
        doc: Document<'_>,
        statements: &[Statement],
        proposals: &[Proposal],
        answerer: Answerer<'_>,
    ) -> DocumentResolution {
        let n = statements.len();
        let mut plan: Vec<Option<Plan>> = vec![None; n];
        let surface: Vec<&str> = statements
            .iter()
            .map(|s| doc.body.get(s.start..s.end).unwrap_or(""))
            .collect();
        let folded_keys: Vec<Vec<(String, String)>> = statements
            .iter()
            .map(|s| declared_keys(criterion, s))
            .collect();

        for (i, s) in statements.iter().enumerate() {
            if surface[i].trim().is_empty() {
                plan[i] = Some(Plan::Refuse(Refusal::Unreadable {
                    reason: format!(
                        "span {}..{} is not text of the {}-byte body",
                        s.start,
                        s.end,
                        doc.body.len()
                    ),
                }));
                continue;
            }
            let hits: Vec<(usize, &str, &str)> = folded_keys[i]
                .iter()
                .filter_map(|(k, v)| Some((*self.by_key.get(&(k.clone(), v.clone()))?, &**k, &**v)))
                .collect();
            let held: BTreeSet<usize> = hits.iter().map(|h| h.0).collect();
            plan[i] = match (held.len(), hits.first()) {
                (0, _) | (_, None) => None,
                (1, Some(&(record, key, value))) => Some(Plan::Key {
                    record,
                    key: key.to_string(),
                    value: value.to_string(),
                }),
                _ => Some(Plan::Refuse(Refusal::Contradiction {
                    targets: held.iter().map(|&r| self.records[r].id.clone()).collect(),
                })),
            };
        }

        // An evidential field settles what no key did, before any answerer.
        if let Some(field) = fields::settle(criterion, doc, &self.by_field) {
            for p in plan.iter_mut().filter(|p| p.is_none()) {
                *p = Some(field.clone());
            }
        }

        // The rest go to the model, in document order.
        let mut asked: Vec<usize> = (0..n).filter(|&i| plan[i].is_none()).collect();
        asked.sort_by_key(|&i| (statements[i].start, statements[i].end));
        let mut shown: Vec<(usize, &Proposal)> = Vec::new();
        for p in proposals {
            match self.position.get(&p.record) {
                Some(&r) if !shown.iter().any(|&(s, _)| s == r) => shown.push((r, p)),
                Some(_) => {}
                None => {
                    debug!(document = doc.id, candidate = %p.record, "atlas/resolve: proposed id is no open record; dropped")
                }
            }
        }
        let shown_records: Vec<usize> = shown.iter().map(|&(r, _)| r).collect();
        let key_edges = key_edges(&asked, &folded_keys);
        let mut calls = 0;
        let mut choices: Vec<Option<Choice>> = vec![None; n];
        let mut read_of: Vec<BTreeMap<String, String>> = vec![BTreeMap::new(); n];
        let (mut vetoed, mut unread) = (0, 0);
        let decided: Vec<Plan> = if let (Answerer::Select(infer) | Answerer::Reason(infer), false) =
            (answerer, asked.is_empty())
        {
            // READ each asked statement's necessary attributes first: a
            // candidate whose value differs is never offered.
            let read = read::read(criterion, doc, statements, &asked, infer).await;
            for (&i, values) in asked.iter().zip(&read.values) {
                read_of[i] = values.clone();
            }
            let proposed = criterion
                .proposed_answer
                .map(|_| Proposed::of(self.rule, doc, statements, &asked, &shown, &self.records));
            let chosen = select::choose(
                criterion,
                doc,
                statements,
                &asked,
                &shown,
                &self.records,
                &key_edges,
                &read.values,
                proposed.as_ref(),
                matches!(answerer, Answerer::Reason(_)),
                infer,
            )
            .await;
            calls = read.calls + chosen.calls;
            vetoed = chosen.vetoed;
            unread = read.unknown;
            for (&i, c) in asked.iter().zip(chosen.choices) {
                choices[i] = c;
            }
            chosen.plans
        } else if asked.is_empty() {
            Vec::new()
        } else if asked.len() == 1 && shown.is_empty() {
            vec![Plan::Open {
                group: 0,
                cite: None,
            }]
        } else {
            let answer = match answerer {
                Answerer::Proposed => {
                    Ok(
                        Proposed::of(self.rule, doc, statements, &asked, &shown, &self.records)
                            .as_answer(doc, statements, &asked, &self.records),
                    )
                }
                // A forced choice is asked statement by statement above; no
                // partition is ever asked for it.
                Answerer::Select(_) | Answerer::Reason(_) => Err(Refusal::NoAnswer {
                    reason: "a forced choice is not a partition".into(),
                }),
                Answerer::Model(_) if criterion.same_when.is_none() => Err(Refusal::NoCriterion),
                Answerer::Model(infer) => {
                    calls = 1;
                    let prompt = prompt(
                        criterion,
                        self.rule,
                        doc,
                        statements,
                        &asked,
                        &shown,
                        &self.records,
                    );
                    infer(&prompt, None).await.map_err(|e| Refusal::NoAnswer {
                        reason: format!("call failed: {e:#}"),
                    })
                }
            };
            match answer {
                Ok(raw) => judge(
                    &raw,
                    asked.len(),
                    &shown_records,
                    &self.records,
                    &key_edges,
                    &fold_ws(doc.body),
                ),
                Err(refusal) => vec![Plan::Refuse(refusal); asked.len()],
            }
        };
        for (&i, p) in asked.iter().zip(decided) {
            plan[i] = Some(p);
        }

        // FOLD, in document order, so opened ids follow the text.
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| (statements[i].start, statements[i].end));
        let mut opened: HashMap<usize, usize> = HashMap::new();
        let mut outcomes: Vec<Option<StatementOutcome>> = vec![None; n];
        for i in order {
            let p = plan[i].take().unwrap_or_else(|| {
                Plan::Refuse(Refusal::NoAnswer {
                    reason: "no plan was made for it".into(),
                })
            });
            let outcome = match p {
                Plan::Refuse(r) => Outcome::Refused(r),
                Plan::Key { record, key, value } => {
                    self.fold(
                        record,
                        doc,
                        &statements[i],
                        surface[i],
                        &folded_keys[i],
                        &read_of[i],
                        None,
                    );
                    Outcome::Decided(Decision::Key {
                        record: self.records[record].id.clone(),
                        key,
                        value,
                    })
                }
                Plan::Join { record, cite } => {
                    self.fold(
                        record,
                        doc,
                        &statements[i],
                        surface[i],
                        &folded_keys[i],
                        &read_of[i],
                        Some(&cite),
                    );
                    Outcome::Decided(Decision::Cited {
                        record: self.records[record].id.clone(),
                        cite,
                    })
                }
                Plan::Selected {
                    record,
                    probability,
                } => {
                    self.fold(
                        record,
                        doc,
                        &statements[i],
                        surface[i],
                        &folded_keys[i],
                        &read_of[i],
                        None,
                    );
                    Outcome::Decided(Decision::Selected {
                        record: self.records[record].id.clone(),
                        probability,
                    })
                }
                Plan::Field {
                    record,
                    stamp,
                    value,
                    precision,
                } => {
                    self.fold(
                        record,
                        doc,
                        &statements[i],
                        surface[i],
                        &folded_keys[i],
                        &read_of[i],
                        None,
                    );
                    Outcome::Decided(Decision::Field {
                        record: self.records[record].id.clone(),
                        field: stamp.attr(),
                        value,
                        precision,
                    })
                }
                Plan::Proposed { record, precision } => {
                    self.fold(
                        record,
                        doc,
                        &statements[i],
                        surface[i],
                        &folded_keys[i],
                        &read_of[i],
                        None,
                    );
                    Outcome::Decided(Decision::Proposed {
                        record: self.records[record].id.clone(),
                        precision,
                    })
                }
                Plan::Open { group, cite } => {
                    let record = *opened
                        .entry(group)
                        .or_insert_with(|| self.open(&statements[i].id));
                    self.fold(
                        record,
                        doc,
                        &statements[i],
                        surface[i],
                        &folded_keys[i],
                        &read_of[i],
                        cite.as_deref(),
                    );
                    Outcome::Decided(Decision::Opened {
                        record: self.records[record].id.clone(),
                        cite,
                    })
                }
            };
            debug!(
                document = doc.id,
                statement = %statements[i].id,
                decision = outcome.label(),
                record = outcome.record().unwrap_or("-"),
                ?outcome,
                "atlas/resolve: decided"
            );
            outcomes[i] = Some(StatementOutcome {
                statement: statements[i].id.clone(),
                outcome,
                choice: choices[i].take(),
            });
        }
        let resolution = DocumentResolution {
            document: doc.id.to_string(),
            candidates: shown
                .iter()
                .map(|&(r, _)| self.records[r].id.clone())
                .collect(),
            calls,
            vetoed,
            unread,
            outcomes: outcomes.into_iter().flatten().collect(),
        };
        info!(
            document = doc.id,
            statements = n,
            candidates = resolution.candidates.len(),
            calls,
            tally = ?resolution.tally(),
            "atlas/resolve: document resolved"
        );
        resolution
    }

    /// Open a record introduced by statement `opener`. Statement ids are the
    /// caller's and must be unique; `resolve_statements` refuses a file that
    /// repeats one.
    fn open(&mut self, opener: &str) -> usize {
        let at = self.records.len();
        let clash = self.position.insert(opener.to_string(), at);
        debug_assert!(
            clash.is_none(),
            "statement id `{opener}` opened two records"
        );
        self.records.push(Record {
            id: opener.to_string(),
            handle: format!("r{at}"),
            statements: Vec::new(),
            keys: BTreeMap::new(),
            fields: BTreeMap::new(),
            evidence: Vec::new(),
        });
        self.records.len() - 1
    }

    fn fold(
        &mut self,
        r: usize,
        doc: Document<'_>,
        statement: &Statement,
        surface: &str,
        keys: &[(String, String)],
        read: &BTreeMap<String, String>,
        cite: Option<&str>,
    ) {
        let record = &mut self.records[r];
        for (attr, v) in read {
            record
                .fields
                .entry(attr.clone())
                .or_default()
                .insert(v.clone());
        }
        record.statements.push(statement.id.clone());
        for (k, v) in keys {
            record.keys.entry(k.clone()).or_default().insert(v.clone());
            self.by_key.entry((k.clone(), v.clone())).or_insert(r);
        }
        for (stamp, v) in doc.stamps {
            record
                .fields
                .entry(stamp.attr().to_string())
                .or_default()
                .insert(v.clone());
            self.by_field
                .entry((*stamp, v.clone()))
                .or_default()
                .insert(r);
        }
        record.evidence.push(Evidence {
            document: doc.id.to_string(),
            title: doc.title.map(str::to_string),
            surface: surface.to_string(),
            cite: cite.map(str::to_string),
            context: context(doc.body, statement.start, statement.end),
        });
    }
}

/// The declared keys a statement carries, folded the way every identity
/// comparison folds them (`fold_identity_value`, one decider with the reconciler).
fn declared_keys(criterion: &Criterion, s: &Statement) -> Vec<(String, String)> {
    criterion
        .keys
        .iter()
        .filter_map(|k| Some((k.clone(), fold_identity_value(s.keys.get(k)?)?)))
        .collect()
}

/// Pairs of asked statements (positions in `asked`) that share a declared key value.
fn key_edges(asked: &[usize], keys: &[Vec<(String, String)>]) -> Vec<(usize, usize)> {
    let mut first: HashMap<&(String, String), usize> = HashMap::new();
    let mut edges = Vec::new();
    for (j, &i) in asked.iter().enumerate() {
        for kv in &keys[i] {
            match first.get(kv) {
                Some(&f) => edges.push((f, j)),
                None => {
                    first.insert(kv, j);
                }
            }
        }
    }
    edges
}

/// `CONTEXT_BYTES` of `body` either side of a span, cut back to whitespace so
/// no word is split: the bounds of a statement's passage.
fn window(body: &str, start: usize, end: usize) -> (usize, usize) {
    let mut lo = start.saturating_sub(CONTEXT_BYTES);
    while !body.is_char_boundary(lo) {
        lo -= 1;
    }
    let mut hi = (end + CONTEXT_BYTES).min(body.len());
    while !body.is_char_boundary(hi) {
        hi += 1;
    }
    if lo > 0 {
        if let Some(cut) = body[lo..start].find(char::is_whitespace) {
            lo += cut;
        }
    }
    if hi < body.len() {
        if let Some(cut) = body[end..hi].rfind(char::is_whitespace) {
            hi = end + cut;
        }
    }
    (lo, hi)
}

/// A statement's passage, whitespace folded.
fn context(body: &str, start: usize, end: usize) -> String {
    let (lo, hi) = window(body, start, end);
    fold_ws(&body[lo..hi])
}

/// A statement's passage with its own words in `[[` `]]`.
fn marked_context(body: &str, start: usize, end: usize) -> String {
    let (lo, hi) = window(body, start, end);
    fold_ws(&format!(
        "{}[[{}]]{}",
        &body[lo..start],
        &body[start..end],
        &body[end..hi]
    ))
}

mod answer;
mod fields;
pub mod propose;
mod read;
mod select;

pub use answer::{cite_found, ProposalRule};
use answer::{judge, prompt, Proposed, ProposedVerdict};

#[cfg(test)]
mod tests;
