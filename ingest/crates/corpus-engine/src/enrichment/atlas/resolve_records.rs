// SPDX-License-Identifier: AGPL-3.0-or-later
//! RESOLVE (svrn/docs/specs/ONTOLOGY_METHOD.md §The core): each statement READ
//! from one document goes to an open record, or to none, which opens one,
//! under its declared type's identity criterion and against the candidates a
//! proposer offered. GROUP and JOIN are one step; novelty is the "none" answer.
//!
//! Identity (§Invariants 2): a sufficient key links, a differing SUPPLIED
//! necessary value forbids, otherwise every source is weighed (`weigh.rs`) at
//! what it is estimated to be worth on this corpus (`estimate.rs`, refitted
//! after every document): link at the bar, open where nothing raised a
//! candidate, else held.
//! Candidates only bound what the model is shown. An unverifiable answer
//! refuses its statement, counted and traced, never defaulted (§4).

use super::precision::SourcePrecision;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::resolution_documents::fold_ws;
use crate::enrichment::ontology::{AttrDecl, AttrFamily, DocumentStamp, OntologyTypeDecl};
use crate::InferenceFn;

/// Bytes of the asked document shown either side of a statement in its own
/// question. A cost knob: it bounds the prompt, it decides nothing.
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
    /// The posterior a link decided by evidence must reach. `None`: the most
    /// probable of the alternatives and none decides (`weigh.rs`).
    pub bar: Option<f64>,
    /// The attributes whose supplied or read values must agree.
    pub necessary: Vec<ClosedAttr>,
}

/// A closed-valued attribute as READ asks it: one forced choice over its
/// declared values. RESOLVE asks it of identity-necessary attributes, the
/// passes reader of every closed-valued field it reads.
#[derive(Debug, Clone)]
pub struct ClosedAttr {
    pub name: String,
    /// The author's words for what it holds, shown beside its name; empty
    /// when the recipe declares none. Declared value meanings lifted stage
    /// on ward's gold statements .525 -> .663 (crm-proof loop 13, C2).
    pub description: String,
    /// The declared closed set, one single-token label each.
    pub values: Vec<String>,
}

impl ClosedAttr {
    /// The attribute as one forced choice, when it declares 1..=25 values
    /// (one single-token label each); `None` for any other attribute.
    pub fn of(attr: &AttrDecl) -> Option<Self> {
        match &attr.family {
            AttrFamily::Text { values }
                if !values.is_empty() && values.len() <= MAX_READ_VALUES =>
            {
                Some(Self {
                    name: attr.name.clone(),
                    description: attr.description.clone(),
                    values: values.clone(),
                })
            }
            _ => None,
        }
    }
}

impl Criterion {
    /// `keys` is the type's effective identity, its parents' included. What
    /// each source is worth is estimated on the corpus, never declared: the
    /// retired `identity_evidential` key is dropped by the loader, warned.
    pub fn of(decl: &OntologyTypeDecl, keys: Vec<String>) -> Result<Self, String> {
        let necessary = decl
            .identity_necessary
            .iter()
            .map(|n| {
                decl.attributes
                    .iter()
                    .find(|a| a.name == *n)
                    .and_then(ClosedAttr::of)
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
            bar: decl.identity_bar,
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
    /// Necessary values a reader chose for it (a model's read): weighed as a
    /// source, never forbidding a candidate as a supplied `keys` value does.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub read: BTreeMap<String, String>,
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
    /// The document stamps and necessary values supplied or READ for its statements.
    pub fields: BTreeMap<String, BTreeSet<String>>,
    /// The necessary values its statements carried SUPPLIED (never READ):
    /// one that differs forbids a link outright.
    #[serde(skip)]
    pub supplied: BTreeMap<String, BTreeSet<String>>,
    /// One entry per folded statement; later calls are shown these.
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub document: String,
    pub title: Option<String>,
    pub surface: String,
    pub cite: Option<String>,
    /// The lines the statement cites, whole (`passage::lines_of`): what a
    /// later RESOLVE question quotes, marked as from another document.
    pub quote: String,
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
    /// Every source that spoke was weighed at its estimated weight
    /// (`weigh.rs`: fields, necessary values, the proposed answer, the model's
    /// choice), and the record's posterior reached the type's bar ahead of
    /// every other. `sources` are those that agreed with it.
    Weighed {
        record: String,
        posterior: f64,
        sources: Vec<Vote>,
    },
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
    /// Unsettled (`weigh::Zone::Unsettled`): sources named candidates and none
    /// reached the bar. Held, counted, never opened as a record (Ring 2).
    Held(Held),
}

/// What an unsettled statement was held with: each alternative's posterior,
/// in the order weighed, none's, and every source's vote; and the necessary
/// values it was weighed with, read and supplied, which settling it after the
/// last document weighs again (`settle.rs`, E3).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Held {
    pub alternatives: Vec<(String, f64)>,
    pub none: f64,
    pub sources: Vec<Vote>,
    #[serde(skip)]
    pub read: BTreeMap<String, BTreeSet<String>>,
    #[serde(skip)]
    pub supplied: BTreeMap<String, BTreeSet<String>>,
}

impl Outcome {
    /// The constructor's name, for counting.
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Decided(Decision::Key { .. }) => "key",
            Outcome::Decided(Decision::Cited { .. }) => "cited",
            Outcome::Decided(Decision::Weighed { .. }) => "weighed",
            Outcome::Decided(Decision::Opened { .. }) => "opened",
            Outcome::Held(_) => "held",
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
                | Decision::Weighed { record, .. }
                | Decision::Opened { record, .. },
            ) => Some(record),
            Outcome::Refused(_) | Outcome::Held(_) => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StatementOutcome {
    pub statement: String,
    pub outcome: Outcome,
    /// The link's sources and their precisions (C3, `by.rs`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by: Vec<SourcePrecision>,
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
    /// Join routes vetoed because necessary values conflict.
    pub vetoed: u32,
    /// Necessary READs that returned none of their declared values or were refused.
    pub unread: u32,
    /// The necessary values RESOLVE used, by where each came from (C3):
    /// `supplied` (a declared field: forbids outright when it differs),
    /// `reader` (the document reader's Choose) and `resolve_read` (RESOLVE's
    /// own READ), both model reads, weighed.
    pub necessary: BTreeMap<&'static str, u32>,
    pub outcomes: Vec<StatementOutcome>,
    /// Held statements of this document settled after the last document
    /// (`settle.rs`, E3), each outcome replacing its held one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub settles: bool,
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
    Weighed {
        record: usize,
        posterior: f64,
        votes: Vec<Vote>,
    },
    Held(Held),
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
    /// (stamp, value) -> the records holding it, for the declared fields.
    by_field: HashMap<(DocumentStamp, String), BTreeSet<usize>>,
    /// Every comparison weighed so far, and the weights fitted to them
    /// (`estimate.rs`): refitted after each document, so a document is
    /// weighed at what the documents before it, in clock order, estimate.
    pairs: Pairs,
    estimate: Estimate,
    carried: BTreeMap<String, Carried>,
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
        let key_hits = select::key_hits(&self.by_key, &folded_keys);
        let field_named = fields::named(doc, &self.by_field);
        let supplied = read::provided(criterion, doc, statements, |s| &s.keys);
        let chosen = read::provided(criterion, doc, statements, |s| &s.read);
        let mut necessary: BTreeMap<&'static str, u32> = BTreeMap::new();
        let mut read_of = supplied.clone();
        for ((known, by_reader), given) in read_of.iter_mut().zip(chosen).zip(&supplied) {
            *necessary.entry("supplied").or_default() += given.len() as u32;
            for (attr, values) in by_reader {
                if known.contains_key(&attr) {
                    continue;
                }
                *necessary.entry("reader").or_default() += 1;
                known.insert(attr, values);
            }
        }
        let mut seen: Vec<Comparison> = Vec::new();
        let (mut calls, mut unread, mut vetoed) = (0, 0, 0);

        // READ once before any route can settle: sufficient keys and evidential
        // fields need the same necessary values as the answerer routes do.
        let read_infer = match answerer {
            Answerer::Select(infer) | Answerer::Reason(infer) => Some(infer),
            Answerer::Model(_) | Answerer::Proposed => None,
        };
        if let Some(infer) = read_infer {
            let to_read: Vec<usize> = (0..n)
                .filter(|&i| !surface[i].trim().is_empty())
                .filter(|&i| {
                    let held: BTreeSet<usize> =
                        key_hits[i].iter().map(|(record, _, _)| *record).collect();
                    held.len() <= 1
                })
                .collect();
            let read = read::read(criterion, doc, statements, &to_read, &read_of, infer).await;
            calls += read.calls;
            unread += read.unknown;
            *necessary.entry("resolve_read").or_default() += read.calls - read.unknown;
            for (&i, values) in to_read.iter().zip(&read.values) {
                read_of[i] = values.clone();
            }
        }

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
            plan[i] = select::key_plan(
                criterion,
                s,
                &key_hits[i],
                &supplied[i],
                &self.records,
                doc.id,
                &mut vetoed,
            );
        }

        // A partition answerer weighs no source, so the fields alone settle
        // what they can before it, and only statements compatible with the
        // necessary values READ supplied. A forced choice weighs the fields
        // with every other source, statement by statement (`select::choose`).
        if read_infer.is_none() {
            let field = fields::settle(
                criterion,
                doc,
                &self.records,
                &self.by_field,
                &self.estimate,
                &mut seen,
            );
            select::settle_field(
                criterion,
                statements,
                &supplied,
                &self.records,
                &mut plan,
                field,
                doc.id,
                &mut vetoed,
            );
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
        let mut choices: Vec<Option<Choice>> = vec![None; n];
        let decided: Vec<Plan> = if let (Answerer::Select(infer) | Answerer::Reason(infer), false) =
            (answerer, asked.is_empty())
        {
            let asked_read: Vec<BTreeMap<String, BTreeSet<String>>> =
                asked.iter().map(|&i| read_of[i].clone()).collect();
            let asked_supplied: Vec<BTreeMap<String, BTreeSet<String>>> =
                asked.iter().map(|&i| supplied[i].clone()).collect();
            let proposed = Proposed::of(self.rule, doc, statements, &asked, &shown, &self.records);
            let chosen = select::choose(
                criterion,
                doc,
                statements,
                &select::Weighing {
                    asked: &asked,
                    shown: &shown,
                    records: &self.records,
                    key_edges: &key_edges,
                    read: &asked_read,
                    supplied: &asked_supplied,
                    proposed: &proposed,
                    field_named: &field_named,
                    estimate: &self.estimate,
                },
                matches!(answerer, Answerer::Reason(_)),
                infer,
            )
            .await;
            seen.extend(chosen.seen);
            for (source, c) in chosen.carried {
                let t = self.carried.entry(source).or_default();
                t.links += c.links;
                t.pivotal_links += c.pivotal_links;
                t.vetoes += c.vetoes;
            }
            calls += chosen.calls;
            vetoed += chosen.vetoed;
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
        let reads_weighed = read_infer.is_some();
        let gated = select::gate_plans(
            criterion,
            statements,
            &asked,
            if reads_weighed { &supplied } else { &read_of },
            &self.records,
            reads_weighed,
            decided,
            doc.id,
            &mut vetoed,
        );
        for (&i, p) in asked.iter().zip(gated) {
            plan[i] = Some(p);
        }

        // `group` is a per-answer partition label, not identity. Choose the
        // lexically stable statement identity of each linked open group so
        // equal spans and input order cannot alias or rename its record.
        let mut open_identity_by_group = HashMap::<usize, String>::new();
        for &i in &asked {
            let Some(Plan::Open { group, .. }) = plan[i].as_ref() else {
                continue;
            };
            let identity = &statements[i].id;
            open_identity_by_group
                .entry(*group)
                .and_modify(|current| {
                    if identity.as_str() < current.as_str() {
                        *current = identity.clone();
                    }
                })
                .or_insert_with(|| identity.clone());
        }
        tracing::debug!(
            document = doc.id,
            open_groups = open_identity_by_group.len(),
            "atlas/resolve: open identities derived from statement ids"
        );

        // FOLD, in document order, so opened ids follow the text.
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| (statements[i].start, statements[i].end));
        let mut opened: HashMap<String, usize> = HashMap::new();
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
                        &supplied[i],
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
                        &supplied[i],
                        Some(&cite),
                    );
                    Outcome::Decided(Decision::Cited {
                        record: self.records[record].id.clone(),
                        cite,
                    })
                }
                Plan::Weighed {
                    record,
                    posterior,
                    votes,
                } => {
                    self.fold(
                        record,
                        doc,
                        &statements[i],
                        surface[i],
                        &folded_keys[i],
                        &read_of[i],
                        &supplied[i],
                        None,
                    );
                    Outcome::Decided(Decision::Weighed {
                        record: self.records[record].id.clone(),
                        posterior,
                        sources: votes,
                    })
                }
                // Held: no record opens and nothing folds, so no later
                // statement can join it at first sight.
                Plan::Held(held) => Outcome::Held(held),
                Plan::Open { group, cite } => {
                    let identity = open_identity_by_group
                        .get(&group)
                        .expect("every open plan has a linked statement identity")
                        .clone();
                    let record = *opened
                        .entry(identity.clone())
                        .or_insert_with(|| self.open(&identity));
                    self.fold(
                        record,
                        doc,
                        &statements[i],
                        surface[i],
                        &folded_keys[i],
                        &read_of[i],
                        &supplied[i],
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
                by: outcome.by(),
                outcome,
                choice: choices[i].take(),
            });
        }
        for c in &seen {
            self.pairs.add(c);
        }
        if !seen.is_empty() {
            self.estimate = Estimate::fit(&self.pairs);
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
            necessary,
            outcomes: outcomes.into_iter().flatten().collect(),
            settles: false,
        };
        info!(
            document = doc.id,
            statements = n,
            candidates = resolution.candidates.len(),
            calls,
            vetoed,
            unread,
            tally = ?resolution.tally(),
            "atlas/resolve: document resolved"
        );
        resolution
    }

    /// Open a record under its content-derived statement identity. The
    /// caller supplies a stable id containing the local reference where one
    /// exists; statement ids are unique within the document.
    fn open(&mut self, statement_identity: &str) -> usize {
        let at = self.records.len();
        let clash = self.position.insert(statement_identity.to_string(), at);
        debug_assert!(
            clash.is_none(),
            "statement id `{statement_identity}` opened two records"
        );
        self.records.push(Record {
            id: statement_identity.to_string(),
            handle: format!("r{at}"),
            statements: Vec::new(),
            keys: BTreeMap::new(),
            fields: BTreeMap::new(),
            supplied: BTreeMap::new(),
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
        read: &BTreeMap<String, BTreeSet<String>>,
        supplied: &BTreeMap<String, BTreeSet<String>>,
        cite: Option<&str>,
    ) {
        let record = &mut self.records[r];
        for (attr, values) in supplied {
            record
                .supplied
                .entry(attr.clone())
                .or_default()
                .extend(values.iter().cloned());
        }
        for (attr, values) in read {
            record
                .fields
                .entry(attr.clone())
                .or_default()
                .extend(values.iter().cloned());
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
            quote: lines_of(doc.body, statement.start, statement.end),
        });
    }
}

mod answer;
mod by;
use estimate::{Comparison, Pairs};
pub use estimate::{Estimate, SourceWeight};
pub use weigh::Carried;
mod passage;
pub(crate) use passage::marked_context;
use passage::{context, lines_of};
mod drive;
mod estimate;
mod fields;
pub mod propose;
mod read;
mod select;
mod settle;
mod weigh;
use fields::{declared_keys, key_edges};

pub(crate) use read::choice_question;
pub(crate) use select::{decision_call, LABELS, NONE};

pub use answer::{cite_found, ProposalRule, QUOTE_LABEL};
use answer::{judge, prompt, Proposed};
pub use drive::{clock, resolve_in_clock_order};
pub use weigh::Vote;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "resolve_records/open_identity_tests.rs"]
mod open_identity_tests;
