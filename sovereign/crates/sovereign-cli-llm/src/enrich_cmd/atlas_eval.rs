//! `sovereign enrich atlas-eval <atlas-corpus> --bank <path>` —
//! score the structural atlas against a question bank by tokenized
//! title-overlap retrieval.
//!
//! For each question:
//!   1. Tokenize the question (lowercase, drop stopwords + short
//!      tokens). The question itself is the only signal — no LLM,
//!      no vector search, no chunk reads.
//!   2. Score every Entity by Jaccard overlap of question tokens vs
//!      tokens in `canonical_name + aliases + description`.
//!   3. Take top-K by score.
//!   4. Compare top-K canonical_names against the question's
//!      `expected_sources` (case + underscore-folded).
//!
//! Output:
//!   - per-question hit/miss table (which expected_sources made
//!     top-K, at what rank).
//!   - aggregate precision@K, recall@K, MRR, median-rank-of-source.
//!
//! This is the structural-atlas-only signal — no chunk lookup, no
//! atom-edge expansion. A future variant can opt in to one-hop
//! Involves expansion to test "does the link graph add recall?".

use std::collections::HashSet;
use std::path::PathBuf;

use corpus_engine::enrichment::atlas::{read_atlas_atoms, AtomEnvelope, ATLAS_DIRNAME};
use corpus_engine::filters::normalize_title;

use super::paths;
use crate::eval_cmd::bank::load_bank;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich atlas-eval",
    summary: "Score the structural atlas against a question bank by tokenized title-overlap retrieval.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich atlas-eval <atlas-corpus> --bank <path> [--top-k N] [--include-placeholders] [--json]",
        ),
        HelpSection::Flags(&[
            (
                "--bank <path>",
                "Path to the eval bank TOML (e.g. sovereign/bench/wikipedia/questions.toml). Required.",
            ),
            (
                "--top-k <N>",
                "Top-K results per question for precision/recall scoring (default 10).",
            ),
            (
                "--include-placeholders",
                "Score placeholder entities too. Off by default — placeholders have empty descriptions, so the only signal is canonical_name.",
            ),
            (
                "--json",
                "Emit per-question results as JSON instead of the human-readable table.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign enrich atlas-eval wiki-l5-struct --bank sovereign/bench/wikipedia/questions.toml --top-k 10",
                "Score the structural-only retrieval against the wiki-core-v2 bank.",
            ),
        ]),
        HelpSection::Notes(
            "Tokenization is bag-of-words (lowercase, alphanumeric runs, drop tokens ≤ 2 chars). \
             Score = |question_tokens ∩ entity_tokens| / |question_tokens|. \
             Title match against expected_sources uses corpus_engine::filters::normalize_title \
             (case + underscore folded), the same normalisation the standard eval uses.",
        ),
    ],
};

pub async fn cmd_atlas_eval(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };
    let Some(corpus_id) = parsed.corpus_id.as_deref() else {
        eprintln!("error: missing <atlas-corpus> id");
        return 2;
    };
    let Some(bank_path) = parsed.bank.as_ref() else {
        eprintln!("error: --bank <path> is required");
        return 2;
    };

    let bank = match load_bank(bank_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    let atlas_dir = paths::index_root(corpus_id).join(ATLAS_DIRNAME);
    if !atlas_dir.exists() {
        eprintln!(
            "error: no atlas at {} — run `sovereign enrich ingest {corpus_id} --strategy structure_first --source-corpus <id>` first",
            atlas_dir.display()
        );
        return 1;
    }
    let atoms_file = match read_atlas_atoms(&atlas_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: reading atoms.json: {e}");
            return 1;
        }
    };

    // Build the searchable entity index. Each entry keeps the
    // canonical_name's tokens, the alias tokens, and the description
    // tokens in SEPARATE bags so the scorer can weight a
    // title-match higher than a description-match (a query
    // containing "Einstein" should rank `Albert Einstein` ahead of
    // every article whose description merely *mentions* Einstein).
    struct EntityIndex {
        canonical_name: String,
        normalized_title: String,
        name_tokens: HashSet<String>,
        alias_tokens: HashSet<String>,
        desc_tokens: HashSet<String>,
        title_char_len: usize,
    }
    let mut index: Vec<EntityIndex> = Vec::with_capacity(atoms_file.atoms.len());
    let mut placeholder_skipped = 0usize;
    for atom in &atoms_file.atoms {
        if let AtomEnvelope::Entity(e) = atom {
            let is_placeholder = e.description.is_empty() && e.salience == 0.0;
            if is_placeholder && !parsed.include_placeholders {
                placeholder_skipped += 1;
                continue;
            }
            let mut name_bag: HashSet<String> = HashSet::new();
            tokenize_into(&e.canonical_name, &mut name_bag);
            let mut alias_bag: HashSet<String> = HashSet::new();
            for a in &e.aliases {
                tokenize_into(a, &mut alias_bag);
            }
            // Subtract name tokens from alias bag so alias matches
            // don't double-count tokens already credited as name
            // matches.
            for t in &name_bag {
                alias_bag.remove(t);
            }
            let mut desc_bag: HashSet<String> = HashSet::new();
            tokenize_into(&e.description, &mut desc_bag);
            for t in &name_bag {
                desc_bag.remove(t);
            }
            for t in &alias_bag {
                desc_bag.remove(t);
            }
            index.push(EntityIndex {
                canonical_name: e.canonical_name.clone(),
                normalized_title: normalize_title(&e.canonical_name),
                name_tokens: name_bag,
                alias_tokens: alias_bag,
                desc_tokens: desc_bag,
                title_char_len: e.canonical_name.chars().count(),
            });
        }
    }

    // Per-question scoring.
    #[derive(Debug, Clone)]
    struct PerQuestion {
        id: String,
        category: String,
        question: String,
        expected_sources_normalized: Vec<String>,
        // (rank_in_topk, canonical_name) for each expected_source that
        // showed up in top-K. None if missed.
        hit_ranks: Vec<Option<usize>>,
        top_k_titles: Vec<String>,
    }
    let mut per_question: Vec<PerQuestion> = Vec::with_capacity(bank.questions.len());

    for q in &bank.questions {
        let mut q_tokens: HashSet<String> = HashSet::new();
        tokenize_into(&q.question, &mut q_tokens);
        if q_tokens.is_empty() {
            // Defensive; bank validation should reject empty questions.
            per_question.push(PerQuestion {
                id: q.id.clone(),
                category: q.category.clone(),
                question: q.question.clone(),
                expected_sources_normalized: q
                    .expected_sources
                    .iter()
                    .map(|s| normalize_title(s))
                    .collect(),
                hit_ranks: q.expected_sources.iter().map(|_| None).collect(),
                top_k_titles: Vec::new(),
            });
            continue;
        }

        // Score every entity by weighted token-overlap.
        //
        // Weights (3 / 2 / 1) reflect the prior that a title match is
        // a stronger retrieval signal than a description mention,
        // because Wikipedia titles ARE the canonical entity name.
        // Empirically: with Jaccard scoring divided by question-token
        // count, "Albert Einstein" got ranked below denser overlaps in
        // long questions; weighting name-tokens 3× brings it back to
        // the top when "einstein" appears in the query.
        //
        // Tiebreak: shorter canonical_name first (more specific —
        // "Einstein" beats "Albert Einstein in popular media").
        let mut scored: Vec<(u32, &EntityIndex)> = Vec::new();
        for e in &index {
            let name_hits: u32 = q_tokens
                .iter()
                .filter(|t| e.name_tokens.contains(*t))
                .count() as u32;
            let alias_hits: u32 = q_tokens
                .iter()
                .filter(|t| e.alias_tokens.contains(*t))
                .count() as u32;
            let desc_hits: u32 = q_tokens
                .iter()
                .filter(|t| e.desc_tokens.contains(*t))
                .count() as u32;
            let score = name_hits * 3 + alias_hits * 2 + desc_hits;
            if score == 0 {
                continue;
            }
            scored.push((score, e));
        }
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.title_char_len.cmp(&b.1.title_char_len))
                .then_with(|| a.1.canonical_name.cmp(&b.1.canonical_name))
        });

        let top_k_titles: Vec<String> = scored
            .iter()
            .take(parsed.top_k)
            .map(|(_, e)| e.canonical_name.clone())
            .collect();
        let top_k_normalized: Vec<String> = scored
            .iter()
            .take(parsed.top_k)
            .map(|(_, e)| e.normalized_title.clone())
            .collect();

        let expected_sources_normalized: Vec<String> = q
            .expected_sources
            .iter()
            .map(|s| normalize_title(s))
            .collect();

        let hit_ranks: Vec<Option<usize>> = expected_sources_normalized
            .iter()
            .map(|exp| top_k_normalized.iter().position(|t| t == exp))
            .collect();

        per_question.push(PerQuestion {
            id: q.id.clone(),
            category: q.category.clone(),
            question: q.question.clone(),
            expected_sources_normalized,
            hit_ranks,
            top_k_titles,
        });
    }

    // ── Aggregate ────────────────────────────────────────────
    let mut total_expected = 0usize;
    let mut total_hits = 0usize;
    let mut sum_recall = 0.0f64;
    let mut sum_precision = 0.0f64;
    let mut mrr_sum = 0.0f64;
    let mut question_with_at_least_one_source = 0usize;
    let mut sum_first_rank = 0usize;
    let mut first_rank_n = 0usize;

    for r in &per_question {
        let exp = r.expected_sources_normalized.len();
        let hits = r.hit_ranks.iter().filter(|h| h.is_some()).count();
        if exp == 0 {
            continue;
        }
        question_with_at_least_one_source += 1;
        total_expected += exp;
        total_hits += hits;
        sum_recall += (hits as f64) / (exp as f64);
        let prec = if r.top_k_titles.is_empty() {
            0.0
        } else {
            (hits as f64) / (r.top_k_titles.len() as f64)
        };
        sum_precision += prec;
        // MRR per question: 1/rank of the FIRST expected source that
        // appears in top-K, or 0 if none.
        let first_hit_rank = r.hit_ranks.iter().filter_map(|h| h.as_ref().copied()).min();
        if let Some(rank) = first_hit_rank {
            mrr_sum += 1.0 / ((rank + 1) as f64);
            sum_first_rank += rank + 1;
            first_rank_n += 1;
        }
    }
    let n = question_with_at_least_one_source.max(1);
    let avg_recall = sum_recall / (n as f64);
    let avg_precision = sum_precision / (n as f64);
    let mrr = mrr_sum / (n as f64);
    let micro_recall = if total_expected > 0 {
        (total_hits as f64) / (total_expected as f64)
    } else {
        0.0
    };
    let median_first_rank = if first_rank_n > 0 {
        Some((sum_first_rank as f64) / (first_rank_n as f64))
    } else {
        None
    };

    if parsed.json {
        let payload = serde_json::json!({
            "atlas_corpus": corpus_id,
            "bank": bank.bank.name,
            "bank_corpus": bank.bank.corpus,
            "top_k": parsed.top_k,
            "include_placeholders": parsed.include_placeholders,
            "scored_entities": index.len(),
            "placeholders_skipped": placeholder_skipped,
            "aggregate": {
                "questions_scored": question_with_at_least_one_source,
                "macro_recall": avg_recall,
                "macro_precision": avg_precision,
                "micro_recall": micro_recall,
                "mrr": mrr,
                "mean_first_hit_rank": median_first_rank,
            },
            "per_question": per_question.iter().map(|r| serde_json::json!({
                "id": r.id,
                "category": r.category,
                "question": r.question,
                "expected_sources": r.expected_sources_normalized,
                "hit_ranks": r.hit_ranks,
                "top_k": r.top_k_titles,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
        return 0;
    }

    println!("Atlas: {corpus_id}");
    println!("  bank             : {}", bank.bank.name);
    println!("  bank.corpus      : {}", bank.bank.corpus);
    println!(
        "  scored entities  : {} (placeholders skipped: {placeholder_skipped})",
        index.len()
    );
    println!("  top_k            : {}", parsed.top_k);
    println!();
    println!("Aggregate:");
    println!("  questions scored        : {question_with_at_least_one_source}");
    println!("  macro recall@K          : {:>6.3}", avg_recall);
    println!("  macro precision@K       : {:>6.3}", avg_precision);
    println!(
        "  micro recall@K          : {:>6.3} ({total_hits}/{total_expected})",
        micro_recall
    );
    println!("  MRR                     : {:>6.3}", mrr);
    if let Some(r) = median_first_rank {
        println!("  mean first-hit rank     : {:>6.2} (n={first_rank_n})", r);
    } else {
        println!("  mean first-hit rank     : (no hits)");
    }
    println!();
    println!(
        "Per-question (rank of each expected_source in top-{}):",
        parsed.top_k
    );
    for r in &per_question {
        let mark = if r.expected_sources_normalized.is_empty() {
            "—"
        } else if r.hit_ranks.iter().all(|h| h.is_some()) {
            "✓"
        } else if r.hit_ranks.iter().any(|h| h.is_some()) {
            "~"
        } else {
            "✗"
        };
        println!(
            "  {mark} [{}] {} ({})",
            r.category,
            r.id,
            short(&r.question, 80)
        );
        for (exp, rank) in r.expected_sources_normalized.iter().zip(r.hit_ranks.iter()) {
            match rank {
                Some(rk) => println!("        rank {:>3}  {}", rk + 1, exp),
                None => println!("        miss     {}", exp),
            }
        }
    }
    0
}

/// Tokenize on alphanumeric runs, lowercase, drop stopwords + tokens
/// ≤ 2 chars. Bag-of-words (no count). Inserts into `bag`.
fn tokenize_into(text: &str, bag: &mut HashSet<String>) {
    let mut current = String::new();
    let push = |bag: &mut HashSet<String>, current: &mut String| {
        if current.len() > 2 && !STOPWORDS.contains(&current.as_str()) {
            bag.insert(std::mem::take(current));
        } else {
            current.clear();
        }
    };
    for c in text.chars() {
        if c.is_alphanumeric() {
            for low in c.to_lowercase() {
                current.push(low);
            }
        } else if !current.is_empty() {
            push(bag, &mut current);
        }
    }
    if !current.is_empty() {
        push(bag, &mut current);
    }
}

/// Tiny English stopword list — only the highest-frequency function
/// words that would otherwise dominate Jaccard scores. Deliberately
/// short to avoid over-pruning rare-word signals.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "any", "can", "had", "her", "was",
    "one", "our", "out", "day", "get", "has", "him", "his", "how", "man", "new", "now", "old",
    "see", "two", "way", "who", "boy", "did", "its", "let", "put", "say", "she", "too", "use",
    "what", "when", "with", "from", "have", "this", "that", "they", "their", "them", "then",
    "than", "into", "were", "your", "about", "which", "would", "there", "between", "should",
    "could",
];

fn short(s: &str, max: usize) -> String {
    let collected: String = s.chars().take(max).collect();
    if collected.chars().count() < s.chars().count() {
        format!("{collected}…")
    } else {
        collected
    }
}

#[derive(Debug, Default)]
struct ParsedAtlasEval {
    corpus_id: Option<String>,
    bank: Option<PathBuf>,
    top_k: usize,
    include_placeholders: bool,
    json: bool,
}

fn parse_args(args: &[String]) -> Result<ParsedAtlasEval, String> {
    let mut out = ParsedAtlasEval {
        top_k: 10,
        ..Default::default()
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bank" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--bank requires a path".to_string())?;
                out.bank = Some(PathBuf::from(v));
                i += 2;
            }
            "--top-k" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--top-k requires a value".to_string())?;
                let n: usize = v
                    .parse()
                    .map_err(|e| format!("--top-k must be a positive integer: {e}"))?;
                if n == 0 {
                    return Err("--top-k must be > 0".into());
                }
                out.top_k = n;
                i += 2;
            }
            "--include-placeholders" => {
                out.include_placeholders = true;
                i += 1;
            }
            "--json" => {
                out.json = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if out.corpus_id.is_none() {
                    out.corpus_id = Some(other.to_string());
                    i += 1;
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_drops_short_and_stopwords() {
        let mut bag = HashSet::new();
        tokenize_into("The Big Bang theory was proposed", &mut bag);
        assert!(bag.contains("big"));
        assert!(bag.contains("bang"));
        assert!(bag.contains("theory"));
        assert!(bag.contains("proposed"));
        assert!(!bag.contains("the")); // stopword
        assert!(!bag.contains("was")); // stopword
    }

    #[test]
    fn tokenize_handles_punctuation_and_apostrophes() {
        let mut bag = HashSet::new();
        tokenize_into("Einstein's 1905 'Annus Mirabilis' papers", &mut bag);
        assert!(bag.contains("einstein"));
        assert!(bag.contains("1905"));
        assert!(bag.contains("annus"));
        assert!(bag.contains("mirabilis"));
        assert!(bag.contains("papers"));
        // Apostrophe-s drops to "s" → too short → not in bag.
        assert!(!bag.contains("s"));
    }

    #[test]
    fn parse_minimal_invocation() {
        let p = parse_args(&[
            "wiki-l5-struct".into(),
            "--bank".into(),
            "/tmp/x.toml".into(),
        ])
        .unwrap();
        assert_eq!(p.corpus_id.as_deref(), Some("wiki-l5-struct"));
        assert_eq!(p.bank, Some(PathBuf::from("/tmp/x.toml")));
        assert_eq!(p.top_k, 10);
    }
}
