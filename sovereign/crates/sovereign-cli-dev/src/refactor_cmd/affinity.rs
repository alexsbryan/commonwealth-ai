// SPDX-License-Identifier: AGPL-3.0-or-later
//! Destination-first audit: "here is an abstraction — what in this codebase
//! should become it?"
//!
//! # Why this is a different question from the other five detectors
//!
//! The detectors in [`super::detector`] run DUPLICATION-FIRST: they sweep for
//! what is duplicated and hand you the results. This runs the other way, and it
//! is the direction an operator actually asks in — *I have
//! `kernel_types::Verdict`; find everything in the tree that is functionally
//! the same, whatever it is called.*
//!
//! None of the five can answer it:
//!
//! - `name` needs the same identifier. Five differently-named implementations
//!   of one idea are invisible to it.
//! - `shape` needs overlapping FIELD names, so it sees renamed forks of data
//!   and nothing at all about functions.
//! - `behaviour`'s near tier is embedding cosine at 0.95 — that finds
//!   near-copies, not five genuinely different implementations of the same
//!   idea. Different code that does the same thing scores far below it.
//!
//! So: five hand-rolled string reversals with different bodies are missed by
//! every instrument this repo currently has.
//!
//! # What answers it, and why this file mostly refuses
//!
//! Matching *behaviour* needs a description of what each symbol DOES, not what
//! it is spelled. That substrate exists —
//! `corpus-engine/src/enrichment/code_intel/` generates a plain-English intent
//! summary plus "the questions this answers" for every symbol, body-hash
//! cached so a re-run costs only changed bodies. Its stated purpose is exactly
//! this bridge: ask in user vocabulary, match the summary, then walk the SCIP
//! graph from there.
//!
//! **It has never been run on this corpus.** There is no
//! `code_intel_cache.json` under the index directory, so the summaries do not
//! exist.
//!
//! The tempting move is to fall back to the raw code embeddings that DO exist
//! (853MB of `chunks.lance`) and return something. That would match on
//! vocabulary and syntax rather than behaviour, and would return a plausible,
//! confident, wrong answer — the characteristic failure ARCH §18 exists to
//! catch. So this refuses instead, and names the one command that fixes it.
//! Absence is reported, never defaulted (§18.3).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One symbol, as the code-intel pass described it.
///
/// Mirrors `corpus_engine::enrichment::code_intel::SymbolEnrichment`'s wire
/// form. Read rather than imported because this crate must not pull
/// corpus-engine's tree-sitter grammars in to read a JSON sidecar.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SymbolSummary {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub asks: Vec<String>,
    #[serde(default)]
    pub meta: SummaryMeta,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct SummaryMeta {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub qualified_name: String,
    #[serde(default)]
    pub file_path: String,
    #[serde(default)]
    pub line_start: usize,
}

pub fn cache_path(index_path: &Path) -> PathBuf {
    index_path.join("code_intel_cache.json")
}

/// Why an affinity audit could not run, with the remedy named.
#[derive(Debug)]
pub struct Unavailable {
    pub reason: String,
    pub remedy: String,
}

/// Load the per-symbol descriptions, or say precisely what is missing.
pub fn load_summaries(index_path: &Path) -> Result<Vec<SymbolSummary>, Unavailable> {
    let path = cache_path(index_path);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Err(Unavailable {
            reason: format!(
                "no per-symbol descriptions for this corpus ({} does not exist), so there is \
                 nothing to match BEHAVIOUR against. The raw code embeddings that do exist \
                 match vocabulary and syntax, not what a function does, and answering from \
                 them would be a confident wrong answer rather than an absent one.",
                path.display()
            ),
            remedy: "svrn enrich code-intel --corpus commonwealth-ai   (one-time; body-hash \
                     cached afterwards, so only changed bodies ever re-run)"
                .to_string(),
        });
    };
    serde_json::from_str::<Vec<SymbolSummary>>(&text).map_err(|e| Unavailable {
        reason: format!("{}: {e}", path.display()),
        remedy: "re-run `svrn enrich code-intel` to regenerate the cache".to_string(),
    })
}

/// A candidate site the audit surfaced.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub qualified_name: String,
    pub file: String,
    pub line: usize,
    pub summary: String,
    /// Lexical overlap score against the query terms. A PREFILTER, never the
    /// verdict — see [`shortlist`].
    pub score: f64,
}

/// Terms worth matching on, from a free-text description of the destination.
///
/// Deliberately crude: this is a recall-oriented prefilter whose only job is to
/// cut ~25k symbols down to a shortlist a model can afford to read. Precision
/// is the model's job, and calling this step the answer would be the mistake.
pub fn query_terms(query: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "a", "an", "of", "to", "and", "or", "is", "it", "that", "this", "for", "with",
        "into", "from", "in", "on", "by", "as", "be", "are", "was", "one", "any", "all",
    ];
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(|w| w.to_ascii_lowercase())
        .filter(|w| !STOP.contains(&w.as_str()))
        .collect()
}

/// Rank every described symbol against the query terms.
pub fn shortlist(summaries: &[SymbolSummary], query: &str, limit: usize) -> Vec<Candidate> {
    let terms = query_terms(query);
    if terms.is_empty() {
        return Vec::new();
    }
    // Inverse document frequency, so a term every symbol uses ("returns",
    // "value") cannot carry a match on its own.
    let mut df: HashMap<&str, usize> = HashMap::new();
    for s in summaries {
        let hay = format!("{} {}", s.summary, s.asks.join(" ")).to_ascii_lowercase();
        for t in &terms {
            if hay.contains(t.as_str()) {
                *df.entry(t.as_str()).or_insert(0) += 1;
            }
        }
    }
    let n = summaries.len().max(1) as f64;

    let mut out: Vec<Candidate> = summaries
        .iter()
        .filter_map(|s| {
            let hay = format!("{} {}", s.summary, s.asks.join(" ")).to_ascii_lowercase();
            let score: f64 = terms
                .iter()
                .filter(|t| hay.contains(t.as_str()))
                .map(|t| {
                    let d = df.get(t.as_str()).copied().unwrap_or(1).max(1) as f64;
                    (n / d).ln()
                })
                .sum();
            if score <= 0.0 {
                return None;
            }
            Some(Candidate {
                qualified_name: s.meta.qualified_name.clone(),
                file: s.meta.file_path.clone(),
                line: s.meta.line_start,
                summary: s.summary.clone(),
                score,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit);
    out
}

/// The model question for one shortlisted candidate.
///
/// One task, one closed set, one escape hatch — the same discipline as
/// [`super::label_model`], for the same reason: this runs on a small
/// open-weight model and a hedging prompt returns confident noise.
pub fn compose_prompt(destination: &str, description: &str, c: &Candidate) -> String {
    format!(
        "TARGET ABSTRACTION\n`{destination}` — {description}\n\n\
         CANDIDATE\n`{}` ({}:{})\n{}\n\n\
         Does the candidate do functionally the same job as the target abstraction, such that \
         it could be replaced by it?\n\n\
         same-job        yes — it is another implementation of the target's job\n\
         related-not-same it touches the same area but does a different job\n\
         unrelated       no\n\
         unsure          you cannot tell from what is shown\n\n\
         Answer `unsure` rather than guessing. It is a useful answer.\n\n\
         Reply with one JSON object and nothing else:\n\
         {{\"judgement\":\"same-job\",\"why\":\"one short sentence\"}}\n",
        c.qualified_name, c.file, c.line, c.summary
    )
}

pub fn render_unavailable(u: &Unavailable) -> String {
    format!(
        "COULD-NOT-JUDGE — the affinity audit did not run.\n\n\
         {}\n\n  Fix it with:\n    {}\n\n\
         This refuses rather than answering from the raw code embeddings, which would \n\
         match how code is SPELLED rather than what it DOES.\n",
        u.reason, u.remedy
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(name: &str, summary: &str) -> SymbolSummary {
        SymbolSummary {
            summary: summary.to_string(),
            asks: Vec::new(),
            meta: SummaryMeta {
                name: name.to_string(),
                qualified_name: format!("crate::{name}"),
                file_path: format!("src/{name}.rs"),
                line_start: 1,
            },
        }
    }

    /// The whole point of the module: absent substrate REFUSES and names the
    /// remedy, rather than answering from something that would look right.
    #[test]
    fn a_missing_cache_refuses_and_names_the_command_that_fixes_it() {
        let d = tempfile::tempdir().unwrap();
        let err = load_summaries(d.path()).unwrap_err();
        assert!(err.reason.contains("nothing to match BEHAVIOUR against"));
        assert!(err.remedy.contains("svrn enrich code-intel"));
        let rendered = render_unavailable(&err);
        assert!(rendered.contains("COULD-NOT-JUDGE"));
    }

    #[test]
    fn a_corrupt_cache_refuses_too_rather_than_returning_an_empty_audit() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(cache_path(d.path()), "{ not json").unwrap();
        assert!(load_summaries(d.path()).is_err());
    }

    /// The case the other five detectors miss: same job, different names,
    /// different bodies.
    #[test]
    fn differently_named_implementations_of_one_job_cluster_together() {
        let summaries = vec![
            sym(
                "flip_text",
                "Reverses the characters in a string and returns the result.",
            ),
            sym(
                "backwards",
                "Takes a string and returns it with the characters reversed.",
            ),
            sym("add_user", "Inserts a new user record into the database."),
        ];
        let hits = shortlist(&summaries, "reverses the characters in a string", 10);
        let names: Vec<&str> = hits.iter().map(|c| c.qualified_name.as_str()).collect();
        assert!(names.contains(&"crate::flip_text"), "{names:?}");
        assert!(names.contains(&"crate::backwards"), "{names:?}");
        assert!(!names.contains(&"crate::add_user"), "{names:?}");
    }

    #[test]
    fn a_term_every_symbol_uses_cannot_carry_a_match_alone() {
        // "returns" appears in all three, so its IDF is ~0 and it must not
        // pull `add_user` into a string-reversal query.
        let summaries = vec![
            sym("a", "Returns the reversed string."),
            sym("b", "Returns the user id."),
            sym("c", "Returns the config."),
        ];
        let hits = shortlist(&summaries, "returns", 10);
        assert!(
            hits.is_empty(),
            "a ubiquitous term matched on its own: {hits:?}"
        );
    }

    #[test]
    fn the_prompt_carries_the_closed_set_and_the_escape_hatch() {
        let c = Candidate {
            qualified_name: "crate::f".into(),
            file: "src/f.rs".into(),
            line: 1,
            summary: "does a thing".into(),
            score: 1.0,
        };
        let p = compose_prompt("kernel_types::Verdict", "the outcome of any check", &c);
        for opt in ["same-job", "related-not-same", "unrelated", "unsure"] {
            assert!(p.contains(opt), "prompt omits {opt}");
        }
        assert!(p.contains("rather than guessing"));
    }

    #[test]
    fn stop_words_and_short_tokens_are_dropped_and_content_words_survive() {
        let t = query_terms("the outcome of a check is in it");
        // Content words are the query; the glue is not.
        assert_eq!(t, vec!["outcome", "check"]);
        for noise in ["the", "of", "is", "in", "it"] {
            assert!(!t.contains(&noise.to_string()), "{noise} survived");
        }
        // A query that is ONLY glue has nothing to match on, and `shortlist`
        // returns empty rather than matching everything.
        assert!(query_terms("the of a is in it").is_empty());
    }
}
