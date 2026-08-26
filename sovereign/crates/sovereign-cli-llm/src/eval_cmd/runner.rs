// SPDX-License-Identifier: AGPL-3.0-or-later
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

use corpus_engine::enrichment::atlas::{
    read_atlas_atoms, read_atlas_edges, AtomEnvelope, EdgeType, ATLAS_DIRNAME,
};
use corpus_engine::ScoredChunk;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sovereign_core::atlas_context::{atlas_top_k_across, cosine, AtlasContext, AtlasEntry};
use std::collections::{HashMap, HashSet};

use crate::chat_cmd::bootstrap::ChatSession;
use crate::chat_cmd::render::split_reasoning;
use crate::enrich_cmd::paths;
use crate::eval_cmd::attribution;
use crate::eval_cmd::bank::{EvalBank, Question};
use crate::eval_cmd::score::{
    score_essay_readiness, score_facts, score_facts_in_text, score_sources, score_sources_loose,
    score_sources_titles, EssayReadinessScore, FactScore, JudgeSourceDetail, SourceScore,
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
    /// Provenance tag from the chunk's `metadata.source` — "raptor",
    /// "atlas", "atom-enum", or absent for organically-retrieved
    /// chunks. Makes structural-layer injection visible in the run file
    /// so a bench can confirm (not infer) which layer surfaced a hit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

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

    let metrics = crate::eval_cmd::routing_metrics::RoutingMetrics::from_results(&results);
    Ok(RoutingRun {
        bank_name: bank.bank.name.clone(),
        started_at_unix,
        results,
        metrics,
    })
}

async fn run_question_routing(session: &ChatSession, q: &Question) -> RoutingResult {
    use sovereign_core::types::{
        ConversationContext, Effect, Idempotency, Latency, Scope, ToolDescriptor,
    };

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
            enabled_corpora: None,
            searched_sources: None,
        },
        memories: vec![],
        working_memory: None,
        installed_corpora: installed,
        corpus_ceiling: None,
        document_session: None,
        topic_context: None,
        knowledge_view_digests: None,
        temporal_tensions: Vec::new(),
        compacted_history: None,
        history_retrieval_hits: None,
        tool_dossier: None,
        intent_policy: None,
    };

    // Expose a `web_search` tool descriptor so the router's
    // `force_action` gate (which checks `has_search` against
    // available_tools) fires under the same conditions as the
    // production desktop, where SearchTool is always registered.
    // Without this, routing-only eval underrepresents the daemon's
    // real behaviour — temporal/future questions fall through to
    // the LLM Pass 1 instead of taking the heuristic ACTION path.
    let eval_tools = vec![ToolDescriptor {
        id: "web_search".to_string(),
        name: "web_search".to_string(),
        description: "Search the web for current information".to_string(),
        parameters: serde_json::json!({}),
        examples: vec![],
        effect: Effect::Read,
        idempotency: Idempotency::Idempotent,
        latency: Latency::Slow,
        scope: Scope::External,
        output_schema: None,
    }];

    let t = Instant::now();
    let classification = match session
        .runtime
        .router
        .classify(&q.question, &context, &eval_tools)
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

/// Lowercase wire form of an Intent — matches the strings used in the bank's
/// `expected_intent` field and the category-default map.
///
/// The `slug` column of the intent table. It was a thirteen-arm `match` here
/// until 2026-08-20, one of THREE independent implementations of this one wire
/// key (the others in `sovereign_core::router_embed::intent_label` and
/// `runtime::intent_helpers::intent_hint`). One key, one decider.
fn intent_wire_label(intent: &sovereign_core::types::Intent) -> String {
    intent.row().slug.to_string()
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
        .unwrap_or_else(|| sovereign_cli_shared::dirs::sovereign_root().join("corpora"));
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

/// Truncate atlas-entity text for embedding. Embed models cap context
/// somewhere around 8K tokens; entities with augmented descriptions
/// (questions + anchors aggregated across many sections) routinely run
/// 18KB chars. 3000 chars (~750 tokens) keeps headroom while still
/// covering the description and the strongest section signals.
const ATLAS_ENTRY_CHAR_LIMIT: usize = 3000;

/// Render a tension-edge endpoint as a single line for the virtual
/// chunk's embed text. Endpoint atoms are commonly Entities or
/// Claims, but the spec permits any atom type, so we cover the
/// natural-language fields each variant carries. Returns an
/// "<id> (missing)" placeholder when the edge points at an id that
/// doesn't resolve — better to keep the tension visible with a
/// half-known endpoint than to drop it silently.
pub(crate) fn endpoint_text(atom: Option<&AtomEnvelope>, atom_id: &str) -> String {
    use AtomEnvelope::*;
    match atom {
        Some(Entity(e)) => format!("{}: {}", e.canonical_name, e.description),
        Some(Claim(c)) => {
            let act = serde_json::to_string(&c.discourse_act)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            let status = serde_json::to_string(&c.epistemic_status)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            format!("[Claim: {act}, {status}] {}", c.content)
        }
        Some(Question(q)) => format!("Question: {}", q.content),
        Some(State(s)) => format!("State: {}", s.label),
        Some(Relation(r)) => format!("Relation: {}", r.label),
        Some(Event(ev)) => format!("Event: {}", ev.description),
        Some(Configuration(cfg)) => format!("{}: {}", cfg.label, cfg.description),
        Some(ArgumentReconstruction(a)) => format!("Argument: {}", a.name),
        Some(Position(p)) => format!("Position ({}): {}", p.stance, p.canonical_name),
        Some(Opposition(o)) => format!("Opposition: {}", o.canonical_label),
        Some(Asset(a)) => {
            let name = if a.original_filename.is_empty() {
                format!("asset:{}", &a.sha256[..12.min(a.sha256.len())])
            } else {
                a.original_filename.clone()
            };
            format!("Asset ({}): {}", a.asset_kind, name)
        }
        None => format!("{atom_id} (missing)"),
    }
}

// AtlasGraph + ChunkRequest + atlas_navigate_ann + edge_weight live in
// `sovereign_core::atlas_context` — single canonical implementation
// shared by the eval CLI and the production daemon
// (`AtlasContextManager`).
// ATLAS_STORAGE_V2 Phase B — the sync `atlas_navigate` was deleted; the
// production ANN-seeding navigate is now the only path. Both `--atlas-seed`
// modes drive this exact daemon code.
use super::atlas_ann::SeedMode;
pub use sovereign_core::atlas_context::atlas_navigate_ann;
pub use sovereign_core::atlas_context::AtlasGraph;

/// The filter applied during atlas-context loading — the SAME type the
/// production grounding path uses, deliberately.
///
/// This was a private `AtlasLoadFilter` here: a renamed copy of
/// `AtlasContextFilter` carrying six of its seven fields, in a crate that
/// already depends on `sovereign-tools` and already imported the owner one
/// module over (`atlas_cmd::migrate_all`). Two call sites hand-copied
/// `AtlasContextFilter::default()` field-by-field into the copy to keep them
/// aligned, with a comment saying why — a re-derivation nothing enforced.
///
/// It drifted, exactly where that matters most. The owner's
/// `min_description_chars` floor moved 200 → 10 (and became env-aware) after
/// 200 was found to drop ~85% of SEP atoms; the copy still documented "200",
/// and `#[derive(Default)]` gave it 0 rather than either. The eval harness
/// was the one caller that did not hand-copy, so it filtered the atom
/// universe by a rule production had already abandoned.
///
/// Importing the owner is what stops that recurring (`ARCH_PRINCIPLES`
/// §10.6, one decider one name; §10, structural not remembered). Surfaced by
/// nc-22c shape matching — a name-keyed census cannot see a fork that was
/// renamed on copy.
pub use sovereign_tools::atlas_context_manager::AtlasContextFilter;

/// Read `atoms.json` for the named atlas corpus and embed each Entity's
/// `name + aliases + description` once per call. ATLAS_STORAGE_V2 Phase B
/// removed the `atoms.embeddings.bin` cache, so every call re-embeds from
/// `atoms.json` (multi-minute cold load for wiki-scale atlases); the
/// persistent `atoms_ann.lance` seed table is now the durable cross-run
/// artifact.
pub async fn load_atlas_context(
    session: &ChatSession,
    atlas_corpus_id: &str,
    top_k: usize,
    filter: &AtlasContextFilter,
) -> Result<AtlasContext, String> {
    let atlas_dir = paths::index_root(atlas_corpus_id).join(ATLAS_DIRNAME);
    if !atlas_dir.exists() {
        return Err(format!(
            "no atlas at {} — `svrn enrich ingest {atlas_corpus_id} \
             --strategy structure_first --source-corpus <id>` first",
            atlas_dir.display()
        ));
    }

    let atoms = read_atlas_atoms(&atlas_dir).map_err(|e| format!("read atlas atoms.json: {e}"))?;

    // Build embed-text per Entity, applying filters. Counters track
    // why each entity was kept or dropped so the pre-embed log is
    // diagnostic — operators tuning a Tier-2 atlas need to see "we
    // dropped 51000 structural one-liners and kept the 52 extracted
    // entries" rather than just a final total.
    // Path 2 Phase A — Claim atoms ride alongside Entities in the
    // virtual-chunk pool when `--atlas-include claim` is set. They
    // surface with `canonical_name = article_slug` so rigid-source
    // matching credits the article. For per-article SEP atlases the
    // corpus_id is `sep-<slug>`; strip it. Other atlases pass
    // through unchanged.
    let article_slug: String = atlas_corpus_id
        .strip_prefix("sep-")
        .unwrap_or(atlas_corpus_id)
        .to_string();

    // (atom_id, canonical_name, embed_text) per virtual chunk. atom_id is the
    // backing atom's id; it seeds the v2 persistent ANN table. Empty only for
    // edge-derived Tension chunks, which have no single backing atom.
    let mut payloads: Vec<(String, String, String)> = Vec::new();
    let mut total_entities = 0usize;
    let mut total_claims = 0usize;
    let mut kept_claims = 0usize;
    let mut total_configurations = 0usize;
    let mut kept_configurations = 0usize;
    let mut drop_placeholder = 0usize;
    let mut drop_short_desc = 0usize;
    let mut drop_depth = 0usize;
    let mut drop_cap = 0usize;
    for atom in &atoms.atoms {
        match atom {
            AtomEnvelope::Entity(e) => {
                total_entities += 1;
                // A NAMED atom is never a placeholder — names are first-class
                // grounding signal. Drop only atoms with no name AND no
                // description (truly empty); the signal_len floor below governs
                // the rest. (Was `description.is_empty() && salience == 0.0`,
                // which discarded named-but-unscored entities — exactly the
                // baked-in signal the v2 migration must not lose.)
                let is_placeholder = e.canonical_name.trim().is_empty() && e.description.is_empty();
                if is_placeholder {
                    drop_placeholder += 1;
                    continue;
                }
                // Measure the atom's FULL embed signal — name + aliases +
                // description — not the description alone. The embed text
                // (render_atom_entry) is name+aliases+description, so a
                // richly-named entity with a terse description ("Pierre
                // Abelard", "abductive reasoning") is strong grounding signal
                // and must NOT be dropped. Names are first-class.
                let signal_len = e.canonical_name.len()
                    + e.aliases.iter().map(|a| a.len()).sum::<usize>()
                    + e.description.len();
                if signal_len < filter.min_description_chars {
                    drop_short_desc += 1;
                    continue;
                }
                if !filter.depth_allowlist.is_empty() {
                    // Match against the serialised form of EnrichmentDepth.
                    // `serde_json` keeps it lowercase (snake_case) — same form
                    // operators see in atoms.json.
                    let depth_label = serde_json::to_string(&e.enrichment_depth)
                        .unwrap_or_default()
                        .trim_matches('"')
                        .to_string();
                    if !filter
                        .depth_allowlist
                        .iter()
                        .any(|d| d.eq_ignore_ascii_case(&depth_label))
                    {
                        drop_depth += 1;
                        continue;
                    }
                }
                if let Some(cap) = filter.max_entries {
                    if payloads.len() >= cap {
                        drop_cap += 1;
                        continue;
                    }
                }
                let mut text = String::new();
                text.push_str(&e.canonical_name);
                text.push('\n');
                if !e.aliases.is_empty() {
                    text.push_str(&e.aliases.join(", "));
                    text.push('\n');
                }
                text.push_str(&e.description);
                if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                    text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
                }
                payloads.push((e.id.as_str().to_string(), e.canonical_name.clone(), text));
            }
            AtomEnvelope::Claim(c) if filter.include_claims => {
                total_claims += 1;
                if !filter.depth_allowlist.is_empty() {
                    let depth_label = serde_json::to_string(&c.enrichment_depth)
                        .unwrap_or_default()
                        .trim_matches('"')
                        .to_string();
                    if !filter
                        .depth_allowlist
                        .iter()
                        .any(|d| d.eq_ignore_ascii_case(&depth_label))
                    {
                        drop_depth += 1;
                        continue;
                    }
                }
                if let Some(cap) = filter.max_entries {
                    if payloads.len() >= cap {
                        drop_cap += 1;
                        continue;
                    }
                }
                let act = serde_json::to_string(&c.discourse_act)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_string();
                let status = serde_json::to_string(&c.epistemic_status)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_string();
                let mut text = format!("[Claim: {act}, {status}] {content}", content = c.content);
                if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                    text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
                }
                payloads.push((c.id.as_str().to_string(), article_slug.clone(), text));
                kept_claims += 1;
            }
            AtomEnvelope::Configuration(cfg) if filter.include_configurations => {
                total_configurations += 1;
                if !filter.depth_allowlist.is_empty() {
                    let depth_label = serde_json::to_string(&cfg.enrichment_depth)
                        .unwrap_or_default()
                        .trim_matches('"')
                        .to_string();
                    if !filter
                        .depth_allowlist
                        .iter()
                        .any(|d| d.eq_ignore_ascii_case(&depth_label))
                    {
                        drop_depth += 1;
                        continue;
                    }
                }
                if let Some(cap) = filter.max_entries {
                    if payloads.len() >= cap {
                        drop_cap += 1;
                        continue;
                    }
                }
                let mut text = format!("[Configuration: {}] {}", cfg.label, cfg.description);
                if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                    text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
                }
                payloads.push((cfg.id.as_str().to_string(), article_slug.clone(), text));
                kept_configurations += 1;
            }
            AtomEnvelope::ArgumentReconstruction(a) => {
                // Always include — these are the named-argument
                // reconstructions Phase 1 extracted. Embed text is
                // name + premises + conclusion so a question
                // mentioning the argument name OR matching its
                // content can seed navigation onto this atom. The
                // article-slug `canonical_name` lets `score_sources`
                // credit the article when the atom is in top-K.
                if !filter.depth_allowlist.is_empty() {
                    let depth_label = serde_json::to_string(&a.enrichment_depth)
                        .unwrap_or_default()
                        .trim_matches('"')
                        .to_string();
                    if !filter
                        .depth_allowlist
                        .iter()
                        .any(|d| d.eq_ignore_ascii_case(&depth_label))
                    {
                        drop_depth += 1;
                        continue;
                    }
                }
                if let Some(cap) = filter.max_entries {
                    if payloads.len() >= cap {
                        drop_cap += 1;
                        continue;
                    }
                }
                let mut text = String::with_capacity(256);
                text.push_str("[Argument: ");
                text.push_str(&a.name);
                text.push_str("] ");
                for p in &a.premises {
                    text.push_str(p);
                    text.push(' ');
                }
                text.push_str(&a.conclusion);
                // Append objection content so cosine seeding picks
                // this argument when the question vocabulary
                // overlaps with an objection (e.g. "Frankfurt"
                // mentioned ⇒ Consequence Argument seeds).
                for o in &a.objections {
                    if !o.content.trim().is_empty() {
                        text.push(' ');
                        text.push_str(o.content.trim());
                    } else if !o.name.trim().is_empty() {
                        text.push(' ');
                        text.push_str(o.name.trim());
                    }
                }
                if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                    text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
                }
                payloads.push((a.id.as_str().to_string(), article_slug.clone(), text));
            }
            _ => continue,
        }
    }

    // Path 2 Phase B — fold Tension edges into the virtual-chunk pool.
    // Tensions live in `edges.json`, not `atoms.json`. Each edge points
    // at two endpoint atoms (commonly Entities or Claims) and carries
    // a `sub_question` summarising the dialectical question the pair
    // turns on. Surfacing all three pieces in one embed text gives the
    // retriever a hit for questions phrased around that very tension.
    let mut kept_tensions = 0usize;
    let mut total_tensions = 0usize;
    if filter.include_tensions {
        // Build a lookup over atoms keyed by id once, since each edge
        // resolves two endpoints. Cheap — atlases are at most a few
        // thousand atoms.
        use std::collections::HashMap;
        let atoms_by_id: HashMap<&str, &AtomEnvelope> =
            atoms.atoms.iter().map(|a| (a.id().as_str(), a)).collect();
        match read_atlas_edges(&atlas_dir) {
            Ok(edges_file) => {
                for edge in &edges_file.edges {
                    if edge.edge_type != EdgeType::Tension {
                        continue;
                    }
                    total_tensions += 1;
                    if let Some(cap) = filter.max_entries {
                        if payloads.len() >= cap {
                            drop_cap += 1;
                            continue;
                        }
                    }
                    let src = atoms_by_id.get(edge.source.as_str()).copied();
                    let tgt = atoms_by_id.get(edge.target.as_str()).copied();
                    let sub = edge
                        .sub_question
                        .as_deref()
                        .unwrap_or("(no sub_question recorded)");
                    let mut text = format!("[Tension] {sub}");
                    text.push('\n');
                    text.push_str(&endpoint_text(src, edge.source.as_str()));
                    text.push_str("\n↔\n");
                    text.push_str(&endpoint_text(tgt, edge.target.as_str()));
                    if text.len() > ATLAS_ENTRY_CHAR_LIMIT {
                        text.truncate(ATLAS_ENTRY_CHAR_LIMIT);
                    }
                    payloads.push((String::new(), article_slug.clone(), text));
                    kept_tensions += 1;
                }
            }
            Err(e) => {
                // Missing edges.json is fine — older atlases may not
                // have run Phase 6. Log and continue with whatever we
                // already collected.
                eprintln!(
                    "atlas-context: --atlas-include tension requested but \
                     edges.json unreadable for `{atlas_corpus_id}` ({e}); \
                     skipping Tension surface."
                );
            }
        }
    }

    eprintln!(
        "atlas-context: cache MISS — kept {} of {} entities \
         (+ {kept_claims} of {total_claims} claims, + {kept_tensions} of {total_tensions} tensions, \
         + {kept_configurations} of {total_configurations} configurations) \
         from `{atlas_corpus_id}` \
         (placeholder: {}, short_desc<{}: {}, depth: {}, over_cap: {}); top-K per question = {top_k}",
        payloads.len() - kept_claims - kept_tensions - kept_configurations,
        total_entities,
        drop_placeholder,
        filter.min_description_chars,
        drop_short_desc,
        drop_depth,
        drop_cap,
    );
    if payloads.is_empty() {
        return Err(format!(
            "atlas-context: filter excluded every atom in `{atlas_corpus_id}`. \
             Lower --atlas-min-description-chars (currently {}) or check --atlas-depth, \
             or pass --atlas-include claim,tension if the atlas only carries non-Entity surfaces.",
            filter.min_description_chars
        ));
    }

    let mut entries: Vec<AtlasEntry> = Vec::with_capacity(payloads.len());
    let t0 = Instant::now();
    for (atom_id, name, text) in payloads {
        match session.inference.embed_query(&text).await {
            Ok(v) => entries.push(AtlasEntry {
                atom_id,
                canonical_name: name,
                embed_text: text,
                embedding: v,
            }),
            Err(e) => {
                eprintln!("  embed atlas entity `{name}` failed: {e} (skipped)");
            }
        }
    }
    eprintln!(
        "atlas-context: embedded {} entries in {:.1}s",
        entries.len(),
        t0.elapsed().as_secs_f32()
    );

    Ok(AtlasContext {
        atlas_corpus_id: atlas_corpus_id.to_string(),
        entries,
        top_k,
    })
}

/// Run an entire bank, sequentially. Sequential is fine — the daemon's
/// embed slot serialises anyway, and concurrent searches against the
/// same Lance table contend on the same index pages.
pub async fn run_bank(
    session: &ChatSession,
    bank: &EvalBank,
    limit: usize,
    atlases: &[AtlasContext],
    graphs: &[AtlasGraph],
    loose_source_judge: bool,
    essay_judge: bool,
    seed_mode: SeedMode,
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
            "no corpora installed — `svrn corpus install {}` before running this bank",
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

    // ATLAS_STORAGE_V2 3b: when `--atlas-seed ann`, attach each corpus's
    // PERSISTENT ANN seed table (built by `svrn atlas backfill-ann`) to its
    // graph, then drive the PRODUCTION `atlas_navigate_ann` over the
    // ann-attached graphs — the daemon's exact runtime shape, not a fork. The
    // owned graphs must outlive the per-question loop (each holds a live
    // `lancedb::Table` opened on THIS runtime; see
    // `open_and_attach_ann_seed_table`).
    let ann_graphs: Vec<AtlasGraph> = if seed_mode == SeedMode::Ann {
        let mut out = Vec::with_capacity(graphs.len());
        let mut attached = 0usize;
        for g in graphs {
            let atlas_dir = paths::index_root(&g.atlas_corpus_id).join(ATLAS_DIRNAME);
            let g = sovereign_core::atlas_context::open_and_attach_ann_seed_table(
                &g.atlas_corpus_id,
                &atlas_dir,
                g.clone(),
            )
            .await;
            if g.has_ann_seed_table() {
                attached += 1;
            }
            out.push(g);
        }
        eprintln!(
            "atlas-seed=ann: attached persistent ANN seed tables to {attached}/{} graphs \
             (run `svrn atlas backfill-ann <corpus>` for any missing)",
            out.len()
        );
        out
    } else {
        Vec::new()
    };
    let graphs: &[AtlasGraph] = if seed_mode == SeedMode::Ann {
        &ann_graphs
    } else {
        graphs
    };

    let mut results = Vec::with_capacity(bank.questions.len());
    for q in &bank.questions {
        let result = run_question(
            session,
            &target_indexes,
            q,
            limit,
            atlases,
            graphs,
            loose_source_judge,
            essay_judge,
            seed_mode,
        )
        .await;
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

/// Bench-prod parity mode (`--prod-pipeline`): every question drives the
/// PRODUCTION KnowledgeQuery retrieval pipeline in-process
/// (`Runtime::retrieve_evidence` — context build → the 19-step
/// `kq_pipeline()` → merge → truncate) and the returned evidence pool is
/// scored with the same rigid source/fact scorers as the raw-index mode.
/// No synthesis pass, so the lane stays deterministic and cheap while
/// measuring the pipeline chat surfaces actually run
/// (RETRIEVAL_REDESIGN.md §7.1). Note the pool size is the pipeline's own
/// (KQ_MERGED_LIMIT + grounding injections), not the raw lane's `--limit`
/// — scores are baseline-comparable only within this mode.
pub async fn run_bank_prod(
    session: &ChatSession,
    bank: &EvalBank,
    limit: usize,
    isolate: bool,
    loose_source_judge: bool,
) -> Result<EvalRun, String> {
    let started_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let isolate_corpora: Option<Vec<String>> = if isolate {
        Some(vec![bank.bank.corpus.clone()])
    } else {
        None
    };
    let mut results = Vec::with_capacity(bank.questions.len());
    for q in &bank.questions {
        results.push(
            run_question_prod(
                session,
                q,
                limit,
                isolate_corpora.as_deref(),
                loose_source_judge,
            )
            .await,
        );
    }
    Ok(EvalRun {
        bank_name: bank.bank.name.clone(),
        corpus: bank.bank.corpus.clone(),
        limit,
        started_at_unix,
        results,
    })
}

async fn run_question_prod(
    session: &ChatSession,
    q: &Question,
    limit: usize,
    isolate_corpora: Option<&[String]>,
    loose_source_judge: bool,
) -> EvalResult {
    // Fresh conversation per question — same seeding pattern as the synth
    // path so `build_context` + the personal-scope filter see a real row,
    // and isolation scopes retrieval via `enabled_corpora`.
    let conversation_id = uuid::Uuid::new_v4().to_string();
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if let Err(e) = session
        .store
        .insert_empty_conversation(&conversation_id, created_at, None)
        .await
    {
        eprintln!(
            "  warn: prod-pipeline seed (insert) failed for {}: {e}",
            q.id
        );
    } else if let Some(corpora) = isolate_corpora {
        if let Err(e) = session
            .store
            .set_conversation_enabled_corpora(&conversation_id, Some(corpora.to_vec()))
            .await
        {
            eprintln!(
                "  warn: prod-pipeline seed (scope) failed for {}: {e}",
                q.id
            );
        }
    }

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
    };

    let ev = match session
        .runtime
        .retrieve_evidence(&q.question, &conversation_id)
        .await
    {
        Ok(ev) => ev,
        Err(e) => {
            return empty_result(q).with_error(format!("retrieve_evidence: {e}"));
        }
    };

    let mut all_hits = ev.chunks;
    if all_hits.len() > limit {
        all_hits.truncate(limit);
    }
    // Same attribution projection as the raw-index scorer (see
    // run_question step 3): conversation-history banks must not credit a
    // restatement as evidence.
    let attribution_mode = attribution::AttributionMode::from_str(&q.attribution_mode);
    let hits_for_scoring: Vec<ScoredChunk> = if attribution_mode
        == attribution::AttributionMode::Both
    {
        all_hits.clone()
    } else {
        all_hits
            .iter()
            .map(|h| {
                let mut filtered = h.clone();
                filtered.content = attribution::filter_chunk_content(&h.content, attribution_mode);
                filtered
            })
            .collect()
    };
    let rigid_source = score_sources(&q.expected_sources, &hits_for_scoring);
    let source_score: ScoreSnapshot = rigid_source.clone().into();
    let fact_score: ScoreSnapshot = score_facts(&q.expected_facts, &hits_for_scoring).into();

    // Loose-judge source scoring, same contract as the non-prod path in
    // `run_question`: a strict superset of the rigid score that credits a
    // missing expected_source when the retrieved chunks materially cover it.
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
    ) = if loose_source_judge && !q.expected_sources.is_empty() {
        let (loose, details) = score_sources_loose(
            &q.question,
            &rigid_source,
            &hits_for_scoring,
            session.inference.as_ref(),
        )
        .await;
        (Some(loose.into()), details)
    } else {
        (None, Vec::new())
    };

    let corpora_hit: Vec<String> = {
        let mut s: Vec<String> = all_hits.iter().map(|c| c.corpus_id.clone()).collect();
        s.sort();
        s.dedup();
        s
    };
    let retrieved = all_hits
        .iter()
        .map(|c| RetrievedChunk {
            corpus_id: c.corpus_id.clone(),
            title: c.title.clone(),
            url: c.url.clone(),
            score: c.score,
            snippet: truncate(&c.content.replace('\n', " "), 600),
            source: None,
        })
        .collect();

    EvalResult {
        error: None,
        question_id: q.id.clone(),
        category: q.category.clone(),
        question: q.question.clone(),
        retrieved,
        source_score,
        fact_score,
        embed_ms: 0,
        search_ms: ev.search_ms,
        corpora_hit,
        vector_eligible: true,
        synth: None,
        loose_source_score,
        loose_source_evidence,
        essay_readiness: None,
        atlas_navigation: Vec::new(),
        meta_atlas_hits: Vec::new(),
    }
}

async fn run_question(
    session: &ChatSession,
    target_indexes: &[&corpus_engine::IndexInfo],
    q: &Question,
    limit: usize,
    atlases: &[AtlasContext],
    graphs: &[AtlasGraph],
    loose_source_judge: bool,
    essay_judge: bool,
    seed_mode: SeedMode,
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
                loose_source_score: None,
                loose_source_evidence: Vec::new(),
                essay_readiness: None,
                atlas_navigation: Vec::new(),
                meta_atlas_hits: Vec::new(),
                // `with_error` below sets `error`, which is what keeps this
                // row out of the baseline comparison instead of scoring it 0.
                error: None,
            }
            .with_error(format!("embed: {e}"));
        }
    };
    let embed_ms = t_embed.elapsed().as_millis() as u64;

    // 2. Compute per-article atlas relevance scores once, before the
    // search loop. These flow into `search_with_rerank` as a third
    // signal alongside the cross-encoder logit and the hybrid fusion
    // score. The map is `article_slug → max_cosine` across every atlas
    // atom (atoms come from many atom types, all carry an embedding).
    // Cheap: ~few thousand cosines per question for the SEP 57-atlas
    // workload. Skipped entirely when the runtime config opts atlas
    // weight to zero (the default) or when no atlases are loaded —
    // baseline-A/B remains byte-equivalent to prior runs.
    let atlas_article_scores: HashMap<String, f32> = if session
        .runtime
        .lane_sources
        .rerank
        .config
        .atlas_weight
        .abs()
        > f32::EPSILON
        && !atlases.is_empty()
        && !embedding.is_empty()
    {
        let mut by_slug: HashMap<String, f32> = HashMap::new();
        for ctx in atlases {
            let slug = ctx
                .atlas_corpus_id
                .strip_prefix("sep-")
                .unwrap_or(&ctx.atlas_corpus_id)
                .to_string();
            let mut best: f32 = 0.0;
            for entry in &ctx.entries {
                let s = cosine(&embedding, &entry.embedding);
                if s > best {
                    best = s;
                }
            }
            if best > 0.0 {
                by_slug
                    .entry(slug)
                    .and_modify(|cur| {
                        if best > *cur {
                            *cur = best;
                        }
                    })
                    .or_insert(best);
            }
        }
        by_slug
    } else {
        HashMap::new()
    };

    // 3. Search every matching corpus index.
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
        // When atlas grounding is active, pull a wider candidate set
        // from lance so atlas-boost has chunks-from-rank-11-30 to
        // promote into the final top-K. Without this, the boost can
        // only reorder *within* lance's top-K, which is mostly noise
        // (same articles, different positions). With a wider pool,
        // atlas can actually rescue topically-aligned chunks lance
        // ranked just outside the limit.
        let search_limit = if !atlases.is_empty() {
            limit * 3
        } else {
            limit
        };
        // Route through `search_with_rerank` so a runtime-wired
        // cross-encoder reranker actually fires on this path (the
        // eval bypasses `Runtime::search_corpus_indexes`). When no
        // reranker is installed or `rerank_config.enabled = false`,
        // the call is byte-identical to `search()` — same overfetch,
        // same ordering, same threshold semantics — so the
        // baseline-vs-rerank A/B is honest.
        let atlas_scores_opt = if atlas_article_scores.is_empty() {
            None
        } else {
            Some(&atlas_article_scores)
        };
        match idx
            .search_with_rerank(
                query_vec,
                &q.question,
                search_limit,
                session.runtime.lane_sources.rerank.f(),
                &session.runtime.lane_sources.rerank.config,
                atlas_scores_opt,
            )
            .await
        {
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

    // Atlas-as-graph-navigation. The atlas is a typed knowledge graph
    // (entities, claims, tensions, configurations + edges); cosine-
    // matching individual atom embeddings ("bag-of-atoms") only
    // exercises the most surface-level layer. Real navigation seeds
    // via cosine, then BFS-expands across typed edges (Tension for
    // dialectical pairs, Grounds for argument-depth chains, Involves
    // for entity-event context, Configures for interpretive frame),
    // and identifies the source-chunk neighborhood whose evidence
    // density is highest in the question's atom-vicinity. Those
    // chunks are then fetched via FTS-by-passage_preview against the
    // SEP corpus, restricted to the atom's article.
    let atlas_chunk_requests = if !atlases.is_empty() && !graphs.is_empty() && !embedding.is_empty()
    {
        // Seeds: top-12 atom matches across all atlases (more than
        // the operator's atlas-top-k since seeds drive expansion;
        // many seeds → broader neighborhood).
        let max_seeds = atlases.first().map(|c| c.top_k).unwrap_or(3).max(12);
        let ctx_refs: Vec<&AtlasContext> = atlases.iter().collect();
        let graph_refs: Vec<&AtlasGraph> = graphs.iter().collect();
        // ATLAS_STORAGE_V2 Phase B: the sync `atlas_navigate` is gone — both seed
        // modes now drive the PRODUCTION ANN-seeding navigate over graphs carrying
        // their persistent ANN seed table (attached in `run_bank`); the same code
        // the daemon runs. `--atlas-seed cosine` is kept for runbook compatibility
        // but maps to the identical path.
        match seed_mode {
            SeedMode::Ann | SeedMode::Cosine => {
                atlas_navigate_ann(
                    &q.question,
                    &embedding,
                    &ctx_refs,
                    &graph_refs,
                    max_seeds,
                    /*max_hops=*/ 2,
                )
                .await
            }
        }
    } else {
        Vec::new()
    };

    // The audit-only "atlas_navigation" snapshot — top-K atom matches,
    // unchanged from before, so the JSON output preserves a record of
    // what atlas thinks was relevant. Doesn't enter the prompt.
    let atlas_navigation: Vec<ScoredChunk> = if !atlases.is_empty() && !embedding.is_empty() {
        let nav_k = atlases.first().map(|c| c.top_k).unwrap_or(3);
        let refs: Vec<&AtlasContext> = atlases.iter().collect();
        let nav = atlas_top_k_across(&embedding, &refs, nav_k);
        for c in &nav {
            if !corpora_hit.contains(&c.corpus_id) {
                corpora_hit.push(c.corpus_id.clone());
            }
        }
        nav
    } else {
        Vec::new()
    };

    // Resolve atlas chunk requests via FTS against the SEP corpus.
    // Each ChunkRequest names an article slug + passage_preview; we
    // FTS the preview text against each target index, filter to
    // chunks whose title matches the article, and take the top hit.
    // The atom's aggregated score becomes the chunk's score (boosted
    // significantly to ensure atlas-curated chunks compete with
    // lance vector matches — atlas relevance ~0.6-1.5 vs lance
    // scores ~0.02-0.05). Capped at a budget proportional to limit.
    // Atlas-fetch via question-vector + article-filter. Atlas
    // tells us "this article matters for this question" (via the
    // ChunkRequests' article_slug). Lance has 1024-char chunks for
    // every article in SEP and ranks them by semantic match against
    // the question — but the right article often ranks below top-K
    // when the question only partially mentions it (e.g.
    // communitarianism content for a virtue-ethics question that
    // only names MacIntyre once). Solution: collect unique
    // article-slugs from atlas, then for each do a wide lance
    // search with the question embedding and post-filter to that
    // article's chunks. Returns question-relevant chunks from
    // atlas-aligned articles — lance's specificity meets atlas's
    // article-targeting.
    let atlas_fetch_budget = ((limit as f32) * 0.6).ceil() as usize;
    let mut atlas_fetched: Vec<ScoredChunk> = Vec::new();
    // Internal dedupe for the atlas-fetch loop only — separate from
    // the merge-time dedupe. Using one shared set caused atlas
    // chunks to get rejected at merge because their keys were
    // already in the set from the fetch step.
    let mut atlas_internal_seen: HashSet<String> = HashSet::new();
    let debug_fetch = std::env::var("ATLAS_NAVIGATE_DEBUG").is_ok();

    // Collect unique articles ordered by best (highest) atlas score.
    // Cap article distinct-count at the budget — no point fetching
    // from more articles than we can keep.
    let mut article_score: std::collections::HashMap<String, f32> =
        std::collections::HashMap::new();
    // Aggregate atlas verbatim excerpts (concept defining_quotes,
    // claim quotable_excerpts) per article. We dedupe so the same
    // sentence isn't injected twice when several ChunkRequests for
    // an article overlap in motivating atoms. Injected once per
    // first-chunk-fetched per article in the loop below.
    let mut article_excerpts: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for req in &atlas_chunk_requests {
        let s = article_score.entry(req.article_slug.clone()).or_insert(0.0);
        if req.score > *s {
            *s = req.score;
        }
        if !req.verbatim_excerpts.is_empty() {
            let bucket = article_excerpts
                .entry(req.article_slug.clone())
                .or_default();
            // Three-tier priority for the dialectical_breadth axis:
            //   1. ArgumentReconstructions (`Argument: …`) — densest
            //      P/C structure, best for argument_depth.
            //   2. Contested-tagged Claims (`[… — contested]: …`) —
            //      explicitly counter-position content. Promoting
            //      these into a guaranteed slot is the dialectical
            //      lift this commit targets.
            //   3. Everything else — defining_quotes + regular
            //      quotable_excerpts.
            // Cap raised from 6→8 so all three tiers can land
            // typical entries without one starving another.
            let is_contested = |s: &&String| s.contains("— contested]:");
            let mut prioritised: Vec<&String> = req
                .verbatim_excerpts
                .iter()
                .filter(|s| s.starts_with("Argument:"))
                .collect();
            prioritised.extend(req.verbatim_excerpts.iter().filter(is_contested));
            prioritised.extend(
                req.verbatim_excerpts
                    .iter()
                    .filter(|s| !s.starts_with("Argument:") && !is_contested(s)),
            );
            for ex in prioritised {
                if bucket.len() >= 8 {
                    break;
                }
                if !bucket.iter().any(|existing| existing == ex) {
                    bucket.push(ex.clone());
                }
            }
        }
    }
    let mut articles_ranked: Vec<(String, f32)> = article_score.into_iter().collect();
    articles_ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    articles_ranked.truncate(atlas_fetch_budget);

    if debug_fetch {
        eprintln!(
            "  atlas-fetch: {} unique articles in atlas, fetching from top-{}: {:?}",
            atlas_chunk_requests
                .iter()
                .map(|r| &r.article_slug)
                .collect::<HashSet<_>>()
                .len(),
            articles_ranked.len(),
            articles_ranked.iter().map(|(s, _)| s).collect::<Vec<_>>()
        );
    }

    // The wide search below exists solely to serve the atlas-article loop —
    // with no ranked atlas articles (the no-atlas bench path) it would be a
    // full hybrid search per index whose results are thrown away unread.
    let atlas_target_indexes = if articles_ranked.is_empty() {
        &[][..]
    } else {
        target_indexes
    };
    for info in atlas_target_indexes {
        let idx = match session.corpus_engine.open_index(&info.path).await {
            Ok(i) => i,
            Err(_) => continue,
        };
        // One wide lance search reusable across all atlas articles.
        let wide_hits = match idx.search(&embedding, &q.question, 200).await {
            Ok(h) => h,
            Err(_) => continue,
        };
        for (article_slug, atlas_score) in &articles_ranked {
            // Take top 1-2 chunks from this article whose lance
            // ranks against the question are best.
            let mut found = 0usize;
            for hit in &wide_hits {
                if found >= 2 {
                    break;
                }
                if hit.title.as_deref() != Some(article_slug.as_str()) {
                    continue;
                }
                let dedupe_key = format!("{}|{}", article_slug, truncate(&hit.content, 80));
                if !atlas_internal_seen.insert(dedupe_key) {
                    continue;
                }
                let mut boosted = hit.clone();
                // Atlas's contribution: surface chunks lance ranked
                // outside its top-K. Don't boost — let the chunk
                // compete on lance's intrinsic question-relevance
                // score. Atlas adds the chunk to the pool but lance's
                // ranking arbitrates final position. Prevents
                // displacement of specifically-relevant chunks (e.g.
                // ethics-ancient with function-argument detail) by
                // atlas-aligned chunks (communitarianism intro)
                // with weaker question-direct relevance.
                let _ = atlas_score;
                // Inject verbatim atlas excerpts (concept defining
                // sentences + claim quotable_excerpts) on the first
                // chunk fetched for this article. The judge sees the
                // article's exact words for the position the chunk
                // grounds, addressing the 2026-05-06 calibration's
                // "wants direct primary text" finding.
                if found == 0 {
                    if let Some(excerpts) = article_excerpts.get(article_slug) {
                        if !excerpts.is_empty() {
                            let mut head = String::from("[Atlas highlights]\n");
                            for ex in excerpts {
                                head.push_str(ex);
                                head.push('\n');
                            }
                            head.push('\n');
                            head.push_str(&boosted.content);
                            boosted.content = head;
                        }
                    }
                }
                if debug_fetch {
                    eprintln!(
                        "  atlas-fetch: HIT article={} (atlas_score {:.2}, lance {:.4}) → {}",
                        article_slug,
                        atlas_score,
                        hit.score,
                        truncate(&hit.content, 80).replace('\n', " ")
                    );
                }
                atlas_fetched.push(boosted);
                found += 1;
            }
            if found == 0 && debug_fetch {
                eprintln!(
                    "  atlas-fetch: MISS article={} (no chunks with that title in lance top-200)",
                    article_slug
                );
            }
        }
    }

    // Additive atlas merge: lance keeps its full top-`limit` set;
    // atlas-fetched chunks append. Final retrieved is `limit +
    // up-to-atlas_slots` total. This is the "atlas augments,
    // doesn't displace" design — bank-wide reserved-slots showed
    // that swapping lance chunks for atlas chunks at fixed limit=10
    // hurts essay axes (atlas chunks displace lance argument-detail
    // for topical breadth). Additive lets the judge see both:
    // lance's question-direct retrieval AND atlas's article-
    // targeted supplement. Judge prompt grows by ~2K chars per
    // question; well within the fast slot's budget.
    all_hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_hits.truncate(limit);

    // Dedup atlas chunks against lance's retained set.
    let mut seen_chunks: HashSet<String> = HashSet::new();
    for hit in &all_hits {
        let dedupe_key = format!(
            "{}|{}",
            hit.title.clone().unwrap_or_default(),
            truncate(&hit.content, 80)
        );
        seen_chunks.insert(dedupe_key);
    }
    let pre_atlas_count = all_hits.len();
    let mut atlas_added_in = 0usize;
    for af in atlas_fetched.iter() {
        if atlas_added_in >= atlas_fetch_budget {
            break;
        }
        let dedupe_key = format!(
            "{}|{}",
            af.title.clone().unwrap_or_default(),
            truncate(&af.content, 80)
        );
        if seen_chunks.insert(dedupe_key) {
            all_hits.push(af.clone());
            atlas_added_in += 1;
        }
    }
    if pre_atlas_count != all_hits.len() {
        eprintln!(
            "  atlas-navigate: BFS produced {} requests; {} fetched, \
             {} appended (additive: {} lance + {} atlas = {} total)",
            atlas_chunk_requests.len(),
            atlas_fetched.len(),
            all_hits.len() - pre_atlas_count,
            limit,
            atlas_added_in,
            all_hits.len(),
        );
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
    let attribution_mode = attribution::AttributionMode::from_str(&q.attribution_mode);
    let hits_for_scoring: Vec<ScoredChunk> = if attribution_mode
        == attribution::AttributionMode::Both
    {
        all_hits.clone()
    } else {
        all_hits
            .iter()
            .map(|h| {
                let mut filtered = h.clone();
                filtered.content = attribution::filter_chunk_content(&h.content, attribution_mode);
                filtered
            })
            .collect()
    };
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
    ) = if loose_source_judge && !q.expected_sources.is_empty() {
        let (loose, details) = score_sources_loose(
            &q.question,
            &rigid_source,
            &hits_for_scoring,
            session.inference.as_ref(),
        )
        .await;
        (Some(loose.into()), details)
    } else {
        (None, Vec::new())
    };

    // 3c. Essay-readiness multi-axis judge (Option C). Opt-in via
    //     `--essay-judge`. Asks the LLM whether the retrieved set is
    //     enough material for an undergraduate essay, scoring on four
    //     axes (topical_coverage, position_attribution,
    //     dialectical_breadth, argument_depth), each 0–3. Decoupled
    //     from loose source-credit because they answer different
    //     questions: loose = "are the right articles in the bag?",
    //     essay-readiness = "does the bag have essay-worthy substance?"
    let essay_readiness: Option<EssayReadinessScore> = if essay_judge {
        score_essay_readiness(
            &q.question,
            &q.category,
            &hits_for_scoring,
            &atlas_navigation,
            session.inference.as_ref(),
        )
        .await
    } else {
        None
    };

    // 4. Pack.
    let retrieved = all_hits
        .iter()
        .map(|c| RetrievedChunk {
            corpus_id: c.corpus_id.clone(),
            title: c.title.clone(),
            url: c.url.clone(),
            score: c.score,
            snippet: truncate(&c.content.replace('\n', " "), 600),
            source: None,
        })
        .collect();
    let atlas_navigation_packed = atlas_navigation
        .iter()
        .map(|c| RetrievedChunk {
            corpus_id: c.corpus_id.clone(),
            title: c.title.clone(),
            url: c.url.clone(),
            score: c.score,
            snippet: truncate(&c.content.replace('\n', " "), 600),
            source: None,
        })
        .collect();

    EvalResult {
        error: None,
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
        loose_source_score,
        loose_source_evidence,
        essay_readiness,
        atlas_navigation: atlas_navigation_packed,
        meta_atlas_hits: Vec::new(),
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
    fn with_error(mut self, msg: String) -> Self {
        eprintln!("  [{}] {msg}", self.question_id);
        self.error = Some(msg);
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
    isolate: bool,
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
        let result = run_question_synth(session, q, judge, isolate_corpora.as_deref()).await;
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

async fn run_question_synth(
    session: &ChatSession,
    q: &Question,
    judge: bool,
    isolate_corpora: Option<&[String]>,
) -> EvalResult {
    let conversation_id = uuid::Uuid::new_v4().to_string();
    let t_wall = Instant::now();

    // Per-corpus isolation: seed the conversation's corpus allow-list
    // BEFORE the turn. `handle_message_stream` → `build_context` loads
    // `enabled_corpora` and the retrieval fan-out (Filter 4) honors it;
    // `save_message`'s upsert preserves the column (ON CONFLICT updates
    // only `updated_at`). Best-effort — a seeding failure just falls
    // back to unscoped retrieval rather than voiding the question.
    if let Some(corpora) = isolate_corpora {
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Err(e) = session
            .store
            .insert_empty_conversation(&conversation_id, created_at, None)
            .await
        {
            eprintln!("  warn: isolate seed (insert) failed for {}: {e}", q.id);
        } else if let Err(e) = session
            .store
            .set_conversation_enabled_corpora(&conversation_id, Some(corpora.to_vec()))
            .await
        {
            eprintln!("  warn: isolate seed (scope) failed for {}: {e}", q.id);
        }
    }

    // 1. Drive the same path the desktop chat surface uses. Failures
    //    here become an empty-row result so one model-side error
    //    doesn't void the rest of the bank.
    //
    // ONE turn driver (TOPOLOGY §10 phase 6). Instrument-NEUTRAL: this drove
    // `handle_message_stream` and `collect_turn` is `serve_turn` with a
    // collecting sink, so the pipeline under measurement is unchanged.
    //
    // What it deletes is a hand-rolled drain and a fallback whose stated
    // contract had been retired. The comment here used to say
    // `handle_message_stream` returns `NotImplemented` for ComplexTask /
    // Metalingual / Conation / Commissive; all four are handled INLINE now,
    // specifically so they "must NOT dead-end". The one case that still
    // refuses is a document-attached turn, and catching it after the fact was
    // a latent double-write — the streaming path persists the user message
    // BEFORE it bails, so re-running `handle_message` wrote the question
    // twice. `serve_turn` decides that case up front, so it cannot happen.
    let (message_id, raw, stream_wall_ms) = match sovereign_core::runtime::collect_turn(
        &session.runtime,
        session.store.as_ref(),
        &conversation_id,
        &q.question,
        sovereign_contracts::types::TurnMode::Grounded,
        None,
    )
    .await
    {
        Ok(turn) => {
            let wall = t_wall.elapsed().as_millis() as u64;
            (turn.message_id, turn.text, wall)
        }
        Err(e) => {
            return empty_synth_result(q, format!("turn: {e}"), 0);
        }
    };

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
    // Was the host ROUTING when it answered this? See `degraded_router` — the
    // answer decides whether the scores below are a measurement or the shape
    // of one.
    let degraded = degraded_router(prov);
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

    // Move 4 — canonical-entity boosts echoed back from
    // runtime metadata. One row per primary / alternative slot.
    let meta_atlas_hits: Vec<MetaAtlasHitEcho> = metadata
        .as_ref()
        .and_then(|m| m.get("meta_atlas_hits"))
        .and_then(|v| serde_json::from_value::<Vec<MetaAtlasHitEcho>>(v.clone()).ok())
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
            url: c.get("url").and_then(|v| v.as_str()).map(str::to_string),
            score: 0.0,
            snippet: c
                .get("snippet")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            source: c.get("source").and_then(|v| v.as_str()).map(str::to_string),
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
    let (judge_fact_score, judge_evidence): (Option<ScoreSnapshot>, _) = if judge
        && degraded.is_none()
    {
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
        }),
        loose_source_score: None,
        loose_source_evidence: Vec::new(),
        essay_readiness: None,
        atlas_navigation: Vec::new(),
        meta_atlas_hits: Vec::new(),
    };
    row.with_error(err)
}


#[cfg(test)]
mod degraded_router_tests {
    use super::*;
    use sovereign_contracts::types::{ResponseProvenance, RouterStamp};

    /// The legacy shape from `router_stamp_tests::the_field_is_backward_compatible`
    /// — the minimum a `ResponseProvenance` needs to parse.
    fn provenance() -> ResponseProvenance {
        serde_json::from_str(
            r#"{"intent":"SIMPLE","search_method":null,"sources":[],
                "inference_backend":"m","oicp_match":null,
                "total_latency_ms":1,"tokens_used":2}"#,
        )
        .expect("legacy provenance parses")
    }

    /// SERIALISED BY SERDE, never hand-written, and that is the whole point of
    /// this test. The key this reads (`router`) and the four field names inside
    /// it are `ResponseProvenance`'s and `RouterStamp`'s to choose. A hand-typed
    /// `"router"` here would keep passing after a `#[serde(rename)]` renamed the
    /// wire field, and the detector would then silently never fire again —
    /// which is exactly the failure it exists to catch (ARCH §18.1).
    fn as_metadata(stamp: Option<RouterStamp>) -> serde_json::Value {
        let mut p = provenance();
        p.router = stamp;
        serde_json::to_value(&p).expect("provenance serialises")
    }

    #[test]
    fn a_turn_routed_by_no_classifier_is_not_a_measurement() {
        let degraded = as_metadata(Some(RouterStamp::from_liveness(
            false, false, false, false,
        )));
        let why = degraded_router(Some(&degraded))
            .expect("all four classifiers dead is the degraded host");
        assert!(
            why.contains("not a measurement"),
            "the reason reaches the report and one example is printed by \
             `classify_retrieval` — it has to say what happened; got {why}"
        );
    }

    /// The two ways a healthy run reaches here, and neither may be excluded.
    /// Collapsing either into "degraded" would silently shrink every bank.
    #[test]
    fn a_partial_router_and_an_absent_one_are_both_measurements() {
        let partial = as_metadata(Some(RouterStamp::from_liveness(true, false, false, false)));
        assert_eq!(
            degraded_router(Some(&partial)),
            None,
            "one live classifier still routed — degradation is a degree, and \
             `routed_by_none` is the one implementation of the question (§10.6)"
        );

        let absent = as_metadata(None);
        assert_eq!(
            degraded_router(Some(&absent)),
            None,
            "a turn that does not REPORT a router is not a turn that reports a \
             dead one; old messages have no `router` key at all"
        );

        assert_eq!(
            degraded_router(None),
            None,
            "no provenance block at all is not evidence of degradation"
        );
    }

    /// The exclusion has to survive the trip through `EvalResult`, because that
    /// is the only shape `drop_unmeasured` can see.
    #[test]
    fn the_degraded_row_carries_the_error_drop_unmeasured_filters_on() {
        let row = EvalResult {
            error: None,
            question_id: "q1".into(),
            category: "c".into(),
            question: "why".into(),
            retrieved: Vec::new(),
            source_score: score_sources(&[], &[]).into(),
            fact_score: score_facts_in_text(&[], "").into(),
            embed_ms: 0,
            search_ms: 0,
            corpora_hit: Vec::new(),
            vector_eligible: false,
            synth: None,
            loose_source_score: None,
            loose_source_evidence: Vec::new(),
            essay_readiness: None,
            atlas_navigation: Vec::new(),
            meta_atlas_hits: Vec::new(),
        };
        let degraded = as_metadata(Some(RouterStamp::default()));
        let why = degraded_router(Some(&degraded)).expect("default stamp is all-false");
        assert!(
            row.with_error(why).error.is_some(),
            "a row whose scores are arithmetic over an unrouted answer must not \
             reach the baseline diff as a measurement"
        );
    }
}
