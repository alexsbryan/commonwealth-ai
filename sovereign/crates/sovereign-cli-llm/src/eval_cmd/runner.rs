// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-question runner.
//!
//! Two modes share a result shape so a retrieval baseline and a synth
//! baseline can be diffed against each other:
//!
//!   - **Retrieval** ([`run_bank`]) — embed → hybrid search → score
//!     facts/sources against the retrieved chunk bag. Cheap, isolates
//!     the index/embed/filter axis from the chat-model axis.
//!   - **Synth** ([`run_bank_synth`]) — ask svrn over its turn route: the full
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

use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::bench_cmd::subject::SubjectDial;
use crate::chat_cmd::render::split_reasoning;
use crate::eval_cmd::atlas_walk_meta::atlas_walk_from_metadata;
use crate::eval_cmd::attribution;
use crate::eval_cmd::bank::{EvalBank, Question};
use crate::eval_cmd::score::{
    score_facts_in_text, score_sources, score_sources_titles, EssayReadinessScore, FactScore,
    JudgeSourceDetail, SourceScore,
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
    /// Why this question produced no measurement, when it produced none.
    ///
    /// `None` means the run answered — well or badly, but it answered, and the
    /// scores below are a measurement. `Some` means it did NOT, and the scores
    /// are the shape of a measurement rather than one.
    ///
    /// Minted 2026-08-26. `empty_synth_result` was scoring a failed turn as
    /// `source_score 0.0 / fact_score 0.0`, printing the error to stderr and
    /// putting nothing in the report — so a daemon returning
    /// `503 host busy / local_queue_full` was indistinguishable from a model
    /// that answered with nothing, and the baseline diff counted it as a
    /// regression. That is ARCH §18.3's named smell exactly ("an `Err`
    /// collapsed into a success-shaped value"), and §18.2's four verdicts
    /// collapsed to two. Measured: `synth:sep` reported FAIL(3reg) with 9 of
    /// 15 questions holding a 503 and NOT ONE EXECUTED QUESTION REGRESSED.
    ///
    /// Consumers must EXCLUDE these rows from a comparison rather than score
    /// them — see `bench_cmd::all::classify_retrieval`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    /// Loose-judge source score (Option A). Populated when `eval run`
    /// is launched with `--loose-source-judge`. Treats the rigid
    /// `source_score` as a floor and asks an LLM to additionally
    /// credit any *missing* expected_sources whose topic IS materially
    /// covered by the retrieved chunks (paraphrase / canonical-sibling
    /// / indirect coverage all count). Lets atlas-grounded retrieval
    /// be evaluated honestly on `extraction_first` corpora where
    /// titles don't match slugs literally. `None` when the flag was
    /// not set on the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loose_source_score: Option<ScoreSnapshot>,
    /// Per-source audit trail for the loose judge — short rationale
    /// per source so a reviewer can verify each loose-credit decision
    /// without re-running. Empty when `--loose-source-judge` was not set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loose_source_evidence: Vec<JudgeSourceDetail>,
    /// Essay-readiness multi-axis judge (Option C). Populated when
    /// `eval run` is launched with `--essay-judge`. Where the loose
    /// source judge answers "are the right articles in the bag?", this
    /// answers "does the bag have what an undergraduate essay needs?"
    /// — topical breadth, position attribution, dialectical breadth,
    /// argument depth — each on 0-3 with a short rationale. `None`
    /// when the flag was not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub essay_readiness: Option<EssayReadinessScore>,
    /// Atlas-derived virtual chunks that were surfaced for this
    /// question — entity cards, claim atoms, tension edges,
    /// configurations. Pulled separately from `retrieved` because they
    /// don't compete for source-passage slots: the essay-judge prompt
    /// renders them as a "navigation, not evidence" section. Captured
    /// in the JSON output so a reviewer can audit *what* the navigation
    /// section showed the model, distinct from what it claimed to
    /// retrieve.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub atlas_navigation: Vec<RetrievedChunk>,
    /// Move 5: meta-atlas hit records — one per anchor (max 3 per
    /// matched meta-atom, one per articulation axis with a dominant
    /// anchor) the cross-corpus meta-atlas surfaced for this
    /// question. The bench's fourth lens over retrieval: "which
    /// canonical entities did the meta-atlas recognise, and which
    /// stream did each anchor serve?". Empty when the meta-atlas
    /// didn't match the question's entities (the common case) and on
    /// retrieval-mode runs (synth path only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub meta_atlas_hits: Vec<MetaAtlasHitEcho>,
    /// The atlas walk's evidence PATH for this question — which atoms it
    /// reached, by which edge, and what the fetch did with the requests.
    ///
    /// Distinct from `atlas_navigation` above, which is an embedding top-K
    /// snapshot that never enters the prompt. This is the walk that DID enter
    /// it, and until now it existed only as a tracing event `svrn eval run`
    /// cannot emit (`atlas-grounding: fetch ledger`).
    ///
    /// `None` = the walk did not run for this row, which includes every lane
    /// that does not drive the production pipeline. Absent is never "reached
    /// nothing" — `Some` with empty `nodes` is that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atlas_walk: Option<sovereign_contracts::daemon_wire::AtlasWalkEcho>,
}

/// Echo of `sovereign_core::runtime::MetaAtlasHitRecord` for the
/// per-question JSON. Kept as a separate type so the bench schema is
/// not coupled to runtime internals — if the runtime adds fields the
/// bench output stays stable until we explicitly forward them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaAtlasHitEcho {
    pub entity: String,
    pub corpus_id: String,
    /// `"inventory" | "argument" | "trace"` — dominant articulation
    /// of the anchor.
    pub articulation: String,
    /// `"frozen" | "versioned" | "rolling"` or `None` when the
    /// corpus carried no stream block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stability: Option<String>,
    pub chunks_added: usize,
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
    /// Which route the turn took — `routed_intent`, falling back to the
    /// `provenance.intent` display label. Crucial for debugging
    /// routing-layer regressions. See `super::routed_intent`.
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
    /// fact but the model didn't surface it." Scored over the FULL
    /// chunk text since the `snippet` cap was removed, so it is now
    /// what retrieval saw rather than a lower bound on it. Values from
    /// before that change are strictly lower and not comparable.
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
    /// The grounding gate's typed decision for this turn (action id,
    /// retried, violation_prob, refused span members), read back from
    /// the persisted message metadata. `None` when the turn's route
    /// never gated — which is itself a fact about the row, not a
    /// default. Without it a board cannot attribute a zero to the gate
    /// vs retrieval after the fact (ei7-ans, 2026-09-22).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<crate::eval_cmd::gate_meta::GateDecisionEcho>,
}

pub use crate::eval_cmd::retrieved_chunk::RetrievedChunk;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreSnapshot {
    pub matched: Vec<String>,
    pub missing: Vec<String>,
    /// Fact dimension only: expected facts the keyword scorer could not
    /// evaluate at all (every token under 3 alphanumeric chars, e.g.
    /// `"80%"`). Excluded from `ratio`'s denominator — see
    /// [`super::score::FactScore::ratio`]. Always empty for sources.
    ///
    /// `#[serde(default)]` so baselines written before 2026-08-02 still
    /// deserialize; they carry an empty list and their `ratio` reflects
    /// the old `total_expected` denominator, so a pre-2026-08-02
    /// baseline is NOT ratio-comparable with a run after it on any bank
    /// that has unscorable facts (`obsidian`, `sep`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unscorable: Vec<String>,
    /// The bank's declared count, including unscorable entries.
    /// Provenance — not the `ratio` denominator.
    pub total_expected: usize,
    /// `None` when there was nothing scorable. Lets the report
    /// distinguish "passed perfectly" (1.0) from "nothing to measure".
    pub ratio: Option<f32>,
}

impl From<SourceScore> for ScoreSnapshot {
    fn from(s: SourceScore) -> Self {
        let ratio = s.ratio();
        Self {
            matched: s.matched,
            missing: s.missing,
            unscorable: Vec::new(),
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
            unscorable: s.unscorable,
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
    /// Derived view of `results` — layer attribution, per-intent
    /// precision/recall, confusions. Persisted into the baseline JSON
    /// so a later run can diff coverage, not just the correct count.
    ///
    /// `#[serde(default)]` so baselines written before this field
    /// existed still deserialize; they simply carry an empty metrics
    /// block until the next run rewrites them.
    #[serde(default)]
    pub metrics: crate::eval_cmd::routing_metrics::RoutingMetrics,
}

/// Resolve `chunk_id` (format `sec_NNNN`) to the corresponding
/// section text in the article's source markdown, under
/// `<corpora-dir>/sep/articles/<slug>.md`. The corpora dir is
/// `$SOVEREIGN_CORPORA_DIR` when set, else `<sovereign-data-dir>/corpora`
/// (`~/.svrnmesh/corpora` by default).
/// Sections are delimited by `## Section NNN` headings; we extract
/// the body between heading N and heading N+1 (or EOF).
///
/// Returns `None` when the file is missing or the section can't be
/// located. Best-effort — caller falls back gracefully.
fn lookup_section_markdown(article_slug: &str, chunk_id: &str) -> Option<String> {
    let corpora_dir = std::env::var_os("SOVEREIGN_CORPORA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| sovereign_cli_base::dirs::sovereign_root().join("corpora"));
    let path = corpora_dir
        .join("sep")
        .join("articles")
        .join(format!("{article_slug}.md"));
    let body = std::fs::read_to_string(&path).ok()?;
    // chunk_id format `sec_NNNN` → ordinal NNN (strip leading zeros).
    let n: usize = chunk_id.strip_prefix("sec_")?.parse().ok()?;
    let needle = format!("## Section {:03}", n);
    let next = format!("## Section {:03}", n + 1);
    let start = body.find(&needle)?;
    let after_heading = start + needle.len();
    let end = body[after_heading..]
        .find(&next)
        .map(|off| after_heading + off)
        .unwrap_or(body.len());
    Some(body[after_heading..end].trim().to_string())
}

/// Within a multi-paragraph section, return the paragraph that
/// contains `preview` as a substring. Paragraphs are split on blank
/// lines (markdown convention). Falls back to the whole section if
/// no paragraph contains the preview, or the section itself is one
/// paragraph. Truncates to a budget so the judge's snippet window
/// (~500 chars) sees the most relevant part.
fn pick_paragraph(section_text: &str, preview: &str) -> String {
    let preview = preview.trim();
    let paragraphs: Vec<&str> = section_text.split("\n\n").collect();
    let chosen = if !preview.is_empty() {
        paragraphs
            .iter()
            .find(|p| p.contains(preview))
            .copied()
            .unwrap_or(section_text)
    } else {
        section_text
    };
    // Trim to a reasonable single-chunk size (~1500 chars) so it
    // doesn't dominate the prompt budget when judges render
    // snippets at 500 chars truncate.
    let trimmed = chosen.trim();
    if trimmed.len() <= 1500 {
        trimmed.to_string()
    } else {
        // Truncate at char boundary.
        let mut end = 1500;
        while end < trimmed.len() && !trimmed.is_char_boundary(end) {
            end += 1;
        }
        trimmed[..end.min(trimmed.len())].to_string()
    }
}

impl EvalResult {
    /// Used by the embed-failure branch above. Today this just returns
    /// `self`; kept as a hook so a future revision can attach the
    /// error string to a `note` field without changing call sites.
    /// Mark this row as UNMEASURED, with why.
    ///
    /// It used to `eprintln!` and return `self` untouched, so the error left
    /// no trace in the report and the row's `0.0` scores read as a
    /// measurement. Consumers must exclude an errored row rather than score
    /// it (ARCH §18.2, §18.3).
    pub(super) fn with_error(mut self, msg: String) -> Self {
        eprintln!("  [{}] {msg}", self.question_id);
        self.error = Some(msg);
        self
    }
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

/// Ask svrn every question over its turn route (`SubjectDial`) and
/// score the persisted answer + provenance. Sequential — the chat
/// model is a single GPU slot and concurrent turns would just queue.
///
/// `judge` toggles the LLM-as-judge "instructor mode" pass. When on,
/// each question's answer is also scored by a fast-slot judge that
/// asks per-fact whether the concept is conveyed; results land in
/// `synth.judge_fact_score`. The strict keyword scorer always runs
/// regardless. See `score::score_facts_judge`.
///
/// `mode` is the turn mode every question runs under. `Grounded` is the
/// pipeline this harness has always driven; `Naked` is the closed-book arm
/// (`--closed-book`), which bypasses retrieval, the router, the grounding
/// gate, tools and the atlas — see `run_question_synth` for what that means
/// for the row's `retrieved`.
pub async fn run_bank_synth(
    subject: &SubjectDial,
    bank: &EvalBank,
    judge: bool,
    isolate: bool,
    mode: sovereign_contracts::types::TurnMode,
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
    // already failed at `SubjectDial::dial` upstream.

    // Per-corpus isolation: scope retrieval to the bank's target corpus
    // so the run measures THAT corpus's integrity (does it hold +
    // retrieve the facts its queries need?) rather than its performance
    // amid cross-corpus competition. Empty target = can't scope; warn
    // and fall back to unscoped.
    let isolate_corpora: Option<Vec<String>> = if isolate {
        if bank.bank.corpus.is_empty() {
            eprintln!("warn: --isolate set but bank declares no target corpus; running unscoped");
            None
        } else {
            eprintln!(
                "isolation mode — retrieval scoped to corpus `{}`",
                bank.bank.corpus
            );
            Some(vec![bank.bank.corpus.clone()])
        }
    } else {
        None
    };

    let mut results = Vec::with_capacity(bank.questions.len());
    for q in &bank.questions {
        let result = run_question_synth(subject, q, judge, isolate_corpora.as_deref(), mode).await;
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

/// One question, one fresh conversation, one turn under `mode`.
///
/// Under `TurnMode::Naked` the assistant row persists `metadata: None`
/// (`Runtime::handle_message_stream_naked_unleased`), so `retrieved`,
/// `corpora_hit` and every provenance-derived field below are empty BY
/// CONSTRUCTION — that is the closed-book arm's definition, not a failure.
/// The row stays a measurement: `empty_synth_result` is reached only when
/// the dialed turn returns `Err`, and `degraded_router` reads `None` from
/// absent provenance rather than stamping the row unmeasured. A naked answer
/// is scored on its text, which is the whole point of the arm.
async fn run_question_synth(
    subject: &SubjectDial,
    q: &Question,
    judge: bool,
    isolate_corpora: Option<&[String]>,
    mode: sovereign_contracts::types::TurnMode,
) -> EvalResult {
    let t_wall = Instant::now();

    // 1. Ask svrn, as the desktop chat surface does. Per-corpus isolation
    //    seals the conversation to the bank's corpus before the turn; svrn
    //    refuses a corpus it does not have, so a sealing it cannot apply is
    //    this question's error, never an unscoped measurement. Failures
    //    become an empty-row result so one model-side error doesn't void
    //    the rest of the bank.
    //
    // 3. The metadata block comes back with the turn, from the message svrn
    //    persisted. This is where `retrieved_chunks` and `provenance` live;
    //    without them we can't score sources.
    let seal = isolate_corpora.and_then(|c| c.first()).map(String::as_str);
    let (raw, metadata, stream_wall_ms) = match subject.ask(seal, &q.question, mode, None).await {
        Ok(turn) => {
            let wall = t_wall.elapsed().as_millis() as u64;
            (turn.text, turn.metadata, wall)
        }
        Err(e) => {
            return empty_synth_result(q, format!("turn: {e}"), 0);
        }
    };

    // 4. Split reasoning vs answer the same way the desktop client does.
    let (reasoning_blocks, visible) = split_reasoning(&raw);
    let reasoning_chars: usize = reasoning_blocks.iter().map(|b| b.chars().count()).sum();

    // 5. Pull provenance signals out of the metadata. Anything missing
    //    becomes None / empty rather than aborting — a model that
    //    answered without retrieval is a valid (if pessimistic)
    //    measurement.
    let prov = metadata.as_ref().and_then(|m| m.get("provenance"));
    // Was the host ROUTING when it answered this? See `degraded_router` — the
    // answer decides whether the scores below are a measurement or the shape
    // of one.
    let degraded = degraded_router(prov);
    let total_latency_ms = prov
        .and_then(|p| p.get("total_latency_ms"))
        .and_then(|v| v.as_u64());
    // WHICH ROUTE, not which display label — see `routed_intent` for why the
    // two are different questions and why the old label is still the
    // fallback.
    let intent = super::routed_intent::snapshot_intent(&metadata);
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

    // Move 4 — canonical-entity boosts echoed back from
    // runtime metadata. One row per primary / alternative slot.
    let meta_atlas_hits: Vec<MetaAtlasHitEcho> = metadata
        .as_ref()
        .and_then(|m| m.get("meta_atlas_hits"))
        .and_then(|v| serde_json::from_value::<Vec<MetaAtlasHitEcho>>(v.clone()).ok())
        .unwrap_or_default();

    // The atlas walk's evidence path, off the same persisted metadata block
    // (written at `runtime/streaming.rs`, beside `meta_atlas_hits`).
    let atlas_walk = atlas_walk_from_metadata(metadata.as_ref(), &q.id);

    // The gate's typed decision, off the same block (written beside
    // `provenance` in knowledge_query.rs / streaming.rs).
    let gate = crate::eval_cmd::gate_meta::gate_decision_from_metadata(metadata.as_ref(), &q.id);

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
            url: c.get("url").and_then(|v| v.as_str()).map(str::to_string),
            score: 0.0,
            snippet: c
                .get("snippet")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            source: c.get("source").and_then(|v| v.as_str()).map(str::to_string),
            in_prompt: c.get("in_prompt").and_then(|v| v.as_bool()),
            prompt_text: c
                .get("prompt_text")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        })
        .collect();

    // 8. Score: facts → answer text, sources → retrieved-chunk titles,
    //    plus the snippet-fact diagnostic.
    //
    // For attribution_mode ∈ {user, assistant}, the snippet-fact
    // diagnostic filters opposite-attribution turn blocks out of
    // each snippet before joining. The synth answer text itself is
    // NOT filterable here — the LLM saw the unfiltered chunks at
    // generation time. Closing that gap requires runtime-side
    // attribution-aware retrieval; tracked as a follow-up.
    let attribution_mode = attribution::AttributionMode::from_str(&q.attribution_mode);
    let snippet_haystack = if attribution_mode == attribution::AttributionMode::Both {
        snippets.join("\n")
    } else {
        snippets
            .iter()
            .map(|s| attribution::filter_chunk_content(s, attribution_mode))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let fact_score: ScoreSnapshot = score_facts_in_text(&q.expected_facts, &visible).into();
    let chunks_fact_score: ScoreSnapshot =
        score_facts_in_text(&q.expected_facts, &snippet_haystack).into();
    let source_score: ScoreSnapshot = score_sources_titles(&q.expected_sources, &titles).into();

    // 8b. Instructor-mode pass — LLM-as-judge concept-conveyed score.
    //     Strict keyword score above is preserved unchanged; this
    //     adds a parallel column in the report. Skipped under
    //     `--no-judge`. The judge call also returns a per-fact
    //     evidence trail (quote or "(absent)") for auditability.
    // `degraded.is_none()` is not an optimisation. The judge is one more model
    // call against the same host that just failed to build a single classifier,
    // and a judgement produced there is no more a measurement than the answer it
    // would be judging. The row is excluded below either way.
    let (judge_fact_score, judge_evidence): (Option<ScoreSnapshot>, _) =
        if judge && degraded.is_none() {
            let (score, details) = crate::eval_cmd::score::score_facts_judge(
                &q.expected_facts,
                &visible,
                subject.inference.as_ref(),
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
        gate,
    };

    let row = EvalResult {
        error: None,
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
        loose_source_score: None,
        loose_source_evidence: Vec::new(),
        essay_readiness: None,
        atlas_navigation: Vec::new(),
        meta_atlas_hits,
        atlas_walk,
    };

    // The scores above are real arithmetic over a real answer — and on a
    // degraded host they are arithmetic over an answer the router never routed.
    // `with_error` is the ONE way this shape says "not a measurement", and
    // `drop_unmeasured` is already the ONE consumer that honours it.
    match degraded {
        Some(why) => row.with_error(why),
        None => row,
    }
}

/// The router's own account of whether it was ROUTING, read off the turn's
/// provenance. `Some(why)` means it was not, and the row is not a measurement.
///
/// THE FAILING INPUT IS PRODUCTION, not a hypothetical. On 2026-08-26 a dead
/// embed slot left `build_llm_router` returning `None` for all four
/// classifiers; atlas grounding went from 1082 loads to zero and turns KEPT
/// ANSWERING — worse, not louder. This harness scored those answers against a
/// baseline and reported SEP overview title-coverage 1.00 -> 0.83 as a code
/// regression. It cost most of a session to attribute, and the lesson is that
/// REPRODUCIBLE IS NOT ATTRIBUTABLE (note `f4972e1b`).
///
/// It returns an error STRING rather than its own verdict on purpose.
/// `EvalResult` already has exactly one way to say "this row is not a
/// measurement", and `bench_cmd::all::classify_retrieval` already excludes on
/// it via `drop_unmeasured`. A second exclusion rule would be a second decider
/// for one question (ARCH §10.6) — and the one that exists was earned by the
/// same class of defect (note `933dccee`).
///
/// `None` covers two turns and treats them alike, correctly: one that reports
/// no router at all (an old message, or a path that never routed) and one that
/// routed with at least one classifier live. Neither is degraded, and absent
/// is deliberately not the same value as all-four-false (`RouterStamp`).
fn degraded_router(prov: Option<&serde_json::Value>) -> Option<String> {
    let stamp: sovereign_contracts::types::RouterStamp = prov
        .and_then(|p| p.get("router"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())?;
    stamp.routed_by_none().then(|| {
        "router: no classifier was live — the host was degraded when this turn was \
         routed, so its answer is not a measurement (see `RouterStamp`)"
            .to_string()
    })
}

/// A synth row for a question the run could NOT measure — the turn errored.
///
/// The scores below are zero because there is nothing to score, NOT because
/// the answer was empty. `with_error` is what says so; without it a daemon
/// returning `503 host busy` is indistinguishable in the report from a model
/// that answered with nothing (ARCH §18.3).
fn empty_synth_result(q: &Question, err: String, stream_wall_ms: u64) -> EvalResult {
    let row = EvalResult {
        error: None,
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
            gate: None,
        }),
        loose_source_score: None,
        loose_source_evidence: Vec::new(),
        essay_readiness: None,
        atlas_navigation: Vec::new(),
        meta_atlas_hits: Vec::new(),
        atlas_walk: None,
    };
    row.with_error(err)
}

// The degraded-router tests live in a sibling file: keeping them inline put
// this file past its arch-gate slack (ARCH §3.1) when the grounding gate's
// echo field landed (a313c9c18). `#[path]`, so the names are unchanged — the
// module stays a child of `runner` and reads private items through `super::*`.
#[cfg(test)]
#[path = "runner/degraded_router_tests.rs"]
mod degraded_router_tests;
