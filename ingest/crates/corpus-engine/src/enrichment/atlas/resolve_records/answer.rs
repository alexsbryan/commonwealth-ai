// SPDX-License-Identifier: AGPL-3.0-or-later
//! What RESOLVE asks the model and how code judges the answer: the prompt and
//! its schema, the citation check, and `judge`, which turns one answer into a
//! plan per statement. Split from `resolve_records.rs` (ARCH §3.1).

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::debug;

use super::super::resolution_documents::fold_ws;
use super::{Criterion, Document, Plan, Proposal, Record, Refusal, Statement};
use crate::enrichment::pipeline::types::ChatPrompt;

const SYSTEM: &str = include_str!("../resolve_records_prompt.md");

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

/// ASCII, so a model copying a marker into a cite copies it whole: the
/// multi-byte `⟦s3⟧` came back as U+FFFD in 50 GVC dev v0 cites.
fn marker(j: usize) -> String {
    format!("[s{j}]")
}

pub(super) fn strip_markers(s: &str) -> String {
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

pub(super) fn prompt(
    criterion: &Criterion,
    rule: ProposalRule,
    doc: Document<'_>,
    statements: &[Statement],
    asked: &[usize],
    shown: &[(usize, &Proposal)],
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
    for &(r, proposal) in shown {
        let rec = &records[r];
        u.push_str(&format!(
            "- {} {}",
            rec.handle,
            describe(rec, &reasons(proposal), doc.id)
        ));
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
    u.push_str(&Proposed::of(rule, doc, statements, asked, shown, records).render(records));

    let labels: Vec<String> = (0..asked.len()).map(|j| format!("s{j}")).collect();
    let mut targets = vec!["none".to_string()];
    targets.extend(shown.iter().map(|&(r, _)| records[r].handle.clone()));
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

/// Why a candidate was offered, as the model is shown it.
pub(super) fn reasons(proposal: &Proposal) -> String {
    let mut why: Vec<String> = Vec::new();
    if proposal.same_thread() {
        why.push("same thread".into());
    }
    if proposal.similarity() > 0.0 {
        why.push(format!("document similarity {:.2}", proposal.similarity()));
    }
    why.join("; ")
}

/// How many of a candidate's statements the model is shown: its distinct
/// surfaces, as the pre-C2 renderer bounded them. A cost knob: it bounds the
/// prompt, it decides nothing.
const SHOWN_SURFACES: usize = 4;

/// The label every quote of another document carries in a RESOLVE question.
pub const QUOTE_LABEL: &str = "quoted from another document";

/// A candidate record as every RESOLVE question shows it: why it was
/// offered, how many statements of how many documents it holds, the values
/// declared structure gave it, and up to `SHOWN_SURFACES` of its distinct
/// surfaces from other documents, one line each, marked as quoted from that
/// document. A surface is its statement's span, the lines the statement
/// cites (`Resolver::resolve_document`), so nothing else of that document is
/// shown (C2, narrowed 2026-10-10): a RESOLVE answer is a choice, never a
/// citation, and code checks every cite against the asked statement's own
/// document (`cite_found`). This question's own document is already in the
/// question, so none of it is quoted. One renderer for the partition and the
/// forced choice, so the two arms differ only in the question.
pub(super) fn describe(rec: &Record, why: &str, this_document: &str) -> String {
    let documents: BTreeSet<&str> = rec.evidence.iter().map(|e| e.document.as_str()).collect();
    let values: Vec<String> = rec
        .keys
        .iter()
        .chain(rec.fields.iter())
        .map(|(k, vs)| {
            format!(
                "{k}: {}",
                vs.iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(" | ")
            )
        })
        .collect();
    let mut out = format!(
        "({why}), {} statement(s) in {} document(s); {}\n",
        rec.statements.len().max(rec.evidence.len()),
        documents.len(),
        if values.is_empty() {
            "no declared value".to_string()
        } else {
            values.join("; ")
        }
    );
    let mut shown: Vec<String> = Vec::new();
    let mut withheld = 0usize;
    for e in rec.evidence.iter().filter(|e| e.document != this_document) {
        let quote = fold_ws(&e.surface);
        if shown.contains(&quote) {
            continue;
        }
        if shown.len() == SHOWN_SURFACES {
            withheld += 1;
            continue;
        }
        match &e.title {
            Some(t) => out.push_str(&format!("    {QUOTE_LABEL} {t:?}: {quote:?}\n")),
            None => out.push_str(&format!("    {QUOTE_LABEL}: {quote:?}\n")),
        }
        shown.push(quote);
    }
    debug!(
        record = %rec.id,
        quoted = shown.len(),
        withheld,
        "atlas/resolve: a candidate's spans from other documents, quoted up to SHOWN_SURFACES"
    );
    out
}

/// A record this document opened earlier, shown by its own words in this
/// document: the one document the question holds.
pub(super) fn describe_opened(surface: &str, context: &str) -> String {
    format!(
        "(opened earlier in this document), said as {:?}\n    \"…{context}…\"\n",
        fold_ws(surface)
    )
}

/// Below this TF-IDF similarity a shared wording is not proposed as the same
/// particular. Chosen on GVC and ECB+ train with no model: same wording within
/// documents grouped at this similarity scored best on both (CoNLL .547 and
/// .672).
const SAME_WORDING_SIMILARITY: f32 = 0.4;

/// Below this a record is not proposed for its similarity alone. Chosen with
/// no model on the three systems' train folds (resolve-prereg.md §Knobs: the
/// smallest ratio, ward's .851); every adopted run was measured with it.
const SIMILAR_SIMILARITY: f32 = 0.7;

/// What the proposed answer reads beyond declared threads. Thresholds are
/// chosen on every example's train fold with no model (`Answerer::Proposed`);
/// they propose, they decide nothing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ProposalRule {
    /// A wording is proposed the same as a shown record said in that wording,
    /// from a document at least this alike.
    pub same_wording: f32,
    /// Otherwise the same as the most similar shown record, whatever its
    /// wording, from a document at least this alike. `None`: never.
    pub similar: Option<f32>,
}

impl Default for ProposalRule {
    fn default() -> Self {
        Self {
            same_wording: SAME_WORDING_SIMILARITY,
            similar: Some(SIMILAR_SIMILARITY),
        }
    }
}

/// Why a wording's statements are proposed the same as a record. Declared
/// structure first: a thread outranks every similarity.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Why {
    /// The first record of this document's declared thread among the shown.
    Thread,
    /// The most similar shown record said in this wording.
    Wording(f32),
    /// The most similar shown record, at `ProposalRule::similar` or above.
    Similar(f32),
}

/// The proposed answer for one asked statement (`Proposed::verdict`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum ProposedVerdict {
    /// A shown record, by position in the records.
    Record(usize),
    /// An earlier asked statement of the same wording, by position in `asked`.
    Earlier(usize),
}

/// The answer declared threads, wording and document similarity alone give:
/// a document's statements of one wording are one particular, proposed the
/// same as one shown record (`Why`) or none. The model is shown it and
/// changes what the criterion says is wrong; `Answerer::Proposed` takes it.
pub(super) struct Proposed {
    /// Positions in `asked` of one wording's statements, and the shown record.
    groups: Vec<(Vec<usize>, Option<(usize, Why)>)>,
}

fn wording(s: &str) -> String {
    fold_ws(s).to_lowercase()
}

impl Proposed {
    pub(super) fn of(
        rule: ProposalRule,
        doc: Document<'_>,
        statements: &[Statement],
        asked: &[usize],
        shown: &[(usize, &Proposal)],
        records: &[Record],
    ) -> Self {
        let mut by_wording: Vec<(String, Vec<usize>)> = Vec::new();
        for (j, &i) in asked.iter().enumerate() {
            let w = wording(&doc.body[statements[i].start..statements[i].end]);
            match by_wording.iter_mut().find(|(g, _)| *g == w) {
                Some((_, js)) => js.push(j),
                None => by_wording.push((w, vec![j])),
            }
        }
        let thread = shown
            .iter()
            .filter(|(_, p)| p.same_thread())
            .map(|&(r, _)| r)
            .min();
        let best = |keep: &dyn Fn(usize, f32) -> bool| {
            shown
                .iter()
                .map(|&(r, p)| (r, p.similarity()))
                .filter(|&(r, sim)| keep(r, sim))
                .max_by(|a, b| a.1.total_cmp(&b.1))
        };
        let groups = by_wording
            .into_iter()
            .map(|(w, js)| {
                let said = |r: usize| records[r].evidence.iter().any(|e| wording(&e.surface) == w);
                let target = thread
                    .map(|r| (r, Why::Thread))
                    .or_else(|| {
                        best(&|r, sim| sim >= rule.same_wording && said(r))
                            .map(|(r, sim)| (r, Why::Wording(sim)))
                    })
                    .or_else(|| {
                        let at = rule.similar?;
                        best(&|_, sim| sim >= at).map(|(r, sim)| (r, Why::Similar(sim)))
                    });
                (js, target)
            })
            .collect();
        Self { groups }
    }

    /// What the proposed answer says of asked statement `j`: the shown record
    /// its wording is proposed as, or else the first statement of its wording
    /// earlier in this document, whose plan it shares.
    pub(super) fn verdict(&self, j: usize) -> Option<ProposedVerdict> {
        let (js, target) = self.groups.iter().find(|(js, _)| js.contains(&j))?;
        match target {
            Some((r, _)) => Some(ProposedVerdict::Record(*r)),
            None => js
                .first()
                .filter(|&&f| f != j)
                .map(|&f| ProposedVerdict::Earlier(f)),
        }
    }

    /// The prompt's closing lines: one per particular, `r3: s0 s2 (why)`.
    pub(super) fn render(&self, records: &[Record]) -> String {
        let lines: Vec<String> = self
            .groups
            .iter()
            .map(|(js, target)| {
                let members: Vec<String> = js.iter().map(|j| format!("s{j}")).collect();
                let (same, why) = match target {
                    None => ("none".to_string(), String::new()),
                    Some((r, why)) => (
                        records[*r].handle.clone(),
                        match why {
                            Why::Thread => " (same thread)".to_string(),
                            Why::Wording(s) => format!(" (same wording, similarity {s:.2})"),
                            Why::Similar(s) => format!(" (similarity {s:.2})"),
                        },
                    ),
                };
                format!("{same}: {}{why}", members.join(" "))
            })
            .collect();
        format!(
            "\n\nProposed answer, from threads, wording and document similarity alone (may be wrong):\n{}",
            lines.join("\n")
        )
    }

    /// The answer as the model's schema gives it, each statement cited by its
    /// own words, so `judge` checks and folds it like any other.
    pub(super) fn as_answer(
        &self,
        doc: Document<'_>,
        statements: &[Statement],
        asked: &[usize],
        records: &[Record],
    ) -> String {
        let particulars: Vec<serde_json::Value> = self
            .groups
            .iter()
            .map(|(js, target)| {
                let mentions: Vec<serde_json::Value> = js
                    .iter()
                    .map(|&j| {
                        let s = &statements[asked[j]];
                        json!({"statement": format!("s{j}"), "cite": &doc.body[s.start..s.end]})
                    })
                    .collect();
                let same = target.map_or("none", |(r, _)| records[r].handle.as_str());
                json!({"same_as": same, "mentions": mentions})
            })
            .collect();
        json!({ "particulars": particulars }).to_string()
    }
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
pub(super) fn judge(
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
