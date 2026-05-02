//! Per-question runner.
//!
//! Two modes share a result shape so a retrieval baseline and a synth
//! baseline can be diffed against each other:
//!
//!   - **Retrieval** ([`run_bank`]) — embed → hybrid search → score
//!     facts/sources against the retrieved chunk bag. Cheap, isolates
//!     the index/embed/filter axis from the chat-model axis.
//!   - **Synth** ([`run_bank_synth`]) — drive the full
//!     `Runtime::handle_message_stream` path the desktop chat surface
//!     uses (intent classifier → router → search tools → prompt
//!     assembly → chat completion). Score `expected_facts` against the
//!     synthesised answer text and `expected_sources` against the
//!     `retrieved_chunks` provenance metadata. Exercises the routing
//!     and aggregation layers (which are tunable knobs in their own
//!     right), at the cost of one chat-model call per question.
//!
//! Both modes serialise into the same `EvalRun` so a single JSON file
//! can be diffed against another regardless of which mode produced it;
//! the synth-specific payload lives under the optional `synth` field
//! on `EvalResult`.

use std::time::Instant;

use corpus_engine::ScoredChunk;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::chat_cmd::bootstrap::ChatSession;
use crate::chat_cmd::render::split_reasoning;
use crate::eval_cmd::bank::{EvalBank, Question};
use crate::eval_cmd::score::{
    score_facts, score_facts_in_text, score_sources, score_sources_titles, FactScore, SourceScore,
};

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
    /// In retrieval mode this is "facts present in the retrieved chunk
    /// text"; in synth mode this is "facts present in the model's
    /// answer". `synth.chunks_fact_score` carries the
    /// retrieval-haystack version when synth mode is active, so the
    /// answer-vs-retrieval delta is directly readable.
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
    /// Populated only by [`run_bank_synth`]. Carries the synthesised
    /// answer + provenance signals so reports / diffs can show how the
    /// chat-model and routing layers performed on top of retrieval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synth: Option<SynthSnapshot>,
}

/// Synth-mode payload. Only populated when the eval drove the full
/// chat pipeline (intent classifier → router → search → completion).
/// All fields are best-effort: the metadata block on the persisted
/// assistant message is the source of truth, and missing fields stay
/// `None` rather than poisoning the row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthSnapshot {
    /// The visible portion of the model's answer (after `<think>` blocks
    /// are stripped, mirroring the desktop's `parse-message.ts`).
    pub answer: String,
    /// Total chars across all `<think>` blocks. Cheap signal for "did
    /// the model spend more time reasoning than answering?".
    pub reasoning_chars: usize,
    /// Wall-time around `handle_message_stream` until the stream
    /// drained. Distinct from `provenance.total_latency_ms`, which the
    /// runtime measures on its own clock.
    pub stream_wall_ms: u64,
    /// `provenance.total_latency_ms` from the persisted message
    /// metadata, when present.
    pub total_latency_ms: Option<u64>,
    /// `provenance.intent` — what the classifier decided. Crucial for
    /// debugging routing-layer regressions ("why is this question
    /// routing to ChitChat instead of KnowledgeQuery?").
    pub intent: Option<String>,
    /// Origins of every retrieval source the runtime touched, e.g.
    /// `corpus-wikipedia`, `web`, `conversation-history`. Empty when
    /// the runtime answered without retrieval.
    pub source_origins: Vec<String>,
    /// Number of chunks the runtime ultimately surfaced for synthesis.
    pub retrieved_chunk_count: usize,
    /// Diagnostic: the same fact rule applied to the *snippets* in
    /// `retrieved_chunks` rather than the answer. Lets the report
    /// distinguish "retrieval missed the fact" from "retrieval had the
    /// fact but the model didn't surface it." Snippets are truncated
    /// to ~200 chars by the runtime, so this is a lower bound on what
    /// retrieval actually saw — read alongside the retrieval-mode
    /// baseline for the unbiased number.
    pub chunks_fact_score: ScoreSnapshot,
    /// Instructor-mode (LLM-as-judge) score: per fact, did a fast-slot
    /// model decide the concept was conveyed by the answer? Catches
    /// paraphrase coverage that the strict keyword-AND scorer misses.
    /// `None` when the run was launched with `--no-judge`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_fact_score: Option<ScoreSnapshot>,
    /// Per-fact audit trail for the judge calls — verbatim evidence
    /// quote (or `"(absent)"`) so a reviewer can verify yes/no
    /// decisions without re-running. Empty when `--no-judge`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub judge_evidence: Vec<crate::eval_cmd::score::JudgeFactDetail>,
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

/// One classifier decision against the bank, scored against the
/// per-question expected intent (or category default). Output of the
/// `--routing-only` mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingResult {
    pub question_id: String,
    pub category: String,
    pub question: String,
    pub expected: String,
    pub actual_intent: String,
    pub coarse_intent: Option<String>,
    pub confidence: f32,
    pub rationale: Option<String>,
    pub correct: bool,
    pub latency_ms: u64,
}

/// Roll-up of a routing-only run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRun {
    pub bank_name: String,
    pub started_at_unix: i64,
    pub results: Vec<RoutingResult>,
}

/// Drive every question through the router classifier ONLY — no
/// retrieval, no synthesis. Scores the routing decision against each
/// question's `expected_intent` (or, if absent, the category default
/// from `Question::default_expected_intent`). Wall time is dominated
/// by the classifier LLM call (~0.5-2s on the fast slot) so a 20-row
/// bank finishes in ~30s. Used to tune the classifier prompt against
/// a small fast-slot model without burning a full synth eval per
/// iteration.
pub async fn run_bank_routing(
    session: &ChatSession,
    bank: &EvalBank,
) -> Result<RoutingRun, String> {
    let started_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut results = Vec::with_capacity(bank.questions.len());
    for q in &bank.questions {
        let result = run_question_routing(session, q).await;
        results.push(result);
    }

    Ok(RoutingRun {
        bank_name: bank.bank.name.clone(),
        started_at_unix,
        results,
    })
}

async fn run_question_routing(session: &ChatSession, q: &Question) -> RoutingResult {
    use sovereign_core::types::{ConversationContext, Intent};

    let expected = match &q.expected_intent {
        Some(s) => crate::eval_cmd::bank::ExpectedIntent::Exact(
            // Expected strings are stored as owned Strings on the
            // bank. The `ExpectedIntent::Exact` variant takes a
            // 'static str for the category-default path; for an
            // operator-supplied override we leak the string into a
            // `String` and compare via `matches` below. Cheaper to
            // just match here directly.
            Box::leak(s.clone().into_boxed_str()),
        ),
        None => q.default_expected_intent(),
    };

    // Build a near-empty context. The classifier prompt reads
    // `installed_corpora` from this struct (it tells the model "we
    // have wikipedia, sep loaded — prefer LOOKUP for factual
    // questions"), so we mirror what `build_session` would have
    // surfaced. Skill hints / corrections are intentionally absent:
    // the eval scores BASE classifier behaviour, not the corrected
    // behaviour.
    let installed = session
        .corpus_engine
        .installed_indexes()
        .await
        .map(|ix| ix.into_iter().map(|i| i.corpus_id).collect::<Vec<_>>())
        .unwrap_or_default();
    let context = ConversationContext {
        conversation: sovereign_core::types::Conversation {
            id: "eval-routing".into(),
            title: None,
            messages: vec![],
            created_at: 0,
            updated_at: 0,
            version: 0,
            deleted_at: None,
            skill_id: None,
        },
        memories: vec![],
        working_memory: None,
        installed_corpora: installed,
        document_session: None,
        topic_context: None,
        knowledge_view_digests: None,
        temporal_tensions: Vec::new(),
    };

    let t = Instant::now();
    let classification = match session
        .runtime
        .router
        .classify(&q.question, &context, &[])
        .await
    {
        Ok(c) => c,
        Err(e) => {
            return RoutingResult {
                question_id: q.id.clone(),
                category: q.category.clone(),
                question: q.question.clone(),
                expected: expected.label(),
                actual_intent: format!("error: {e}"),
                coarse_intent: None,
                confidence: 0.0,
                rationale: None,
                correct: false,
                latency_ms: t.elapsed().as_millis() as u64,
            };
        }
    };
    let latency_ms = t.elapsed().as_millis() as u64;

    let actual_intent = intent_wire_label(&classification.primary.intent);
    let correct = expected.matches(&actual_intent);

    RoutingResult {
        question_id: q.id.clone(),
        category: q.category.clone(),
        question: q.question.clone(),
        expected: expected.label(),
        actual_intent,
        coarse_intent: classification.coarse_intent.clone(),
        confidence: classification.primary.confidence,
        rationale: classification.rationale.clone(),
        correct,
        latency_ms,
    }
}

/// Lowercase wire form of an Intent — matches the strings used in
/// the bank's `expected_intent` field and the category-default map.
fn intent_wire_label(intent: &sovereign_core::types::Intent) -> String {
    use sovereign_core::types::Intent;
    match intent {
        Intent::SimpleQuery => "simple_query".into(),
        Intent::KnowledgeQuery => "knowledge_query".into(),
        Intent::DeepQuery => "deep_query".into(),
        Intent::ComparisonQuery => "comparison_query".into(),
        Intent::MetalingualQuery => "metalingual_query".into(),
        Intent::ConationQuery => "conation_query".into(),
        Intent::CommissiveQuery => "commissive_query".into(),
        Intent::ExpressiveQuery => "expressive_query".into(),
        Intent::ComplexTask => "complex_task".into(),
        Intent::SimpleAction { .. } => "simple_action".into(),
        Intent::Continuation { .. } => "continuation".into(),
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
                synth: None,
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
        synth: None,
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

// ─── synth path ────────────────────────────────────────────────────
//
// The synth path is a separate top-level entry point because it shares
// almost nothing with the retrieval path at the call-site: there's no
// per-corpus search loop here (the runtime owns that), and the
// per-question result is constructed from a `Conversation` row in the
// state store rather than from `ScoredChunk`s the CLI got back
// directly. The eval framework's `EvalResult` is the one
// abstraction-boundary that they share.

/// Drive every question through `Runtime::handle_message_stream` and
/// score the persisted answer + provenance. Sequential — the chat
/// model is a single GPU slot and concurrent turns would just queue.
///
/// `judge` toggles the LLM-as-judge "instructor mode" pass. When on,
/// each question's answer is also scored by a fast-slot judge that
/// asks per-fact whether the concept is conveyed; results land in
/// `synth.judge_fact_score`. The strict keyword scorer always runs
/// regardless. See `score::score_facts_judge`.
pub async fn run_bank_synth(
    session: &ChatSession,
    bank: &EvalBank,
    judge: bool,
) -> Result<EvalRun, String> {
    let started_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // We don't gate synth on `installed_indexes()` the way `run_bank`
    // does — the runtime will route to the corpus tools (or web, or
    // none) on its own based on intent, and a question that ends up
    // routed to web-only is a meaningful eval signal, not a precondition
    // failure. Misconfigured (no corpus AND no chat model) bootstraps
    // already failed in `build_session` upstream.

    let mut results = Vec::with_capacity(bank.questions.len());
    for q in &bank.questions {
        let result = run_question_synth(session, q, judge).await;
        results.push(result);
    }

    Ok(EvalRun {
        bank_name: bank.bank.name.clone(),
        corpus: bank.bank.corpus.clone(),
        // `limit` is meaningless under synth — the runtime decides how
        // many chunks to surface. Surface zero so the JSON makes that
        // explicit rather than implying a bound that wasn't enforced.
        limit: 0,
        started_at_unix,
        results,
    })
}

async fn run_question_synth(session: &ChatSession, q: &Question, judge: bool) -> EvalResult {
    let conversation_id = uuid::Uuid::new_v4().to_string();
    let t_wall = Instant::now();

    // 1. Drive the same path the desktop chat surface uses. Failures
    //    here become an empty-row result so one model-side error
    //    doesn't void the rest of the bank.
    let handle = match session
        .runtime
        .handle_message_stream(&q.question, &conversation_id)
        .await
    {
        Ok(h) => h,
        Err(e) => {
            return empty_synth_result(q, format!("stream start: {e}"), 0);
        }
    };
    let message_id = handle.message_id.clone();
    let mut stream = handle.stream;

    // 2. Drain the token stream. Buffer raw — we'll split out the
    //    `<think>` blocks once before scoring rather than streaming
    //    deltas the way the chat CLI does (no human is watching).
    let mut raw = String::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => raw.push_str(&chunk),
            Err(e) => {
                let elapsed = t_wall.elapsed().as_millis() as u64;
                return empty_synth_result(q, format!("stream error: {e}"), elapsed);
            }
        }
    }
    let stream_wall_ms = t_wall.elapsed().as_millis() as u64;

    // 3. Pull the persisted assistant row to recover the metadata
    //    block. This is where `retrieved_chunks` and `provenance` live;
    //    without them we can't score sources.
    let metadata = session
        .store
        .get_conversation(&conversation_id)
        .await
        .ok()
        .and_then(|c| {
            c.messages
                .iter()
                .find(|m| m.id == message_id)
                .and_then(|m| m.metadata.clone())
        });

    // 4. Split reasoning vs answer the same way the desktop client does.
    let (reasoning_blocks, visible) = split_reasoning(&raw);
    let reasoning_chars: usize = reasoning_blocks.iter().map(|b| b.chars().count()).sum();

    // 5. Pull provenance signals out of the metadata. Anything missing
    //    becomes None / empty rather than aborting — a model that
    //    answered without retrieval is a valid (if pessimistic)
    //    measurement.
    let prov = metadata.as_ref().and_then(|m| m.get("provenance"));
    let total_latency_ms = prov
        .and_then(|p| p.get("total_latency_ms"))
        .and_then(|v| v.as_u64());
    let intent = prov
        .and_then(|p| p.get("intent"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let source_origins: Vec<String> = prov
        .and_then(|p| p.get("sources"))
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("origin").and_then(|o| o.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    // 6. Walk `retrieved_chunks` for source-title matching + the
    //    snippet-haystack diagnostic. We deliberately do NOT filter to
    //    the bank's `corpus` field here: a chunk with a matching title
    //    but a different `corpus_id` (e.g. a folder corpus the user
    //    happens to have indexed alongside wikipedia) is still a
    //    legitimate source hit, and filtering would create false
    //    "missed" rows that mask a real win.
    let retrieved_chunks_meta = metadata
        .as_ref()
        .and_then(|m| m.get("retrieved_chunks"))
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();

    let titles: Vec<String> = retrieved_chunks_meta
        .iter()
        .filter_map(|c| c.get("title").and_then(|t| t.as_str()))
        .map(str::to_string)
        .collect();
    let snippets: Vec<String> = retrieved_chunks_meta
        .iter()
        .filter_map(|c| c.get("snippet").and_then(|t| t.as_str()))
        .map(str::to_string)
        .collect();
    let corpora_hit: Vec<String> = {
        let mut seen: Vec<String> = Vec::new();
        for c in &retrieved_chunks_meta {
            if let Some(cid) = c.get("corpus_id").and_then(|v| v.as_str()) {
                if !cid.is_empty() && !seen.iter().any(|s| s == cid) {
                    seen.push(cid.to_string());
                }
            }
        }
        seen
    };

    // 7. Build the `RetrievedChunk` summaries the report renders.
    //    Score field is `0.0` because the metadata doesn't carry it —
    //    consumers that care about ranking can rerun in retrieval
    //    mode where it does.
    let retrieved: Vec<RetrievedChunk> = retrieved_chunks_meta
        .iter()
        .map(|c| RetrievedChunk {
            corpus_id: c
                .get("corpus_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            title: c
                .get("title")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            url: c
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            score: 0.0,
            snippet: c
                .get("snippet")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .collect();

    // 8. Score: facts → answer text, sources → retrieved-chunk titles,
    //    plus the snippet-fact diagnostic.
    let snippet_haystack = snippets.join("\n");
    let fact_score: ScoreSnapshot =
        score_facts_in_text(&q.expected_facts, &visible).into();
    let chunks_fact_score: ScoreSnapshot =
        score_facts_in_text(&q.expected_facts, &snippet_haystack).into();
    let source_score: ScoreSnapshot =
        score_sources_titles(&q.expected_sources, &titles).into();

    // 8b. Instructor-mode pass — LLM-as-judge concept-conveyed score.
    //     Strict keyword score above is preserved unchanged; this
    //     adds a parallel column in the report. Skipped under
    //     `--no-judge`. The judge call also returns a per-fact
    //     evidence trail (quote or "(absent)") for auditability.
    let (judge_fact_score, judge_evidence): (Option<ScoreSnapshot>, _) = if judge {
        let (score, details) = crate::eval_cmd::score::score_facts_judge(
            &q.expected_facts,
            &visible,
            session.inference.as_ref(),
        )
        .await;
        (Some(score.into()), details)
    } else {
        (None, Vec::new())
    };

    let synth = SynthSnapshot {
        answer: visible,
        reasoning_chars,
        stream_wall_ms,
        total_latency_ms,
        intent,
        source_origins,
        retrieved_chunk_count: retrieved_chunks_meta.len(),
        chunks_fact_score,
        judge_fact_score,
        judge_evidence,
    };

    EvalResult {
        question_id: q.id.clone(),
        category: q.category.clone(),
        question: q.question.clone(),
        retrieved,
        source_score,
        fact_score,
        // Synth doesn't measure embed/search latency directly — those
        // are folded into `total_latency_ms`. Zero here is "not
        // applicable in this mode" and the report renders it as such.
        embed_ms: 0,
        search_ms: 0,
        corpora_hit,
        // Vector eligibility is a retrieval-mode concept (does the
        // embed dim match the index dim?). True under synth means
        // "the runtime did not fall back to FTS-only" — but the
        // runtime doesn't expose that today, so we report `true` and
        // let consumers consult the retrieval-mode baseline.
        vector_eligible: true,
        synth: Some(synth),
    }
}

fn empty_synth_result(q: &Question, err: String, stream_wall_ms: u64) -> EvalResult {
    eprintln!("  [{}] {err}", q.id);
    EvalResult {
        question_id: q.id.clone(),
        category: q.category.clone(),
        question: q.question.clone(),
        retrieved: Vec::new(),
        source_score: score_sources(&q.expected_sources, &[]).into(),
        fact_score: score_facts_in_text(&q.expected_facts, "").into(),
        embed_ms: 0,
        search_ms: 0,
        corpora_hit: Vec::new(),
        vector_eligible: false,
        synth: Some(SynthSnapshot {
            answer: String::new(),
            reasoning_chars: 0,
            stream_wall_ms,
            total_latency_ms: None,
            intent: None,
            source_origins: Vec::new(),
            retrieved_chunk_count: 0,
            chunks_fact_score: score_facts_in_text(&q.expected_facts, "").into(),
            judge_fact_score: None,
            judge_evidence: Vec::new(),
        }),
    }
}
