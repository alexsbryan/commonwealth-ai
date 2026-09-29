// SPDX-License-Identifier: AGPL-3.0-or-later
//! The three white-box modes of `eval run` — `--routing-only`,
//! `--prod-pipeline` and the default raw-index mode — as bench runs them:
//! exec svrn's probe (`svrn __probe`), read the evidence it writes, and score
//! it against the bank with the scorers this module's siblings own.
//!
//! svrn describes its internals; bench owns the bank, the expectations and
//! the verdict (phase-b-58, ARCH principle 12). The precedent is
//! `bench_cmd::all::run_routing_only`, which already reads this lane's
//! output across a process boundary.

use std::process::{Command, Stdio};

use corpus_index::types::ScoredChunk;
use sovereign_contracts::probe::{
    PoolChunk, PoolEvidence, ProbeEvidence, ProbeQuestion, ProbeRequest, RoutingEvidence,
};
use sovereign_core::traits::InferenceProvider;

use super::attribution;
use super::bank::{EvalBank, ExpectedIntent, Question};
use super::lost_corpora;
use super::runner::{
    EvalResult, EvalRun, RetrievedChunk, RoutingResult, RoutingRun, ScoreSnapshot,
};
use super::score::{
    score_essay_readiness, score_facts, score_sources, score_sources_loose, EssayReadinessScore,
    JudgeSourceDetail,
};
use crate::chat_cmd::config::ChatGlobals;

/// The bank's questions as the probe takes them: id and text, nothing the
/// bank expects of them.
pub(super) fn probe_questions(bank: &EvalBank) -> Vec<ProbeQuestion> {
    bank.questions
        .iter()
        .map(|q| ProbeQuestion {
            id: q.id.clone(),
            question: q.question.clone(),
        })
        .collect()
}

/// Exec `svrn __probe` with this invocation's globals and `request`, and read
/// back what it wrote. The child's stdout goes to our stderr, so `eval run
/// --format json` keeps a clean stdout.
pub(crate) fn run_probe(
    globals: &ChatGlobals,
    request: &ProbeRequest,
) -> Result<ProbeEvidence, String> {
    let tmp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let request_path = tmp.path().join("request.json");
    let output_path = tmp.path().join("evidence.json");
    let bytes = serde_json::to_vec(request).map_err(|e| format!("serialise request: {e}"))?;
    std::fs::write(&request_path, bytes).map_err(|e| format!("write request: {e}"))?;

    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let svrn = sovereign_cli_shared::dispatcher::dispatcher_exe(&exe)?;
    let mut argv = vec!["__probe".to_string()];
    argv.extend(globals.to_argv());
    argv.extend([
        "--request".to_string(),
        request_path.display().to_string(),
        "--output".to_string(),
        output_path.display().to_string(),
    ]);
    tracing::debug!(svrn = %svrn.display(), ?argv, "exec probe");
    let status = Command::new(&svrn)
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .status()
        .map_err(|e| format!("spawn {}: {e}", svrn.display()))?;
    if !status.success() {
        return Err(format!(
            "`svrn __probe` ({}) exited {}",
            request.mode.as_str(),
            status
                .code()
                .map_or("by signal".to_string(), |c| c.to_string())
        ));
    }
    let bytes = std::fs::read(&output_path).map_err(|e| format!("read evidence: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse evidence: {e}"))
}

/// The probe answers in request order; a row that does not line up with the
/// bank is refused, never scored against the wrong question.
fn paired<'a, T>(
    bank: &'a EvalBank,
    rows: &'a [T],
    id: impl Fn(&T) -> &str,
) -> Result<impl Iterator<Item = (&'a Question, &'a T)>, String> {
    if rows.len() != bank.questions.len() {
        return Err(format!(
            "probe answered {} of {} questions",
            rows.len(),
            bank.questions.len()
        ));
    }
    if let Some((q, r)) = bank.questions.iter().zip(rows).find(|(q, r)| q.id != id(r)) {
        return Err(format!(
            "probe row `{}` is out of order (expected `{}`)",
            id(r),
            q.id
        ));
    }
    Ok(bank.questions.iter().zip(rows))
}

/// Score the classifier's decisions against each question's
/// `expected_intent` (or, if absent, the category default from
/// `Question::default_expected_intent`).
pub(super) fn score_routing(
    bank: &EvalBank,
    started_at_unix: i64,
    rows: &[RoutingEvidence],
) -> Result<RoutingRun, String> {
    let results: Vec<RoutingResult> = paired(bank, rows, |r| r.id.as_str())?
        .map(|(q, r)| score_routing_row(q, r))
        .collect();
    let metrics = crate::eval_cmd::routing_metrics::RoutingMetrics::from_results(&results);
    Ok(RoutingRun {
        bank_name: bank.bank.name.clone(),
        started_at_unix,
        results,
        metrics,
    })
}

fn score_routing_row(q: &Question, r: &RoutingEvidence) -> RoutingResult {
    let expected = match &q.expected_intent {
        Some(s) => ExpectedIntent::Exact(
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
    if let Some(e) = &r.error {
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
            latency_ms: r.latency_ms,
        };
    }
    RoutingResult {
        question_id: q.id.clone(),
        category: q.category.clone(),
        question: q.question.clone(),
        expected: expected.label(),
        actual_intent: r.intent.clone(),
        coarse_intent: r.coarse_intent.clone(),
        confidence: r.confidence,
        rationale: r.rationale.clone(),
        correct: expected.matches(&r.intent),
        latency_ms: r.latency_ms,
    }
}

/// Bench-prod parity mode (`--prod-pipeline`): the pool the PRODUCTION
/// KnowledgeQuery retrieval pipeline returned for each question, scored with
/// the same rigid source/fact scorers as the raw-index mode. Note the pool
/// size is the pipeline's own (KQ_MERGED_LIMIT + grounding injections), not
/// the raw lane's `--limit` — scores are baseline-comparable only within this
/// mode. `loose` is the judge for `--loose-source-judge`, when it was passed.
pub(super) async fn score_prod(
    bank: &EvalBank,
    limit: usize,
    started_at_unix: i64,
    rows: &[PoolEvidence],
    loose: Option<&dyn InferenceProvider>,
) -> Result<EvalRun, String> {
    let mut results = Vec::with_capacity(rows.len());
    for (q, ev) in paired(bank, rows, |r| r.id.as_str())? {
        results.push(score_prod_row(q, ev, loose).await?);
    }
    Ok(EvalRun {
        bank_name: bank.bank.name.clone(),
        corpus: bank.bank.corpus.clone(),
        limit,
        started_at_unix,
        results,
    })
}

async fn score_prod_row(
    q: &Question,
    ev: &PoolEvidence,
    loose: Option<&dyn InferenceProvider>,
) -> Result<EvalResult, String> {
    let empty_result = |qq: &Question| EvalResult {
        error: None,
        question_id: qq.id.clone(),
        category: qq.category.clone(),
        question: qq.question.clone(),
        retrieved: Vec::new(),
        source_score: score_sources(&qq.expected_sources, &[]).into(),
        fact_score: score_facts(&qq.expected_facts, &[]).into(),
        embed_ms: 0,
        search_ms: 0,
        corpora_hit: Vec::new(),
        vector_eligible: true,
        synth: None,
        loose_source_score: None,
        loose_source_evidence: Vec::new(),
        essay_readiness: None,
        atlas_navigation: Vec::new(),
        meta_atlas_hits: Vec::new(),
        atlas_walk: None,
    };
    if let Some(e) = &ev.error {
        return Ok(empty_result(q).with_error(e.clone()));
    }

    // A turn that lost a corpus in scope cannot be scored — see
    // `lost_corpora::refusal_for_lost_corpora` for why, and why the refusal
    // lives here rather than in the mesh fan-out.
    if let Some(why) = lost_corpora::refusal_for_lost_corpora(&ev.unavailable_corpora) {
        return Ok(empty_result(q).with_error(why));
    }

    let all_hits = scored_chunks(&ev.chunks);
    // Same attribution projection as the raw-index scorer (see
    // score_retrieve_row): conversation-history banks must not credit a
    // restatement as evidence.
    let hits_for_scoring = attributed(q, &all_hits);
    let rigid_source = score_sources(&q.expected_sources, &hits_for_scoring);
    let source_score: ScoreSnapshot = rigid_source.clone().into();
    let fact_score: ScoreSnapshot = score_facts(&q.expected_facts, &hits_for_scoring).into();

    // Loose-judge source scoring, same contract as the raw-index path:
    // a strict superset of the rigid score that credits a missing
    // expected_source when the retrieved chunks materially cover it.
    //
    // This path used to hardcode `loose_source_score: None` and drop the
    // flag on the floor — `--prod-pipeline --loose-source-judge` parsed,
    // threaded this far, then produced a rigid-only result with exit 0 and
    // no warning (note 890823ac). That made the ONE question the flag
    // exists to answer unanswerable on the only surface worth asking it
    // on, which is what has kept the GLiNER deletion (L0, up to 2.07x on
    // time-to-enriched) unresolved. §18.3: never silently substitute.
    let (loose_source_score, loose_source_evidence): (
        Option<ScoreSnapshot>,
        Vec<JudgeSourceDetail>,
    ) = match loose {
        Some(judge) if !q.expected_sources.is_empty() => {
            let (loose, details) =
                score_sources_loose(&q.question, &rigid_source, &hits_for_scoring, judge).await;
            (Some(loose.into()), details)
        }
        _ => (None, Vec::new()),
    };

    // An echo this build cannot read is a broken contract between svrn and
    // bench; the run refuses rather than reporting "the walk did not run".
    let atlas_walk = match &ev.atlas_walk {
        None => None,
        Some(v) => Some(
            serde_json::from_value(v.clone())
                .map_err(|e| format!("question `{}`: atlas walk echo unreadable: {e}", q.id))?,
        ),
    };

    Ok(EvalResult {
        error: None,
        question_id: q.id.clone(),
        category: q.category.clone(),
        question: q.question.clone(),
        retrieved: retrieved(&ev.chunks),
        source_score,
        fact_score,
        embed_ms: 0,
        search_ms: ev.search_ms,
        corpora_hit: ev.corpora_hit.clone(),
        vector_eligible: true,
        synth: None,
        loose_source_score,
        loose_source_evidence,
        essay_readiness: None,
        atlas_navigation: Vec::new(),
        meta_atlas_hits: Vec::new(),
        atlas_walk,
    })
}

/// Raw-index mode: the pool svrn's own index search returned for each
/// question, scored. `loose` and `essay` are the judges for
/// `--loose-source-judge` and `--essay-judge`, when they were passed.
pub(super) async fn score_retrieve(
    bank: &EvalBank,
    limit: usize,
    started_at_unix: i64,
    rows: &[PoolEvidence],
    loose: Option<&dyn InferenceProvider>,
    essay: Option<&dyn InferenceProvider>,
) -> Result<EvalRun, String> {
    let mut results = Vec::with_capacity(rows.len());
    for (q, ev) in paired(bank, rows, |r| r.id.as_str())? {
        results.push(score_retrieve_row(q, ev, loose, essay).await);
    }
    Ok(EvalRun {
        bank_name: bank.bank.name.clone(),
        corpus: bank.bank.corpus.clone(),
        limit,
        started_at_unix,
        results,
    })
}

async fn score_retrieve_row(
    q: &Question,
    ev: &PoolEvidence,
    loose: Option<&dyn InferenceProvider>,
    essay: Option<&dyn InferenceProvider>,
) -> EvalResult {
    if let Some(e) = &ev.error {
        // An errored row rather than aborting the whole run — one bad
        // question shouldn't void the bank.
        return EvalResult {
            question_id: q.id.clone(),
            category: q.category.clone(),
            question: q.question.clone(),
            retrieved: Vec::new(),
            source_score: score_sources(&q.expected_sources, &[]).into(),
            fact_score: score_facts(&q.expected_facts, &[]).into(),
            embed_ms: ev.embed_ms,
            search_ms: 0,
            corpora_hit: Vec::new(),
            vector_eligible: false,
            synth: None,
            loose_source_score: None,
            loose_source_evidence: Vec::new(),
            essay_readiness: None,
            atlas_navigation: Vec::new(),
            meta_atlas_hits: Vec::new(),
            atlas_walk: None,
            // `with_error` below sets `error`, which is what keeps this
            // row out of the baseline comparison instead of scoring it 0.
            error: None,
        }
        .with_error(e.clone());
    }

    // 3. Score. Rigid source/fact match runs against actual source
    // passages only — atlas navigation does not credit
    // `expected_sources` (a virtual entity card titled `physicalism`
    // isn't a passage from the physicalism article, just a pointer to
    // it).
    //
    // For conversation-history banks where `attribution_mode` is
    // `user` or `assistant`, hits are projected through
    // `attribution::filter_chunk_content` first so a model's
    // restatement of the user's question does not score as evidence
    // of the user having said it (or vice versa). No-op for
    // non-conversation chunks (no turn headers to match).
    let all_hits = scored_chunks(&ev.chunks);
    let atlas_navigation = scored_chunks(&ev.atlas_navigation);
    let hits_for_scoring = attributed(q, &all_hits);
    let rigid_source = score_sources(&q.expected_sources, &hits_for_scoring);
    let source_score: ScoreSnapshot = rigid_source.clone().into();
    let fact_score: ScoreSnapshot = score_facts(&q.expected_facts, &hits_for_scoring).into();

    // 3b. Loose-judge source scoring (Option A). Opt-in via
    //     `--loose-source-judge`. Adds an LLM pass that looks at the
    //     missing expected_sources and credits any whose topic IS
    //     materially covered by the retrieved chunks (paraphrase /
    //     canonical-sibling / indirect coverage). Pairs with the rigid
    //     score as a strict superset — never lowers the matched count,
    //     only lifts it. Audit detail per source lands in
    //     `loose_source_evidence` so a reviewer can verify each
    //     loose-credit decision without re-running.
    let (loose_source_score, loose_source_evidence): (
        Option<ScoreSnapshot>,
        Vec<JudgeSourceDetail>,
    ) = match loose {
        Some(judge) if !q.expected_sources.is_empty() => {
            let (loose, details) =
                score_sources_loose(&q.question, &rigid_source, &hits_for_scoring, judge).await;
            (Some(loose.into()), details)
        }
        _ => (None, Vec::new()),
    };

    // 3c. Essay-readiness multi-axis judge (Option C). Opt-in via
    //     `--essay-judge`. Asks the LLM whether the retrieved set is
    //     enough material for an undergraduate essay, scoring on four
    //     axes (topical_coverage, position_attribution,
    //     dialectical_breadth, argument_depth), each 0–3. Decoupled
    //     from loose source-credit because they answer different
    //     questions: loose = "are the right articles in the bag?",
    //     essay-readiness = "does the bag have essay-worthy substance?"
    let essay_readiness: Option<EssayReadinessScore> = match essay {
        Some(judge) => {
            score_essay_readiness(
                &q.question,
                &q.category,
                &hits_for_scoring,
                &atlas_navigation,
                judge,
            )
            .await
        }
        None => None,
    };

    // 4. Pack.
    EvalResult {
        error: None,
        question_id: q.id.clone(),
        category: q.category.clone(),
        question: q.question.clone(),
        retrieved: retrieved(&ev.chunks),
        source_score,
        fact_score,
        embed_ms: ev.embed_ms,
        search_ms: ev.search_ms,
        corpora_hit: ev.corpora_hit.clone(),
        vector_eligible: ev.vector_eligible,
        synth: None,
        loose_source_score,
        loose_source_evidence,
        essay_readiness,
        atlas_navigation: retrieved(&ev.atlas_navigation),
        meta_atlas_hits: Vec::new(),
        atlas_walk: None,
    }
}

/// The pool as the scorers read it. They read `content` and `title` only;
/// nothing in this process acquired these chunks, and the provenance says so.
fn scored_chunks(chunks: &[PoolChunk]) -> Vec<ScoredChunk> {
    chunks
        .iter()
        .map(|c| ScoredChunk {
            content: c.content.clone(),
            title: c.title.clone(),
            url: c.url.clone(),
            corpus_id: c.corpus_id.clone(),
            score: c.score,
            metadata: Default::default(),
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
            provenance: corpus_index::index::ChunkProvenance::off_the_wire(),
        })
        .collect()
}

/// The bank's `attribution_mode` projection over the pool.
fn attributed(q: &Question, hits: &[ScoredChunk]) -> Vec<ScoredChunk> {
    let attribution_mode = attribution::AttributionMode::from_str(&q.attribution_mode);
    if attribution_mode == attribution::AttributionMode::Both {
        hits.to_vec()
    } else {
        hits.iter()
            .map(|h| {
                let mut filtered = h.clone();
                filtered.content = attribution::filter_chunk_content(&h.content, attribution_mode);
                filtered
            })
            .collect()
    }
}

/// The pool as the run record carries it.
fn retrieved(chunks: &[PoolChunk]) -> Vec<RetrievedChunk> {
    chunks
        .iter()
        .map(|c| RetrievedChunk {
            corpus_id: c.corpus_id.clone(),
            title: c.title.clone(),
            url: c.url.clone(),
            score: c.score,
            snippet: c.content.replace('\n', " "),
            source: None,
            // retrieval mode builds no prompt — `None` is
            // "not known here", never a defaulted true.
            in_prompt: None,
            prompt_text: None,
        })
        .collect()
}
