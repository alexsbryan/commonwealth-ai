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
use serde_json::json;
use tracing::{debug, info};

use super::resolution_documents::fold_ws;
use crate::enrichment::ontology::OntologyTypeDecl;
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::enrichment::reconciliation::identity_signals::fold_identity_value;
use crate::InferenceFn;

const SYSTEM: &str = include_str!("resolve_records_prompt.md");

/// How much of a candidate the model is shown: its distinct surfaces, its
/// first cites, the passages around its statements from this many documents,
/// and this many bytes either side of each statement. Cost knobs: they bound
/// the prompt, they decide nothing.
const SHOWN_SURFACES: usize = 4;
const SHOWN_CITES: usize = 3;
const SHOWN_DOCUMENTS: usize = 2;
const CONTEXT_BYTES: usize = 200;

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
}

impl Criterion {
    /// `keys` is the type's effective identity, its parents' included.
    pub fn of(decl: &OntologyTypeDecl, keys: Vec<String>) -> Self {
        Self {
            type_name: decl.name.clone(),
            description: decl.description.clone(),
            same_when: decl.identity_criterion.clone(),
            keys,
        }
    }
}

/// The document statements were read from. Citations are checked against `body`.
#[derive(Debug, Clone, Copy)]
pub struct Document<'a> {
    pub id: &'a str,
    pub title: Option<&'a str>,
    pub body: &'a str,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
}

/// One document's resolution: what was shown, what it cost, what was decided.
#[derive(Debug, Clone, Serialize)]
pub struct DocumentResolution {
    pub document: String,
    /// The candidate records the model was shown, in the proposer's order.
    pub candidates: Vec<String>,
    pub calls: u32,
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
    Open {
        group: usize,
        cite: Option<String>,
    },
    Refuse(Refusal),
}

/// The open records of one declared type, and the declared keys they hold.
#[derive(Debug, Default)]
pub struct Resolver {
    records: Vec<Record>,
    /// Record id -> position in `records`.
    position: HashMap<String, usize>,
    by_key: HashMap<(String, String), usize>,
}

impl Resolver {
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// Resolve every statement of `doc`. `candidates` are record ids a
    /// proposer offered; unknown ids are dropped, and only these are shown.
    pub async fn resolve_document(
        &mut self,
        criterion: &Criterion,
        doc: Document<'_>,
        statements: &[Statement],
        candidates: &[String],
        infer: &InferenceFn,
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

        // The rest go to the model, in document order.
        let mut asked: Vec<usize> = (0..n).filter(|&i| plan[i].is_none()).collect();
        asked.sort_by_key(|&i| (statements[i].start, statements[i].end));
        let mut shown: Vec<usize> = Vec::new();
        for c in candidates {
            match self.position.get(c) {
                Some(&r) if !shown.contains(&r) => shown.push(r),
                Some(_) => {}
                None => {
                    debug!(document = doc.id, candidate = %c, "atlas/resolve: proposed id is no open record; dropped")
                }
            }
        }
        let key_edges = key_edges(&asked, &folded_keys);
        let mut calls = 0;
        let decided: Vec<Plan> = if asked.is_empty() {
            Vec::new()
        } else if asked.len() == 1 && shown.is_empty() {
            vec![Plan::Open {
                group: 0,
                cite: None,
            }]
        } else if criterion.same_when.is_none() {
            vec![Plan::Refuse(Refusal::NoCriterion); asked.len()]
        } else {
            calls = 1;
            let prompt = prompt(criterion, doc, statements, &asked, &shown, &self.records);
            match infer(&prompt, None).await {
                Ok(raw) => judge(
                    &raw,
                    asked.len(),
                    &shown,
                    &self.records,
                    &key_edges,
                    &fold_ws(doc.body),
                ),
                Err(e) => vec![
                    Plan::Refuse(Refusal::NoAnswer {
                        reason: format!("call failed: {e:#}")
                    });
                    asked.len()
                ],
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
                        Some(&cite),
                    );
                    Outcome::Decided(Decision::Cited {
                        record: self.records[record].id.clone(),
                        cite,
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
            });
        }
        let resolution = DocumentResolution {
            document: doc.id.to_string(),
            candidates: shown.iter().map(|&r| self.records[r].id.clone()).collect(),
            calls,
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
        cite: Option<&str>,
    ) {
        let record = &mut self.records[r];
        record.statements.push(statement.id.clone());
        for (k, v) in keys {
            record.keys.entry(k.clone()).or_default().insert(v.clone());
            self.by_key.entry((k.clone(), v.clone())).or_insert(r);
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

/// Whether `cite`, with our statement markers removed and whitespace folded,
/// is non-empty and occurs in the folded body, as written or inside the one
/// pair of quotation marks it is wrapped in. Exact otherwise: case and
/// punctuation count, so a paraphrase is not found. (GVC dev v0: 134 of 192
/// refused cites were a verbatim passage in quotation marks.)
pub fn cite_found(folded_body: &str, cite: &str) -> bool {
    let c = fold_ws(&strip_markers(cite));
    let found = |c: &str| !c.is_empty() && folded_body.contains(c);
    found(&c) || unquote(&c).is_some_and(|inner| found(inner.trim()))
}

fn unquote(s: &str) -> Option<&str> {
    const PAIRS: [(char, char); 5] = [('"', '"'), ('“', '”'), ('\'', '\''), ('‘', '’'), ('«', '»')];
    PAIRS.iter().find_map(|&(open, close)| {
        let inner = s.strip_prefix(open)?.strip_suffix(close)?;
        Some(inner)
    })
}

/// `CONTEXT_BYTES` of `body` either side of a span, cut back to whitespace so
/// no word is split, with whitespace folded.
fn context(body: &str, start: usize, end: usize) -> String {
    let mut lo = start.saturating_sub(CONTEXT_BYTES);
    while !body.is_char_boundary(lo) {
        lo -= 1;
    }
    let mut hi = (end + CONTEXT_BYTES).min(body.len());
    while !body.is_char_boundary(hi) {
        hi += 1;
    }
    let mut window = &body[lo..hi];
    if lo > 0 {
        if let Some(cut) = window[..start - lo].find(char::is_whitespace) {
            window = &window[cut..];
        }
    }
    if hi < body.len() {
        let from = window.len() - (hi - end);
        if let Some(cut) = window[from..].rfind(char::is_whitespace) {
            window = &window[..from + cut];
        }
    }
    fold_ws(window)
}

/// ASCII, so a model copying a marker into a cite copies it whole: the
/// multi-byte `⟦s3⟧` came back as U+FFFD in 50 GVC dev v0 cites.
fn marker(j: usize) -> String {
    format!("[s{j}]")
}

fn strip_markers(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find("[s") {
        out.push_str(&rest[..at]);
        let tail = &rest[at + "[s".len()..];
        let digits = tail.len() - tail.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        match tail[digits..].strip_prefix(']') {
            Some(after) if digits > 0 => rest = after,
            _ => {
                out.push_str("[s");
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

fn label_index(label: &str, m: usize) -> Option<usize> {
    label.strip_prefix('s')?.parse().ok().filter(|&j| j < m)
}

fn prompt(
    criterion: &Criterion,
    doc: Document<'_>,
    statements: &[Statement],
    asked: &[usize],
    shown: &[usize],
    records: &[Record],
) -> ChatPrompt {
    let mut u = format!("Type: {}", criterion.type_name);
    if !criterion.description.is_empty() {
        u.push_str(&format!(" ({})", criterion.description));
    }
    u.push_str(&format!(
        "\nSame particular when: {}\n\nCandidate records:\n",
        criterion.same_when.as_deref().unwrap_or_default()
    ));
    if shown.is_empty() {
        u.push_str("(none: every particular is \"none\")\n");
    }
    for &r in shown {
        let rec = &records[r];
        let mut surfaces: Vec<&str> = Vec::new();
        for e in &rec.evidence {
            if surfaces.len() < SHOWN_SURFACES && !surfaces.contains(&e.surface.as_str()) {
                surfaces.push(&e.surface);
            }
        }
        let cites: Vec<String> = rec
            .evidence
            .iter()
            .filter_map(|e| e.cite.as_deref())
            .take(SHOWN_CITES)
            .map(|c| format!("{:?}", fold_ws(c)))
            .collect();
        u.push_str(&format!(
            "- {}, said as {}; cited: {}\n",
            rec.handle,
            surfaces
                .iter()
                .map(|s| format!("{s:?}"))
                .collect::<Vec<_>>()
                .join(", "),
            if cites.is_empty() {
                "nothing".to_string()
            } else {
                cites.join(" | ")
            }
        ));
        let mut documents: Vec<&str> = Vec::new();
        for e in &rec.evidence {
            if documents.contains(&e.document.as_str()) || documents.len() == SHOWN_DOCUMENTS {
                continue;
            }
            documents.push(&e.document);
            match &e.title {
                Some(t) => u.push_str(&format!("    {t:?}: \"…{}…\"\n", e.context)),
                None => u.push_str(&format!("    \"…{}…\"\n", e.context)),
            }
        }
    }
    let mut marks: Vec<(usize, usize)> = asked
        .iter()
        .enumerate()
        .map(|(j, &i)| (statements[i].end, j))
        .collect();
    marks.sort();
    let mut body = String::with_capacity(doc.body.len() + 8 * marks.len());
    let mut at = 0;
    for (end, j) in marks {
        body.push_str(&doc.body[at..end]);
        body.push_str(&marker(j));
        at = end;
    }
    body.push_str(&doc.body[at..]);
    u.push_str(&format!(
        "\nDocument{}:\n<<<\n{body}\n>>>\n\nStatements: ",
        match doc.title {
            Some(t) => format!(" titled {t:?}"),
            None => String::new(),
        }
    ));
    let listed: Vec<String> = asked
        .iter()
        .enumerate()
        .map(|(j, &i)| {
            format!(
                "s{j} {:?}",
                &doc.body[statements[i].start..statements[i].end]
            )
        })
        .collect();
    u.push_str(&listed.join(", "));

    let labels: Vec<String> = (0..asked.len()).map(|j| format!("s{j}")).collect();
    let mut targets = vec!["none".to_string()];
    targets.extend(shown.iter().map(|&r| records[r].handle.clone()));
    let schema = json!({
        "type": "object",
        "properties": {"particulars": {
            "type": "array",
            "minItems": 1,
            "maxItems": asked.len(),
            "items": {
                "type": "object",
                "properties": {
                    "same_as": {"enum": targets},
                    "mentions": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "statement": {"enum": labels},
                                "cite": {"type": "string", "minLength": 1}
                            },
                            "required": ["statement", "cite"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["same_as", "mentions"],
                "additionalProperties": false
            }
        }},
        "required": ["particulars"],
        "additionalProperties": false
    });
    ChatPrompt::new(SYSTEM, u)
        .with_response_schema("resolve", schema)
        .with_phase_id("resolve")
        .with_temperature(0.0)
        .with_max_output_tokens(output_budget(asked.len()))
}

/// Output tokens for `m` statements: generous, because a cut-off answer
/// refuses the whole document (GVC dev v1a: two 19-statement documents cut
/// at 64 + 96 per statement). Capped at the primary slot's 4,000.
fn output_budget(m: usize) -> u32 {
    (256 + 128 * m as u32).min(4000)
}

#[derive(Deserialize)]
struct Answers {
    particulars: Vec<Particular>,
}

#[derive(Deserialize)]
struct Particular {
    same_as: String,
    mentions: Vec<Mention>,
}

#[derive(Deserialize)]
struct Mention {
    statement: String,
    cite: String,
}

/// What the model's answer decides for the `m` asked statements, in `asked`
/// order. The answer partitions them into particulars, each the same as one
/// shown candidate or none. Every check here is code: each statement in
/// exactly one particular, its cite found, the target shown. Statements of
/// one particular, and those sharing a declared key, form a group; a group
/// takes the ONE record it names, opens one if it names none, and is refused
/// if it names two, or a record and none.
fn judge(
    raw: &str,
    m: usize,
    shown: &[usize],
    records: &[Record],
    key_edges: &[(usize, usize)],
    folded_body: &str,
) -> Vec<Plan> {
    let parsed: Answers = match serde_json::from_str(raw.trim()) {
        Ok(a) => a,
        Err(e) => {
            let head: String = raw.chars().take(200).collect();
            return vec![
                Plan::Refuse(Refusal::NoAnswer {
                    reason: format!("not the schema's JSON: {e}; head {head:?}")
                });
                m
            ];
        }
    };
    let shown_id: HashMap<&str, usize> = shown
        .iter()
        .map(|&r| (records[r].handle.as_str(), r))
        .collect();
    // `Some(r)` a shown record, `None` "none"; `Err` a target that was not shown.
    let targets: Vec<Result<Option<usize>, &str>> = parsed
        .particulars
        .iter()
        .map(|p| match p.same_as.as_str() {
            "none" => Ok(None),
            t => shown_id.get(t).map(|&r| Some(r)).ok_or(t),
        })
        .collect();
    let mut given: Vec<Vec<(usize, &Mention)>> = (0..m).map(|_| Vec::new()).collect();
    for (p, particular) in parsed.particulars.iter().enumerate() {
        for mention in &particular.mentions {
            match label_index(&mention.statement, m) {
                Some(j) => given[j].push((p, mention)),
                None => {
                    debug!(statement = %mention.statement, "atlas/resolve: answer names no asked statement; ignored")
                }
            }
        }
    }
    let state: Vec<Result<(usize, Option<usize>, String), Refusal>> = given
        .iter()
        .map(|g| {
            let [(p, mention)] = g.as_slice() else {
                return Err(if g.is_empty() {
                    Refusal::Unanswered
                } else {
                    Refusal::Duplicated
                });
            };
            if !cite_found(folded_body, &mention.cite) {
                return Err(Refusal::CiteNotFound {
                    cite: mention.cite.clone(),
                });
            }
            match targets[*p] {
                Ok(t) => Ok((*p, t, mention.cite.clone())),
                Err(t) => Err(Refusal::UnknownTarget {
                    target: t.to_string(),
                }),
            }
        })
        .collect();

    let mut parent: Vec<usize> = (0..m).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let mut first_of: HashMap<usize, usize> = HashMap::new();
    let mut edges: Vec<(usize, usize)> = key_edges.to_vec();
    for (j, s) in state.iter().enumerate() {
        if let Ok((p, _, _)) = s {
            match first_of.get(p) {
                Some(&f) => edges.push((f, j)),
                None => {
                    first_of.insert(*p, j);
                }
            }
        }
    }
    for (a, b) in edges {
        if state[a].is_ok() && state[b].is_ok() {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            parent[ra] = rb;
        }
    }
    let mut anchors: HashMap<usize, (BTreeSet<usize>, usize)> = HashMap::new();
    for (j, s) in state.iter().enumerate() {
        if let Ok((_, t, _)) = s {
            let a = anchors.entry(find(&mut parent, j)).or_default();
            match t {
                Some(r) => {
                    a.0.insert(*r);
                }
                None => a.1 += 1,
            }
        }
    }
    state
        .into_iter()
        .enumerate()
        .map(|(j, s)| match s {
            Err(r) => Plan::Refuse(r),
            Ok((_, _, cite)) => {
                let root = find(&mut parent, j);
                let (recs, nones) = &anchors[&root];
                match (recs.iter().next(), recs.len(), *nones) {
                    (Some(&record), 1, 0) => Plan::Join { record, cite },
                    (None, _, _) => Plan::Open {
                        group: root,
                        cite: Some(cite),
                    },
                    _ => {
                        let mut targets: Vec<String> =
                            recs.iter().map(|&r| records[r].id.clone()).collect();
                        if *nones > 0 {
                            targets.push("none".into());
                        }
                        Plan::Refuse(Refusal::Contradiction { targets })
                    }
                }
            }
        })
        .collect()
}

pub mod propose;

#[cfg(test)]
mod tests;
