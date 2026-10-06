// SPDX-License-Identifier: AGPL-3.0-or-later
//! What RESOLVE asks the model and how code judges the answer: the prompt and
//! its schema, the citation check, and `judge`, which turns one answer into a
//! plan per statement. Split from `resolve_records.rs` (ARCH §3.1).

use std::collections::{BTreeSet, HashMap};

use serde::Deserialize;
use serde_json::json;
use tracing::debug;

use super::super::resolution_documents::fold_ws;
use super::{Criterion, Document, Plan, Record, Refusal, Statement};
use crate::enrichment::pipeline::types::ChatPrompt;

const SYSTEM: &str = include_str!("../resolve_records_prompt.md");

/// How much of a candidate the model is shown: its distinct surfaces, its
/// first cites, and the passages around its statements from this many
/// documents. Cost knobs: they bound the prompt, they decide nothing.
const SHOWN_SURFACES: usize = 4;
const SHOWN_CITES: usize = 3;
const SHOWN_DOCUMENTS: usize = 2;

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
