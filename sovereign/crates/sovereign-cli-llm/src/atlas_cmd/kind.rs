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

use std::collections::BTreeMap;
use std::path::Path;

use corpus_engine::atlas_traversal::question_kind::{KindScore, QuestionKindClassifier};
use corpus_engine::enrichment::atlas::{read_atlas_ontology, ATLAS_DIRNAME};
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
) -> KindRow {
    let runner = race.as_ref().and_then(|r| r.get(1).copied());
    KindRow {
        id: id.to_string(),
        question: question.to_string(),
        verdict: verdict_for(score, gates).to_string(),
        winner: score.map(|s| s.kind.as_str().to_string()),
        sim: score.map(|s| s.sim),
        margin: score.map(|s| s.margin),
        runner_up: runner.map(|(k, _)| k.as_str().to_string()),
        runner_up_sim: runner.map(|(_, s)| s),
    }
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
        out.push(row_for(id, q, race, score, gates));
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
            "{:<15} {:<11} {:>6} {:>7}  {:<11} {:>6}  {}",
            "verdict", "winner", "sim", "margin", "runner-up", "sim", "id"
        );
        for r in &out {
            println!(
                "{:<15} {:<11} {:>6} {:>7}  {:<11} {:>6}  {}",
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
                r.id,
            );
        }
    }

    // The footer is the number the ledger used to be read for.
    let mut by_verdict: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_winner: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &out {
        *by_verdict.entry(r.verdict.as_str()).or_default() += 1;
        if r.verdict == "classified" {
            *by_winner
                .entry(r.winner.as_deref().unwrap_or("-"))
                .or_default() += 1;
        }
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
        );
        assert_eq!(r.verdict, "abstain:margin");
        assert_eq!(r.winner.as_deref(), Some("tension"));
        assert_eq!(r.runner_up.as_deref(), Some("thematic"));
        assert!((r.runner_up_sim.unwrap() - 0.39).abs() < 1e-6);
    }
}
