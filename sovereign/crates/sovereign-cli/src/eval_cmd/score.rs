//! Deterministic, glassbox scoring of retrieval results.
//!
//! Two scorers, both pure (no LLM, no embeddings). They're crude on
//! purpose: scoring is supposed to be readable so a developer can tell
//! at a glance which fact matched and which didn't. Embedding-based
//! and LLM-judge scorers can layer on later behind opt-in flags
//! without changing the call sites.
//!
//! Score conventions:
//!   - `matched / expected` is the headline number (0..1).
//!   - `missing` is preserved verbatim so the report can list exactly
//!     what slipped through.
//!   - A question with zero expected items in a dimension is treated
//!     as N/A (not 0/0 = NaN, not 1.0). The runner skips that dimension.

use corpus_engine::ScoredChunk;

/// Result of comparing a question's `expected_sources` against the
/// titles of chunks that came back from retrieval.
#[derive(Debug, Clone)]
pub struct SourceScore {
    pub matched: Vec<String>,
    pub missing: Vec<String>,
    pub total_expected: usize,
}

impl SourceScore {
    pub fn ratio(&self) -> Option<f32> {
        if self.total_expected == 0 {
            None
        } else {
            Some(self.matched.len() as f32 / self.total_expected as f32)
        }
    }
}

/// Match `expected_sources` against the titles of `retrieved` chunks.
/// Title comparison goes through `corpus_engine::filters::normalize_title`,
/// which lowercases and folds underscores/whitespace — so `"Albert
/// Einstein"`, `"albert_einstein"`, and `"Albert  Einstein"` all match
/// the same expected entry.
pub fn score_sources(expected: &[String], retrieved: &[ScoredChunk]) -> SourceScore {
    let retrieved_titles: Vec<String> = retrieved
        .iter()
        .filter_map(|c| c.title.as_deref())
        .map(corpus_engine::filters::normalize_title)
        .collect();

    let mut matched = Vec::new();
    let mut missing = Vec::new();
    for want in expected {
        let want_norm = corpus_engine::filters::normalize_title(want);
        if retrieved_titles.iter().any(|t| t == &want_norm) {
            matched.push(want.clone());
        } else {
            missing.push(want.clone());
        }
    }
    SourceScore {
        matched,
        missing,
        total_expected: expected.len(),
    }
}

/// Result of a fact-coverage check across the retrieved bag-of-text.
#[derive(Debug, Clone)]
pub struct FactScore {
    pub matched: Vec<String>,
    pub missing: Vec<String>,
    pub total_expected: usize,
}

impl FactScore {
    pub fn ratio(&self) -> Option<f32> {
        if self.total_expected == 0 {
            None
        } else {
            Some(self.matched.len() as f32 / self.total_expected as f32)
        }
    }
}

/// Crude fact-coverage scorer: a fact is "matched" if every space-
/// separated keyword token in it appears (case-insensitive substring)
/// somewhere in the concatenated retrieved-chunk text. Tokens shorter
/// than 3 chars are dropped — they're stopword-y and produce noise.
///
/// This is glassbox by construction. The bank author can read each
/// expected_fact, see which keywords it'll match on, and tighten or
/// loosen accordingly. Fancier scorers (embedding cosine, LLM judge)
/// can layer on later; this one returns a number you can defend.
pub fn score_facts(expected: &[String], retrieved: &[ScoredChunk]) -> FactScore {
    let haystack = retrieved
        .iter()
        .map(|c| c.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();

    let mut matched = Vec::new();
    let mut missing = Vec::new();
    for fact in expected {
        let tokens = keyword_tokens(fact);
        if tokens.is_empty() {
            // No content tokens — treat as a no-op rather than a miss
            // (a zero-token fact is a bank bug, not a retrieval miss).
            continue;
        }
        if tokens.iter().all(|t| haystack.contains(t)) {
            matched.push(fact.clone());
        } else {
            missing.push(fact.clone());
        }
    }
    FactScore {
        matched,
        missing,
        total_expected: expected.len(),
    }
}

/// Same matching rule as [`score_facts`] but against an arbitrary
/// haystack — used by the `--synth` path to score expected_facts
/// against the model's synthesised answer rather than the bag of
/// retrieved chunks. Keeping the rule identical means a fact that
/// would have scored against the chunks scores the same way against
/// the answer; the *only* thing that changes is the haystack.
pub fn score_facts_in_text(expected: &[String], text: &str) -> FactScore {
    let haystack = text.to_lowercase();
    let mut matched = Vec::new();
    let mut missing = Vec::new();
    for fact in expected {
        let tokens = keyword_tokens(fact);
        if tokens.is_empty() {
            continue;
        }
        if tokens.iter().all(|t| haystack.contains(t)) {
            matched.push(fact.clone());
        } else {
            missing.push(fact.clone());
        }
    }
    FactScore {
        matched,
        missing,
        total_expected: expected.len(),
    }
}

/// Source-match against pre-extracted titles. Used by the synth path,
/// which only has the metadata `retrieved_chunks` array (titles, no
/// `ScoredChunk`s) to work with. Normalisation matches `score_sources`.
pub fn score_sources_titles<S: AsRef<str>>(expected: &[String], titles: &[S]) -> SourceScore {
    let normalized: Vec<String> = titles
        .iter()
        .map(|t| corpus_engine::filters::normalize_title(t.as_ref()))
        .collect();

    let mut matched = Vec::new();
    let mut missing = Vec::new();
    for want in expected {
        let want_norm = corpus_engine::filters::normalize_title(want);
        if normalized.iter().any(|t| t == &want_norm) {
            matched.push(want.clone());
        } else {
            missing.push(want.clone());
        }
    }
    SourceScore {
        matched,
        missing,
        total_expected: expected.len(),
    }
}

fn keyword_tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3)
        .map(|t| t.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn chunk(title: &str, content: &str) -> ScoredChunk {
        ScoredChunk {
            content: content.into(),
            title: Some(title.into()),
            url: None,
            corpus_id: "wikipedia".into(),
            score: 1.0,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn source_match_normalizes_titles() {
        let retrieved = vec![chunk("Albert_Einstein", "...")];
        let s = score_sources(&["Albert Einstein".into()], &retrieved);
        assert_eq!(s.matched, vec!["Albert Einstein".to_string()]);
        assert!(s.missing.is_empty());
    }

    #[test]
    fn source_match_reports_missing() {
        let retrieved = vec![chunk("Niels Bohr", "...")];
        let s = score_sources(
            &["Albert Einstein".into(), "Niels Bohr".into()],
            &retrieved,
        );
        assert_eq!(s.matched, vec!["Niels Bohr".to_string()]);
        assert_eq!(s.missing, vec!["Albert Einstein".to_string()]);
    }

    #[test]
    fn fact_match_requires_all_tokens() {
        let retrieved = vec![chunk("Einstein", "Einstein discovered photoelectric effect in 1905.")];
        let s = score_facts(&["photoelectric effect".into(), "Brownian motion".into()], &retrieved);
        assert_eq!(s.matched, vec!["photoelectric effect".to_string()]);
        assert_eq!(s.missing, vec!["Brownian motion".to_string()]);
    }

    #[test]
    fn fact_match_is_case_insensitive() {
        let retrieved = vec![chunk("X", "PHOTOELECTRIC EFFECT happened.")];
        let s = score_facts(&["photoelectric effect".into()], &retrieved);
        assert_eq!(s.matched.len(), 1);
    }

    #[test]
    fn empty_expected_yields_none_ratio() {
        let s = score_facts(&[], &[]);
        assert!(s.ratio().is_none());
    }

    #[test]
    fn keyword_tokens_drops_short_words() {
        let toks = keyword_tokens("a brief fact about Newton");
        assert_eq!(toks, vec!["brief", "fact", "about", "newton"]);
    }
}
