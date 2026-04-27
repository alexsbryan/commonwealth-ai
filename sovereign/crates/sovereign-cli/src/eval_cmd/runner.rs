//! Per-question runner.
//!
//! For each question:
//!   1. Embed the question through the daemon's embed slot.
//!   2. Open every installed corpus index whose `corpus_id` matches the
//!      bank's target. Fall back to FTS-only when query dims don't
//!      match the index's stored embedding dims (mirrors `chat_cmd::inspect`).
//!   3. Run hybrid (vector + FTS) search up to `--limit` chunks.
//!   4. Score sources + facts via `score::*`.
//!   5. Capture the result for `report::*` to render.
//!
//! Synthesis (calling `/v1/chat/completions` and judging the answer) is
//! deliberately deferred to a later iteration. The retrieval-only loop
//! is enough to localise where atlas / filter / chunker changes are
//! actually moving the needle, and synthesis can layer on without
//! re-shaping the runner.

use std::time::Instant;

use corpus_engine::ScoredChunk;
use serde::{Deserialize, Serialize};

use crate::chat_cmd::bootstrap::ChatSession;
use crate::eval_cmd::bank::{EvalBank, Question};
use crate::eval_cmd::score::{score_facts, score_sources, FactScore, SourceScore};

/// One full run of a bank against a corpus. Serialisable so a run can
/// be archived and diffed against a later run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRun {
    pub bank_name: String,
    pub corpus: String,
    pub limit: usize,
    pub started_at_unix: i64,
    pub results: Vec<EvalResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalResult {
    pub question_id: String,
    pub category: String,
    pub question: String,
    pub retrieved: Vec<RetrievedChunk>,
    pub source_score: ScoreSnapshot,
    pub fact_score: ScoreSnapshot,
    pub embed_ms: u64,
    pub search_ms: u64,
    /// Distinct corpora that contributed at least one chunk. Useful for
    /// detecting cases where the bank's `corpus` filter and the
    /// installed-index landscape disagreed.
    pub corpora_hit: Vec<String>,
    /// True iff the embed dim matched the index dim. False = FTS-only,
    /// which is a meaningful signal for "your embed model and your
    /// index are not the same vintage."
    pub vector_eligible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievedChunk {
    pub corpus_id: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub score: f32,
    /// Truncated to ~600 chars to keep run files readable; the full
    /// chunk lives in the index if the developer wants to drill in.
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreSnapshot {
    pub matched: Vec<String>,
    pub missing: Vec<String>,
    pub total_expected: usize,
    /// `None` when `total_expected == 0`. Lets the report distinguish
    /// "passed perfectly" (1.0) from "nothing to measure" (None).
    pub ratio: Option<f32>,
}

impl From<SourceScore> for ScoreSnapshot {
    fn from(s: SourceScore) -> Self {
        let ratio = s.ratio();
        Self {
            matched: s.matched,
            missing: s.missing,
            total_expected: s.total_expected,
            ratio,
        }
    }
}

impl From<FactScore> for ScoreSnapshot {
    fn from(s: FactScore) -> Self {
        let ratio = s.ratio();
        Self {
            matched: s.matched,
            missing: s.missing,
            total_expected: s.total_expected,
            ratio,
        }
    }
}

/// Run an entire bank, sequentially. Sequential is fine — the daemon's
/// embed slot serialises anyway, and concurrent searches against the
/// same Lance table contend on the same index pages.
pub async fn run_bank(
    session: &ChatSession,
    bank: &EvalBank,
    limit: usize,
) -> Result<EvalRun, String> {
    let started_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let indexes = session
        .corpus_engine
        .installed_indexes()
        .await
        .map_err(|e| format!("installed_indexes(): {e}"))?;

    if indexes.is_empty() {
        return Err(format!(
            "no corpora installed — `sovereign corpus install {}` before running this bank",
            bank.bank.corpus
        ));
    }

    let target_indexes: Vec<_> = indexes
        .iter()
        .filter(|info| info.corpus_id == bank.bank.corpus)
        .collect();

    if target_indexes.is_empty() {
        let installed: Vec<&str> = indexes.iter().map(|i| i.corpus_id.as_str()).collect();
        return Err(format!(
            "bank corpus `{}` is not installed. Installed: {installed:?}",
            bank.bank.corpus
        ));
    }

    let mut results = Vec::with_capacity(bank.questions.len());
    for q in &bank.questions {
        let result = run_question(session, &target_indexes, q, limit).await;
        results.push(result);
    }

    Ok(EvalRun {
        bank_name: bank.bank.name.clone(),
        corpus: bank.bank.corpus.clone(),
        limit,
        started_at_unix,
        results,
    })
}

async fn run_question(
    session: &ChatSession,
    target_indexes: &[&corpus_engine::IndexInfo],
    q: &Question,
    limit: usize,
) -> EvalResult {
    // 1. Embed.
    let t_embed = Instant::now();
    let embedding = match session.inference.embed_query(&q.question).await {
        Ok(v) => v,
        Err(e) => {
            // Bubble up as an empty-result row rather than aborting the
            // whole run — one bad question shouldn't void the bank.
            return EvalResult {
                question_id: q.id.clone(),
                category: q.category.clone(),
                question: q.question.clone(),
                retrieved: Vec::new(),
                source_score: score_sources(&q.expected_sources, &[]).into(),
                fact_score: score_facts(&q.expected_facts, &[]).into(),
                embed_ms: t_embed.elapsed().as_millis() as u64,
                search_ms: 0,
                corpora_hit: Vec::new(),
                vector_eligible: false,
                // Note: error message isn't carried in the result row
                // today; the runner's stderr already logged it. Add a
                // `note: Option<String>` field if this becomes annoying.
            }
            .with_error(format!("embed: {e}"));
        }
    };
    let embed_ms = t_embed.elapsed().as_millis() as u64;

    // 2. Search every matching corpus index.
    let t_search = Instant::now();
    let mut all_hits: Vec<ScoredChunk> = Vec::new();
    let mut any_vector_eligible = false;
    let mut corpora_hit: Vec<String> = Vec::new();

    for info in target_indexes {
        let dim_match = info.embedding_dimensions == embedding.len();
        if dim_match {
            any_vector_eligible = true;
        }
        let query_vec: &[f32] = if dim_match { &embedding } else { &[] };
        let idx = match session.corpus_engine.open_index(&info.path).await {
            Ok(i) => i,
            Err(e) => {
                eprintln!("  open_index({}): {e}", info.corpus_id);
                continue;
            }
        };
        match idx.search(query_vec, &q.question, limit).await {
            Ok(hits) => {
                if !hits.is_empty() && !corpora_hit.contains(&info.corpus_id) {
                    corpora_hit.push(info.corpus_id.clone());
                }
                all_hits.extend(hits);
            }
            Err(e) => {
                eprintln!("  search({}): {e}", info.corpus_id);
            }
        }
    }
    let search_ms = t_search.elapsed().as_millis() as u64;

    // Re-rank merged hits by score descending and trim to limit (we
    // searched up to `limit` per corpus, so the merged set may be
    // larger; rank-merge keeps the report focused on the strongest
    // hits regardless of which index produced them).
    all_hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    all_hits.truncate(limit);

    // 3. Score.
    let source_score: ScoreSnapshot = score_sources(&q.expected_sources, &all_hits).into();
    let fact_score: ScoreSnapshot = score_facts(&q.expected_facts, &all_hits).into();

    // 4. Pack.
    let retrieved = all_hits
        .iter()
        .map(|c| RetrievedChunk {
            corpus_id: c.corpus_id.clone(),
            title: c.title.clone(),
            url: c.url.clone(),
            score: c.score,
            snippet: truncate(&c.content.replace('\n', " "), 600),
        })
        .collect();

    EvalResult {
        question_id: q.id.clone(),
        category: q.category.clone(),
        question: q.question.clone(),
        retrieved,
        source_score,
        fact_score,
        embed_ms,
        search_ms,
        corpora_hit,
        vector_eligible: any_vector_eligible,
    }
}

impl EvalResult {
    /// Used by the embed-failure branch above. Today this just returns
    /// `self`; kept as a hook so a future revision can attach the
    /// error string to a `note` field without changing call sites.
    fn with_error(self, msg: String) -> Self {
        eprintln!("  [{}] {msg}", self.question_id);
        self
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}
