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
//! **It was first run on this corpus 2026-08-31**: 20,668 summaries in 9h32m
//! against the `fast` slot, written to `code_intel_cache.json` under the index
//! directory. Before that the substrate genuinely did not exist and this module
//! refused for that reason; the refusal path below is still live and still
//! correct for a corpus that has not been enriched.
//!
//! The tempting move is to fall back to the raw code embeddings that DO exist
//! (853MB of `chunks.lance`) and return something. That would match on
//! vocabulary and syntax rather than behaviour, and would return a plausible,
//! confident, wrong answer — the characteristic failure ARCH §18 exists to
//! catch. So this refuses instead, and names the one command that fixes it.
//! Absence is reported, never defaulted (§18.3).

use std::collections::{HashMap, HashSet};
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
///
/// `source_root` is the repository the corpus was built from, and it is
/// REQUIRED rather than optional. The ingest applies no scope filter and does
/// not consult `.gitignore`, so the cache indexes every path it walked — on
/// this corpus that includes `target/*/build/llama-cpp-sys-*/out/` (the same
/// vendored file once per cargo build hash), `target-xwin/`, and vendored
/// trees under `research/`. Measured 2026-08-31: of 63 symbols the enrichment
/// pass could not summarize, 45 were generated or vendored and 18 were ours.
///
/// Prior art means "already exists HERE". A hit against llama.cpp's
/// `LlamaFileType` is a false positive, and per `concept_gate` a
/// false-positive machine gets switched off inside a week. The git index is
/// the decider for source-versus-generated, so this argument is positional:
/// you cannot load summaries without saying whose they are (§7, structural
/// rather than remembered).
pub fn load_summaries(
    index_path: &Path,
    source_root: &Path,
) -> Result<Vec<SymbolSummary>, Unavailable> {
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
    let all = serde_json::from_str::<Vec<SymbolSummary>>(&text).map_err(|e| Unavailable {
        reason: format!("{}: {e}", path.display()),
        remedy: "re-run `svrn enrich code-intel` to regenerate the cache".to_string(),
    })?;
    Ok(retain_tracked(all, source_root))
}

/// Drop summaries for paths the git index does not track.
///
/// A repo git cannot answer for degrades to UNFILTERED and says so at WARN.
/// The substitution is named, never silent (§18.3) — a caller that silently
/// matched against build output would be the confident-wrong-answer failure
/// this module exists to refuse.
fn retain_tracked(all: Vec<SymbolSummary>, source_root: &Path) -> Vec<SymbolSummary> {
    let total = all.len();
    let Some(tracked) = tracked_paths(source_root) else {
        tracing::warn!(
            target: "affinity.prior_art",
            root = %source_root.display(),
            summaries = total,
            "git ls-files failed: prior art is NOT filtered, so generated and \
             vendored paths are eligible to match"
        );
        return all;
    };
    let kept: Vec<SymbolSummary> = all
        .into_iter()
        .filter(|s| tracked.contains(s.meta.file_path.as_str()))
        .collect();
    tracing::debug!(
        target: "affinity.prior_art",
        listed = total,
        tracked = kept.len(),
        dropped = total - kept.len(),
        "summaries outside the git index are not this codebase's prior art"
    );
    kept
}

/// Every path the git index tracks, repo-relative — the same spelling the
/// enrichment cache stores in `meta.file_path`.
fn tracked_paths(source_root: &Path) -> Option<HashSet<String>> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(source_root)
        .args(["ls-files", "-z"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// Narrow the corpus to symbols of the DESTINATION's own kind, and drop rows
/// the graph lists more than once.
///
/// Measured by running the tool on a real order (`rf-field-atom-kernel-types-corpusid`,
/// destination `kernel_types::CorpusId`) 2026-08-31: of the top ten, nine were
/// FUNCTIONS — `resolve_source_corpus()`, `main()`, `entity_content_hash()` —
/// because a function that merely *mentions* corpora shares vocabulary with a
/// description of one. "What should become this type" is a question about
/// declarations. The cache is ~87% callables (19,363 to 2,497 here), so without
/// this the answer is swamped by construction.
///
/// Kind comes from the SCIP descriptor suffix, the same decider
/// `code_intel::prompt_kind_for` uses: `Name#` is a type, `name().` a callable
/// (ARCH §10.6). A destination we cannot classify narrows nothing — absence of
/// a signal is not a licence to filter (§18.3).
///
/// The dedup is the second thing that run surfaced: `attribute_failures.py:99`
/// appeared TWICE at an identical score, once per scip-python commit hash, so
/// two of ten slots were one row.
pub fn narrow_to_destination_kind(
    summaries: &[SymbolSummary],
    destination: &str,
) -> Vec<SymbolSummary> {
    let wants_type = destination
        .rsplit("::")
        .next()
        .and_then(|seg| seg.chars().next())
        .map(char::is_uppercase);
    let mut seen: HashSet<(String, usize, String)> = HashSet::new();
    summaries
        .iter()
        .filter(|s| match wants_type {
            Some(true) => s.meta.qualified_name.trim_end().ends_with('#'),
            Some(false) => s.meta.qualified_name.trim_end().ends_with("()."),
            None => true,
        })
        .filter(|s| {
            seen.insert((
                s.meta.file_path.clone(),
                s.meta.line_start,
                s.meta.name.clone(),
            ))
        })
        .cloned()
        .collect()
}

/// `crate::name` from a SCIP descriptor, for reading rather than for matching.
///
/// The raw descriptor is
/// `rust-analyzer cargo sovereign-cli-llm 0.6.0 enrich_cmd/atlas_patch_code/resolve_source_corpus().`
/// — 80 characters of exporter bookkeeping around the ~30 that identify the
/// symbol. An agent pays for every one of them on every row. The crate is kept
/// because "the same name in two crates" is the question this tool exists to
/// answer; the rest is dropped.
pub fn readable(qualified_name: &str, fallback: &str) -> String {
    let toks: Vec<&str> = qualified_name.split_whitespace().collect();
    // `<tool> <manager> <crate> <version> <path>` for rust-analyzer; scip-python
    // substitutes a commit hash, which is why the crate is read positionally and
    // an unexpected shape falls back to the plain name rather than guessing.
    let krate = if toks.len() >= 5 && toks[0] == "rust-analyzer" {
        Some(toks[2])
    } else {
        None
    };
    match krate {
        Some(k) => format!("{k}::{fallback}"),
        None => fallback.to_string(),
    }
}

/// One line of a summary, clipped on a word boundary.
///
/// Enrichment summaries run to 80 words. Thirty rows of those is a wall an
/// agent skims past, and a tool that gets skimmed past gets routed around.
pub fn clip(text: &str, max: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let cut: String = t.chars().take(max).collect();
    match cut.rfind(' ') {
        Some(i) => format!("{}…", &cut[..i]),
        None => format!("{cut}…"),
    }
}

/// A candidate site the audit surfaced.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub qualified_name: String,
    /// The bare symbol name. Carried so rendering never has to parse a
    /// descriptor back apart.
    pub name: String,
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
                name: s.meta.name.clone(),
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
        let err = load_summaries(d.path(), d.path()).unwrap_err();
        assert!(err.reason.contains("nothing to match BEHAVIOUR against"));
        assert!(err.remedy.contains("svrn enrich code-intel"));
        let rendered = render_unavailable(&err);
        assert!(rendered.contains("COULD-NOT-JUDGE"));
    }

    #[test]
    fn a_corrupt_cache_refuses_too_rather_than_returning_an_empty_audit() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(cache_path(d.path()), "{ not json").unwrap();
        assert!(load_summaries(d.path(), d.path()).is_err());
    }

    /// A summary positioned in the graph: distinct from the 2-arg `sym`
    /// above, which only needs a name and a description.
    fn sym_at(name: &str, file: &str, line: usize, qn: &str) -> SymbolSummary {
        SymbolSummary {
            summary: format!("about {name}"),
            asks: vec![],
            meta: SummaryMeta {
                name: name.to_string(),
                qualified_name: qn.to_string(),
                file_path: file.to_string(),
                line_start: line,
            },
        }
    }

    /// "What should become this type" is a question about DECLARATIONS.
    ///
    /// Watched to fail first: without the narrowing, this returns 2 — the
    /// function comes back because its description shares vocabulary with the
    /// query. On the real corpus that was nine of the top ten.
    #[test]
    fn a_type_destination_ranks_against_types_not_callables() {
        let all = vec![
            sym_at(
                "CorpusId",
                "kernel-types/src/lib.rs",
                10,
                "rust-analyzer cargo kernel-types 0.1.0 lib/CorpusId#",
            ),
            sym_at(
                "resolve_source_corpus",
                "a.rs",
                20,
                "rust-analyzer cargo c 0.1.0 m/resolve_source_corpus().",
            ),
        ];
        let got = narrow_to_destination_kind(&all, "kernel_types::CorpusId");
        assert_eq!(got.len(), 1, "only the type is eligible prior art");
        assert_eq!(got[0].meta.name, "CorpusId");
    }

    /// The graph lists some symbols once per exporter commit. Two of ten
    /// shortlist slots were one Python `main()` at an identical score.
    #[test]
    fn a_row_the_graph_lists_twice_is_ranked_once() {
        let all = vec![
            sym_at("Thing", "x.rs", 5, "scip-python python . AAAA `m`/Thing#"),
            sym_at("Thing", "x.rs", 5, "scip-python python . BBBB `m`/Thing#"),
        ];
        assert_eq!(narrow_to_destination_kind(&all, "k::Thing").len(), 1);
    }

    /// Absence of a signal is not a licence to filter (§18.3): a destination
    /// whose kind we cannot read must narrow nothing rather than narrow wrongly.
    #[test]
    fn an_unclassifiable_destination_narrows_nothing() {
        let all = vec![
            sym_at("Thing", "x.rs", 5, "rust-analyzer cargo c 0.1.0 m/Thing#"),
            sym_at("go", "y.rs", 6, "rust-analyzer cargo c 0.1.0 m/go()."),
        ];
        assert_eq!(narrow_to_destination_kind(&all, "").len(), 2);
    }

    /// Generated and vendored paths are not this codebase's prior art.
    ///
    /// The ingest applies no scope filter, so the cache legitimately contains
    /// `target/*/build/llama-cpp-sys-*/out/` — the same vendored file once per
    /// cargo build hash. Matching a freshly-minted concept against one of those
    /// is a false positive about code we do not own, and per `concept_gate` a
    /// false-positive machine gets switched off inside a week.
    #[test]
    fn summaries_outside_the_git_index_are_not_prior_art() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.invalid"]);
        git(&["config", "user.name", "t"]);
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/real.rs"), "pub fn f() {}").unwrap();
        git(&["add", "src/real.rs"]);

        let cache = serde_json::json!([
            {"summary":"ours","asks":[],
             "meta":{"name":"Real","qualified_name":"c::Real",
                     "file_path":"src/real.rs","line_start":1}},
            {"summary":"vendored","asks":[],
             "meta":{"name":"Siglip","qualified_name":"x::Siglip",
                     "file_path":"target/debug/build/llama-cpp-sys-4-abc/out/x.py",
                     "line_start":1}}
        ]);
        std::fs::write(cache_path(repo), serde_json::to_string(&cache).unwrap()).unwrap();

        let got = load_summaries(repo, repo).unwrap();
        assert_eq!(got.len(), 1, "only the git-tracked summary is prior art");
        assert_eq!(got[0].meta.name, "Real");
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
            name: "f".into(),
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
