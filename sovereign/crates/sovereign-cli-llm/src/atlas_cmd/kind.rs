// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn atlas kind` — run the question-kind classifier over a bank and show
//! the race, not just the verdict.
//!
//! The walker classifies a question onto one of five rows by centroid
//! (`EPISTEMIC_INDEX.md` §2.2; `atlas_traversal::question_kind`) and abstains
//! when the winner fails either gate. An abstain runs the unfiltered row, which
//! is a fact `ask` states — but nothing said, for a whole bank, how often, on
//! which questions, or which gate refused. Measured once by hand on 2026-09-08:
//! 15 of 21 SEP questions abstain, so the thematic row's composition executed
//! zero times there. That number was read off a lane ledger; this verb is the
//! instrument that reads it on purpose (ARCH §18.4 — validate the instrument
//! before the result).
//!
//! It answers three questions a threshold or exemplar change needs answered
//! before it is made (§18.6): which gate is refusing (`sim` or `margin`), how
//! far each abstain is from admission, and what the runner-up was — a margin
//! abstain between `thematic` and `tension` is a different fact from a sim
//! abstain that was near nothing.
//!
//! Reads the corpus's own map when it has one (`atlas/ontology.json`) and the
//! pre-registered defaults otherwise, and says which. Needs the daemon for the
//! embedder and nothing else — no session bootstrap, no corpus open.
//!
//! Since order epistemic-index-map-conversion it also reads what the corpus
//! CARRIES — `_summary.json`'s atom and edge census, unioned over the corpus
//! and its `<id>-*` siblings, the same scope a walk grounds across — and
//! reports, per classified question, the row that would actually RUN:
//! `ground::admit_winner`, the walk's own decider, so the instrument cannot
//! say "tension" where the walk would fall to lookup.

use std::collections::BTreeMap;
use std::path::Path;

use corpus_engine::atlas_traversal::question_kind::{KindScore, QuestionKindClassifier};
use corpus_engine::enrichment::atlas::ground::{admit_winner, Admission};
use corpus_engine::enrichment::atlas::{
    open_walk_provider_blocking, read_atlas_ontology, read_or_compute_atlas_summary,
    AtlasInventory, ATLAS_DIRNAME,
};
use corpus_engine_vocab::ontology::{NavigationPolicy, QuestionKind};

use crate::chat_cmd::bootstrap::build_inference;
use crate::chat_cmd::config::parse_globals;
use crate::enrich_cmd::paths;
use crate::eval_cmd::bank::load_bank;

/// One row of the report: what the race said about one question.
#[derive(Debug, Clone, serde::Serialize)]
struct KindRow {
    id: String,
    question: String,
    /// `classified`, `abstain:sim`, `abstain:margin`, `abstain:both`, or
    /// `unanswerable` (width mismatch — embedded in another space).
    verdict: String,
    winner: Option<String>,
    sim: Option<f32>,
    margin: Option<f32>,
    runner_up: Option<String>,
    runner_up_sim: Option<f32>,
    /// The row that RUNS for a classified question, once the winner is
    /// checked against the corpus census: the winner itself, the kind it
    /// fell to, or `unfiltered`. `None` for an abstain (the unfiltered row
    /// runs, for the reason `verdict` names) and when no census was read.
    row: Option<String>,
    /// Why the winner could not run, when `row` is not the winner.
    inert: Option<String>,
}

/// Which gate refused, from the score and the classifier's own gates.
fn verdict_for(score: Option<KindScore>, gates: (f32, f32)) -> &'static str {
    let Some(s) = score else {
        return "unanswerable";
    };
    let (min_sim, min_margin) = gates;
    match (s.sim >= min_sim, s.margin >= min_margin) {
        (true, true) => "classified",
        (false, true) => "abstain:sim",
        (true, false) => "abstain:margin",
        (false, false) => "abstain:both",
    }
}

fn row_for(
    id: &str,
    question: &str,
    race: Option<Vec<(QuestionKind, f32)>>,
    score: Option<KindScore>,
    gates: (f32, f32),
    admitted: Option<&(NavigationPolicy, AtlasInventory)>,
) -> KindRow {
    let runner = race.as_ref().and_then(|r| r.get(1).copied());
    let verdict = verdict_for(score, gates);
    let (row, inert) = match (verdict, score, admitted, race.as_ref()) {
        ("classified", Some(s), Some((policy, inventory)), Some(race)) => {
            match admit_winner(s.kind, race, gates.0, policy, inventory) {
                Admission::Fits => (Some(s.kind.as_str().to_string()), None),
                Admission::Inert(r) => (
                    Some(
                        r.fell_to
                            .map(|k| k.as_str().to_string())
                            .unwrap_or_else(|| "unfiltered".to_string()),
                    ),
                    Some(r.why.clause()),
                ),
            }
        }
        _ => (None, None),
    };
    KindRow {
        id: id.to_string(),
        question: question.to_string(),
        verdict: verdict.to_string(),
        winner: score.map(|s| s.kind.as_str().to_string()),
        sim: score.map(|s| s.sim),
        margin: score.map(|s| s.margin),
        runner_up: runner.map(|(k, _)| k.as_str().to_string()),
        runner_up_sim: runner.map(|(_, s)| s),
        row,
        inert,
    }
}

/// The census a walk over this corpus would consult: the corpus's own
/// `_summary.json` plus every `<id>-*` sibling's — the per-article children
/// `ground::candidate_atlas_ids` derives, which is where SEP's atoms live
/// (the `sep` umbrella's own atlas is empty). Returns how many atlases were
/// read beside the union; a corpus with no summary anywhere (a wiki-class
/// store has no `atoms.json`) reads as zero, and the caller says so rather
/// than judging rows against nothing.
fn inventory_for(corpus: &str) -> (AtlasInventory, usize) {
    let mut union = AtlasInventory::default();
    let mut read = 0usize;
    // A wiki-class store (articles.lance + edges.lance, no atoms.json) has
    // no `_summary.json` to read, so its census comes from opening it through
    // the ONE provider opener the walk uses — seconds and a resident copy of
    // the store, paid because the alternative is an instrument that reports
    // lookup as inert on a store carrying forty million Involves edges (the
    // first run of this verb on wikipedia did exactly that, 2026-09-08).
    let indexes = paths::indexes_dir();
    if !paths::index_root(corpus)
        .join(ATLAS_DIRNAME)
        .join("atoms.json")
        .exists()
        && corpus_engine::wikipedia_graph_present(&indexes, corpus)
    {
        let t0 = std::time::Instant::now();
        match open_walk_provider_blocking(&indexes, corpus) {
            Ok(p) => {
                union.absorb(AtlasInventory::of(&[p.as_ref()]));
                read += 1;
                eprintln!(
                    "atlas kind: opened the wiki-class store for `{corpus}` to take its census \
                     ({} ms)",
                    t0.elapsed().as_millis()
                );
            }
            Err(e) => eprintln!("atlas kind: wiki-class store for `{corpus}`: {e}"),
        }
    }
    let mut dirs = vec![paths::index_root(corpus)];
    let prefix = format!("{corpus}-");
    if let Ok(entries) = std::fs::read_dir(paths::indexes_dir()) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                dirs.push(e.path());
            }
        }
    }
    for dir in dirs {
        match read_or_compute_atlas_summary(&dir.join(ATLAS_DIRNAME)) {
            Ok(Some(s)) => {
                union.absorb(AtlasInventory::from_summary(&s));
                read += 1;
            }
            Ok(None) => {}
            Err(e) => eprintln!("atlas kind: summary for {}: {e}", dir.display()),
        }
    }
    (union, read)
}

/// The map a corpus walks under, and where it came from.
fn policy_for(corpus: Option<&str>) -> (NavigationPolicy, String) {
    if let Some(id) = corpus {
        let atlas_dir = paths::index_root(id).join(ATLAS_DIRNAME);
        if let Some(file) = read_atlas_ontology(&atlas_dir) {
            return (
                file.policies.navigation,
                format!(
                    "declared by `{id}` ({})",
                    atlas_dir.join("ontology.json").display()
                ),
            );
        }
        return (
            NavigationPolicy::default(),
            format!("pre-registered defaults (`{id}` has no atlas/ontology.json)"),
        );
    }
    (
        NavigationPolicy::default(),
        "pre-registered defaults (no --corpus given)".to_string(),
    )
}

fn usage() -> i32 {
    eprintln!("usage: svrn atlas kind [--corpus <id>] [--bank <questions.toml>] [--json] [<question> ...]");
    eprintln!(
        "  runs the question-kind classifier over a bank (or the questions given) and prints"
    );
    eprintln!("  the race: winner, sim, margin, runner-up, and which gate refused on an abstain.");
    eprintln!(
        "  --corpus reads that corpus's atlas/ontology.json map; otherwise the pre-registered map."
    );
    2
}

pub async fn run(args: &[String]) -> i32 {
    let (globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("atlas kind: {e}");
            return 2;
        }
    };
    let mut corpus: Option<String> = None;
    let mut bank: Option<String> = None;
    let mut json = false;
    let mut questions: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--corpus" => {
                let Some(v) = rest.get(i + 1) else {
                    return usage();
                };
                corpus = Some(v.clone());
                i += 2;
            }
            "--bank" => {
                let Some(v) = rest.get(i + 1) else {
                    return usage();
                };
                bank = Some(v.clone());
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            "-h" | "--help" => return usage(),
            flag if flag.starts_with('-') => {
                eprintln!("atlas kind: unknown flag {flag}");
                return usage();
            }
            q => {
                questions.push((format!("q{}", questions.len() + 1), q.to_string()));
                i += 1;
            }
        }
    }
    if let Some(path) = bank.as_deref() {
        match load_bank(Path::new(path)) {
            Ok(b) => {
                for q in b.questions {
                    questions.push((q.id, q.question));
                }
            }
            Err(e) => {
                eprintln!("atlas kind: --bank {path}: {e}");
                return 2;
            }
        }
    }
    if questions.is_empty() {
        return usage();
    }

    let (policy, policy_source) = policy_for(corpus.as_deref());
    let rows = policy.classifiable();
    eprintln!("atlas kind: map = {policy_source}");
    // What the corpus carries, and which rows can fire on it at all.
    let admitted: Option<(NavigationPolicy, AtlasInventory)> = match corpus.as_deref() {
        Some(id) => {
            let (inventory, read) = inventory_for(id);
            if read == 0 || inventory.is_empty() {
                eprintln!(
                    "atlas kind: no atlas census for `{id}` ({read} summaries read) — rows are \
                     not checked for admissibility; a wiki-class store carries no _summary.json"
                );
                None
            } else {
                eprintln!(
                    "atlas kind: census over {read} atlas(es): atoms {}; edges {}",
                    inventory
                        .atoms
                        .iter()
                        .map(|(k, n)| format!("{}:{n}", k.label()))
                        .collect::<Vec<_>>()
                        .join(" "),
                    inventory
                        .edges
                        .iter()
                        .map(|(k, n)| format!("{}:{n}", k.label()))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                eprintln!(
                    "atlas kind: row fit: {}",
                    policy
                        .rows()
                        .map(|(k, w)| format!("{} {}", k.as_str(), inventory.fit(w).verdict()))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                Some((policy.clone(), inventory))
            }
        }
        None => None,
    };
    eprintln!(
        "atlas kind: {} classifiable kind(s): {}",
        rows.len(),
        rows.iter()
            .map(|(k, ex)| format!("{}({} exemplars)", k.as_str(), ex.len()))
            .collect::<Vec<_>>()
            .join(", ")
    );

    let inference = match build_inference(&globals).await {
        Ok((inference, _base, _embed_model)) => inference,
        Err(e) => {
            eprintln!("atlas kind: reach the daemon: {e}");
            return 1;
        }
    };
    let embed = sovereign_core::embed_fn::inference_to_embed_query_fn(inference);
    let classifier = match QuestionKindClassifier::build(&policy, &embed).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            eprintln!("atlas kind: the map declares no exemplars on any row; no classifier to run");
            return 1;
        }
        Err(e) => {
            eprintln!("atlas kind: centroids could not be built: {e}");
            return 1;
        }
    };
    let gates = classifier.gates();
    eprintln!(
        "atlas kind: gates min_sim={:.3} min_margin={:.3} (SOVEREIGN_QUESTION_KIND_MIN_SIM / _MIN_MARGIN)",
        gates.0, gates.1
    );
    // The background: how alike the kinds' own centroids are. A query sim
    // has to be read against this, not against an absolute scale.
    let pairs = classifier.inter_centroid_sims();
    if let (Some(hi), Some(lo)) = (pairs.first(), pairs.last()) {
        eprintln!(
            "atlas kind: inter-centroid cosine {:.3} ({}~{}) .. {:.3} ({}~{}), mean {:.3}",
            hi.2,
            hi.0.as_str(),
            hi.1.as_str(),
            lo.2,
            lo.0.as_str(),
            lo.1.as_str(),
            pairs.iter().map(|p| p.2).sum::<f32>() / pairs.len() as f32
        );
    }

    let mut out: Vec<KindRow> = Vec::with_capacity(questions.len());
    for (id, q) in &questions {
        let emb = match embed(q).await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("atlas kind: embed `{id}`: {e}");
                return 1;
            }
        };
        let race = classifier.race(&emb);
        let (_, score) = classifier.classify(&emb);
        out.push(row_for(id, q, race, score, gates, admitted.as_ref()));
    }

    if json {
        match serde_json::to_string_pretty(&out) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("atlas kind: could not serialise the report: {e}");
                return 1;
            }
        }
    } else {
        println!(
            "{:<15} {:<11} {:>6} {:>7}  {:<11} {:>6}  {:<11} {}",
            "verdict", "winner", "sim", "margin", "runner-up", "sim", "row", "id"
        );
        for r in &out {
            println!(
                "{:<15} {:<11} {:>6} {:>7}  {:<11} {:>6}  {:<11} {}",
                r.verdict,
                r.winner.as_deref().unwrap_or("-"),
                r.sim
                    .map(|v| format!("{v:.3}"))
                    .unwrap_or_else(|| "-".into()),
                r.margin
                    .map(|v| format!("{v:+.3}"))
                    .unwrap_or_else(|| "-".into()),
                r.runner_up.as_deref().unwrap_or("-"),
                r.runner_up_sim
                    .map(|v| format!("{v:.3}"))
                    .unwrap_or_else(|| "-".into()),
                r.row.as_deref().unwrap_or("-"),
                r.id,
            );
        }
    }

    // The footer is the number the ledger used to be read for.
    let mut by_verdict: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_winner: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_row: BTreeMap<&str, usize> = BTreeMap::new();
    let mut inert = 0usize;
    for r in &out {
        *by_verdict.entry(r.verdict.as_str()).or_default() += 1;
        if r.verdict == "classified" {
            *by_winner
                .entry(r.winner.as_deref().unwrap_or("-"))
                .or_default() += 1;
            if let Some(row) = r.row.as_deref() {
                *by_row.entry(row).or_default() += 1;
            }
            if r.inert.is_some() {
                inert += 1;
            }
        }
    }
    if admitted.is_some() {
        eprintln!(
            "atlas kind: rows that run on this corpus: {}; {inert} classified winner(s) inert",
            if by_row.is_empty() {
                "none".to_string()
            } else {
                by_row
                    .iter()
                    .map(|(k, n)| format!("{k} {n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
    }
    let classified = by_verdict.get("classified").copied().unwrap_or(0);
    eprintln!(
        "atlas kind: {classified}/{} classified, {} abstained ({}); classified kinds: {}",
        out.len(),
        out.len() - classified,
        by_verdict
            .iter()
            .filter(|(k, _)| **k != "classified")
            .map(|(k, n)| format!("{k} {n}"))
            .collect::<Vec<_>>()
            .join(", "),
        if by_winner.is_empty() {
            "none".to_string()
        } else {
            by_winner
                .iter()
                .map(|(k, n)| format!("{k} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(kind: QuestionKind, sim: f32, margin: f32) -> KindScore {
        KindScore { kind, sim, margin }
    }

    /// The four verdicts are the four cells of the two gates, and the
    /// instrument must name the refusing gate — an operator tuning one gate
    /// on a `both` reading would move the wrong number.
    #[test]
    fn verdict_names_the_gate_that_refused() {
        let gates = (0.34, 0.05);
        assert_eq!(
            verdict_for(Some(score(QuestionKind::Thematic, 0.50, 0.10)), gates),
            "classified"
        );
        assert_eq!(
            verdict_for(Some(score(QuestionKind::Thematic, 0.30, 0.10)), gates),
            "abstain:sim"
        );
        assert_eq!(
            verdict_for(Some(score(QuestionKind::Thematic, 0.50, 0.01)), gates),
            "abstain:margin"
        );
        assert_eq!(
            verdict_for(Some(score(QuestionKind::Thematic, 0.30, 0.01)), gates),
            "abstain:both"
        );
        assert_eq!(verdict_for(None, gates), "unanswerable");
        // Exactly at a gate is admitted — the walker's `>=`, not a second rule.
        assert_eq!(
            verdict_for(Some(score(QuestionKind::Lookup, 0.34, 0.05)), gates),
            "classified"
        );
    }

    #[test]
    fn a_row_carries_the_runner_up_from_the_race() {
        let race = vec![
            (QuestionKind::Tension, 0.41),
            (QuestionKind::Thematic, 0.39),
            (QuestionKind::Lookup, 0.20),
        ];
        let r = row_for(
            "q1",
            "where does it disagree",
            Some(race),
            Some(score(QuestionKind::Tension, 0.41, 0.02)),
            (0.34, 0.05),
            None,
        );
        assert_eq!(r.verdict, "abstain:margin");
        assert!(r.row.is_none());
        assert_eq!(r.winner.as_deref(), Some("tension"));
        assert_eq!(r.runner_up.as_deref(), Some("thematic"));
        assert!((r.runner_up_sim.unwrap() - 0.39).abs() < 1e-6);
    }

    /// A classified winner is reported as the row that RUNS: on an
    /// Entity-only census the tension row is inert and the report says
    /// lookup ran, with the reason — the walk's decider, not a second one.
    #[test]
    fn the_row_column_is_what_the_walk_would_run() {
        use corpus_engine::enrichment::atlas::{AtomType, EdgeType};
        let inventory = AtlasInventory {
            atoms: BTreeMap::from([(AtomType::Entity, 10)]),
            entity_types: BTreeMap::from([("article".to_string(), 10)]),
            edges: BTreeMap::from([(EdgeType::Involves, 20)]),
            declares_types: false,
        };
        let admitted = (NavigationPolicy::default(), inventory);
        let race = vec![
            (QuestionKind::Tension, 0.50),
            (QuestionKind::Lookup, 0.40),
            (QuestionKind::Thematic, 0.20),
        ];
        let r = row_for(
            "q1",
            "where does it disagree",
            Some(race),
            Some(score(QuestionKind::Tension, 0.50, 0.10)),
            (0.34, 0.05),
            Some(&admitted),
        );
        assert_eq!(r.verdict, "classified");
        assert_eq!(r.winner.as_deref(), Some("tension"));
        assert_eq!(r.row.as_deref(), Some("lookup"));
        assert!(r
            .inert
            .as_deref()
            .unwrap()
            .starts_with("no claim or position atoms"));
    }
}
