// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich resolve-statements`: RESOLVE alone, over statements the caller
//! supplies, under one type a recipe declares. The gold-mention setting of
//! ONTOLOGY_METHOD.md §The generality test: statements stand in for READ, so
//! the score measures RESOLVE and its proposer and nothing upstream.
//!
//! The inputs carry no gold. A document is `{id, title?, body, ...}`, its
//! other fields read only as the recipe declares them (`change.document`, each
//! through `read_stamp`, a document lacking one counted in `stamps_unread`);
//! a statement is `{document, id, start, end, keys?}`, byte offsets into the
//! body. Documents resolve on the clock (`resolve_records::clock`: dated first,
//! oldest first, the id breaking ties), whatever order the file names them in.
//! `--answer proposed` makes no call: it takes the proposed answer as given,
//! the zero-model floor the model's answer is held to, through the same judge.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use corpus_engine::enrichment::atlas::read_stamp;
use corpus_engine::enrichment::atlas::resolve_records::propose::{Proposers, SimilarDocuments};
use corpus_engine::enrichment::atlas::resolve_records::{
    resolve_in_clock_order, Answerer, Carried, Criterion, Document, Estimate, Outcome,
    ProposalRule, Resolver, Statement,
};
use corpus_engine::enrichment::ontology::{DocumentStamp, TypeIndex};
use serde::{Deserialize, Serialize};

use super::inference_client::{DaemonInferenceClient, TokenUsageSnapshot};

pub const DEFAULT_MODEL: &str = "commonwealth/primary";
pub use corpus_engine::enrichment::atlas::resolve_records::propose::{
    MAX_CANDIDATES as DEFAULT_MAX_CANDIDATES, MIN_SIMILARITY as DEFAULT_MIN_SIMILARITY,
    NEIGHBOURS as DEFAULT_NEIGHBOURS,
};

/// Who answers: the model's cited partition, the model's forced choice per
/// statement, or the proposed answer as given (no call).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerBy {
    Model,
    Select,
    Reason,
    Proposed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedResolveStatements {
    pub recipe: PathBuf,
    pub type_name: String,
    pub documents: PathBuf,
    pub statements: PathBuf,
    pub out: PathBuf,
    pub model: String,
    pub neighbours: usize,
    pub max_candidates: usize,
    pub min_similarity: f32,
    pub rule: ProposalRule,
    pub answer: AnswerBy,
    pub limit: Option<usize>,
    /// `--asker`: where RESOLVE's answers come from; the store is `out`'s
    /// answers.jsonl (`corpus_engine::enrichment::asker`).
    pub asker: corpus_engine::enrichment::asker::Asker,
}

pub fn parse_args(args: &[String]) -> Result<ParsedResolveStatements, String> {
    let mut flags: HashMap<&str, String> = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let (name, inline) = match args[i].split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (args[i].as_str(), None),
        };
        let known = [
            "--recipe",
            "--type",
            "--documents",
            "--statements",
            "--out",
            "--model",
            "--neighbours",
            "--max-candidates",
            "--min-similarity",
            "--same-wording-similarity",
            "--similar",
            "--answer",
            "--limit",
            "--asker",
        ];
        let Some(&flag) = known.iter().find(|k| **k == name) else {
            return Err(format!("unknown argument `{}`", args[i]));
        };
        let value = match inline {
            Some(v) => v,
            None => {
                i += 1;
                args.get(i)
                    .cloned()
                    .ok_or_else(|| format!("{flag} requires a value"))?
            }
        };
        flags.insert(flag, value);
        i += 1;
    }
    let required = |f: &str| {
        flags
            .get(f)
            .cloned()
            .ok_or_else(|| format!("{f} is required"))
    };
    let count = |f: &str, default: usize| -> Result<usize, String> {
        flags.get(f).map_or(Ok(default), |v| {
            v.parse()
                .map_err(|_| format!("{f} takes a whole number, not `{v}`"))
        })
    };
    let share = |f: &str| -> Result<Option<f32>, String> {
        flags
            .get(f)
            .map(|v| match v.parse::<f32>() {
                Ok(x) if (0.0..=1.0).contains(&x) => Ok(x),
                _ => Err(format!("{f} takes a similarity from 0 to 1, not `{v}`")),
            })
            .transpose()
    };
    let default_rule = ProposalRule::default();
    Ok(ParsedResolveStatements {
        recipe: required("--recipe")?.into(),
        type_name: required("--type")?,
        documents: required("--documents")?.into(),
        statements: required("--statements")?.into(),
        out: required("--out")?.into(),
        model: flags
            .get("--model")
            .cloned()
            .unwrap_or_else(|| DEFAULT_MODEL.into()),
        neighbours: count("--neighbours", DEFAULT_NEIGHBOURS)?,
        max_candidates: count("--max-candidates", DEFAULT_MAX_CANDIDATES)?,
        min_similarity: share("--min-similarity")?.unwrap_or(DEFAULT_MIN_SIMILARITY),
        rule: ProposalRule {
            same_wording: share("--same-wording-similarity")?.unwrap_or(default_rule.same_wording),
            similar: share("--similar")?.or(default_rule.similar),
        },
        answer: match flags.get("--answer").map(String::as_str) {
            None | Some("model") => AnswerBy::Model,
            Some("select") => AnswerBy::Select,
            Some("reason") => AnswerBy::Reason,
            Some("proposed") => AnswerBy::Proposed,
            Some(v) => {
                return Err(format!(
                    "--answer is `model`, `select`, `reason` or `proposed`, not `{v}`"
                ))
            }
        },
        limit: flags
            .get("--limit")
            .map(|_| count("--limit", 0))
            .transpose()?,
        asker: flags
            .get("--asker")
            .map(|v| corpus_engine::enrichment::asker::Asker::parse(v))
            .transpose()?
            .unwrap_or_default(),
    })
}

#[derive(Deserialize)]
struct DocRow {
    id: String,
    #[serde(default)]
    title: Option<String>,
    body: String,
    /// Every other field, read only where the recipe declares it.
    #[serde(flatten)]
    fields: serde_json::Map<String, serde_json::Value>,
}

/// A declared field's value as text: a string as it is, a number written out.
fn field_text(row: &DocRow, field: &str) -> Option<String> {
    match row.fields.get(field)? {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[derive(Deserialize)]
struct StatementRow {
    document: String,
    #[serde(flatten)]
    statement: Statement,
}

/// What a run did, written as `summary.json`.
#[derive(Debug, Serialize)]
pub struct ResolveStatementsSummary {
    pub recipe: String,
    pub type_name: String,
    pub same_when: Option<String>,
    pub keys: Vec<String>,
    pub model: String,
    pub answer: AnswerBy,
    pub rule: ProposalRule,
    /// The field the recipe declares as a document's thread, and how many documents carry it.
    pub thread_field: Option<String>,
    pub threaded_documents: usize,
    /// Per declared stamp, how many documents it could not be read from.
    pub stamps_unread: BTreeMap<&'static str, usize>,
    /// The bar the type declares; `None`: the most probable decides.
    pub bar: Option<f64>,
    /// Each source's weights as estimated on this run's pairs, no labels
    /// (`resolve_records::estimate`), and what each carried.
    pub estimate: Estimate,
    pub carried: BTreeMap<String, Carried>,
    /// The necessary attributes READ, candidates not offered because one
    /// differed, and reads that came back none of the values or refused.
    pub necessary: Vec<String>,
    pub vetoed: u32,
    pub unread: u32,
    pub proposer: String,
    pub documents: usize,
    pub statements: usize,
    pub records: usize,
    pub calls: u32,
    pub calls_per_document: f64,
    pub tally: BTreeMap<String, usize>,
    /// How `clustering.json` carries a refused statement: alone, so a scorer
    /// charges for it rather than dropping it.
    pub refused_scored_as: &'static str,
    /// How a held (unsettled) statement is written in `clustering.json`.
    pub held_scored_as: &'static str,
    pub tokens: TokenUsageSnapshot,
    pub wall_seconds: f64,
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
    std::io::BufReader::new(file)
        .lines()
        .enumerate()
        .filter(|(_, l)| l.as_ref().map_or(true, |l| !l.trim().is_empty()))
        .map(|(n, l)| {
            let l = l.map_err(|e| format!("reading {}: {e}", path.display()))?;
            serde_json::from_str(&l).map_err(|e| format!("{}:{}: {e}", path.display(), n + 1))
        })
        .collect()
}

fn write_json(path: &Path, v: &impl Serialize) -> Result<(), String> {
    let s = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    std::fs::write(path, s).map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Statements grouped by document, documents in the order the rows first
/// name them. Refuses a repeated statement id (a record's id is the id of the
/// statement that opened it) and a document `known` does not hold.
fn group_statements(
    rows: Vec<StatementRow>,
    known: impl Fn(&str) -> bool,
) -> Result<(Vec<String>, HashMap<String, Vec<Statement>>), String> {
    let mut order: Vec<String> = Vec::new();
    let mut by_doc: HashMap<String, Vec<Statement>> = HashMap::new();
    let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in rows {
        if !ids.insert(row.statement.id.clone()) {
            return Err(format!("statement id `{}` appears twice", row.statement.id));
        }
        if !known(&row.document) {
            return Err(format!(
                "statement `{}` names document `{}`, which the documents file does not hold",
                row.statement.id, row.document
            ));
        }
        if !by_doc.contains_key(&row.document) {
            order.push(row.document.clone());
        }
        by_doc.entry(row.document).or_default().push(row.statement);
    }
    Ok((order, by_doc))
}

pub async fn run(p: &ParsedResolveStatements) -> Result<ResolveStatementsSummary, String> {
    let recipe = corpus_engine::Recipe::from_file(&p.recipe)
        .map_err(|e| format!("reading recipe {}: {e}", p.recipe.display()))?;
    let policies = recipe
        .custom_atlas_spec()
        .ok_or_else(|| format!("{} declares no [enrichment.ontology]", p.recipe.display()))?
        .policies();
    let index = TypeIndex::from_policies(&policies);
    let decl = index.get(&p.type_name).ok_or_else(|| {
        let names: Vec<&str> = policies
            .shape
            .types
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        format!(
            "`{}` is not a type {} declares (declared: {})",
            p.type_name,
            p.recipe.display(),
            names.join(", ")
        )
    })?;
    let criterion = Criterion::of(decl, index.effective_identity(&p.type_name).to_vec())?;
    if criterion.same_when.is_none() {
        eprintln!(
            "warning: `{}` declares no identity_criterion; every statement no key settles is refused",
            p.type_name
        );
    }

    let docs: HashMap<String, DocRow> = read_jsonl::<DocRow>(&p.documents)?
        .into_iter()
        .map(|d| (d.id.clone(), d))
        .collect();
    let (mut order, by_doc) = group_statements(read_jsonl::<StatementRow>(&p.statements)?, |d| {
        docs.contains_key(d)
    })?;
    if let Some(n) = p.limit {
        order.truncate(n);
    }

    let thread_field = policies
        .change
        .document
        .as_ref()
        .and_then(|d| d.thread.clone());
    // Every declared stamp of every document, read the one way claims are
    // stamped; a document lacking one is counted and traced, never defaulted.
    let declared: Vec<(DocumentStamp, String)> = match &policies.change.document {
        Some(d) => d.declared().map(|(s, f)| (s, f.to_string())).collect(),
        None => Vec::new(),
    };
    let mut stamps_unread: BTreeMap<&'static str, usize> = BTreeMap::new();
    let stamps_of: HashMap<&str, Vec<(DocumentStamp, String)>> = order
        .iter()
        .map(|id| {
            let mut stamps = Vec::new();
            for (stamp, field) in &declared {
                match read_stamp(&docs[id].fields, *stamp, field) {
                    Ok(v) => stamps.push((*stamp, v)),
                    Err(why) => {
                        tracing::debug!(document = %id, field = stamp.attr(), %why, "resolve-statements: stamp unread");
                        *stamps_unread.entry(stamp.attr()).or_default() += 1;
                    }
                }
            }
            (id.as_str(), stamps)
        })
        .collect();
    let threaded = stamps_of
        .values()
        .filter(|s| s.iter().any(|(st, _)| *st == DocumentStamp::Thread))
        .count();

    let base = sovereign_contracts::setup_config::client_daemon_base()?;
    let client = DaemonInferenceClient::new(&base, &p.model, "")
        .map_err(|e| format!("building the daemon client: {e}"))?;
    let ledger = client.usage_ledger();
    let (_embed, infer) = client.into_asked_closures(p.asker, &p.out)?;
    let answerer = match p.answer {
        AnswerBy::Model => Answerer::Model(&infer),
        AnswerBy::Select => Answerer::Select(&infer),
        AnswerBy::Reason => Answerer::Reason(&infer),
        AnswerBy::Proposed => Answerer::Proposed,
    };

    std::fs::create_dir_all(&p.out).map_err(|e| format!("creating {}: {e}", p.out.display()))?;
    let decisions_path = p.out.join("decisions.jsonl");
    let mut decisions = std::io::BufWriter::new(
        std::fs::File::create(&decisions_path)
            .map_err(|e| format!("creating {}: {e}", decisions_path.display()))?,
    );
    let mut resolver = Resolver::with_rule(p.rule);
    let mut proposer = Proposers::new(
        thread_field.is_some(),
        SimilarDocuments::new(p.neighbours, p.max_candidates, p.min_similarity),
    );
    let mut clustering: BTreeMap<String, String> = BTreeMap::new();
    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    let (mut calls, mut statements) = (0u32, 0usize);
    let (mut vetoed, mut unread) = (0u32, 0u32);
    let started = Instant::now();
    eprintln!(
        "resolve-statements: {} document(s) ({} in a declared thread), type `{}`, answer by {}, proposer {}, rule {:?}",
        order.len(),
        threaded,
        p.type_name,
        match p.answer {
            AnswerBy::Model => format!("model {} at {base}", p.model),
            AnswerBy::Select => format!("model {} at {base}, one forced choice per statement", p.model),
            AnswerBy::Reason => format!(
                "model {} at {base}, one forced choice per statement after its own reasoning",
                p.model
            ),
            AnswerBy::Proposed => "the proposed answer (no model)".to_string(),
        },
        proposer.describe(),
        p.rule
    );
    let mut documents: Vec<(Document<'_>, &[Statement])> = order
        .iter()
        .map(|id| {
            let row = &docs[id];
            (
                Document {
                    id: &row.id,
                    title: row.title.as_deref(),
                    body: &row.body,
                    stamps: &stamps_of[id.as_str()],
                },
                by_doc[id].as_slice(),
            )
        })
        .collect();
    let mut written: Result<(), String> = Ok(());
    resolve_in_clock_order(
        &criterion,
        &mut documents,
        &mut resolver,
        &mut proposer,
        answerer,
        &mut |k, doc, r| {
            if written.is_ok() {
                written = serde_json::to_writer(&mut decisions, r)
                    .map_err(|e| e.to_string())
                    .and_then(|()| decisions.write_all(b"\n").map_err(|e| e.to_string()));
            }
            for o in &r.outcomes {
                if r.settles {
                    // It replaces the statement's held outcome (E3).
                    if let Some(n) = tally.get_mut("held") {
                        *n -= 1;
                    }
                }
                *tally.entry(o.outcome.label().to_string()).or_insert(0) += 1;
                let cluster = match &o.outcome {
                    Outcome::Decided(_) => o.outcome.record().unwrap_or_default().to_string(),
                    Outcome::Refused(_) => format!("refused:{}", o.statement),
                    Outcome::Held(_) => format!("held:{}", o.statement),
                };
                clustering.insert(o.statement.clone(), cluster);
            }
            calls += r.calls;
            vetoed += r.vetoed;
            unread += r.unread;
            if !r.settles {
                statements += r.outcomes.len();
            }
            eprintln!(
                "  [{}/{}] {}: {} statement(s), {} candidate(s), {} call(s) {:?}",
                k + 1,
                order.len(),
                doc.id,
                r.outcomes.len(),
                r.candidates.len(),
                r.calls,
                r.tally()
            );
        },
    )
    .await;
    written?;
    decisions.flush().map_err(|e| e.to_string())?;
    write_json(&p.out.join("clustering.json"), &clustering)?;
    write_json(&p.out.join("records.json"), &resolver.records())?;
    let summary = ResolveStatementsSummary {
        recipe: p.recipe.display().to_string(),
        type_name: p.type_name.clone(),
        same_when: criterion.same_when.clone(),
        keys: criterion.keys.clone(),
        model: p.model.clone(),
        answer: p.answer,
        rule: p.rule,
        thread_field,
        threaded_documents: threaded,
        stamps_unread,
        bar: criterion.bar,
        estimate: resolver.estimate().clone(),
        carried: resolver.carried().clone(),
        necessary: criterion.necessary.iter().map(|n| n.name.clone()).collect(),
        vetoed,
        unread,
        proposer: proposer.describe(),
        documents: order.len(),
        statements,
        records: resolver.records().len(),
        calls,
        calls_per_document: calls as f64 / order.len().max(1) as f64,
        tally,
        refused_scored_as: "singleton",
        held_scored_as: "singleton",
        tokens: ledger.snapshot(),
        wall_seconds: started.elapsed().as_secs_f64(),
    };
    for line in resolver.sources_summary() {
        eprintln!("  source {line}");
    }
    write_json(&p.out.join("summary.json"), &summary)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn parse_args_takes_both_flag_forms_and_defaults() {
        let p = parse_args(&args(
            "--recipe r.toml --type=happening --documents d.jsonl --statements s.jsonl --out o --limit 5",
        ))
        .unwrap();
        assert_eq!(p.type_name, "happening");
        assert_eq!(p.model, DEFAULT_MODEL);
        assert_eq!((p.neighbours, p.max_candidates, p.limit), (3, 12, Some(5)));
        assert_eq!(p.answer, AnswerBy::Model);
        assert_eq!(p.rule, ProposalRule::default());
        let p = parse_args(&args(
            "--recipe r --type t --documents d --statements s --out o --answer proposed --similar 0.3 --min-similarity=0.1",
        ))
        .unwrap();
        assert_eq!(p.answer, AnswerBy::Proposed);
        assert_eq!((p.rule.similar, p.min_similarity), (Some(0.3), 0.1));
    }

    fn rows(json: &[&str]) -> Vec<StatementRow> {
        json.iter()
            .map(|j| serde_json::from_str(j).unwrap())
            .collect()
    }

    #[test]
    fn statements_group_in_first_named_order_and_repeats_or_strangers_are_refused() {
        let known = |d: &str| d == "d1" || d == "d2";
        let (order, by_doc) = group_statements(
            rows(&[
                r#"{"document":"d2","id":"a","start":0,"end":1}"#,
                r#"{"document":"d1","id":"b","start":0,"end":1}"#,
                r#"{"document":"d2","id":"c","start":2,"end":3}"#,
            ]),
            known,
        )
        .unwrap();
        assert_eq!(order, ["d2", "d1"]);
        assert_eq!(by_doc["d2"].len(), 2);
        let repeat = group_statements(
            rows(&[
                r#"{"document":"d1","id":"a","start":0,"end":1}"#,
                r#"{"document":"d2","id":"a","start":0,"end":1}"#,
            ]),
            known,
        );
        assert_eq!(repeat.unwrap_err(), "statement id `a` appears twice");
        let stranger = group_statements(
            rows(&[r#"{"document":"d9","id":"a","start":0,"end":1}"#]),
            known,
        );
        assert!(stranger.unwrap_err().contains("`d9`"));
    }

    #[test]
    fn parse_args_names_what_is_missing_or_unknown() {
        let e = parse_args(&args("--recipe r.toml --type t")).unwrap_err();
        assert_eq!(e, "--documents is required");
        let e = parse_args(&args("--recipe r.toml --bogus 1")).unwrap_err();
        assert_eq!(e, "unknown argument `--bogus`");
        let e = parse_args(&args(
            "--recipe r --type t --documents d --statements s --out o --limit many",
        ))
        .unwrap_err();
        assert_eq!(e, "--limit takes a whole number, not `many`");
        let e = parse_args(&args(
            "--recipe r --type t --documents d --statements s --out o --similar 2",
        ))
        .unwrap_err();
        assert_eq!(e, "--similar takes a similarity from 0 to 1, not `2`");
    }
}
