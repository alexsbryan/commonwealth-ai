//! Core data types for the v2 enrichment pipeline.
//!
//! Defines the `PipelinePhase` enum, phase dependencies, and the input/output
//! structs each phase consumes and produces. Phase outputs are the stable
//! schema written to `cache/phase<N>.json`; changes here bump the
//! corresponding `SCHEMA_VERSION` const on `PhaseOutput`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::atlas::SectionExtraction;

// ── Phase identity + dependencies ─────────────────────────────

/// The eight phases of the v2 enrichment pipeline.
///
/// `Ingest` is phase 0 — it covers the section-aware chunking + embedding
/// work that the pipeline needs before any LLM phase runs. The CLI admin
/// harness models it as a cached phase so `status` can report on it too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelinePhase {
    Ingest,
    /// Stage 1a seed-entity extraction — one LLM call (or
    /// structural parse) over the first section produces the seed
    /// entity list threaded into every Stage 1b map call. Cached
    /// at `cache/seed.json`.
    SeedExtraction,
    Questions,
    QuestionClusters,
    Concerns,
    ChunkClusters,
    Positions,
    Tensions,
    Gaps,
    /// Phase 2 output for atlas pipelines — facet-typed clusters
    /// across questions, claims, entity-states, relation-states,
    /// and events. Parallel to `QuestionClusters` (which only holds
    /// question clusters). The two cache slots coexist so legacy
    /// `literary` runs don't collide with `literary_atlas` runs on
    /// the same corpus.
    AtlasClusters,
    /// Phase 3 output for atlas pipelines — per-cluster `NamedCluster`
    /// labels. Parallel to `Concerns`.
    AtlasNamedClusters,
}

impl PipelinePhase {
    pub const ALL: &'static [Self] = &[
        Self::Ingest,
        Self::Questions,
        Self::QuestionClusters,
        Self::Concerns,
        Self::ChunkClusters,
        Self::Positions,
        Self::Tensions,
        Self::Gaps,
        Self::AtlasClusters,
        Self::AtlasNamedClusters,
        // Stage 1a seed extraction — appended so ordinals stay
        // sequential with positions in this array per the
        // `phase_ordinals_are_sequential` test invariant. The
        // logical position is between Ingest and Questions; the
        // runner dispatches it independent of the v1 cascade.
        Self::SeedExtraction,
    ];

    pub const fn ordinal(&self) -> u8 {
        match self {
            Self::Ingest => 0,
            // Seed extraction slots between Ingest and per-section
            // Questions — produced once before the map loop fires.
            Self::SeedExtraction => 10,
            Self::Questions => 1,
            Self::QuestionClusters => 2,
            Self::Concerns => 3,
            Self::ChunkClusters => 4,
            Self::Positions => 5,
            Self::Tensions => 6,
            Self::Gaps => 7,
            // Atlas phases slot after the v1 chain. They ride on
            // Phase 1 output (Questions) directly and don't gate
            // any of the v1 phases, so their ordinals are
            // informational only.
            Self::AtlasClusters => 8,
            Self::AtlasNamedClusters => 9,
        }
    }

    /// Stable short id used in file names (`phase<N>.json`) and CLI flags.
    pub const fn id(&self) -> &'static str {
        match self {
            Self::Ingest => "ingest",
            Self::SeedExtraction => "seed",
            Self::Questions => "questions",
            Self::QuestionClusters => "question-clusters",
            Self::Concerns => "concerns",
            Self::ChunkClusters => "chunk-clusters",
            Self::Positions => "positions",
            Self::Tensions => "tensions",
            Self::Gaps => "gaps",
            Self::AtlasClusters => "atlas-clusters",
            Self::AtlasNamedClusters => "atlas-named-clusters",
        }
    }

    pub const fn display_name(&self) -> &'static str {
        match self {
            Self::Ingest => "Ingest",
            Self::SeedExtraction => "Extract seed entities (Stage 1a)",
            Self::Questions => "Extract per-chapter questions",
            Self::QuestionClusters => "Cluster questions",
            Self::Concerns => "Name canonical concerns",
            Self::ChunkClusters => "Cluster chunks",
            Self::Positions => "Extract grounded positions",
            Self::Tensions => "Detect pairwise tensions",
            Self::Gaps => "Detect gaps",
            Self::AtlasClusters => "Cluster atlas sketches by facet",
            Self::AtlasNamedClusters => "Name atlas clusters per facet",
        }
    }

    /// Phases whose caches this phase's output depends on.
    ///
    /// Staleness detection walks these: if any upstream cache mtime is
    /// newer than this phase's cache, this phase is stale and must
    /// re-run.
    pub const fn dependencies(&self) -> &'static [Self] {
        match self {
            Self::Ingest => &[],
            // Seed extraction reads the first chapter from the
            // corpus state (which comes from Ingest) — but it's
            // not gated on the Ingest cache since the runner
            // threads the first chapter in directly.
            Self::SeedExtraction => &[Self::Ingest],
            Self::Questions => &[Self::Ingest],
            Self::QuestionClusters => &[Self::Questions],
            Self::Concerns => &[Self::QuestionClusters],
            Self::ChunkClusters => &[Self::Ingest],
            Self::Positions => &[Self::Concerns, Self::ChunkClusters],
            Self::Tensions => &[Self::Positions],
            Self::Gaps => &[Self::Concerns, Self::Positions, Self::Tensions],
            // Atlas phases consume Phase 1 output directly. They
            // don't depend on the v1 clustering chain (different
            // clusters, different downstream consumers).
            Self::AtlasClusters => &[Self::Questions],
            Self::AtlasNamedClusters => &[Self::AtlasClusters],
        }
    }
}

impl FromStr for PipelinePhase {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "ingest" => Ok(Self::Ingest),
            "questions" | "extract" => Ok(Self::Questions),
            "question-clusters" | "cluster-questions" => Ok(Self::QuestionClusters),
            "concerns" | "name-concerns" => Ok(Self::Concerns),
            "chunk-clusters" | "cluster-chunks" => Ok(Self::ChunkClusters),
            "positions" | "extract-positions" => Ok(Self::Positions),
            "tensions" | "detect-tensions" => Ok(Self::Tensions),
            "gaps" | "detect-gaps" => Ok(Self::Gaps),
            "seed" | "seed-extraction" | "stage-1a" => Ok(Self::SeedExtraction),
            "atlas-clusters" | "cluster-atlas" => Ok(Self::AtlasClusters),
            "atlas-named-clusters" | "name-atlas-clusters" => Ok(Self::AtlasNamedClusters),
            other => Err(format!("unknown phase: {other}")),
        }
    }
}

// ── Domain vocabulary + prompt envelope ───────────────────────

/// Epistemic vocabulary for one pipeline (scaffold §8.1 of the spec).
/// Lives on the `Pipeline` trait; the CLI prints these in `show`
/// headers and the `query` LOCATE output so the terminology matches
/// the domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vocabulary {
    pub canonical_concern_term: String,
    pub position_term: String,
    pub tension_term: String,
    pub absence_term: String,
    /// What a single piece of grounding evidence is called
    /// ("paragraph", "passage", "snippet").
    pub evidence_term: String,
}

/// Chat message prompt ready to submit to an OpenAI-compatible endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPrompt {
    pub system: String,
    pub user: String,
}

impl ChatPrompt {
    pub fn new(system: impl Into<String>, user: impl Into<String>) -> Self {
        Self { system: system.into(), user: user.into() }
    }
}

/// Chat-completion function injected by the caller — this is the v2
/// analogue of `crate::types::InferenceFn`, which is single-message
/// only. v2 prompts are multi-message (system + user), so the runner
/// needs a richer entry point.
///
/// Sovereign wraps the Primary slot; Commonwealth wraps the mesh chat
/// endpoint; tests pass a deterministic closure returning canned JSON.
pub type ChatCompletionFn = Arc<
    dyn Fn(&ChatPrompt) -> Pin<Box<dyn Future<Output = crate::error::Result<String>> + Send>>
        + Send
        + Sync,
>;

/// Chat-completion function with a per-call `max_tokens` override.
/// Used by the runner when a retry mode needs a larger output budget
/// for specific chapters without mutating the shared client.
///
/// Callers that don't need per-call overrides can keep using
/// `ChatCompletionFn` directly; the runner selects which closure to
/// invoke based on whether a retry mode is active.
pub type ChatCompletionWithTokensFn = Arc<
    dyn Fn(&ChatPrompt, u32) -> Pin<Box<dyn Future<Output = crate::error::Result<String>> + Send>>
        + Send
        + Sync,
>;

/// Per-invocation retry mode for Phase 1. Passed to
/// `PhaseRunner::phase_1_extract_questions_with_retry` by the CLI
/// when the user requests `--retry-failed --terse`. Extensible —
/// future variants (e.g. a philosophy-specific retry mode) land as
/// additional enum members without changing the runner's entry
/// point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RetryMode {
    /// Terse prompt variant — the pipeline's `compose_phase1_terse`
    /// fires and the chat call honours `max_output_tokens` for the
    /// retry only. A pipeline that doesn't expose a terse variant
    /// (returns `None`) causes the retry to fail fast with a clear
    /// error; the runner does not silently fall back to the default
    /// prompt.
    Terse { max_output_tokens: u32 },
}

// ── Phase 1: per-chapter question extraction ──────────────────

/// Input to phase 1. Constructed by the runner from the chunk index +
/// chapter manifest; the pipeline impl receives one of these per call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterInput {
    pub chapter_id: String,
    pub title: String,
    /// Full chapter body, concatenated from paragraph chunks in order.
    pub text: String,
    /// Detector metadata carried through from `SectionedChunker`.
    pub metadata: HashMap<String, String>,
    /// Approximate token count (4 chars per token heuristic) — used by
    /// the runner to decide whether the input fits the model's context.
    pub approx_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedQuestion {
    pub chapter_id: String,
    pub questions: Vec<String>,
    /// One-line framing of what the chapter is *doing* in the structure
    /// of the work. Optional because early iterations may skip it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reveals: Option<String>,
    /// Characters (or general entities) whose arcs carry this chapter's
    /// thematic weight. Populated on phase 1; merged into the
    /// `ChapterManifest` post-run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thematic_carriers: Vec<String>,
    /// Terse phrase locating the chapter in place/time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setting: Option<String>,
    /// One sentence naming what physically happens in the chapter —
    /// the event the thematic question is carried by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plot: Option<String>,
    /// Atlas v2.1 widening — populated by pipelines that extract the
    /// full typed atom graph (e.g. `literary_atlas`). Left `None` by
    /// v1-shaped pipelines (`literary`) so both schemas coexist in the
    /// same `Phase1Output` cache file. Phase 5 onwards reads this when
    /// present and falls back to the `questions` field otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_extraction: Option<SectionExtraction>,
}

/// Parsed result of one phase-1 call. The runner stamps `chapter_id`
/// from the input it dispatched and combines the result into an
/// `ExtractedQuestion` for the cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase1ChapterResult {
    pub questions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reveals: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thematic_carriers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setting: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plot: Option<String>,
    /// Atlas v2.1 widening (see `ExtractedQuestion::section_extraction`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_extraction: Option<SectionExtraction>,
}

/// Structured classification of a Phase 1 failure. Used alongside
/// the free-text `reason` so the CLI can route each chapter to the
/// right recovery path without sniffing the reason string:
///
/// - `ThinkTruncated` → retry with the terse prompt variant (if the
///   pipeline has one) and/or a larger `max_output_tokens` budget.
/// - `ParseDrift` → the JSON was malformed in a way the tolerant
///   parser couldn't recover; a plain retry may succeed.
/// - `ChatError` → transport-level failure; a plain retry is
///   appropriate.
/// - `EmptyExtraction` → the model returned a well-formed envelope
///   with zero atoms; usually a schema-echo case, needs prompt fix.
/// - `Skipped` → the runner declined to dispatch (body too short,
///   heading-only section).
/// - `Other` → anything the classifier can't bucket yet.
///
/// Kept as a small closed enum so `sovereign enrich status` can
/// tally by kind without re-parsing run files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseFailureKind {
    ThinkTruncated,
    ParseDrift,
    ChatError,
    EmptyExtraction,
    Skipped,
    Other,
}

/// Chapter-level failure record. Carried through `Phase1RunResult` for
/// live reporting and serialized into `Phase1Output.failures` so the
/// run file preserves a truncated head of whatever the model actually
/// said. Without this the only signal on a parse failure is the
/// exception text, which makes prompt debugging a guessing game.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase1Failure {
    pub chapter_id: String,
    pub reason: String,
    /// First ~1 KiB of the raw model response, UTF-8 safe, with a
    /// `… [+N chars]` marker when truncated. `None` only when the
    /// failure happened before any response arrived (e.g. chat error).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_response_head: Option<String>,
    /// Structured failure category. Defaults to `Other` so failure
    /// records written before this field existed still deserialise.
    /// New failures should always populate it — the classifier in
    /// `runner.rs::classify_phase1_failure` maps raw evidence (parse
    /// error, response body, chat transport error) into the enum
    /// without string-sniffing the `reason` field.
    #[serde(default = "phase_failure_kind_other")]
    pub failure_kind: PhaseFailureKind,
}

fn phase_failure_kind_other() -> PhaseFailureKind {
    PhaseFailureKind::Other
}

/// Parsed result of one phase-3 call. Runner stamps `id` + `cluster_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase3ParseResult {
    pub concern_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub primary_arcs: Vec<String>,
}

/// Parsed result of one phase-5 call. Runner stamps `id` + `concern_id` +
/// `chunk_cluster_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase5ParseResult {
    pub position_text: String,
    pub grounding: Vec<Grounding>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extensions: HashMap<String, String>,
}

/// Parsed result of one phase-6 call. `None` when the model reports
/// `{"tension": false}`. Runner stamps the tension `id` +
/// position cross-refs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase6ParseResult {
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specific_disagreement: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structural_type: Option<String>,
}

/// Parsed result of one phase-7 call. Runner stamps ids.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase7ParseItem {
    pub gap_text: String,
    pub evidence: String,
    pub significance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase1Output {
    pub schema_version: u32,
    pub pipeline_id: String,
    pub questions_by_chapter: Vec<ExtractedQuestion>,
    /// Chapters whose chat or parse failed during this run. Persisted
    /// alongside the successes so a run file is self-contained for
    /// post-mortem prompt debugging.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<Phase1Failure>,
    /// ISO-8601 UTC timestamp of when the cache was written.
    pub written_at: String,
}

impl Phase1Output {
    pub const SCHEMA_VERSION: u32 = 1;
}

/// True when a string is indistinguishable from a schema-template
/// placeholder a model would copy verbatim — `"..."`, `"…"`, any
/// combination of those dots and whitespace, or the literal token
/// `TODO`. Trim-tolerant; callers don't need to pre-trim.
///
/// Used wherever we'd otherwise silently persist a placeholder echo
/// (phase-1 parser, `characters_present` merge, manifest hydration).
pub fn is_placeholder_literal(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    if t == "..." || t == "…" || t.eq_ignore_ascii_case("todo") {
        return true;
    }
    t.chars().all(|c| c == '.' || c == '…' || c.is_whitespace())
}

/// Remove any `<think>...</think>` spans from `response`, returning a
/// copy with the reasoning blocks deleted. Thinking-capable models
/// (Qwen3, DeepSeek R1, o1-family) emit chain-of-thought between
/// these tags before their actual answer. The answer we care about
/// follows the closing tag; parsers that only read the head of the
/// response would otherwise miss the JSON entirely.
///
/// Non-destructive: if no `<think>` tag is present, the returned
/// string is byte-identical to the input. If an opening tag has no
/// matching close (the response was truncated mid-think), the entire
/// tail from `<think>` onward is dropped — callers should detect
/// this separately and surface a clear error, since a truncated
/// response has no JSON to parse anyway.
pub fn strip_reasoning_tags(response: &str) -> String {
    let mut out = String::with_capacity(response.len());
    let mut remaining = response;
    while let Some(open_idx) = remaining.find("<think>") {
        out.push_str(&remaining[..open_idx]);
        let after_open = &remaining[open_idx + "<think>".len()..];
        match after_open.find("</think>") {
            Some(close_idx) => {
                remaining = &after_open[close_idx + "</think>".len()..];
            }
            None => {
                // Unclosed <think> — drop the rest.
                remaining = "";
                break;
            }
        }
    }
    out.push_str(remaining);
    out
}

/// True when `response` opens a `<think>` block but never closes it.
/// This is the thinking-model truncation signature: the model spent
/// its whole output budget reasoning and never produced the requested
/// answer. A stray `{` inside the reasoning trace (e.g. the model
/// drafting sample JSON while it thinks) does NOT invalidate the
/// detection — the answer we care about sits after `</think>`, and
/// without that close tag we cannot have reached it.
pub fn is_truncated_thinking_response(response: &str) -> bool {
    let Some(open_idx) = response.find("<think>") else {
        return false;
    };
    let after_open = &response[open_idx + "<think>".len()..];
    !after_open.contains("</think>")
}

/// Extract the first JSON object from a model response, tolerating
/// leading prose and/or surrounding Markdown code fences. Returns the
/// JSON substring (without the fences) or `None` if nothing resembling
/// an object can be located.
///
/// Used by every phase's `parse_*` to be forgiving about model output
/// framing while still rejecting genuinely malformed bodies downstream
/// in the `serde_json::from_str` step.
pub fn extract_json_block(response: &str) -> Option<&str> {
    // Look for a ```json fenced block first.
    if let Some(start) = response.find("```json") {
        let rest = &response[start + "```json".len()..];
        if let Some(end) = rest.find("```") {
            return Some(rest[..end].trim());
        }
    }
    // Or any ``` fenced block whose content starts with `{`.
    if let Some(start) = response.find("```") {
        let rest = &response[start + 3..];
        if let Some(end) = rest.find("```") {
            let inner = rest[..end].trim();
            if inner.starts_with('{') {
                return Some(inner);
            }
        }
    }
    // Fall back to the first `{…}` block, picking the widest balanced
    // braces scan we can do cheaply.
    let bytes = response.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&response[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

// ── Phase 2: question clustering ──────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionCluster {
    pub id: String,
    /// Indices into the flattened list of questions from phase 1.
    pub question_refs: Vec<QuestionRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionRef {
    pub chapter_id: String,
    /// Index into `ExtractedQuestion::questions`.
    pub question_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase2Output {
    pub schema_version: u32,
    pub pipeline_id: String,
    pub clusters: Vec<QuestionCluster>,
    pub unclustered: Vec<QuestionRef>,
    pub written_at: String,
}

impl Phase2Output {
    pub const SCHEMA_VERSION: u32 = 1;
}

// ── Phase 3: canonical concern naming ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalConcern {
    pub id: String,
    pub cluster_id: String,
    pub concern_text: String,
    /// Domain-specific scope (spec §8.1: "novel-wide", "chapter-local").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub primary_arcs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase3Output {
    pub schema_version: u32,
    pub pipeline_id: String,
    pub concerns: Vec<CanonicalConcern>,
    pub written_at: String,
}

impl Phase3Output {
    pub const SCHEMA_VERSION: u32 = 1;
}

// ── Phase 2/3: atlas-pipeline clustering + naming ─────────────
//
// The v1 Phase 2/3 types above (QuestionCluster, CanonicalConcern)
// serve the legacy questions-only flow. The atlas pipeline produces
// typed clusters across five facets — the types below carry that
// shape end-to-end from clustering (Phase 2) through naming
// (Phase 3) and into Phase 5 grounded resolution.

/// Which atlas sketch field a cluster or reference belongs to.
/// Matches the per-section fields on `SectionExtraction`:
/// `questions_raised`, `claims`, `entities_developed`,
/// `relations_developed`, and `events`. `entities_introduced` and
/// `relations_introduced` aren't clustered — they name things, not
/// developments, and feed directly into Phase 3a entity/event
/// resolution rather than into Phase 2 clustering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Facet {
    Question,
    Claim,
    EntityState,
    RelationState,
    Event,
}

impl Facet {
    /// Stable string form. Used by the exemplar directory layout
    /// (`exemplars/<pipeline>/<phase>/<facet>.json`) and by the
    /// naming prompts' file stems
    /// (`phase3_{question,claim,entity_state_trajectory,…}_naming.md`).
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::Claim => "claim",
            Self::EntityState => "entity_state",
            Self::RelationState => "relation_state",
            Self::Event => "event",
        }
    }

    /// Suffix used in Phase 3 prompt file names — tracks the
    /// "trajectory" / "thread" terminology from spec §5.3.
    pub const fn prompt_suffix(&self) -> &'static str {
        match self {
            Self::Question => "question_naming",
            Self::Claim => "claim_naming",
            Self::EntityState => "entity_state_trajectory_naming",
            Self::RelationState => "relation_state_trajectory_naming",
            Self::Event => "event_thread_naming",
        }
    }

    pub const ALL: &'static [Self] = &[
        Self::Question,
        Self::Claim,
        Self::EntityState,
        Self::RelationState,
        Self::Event,
    ];
}

/// A reference from a cluster (or the noise pile) back to a specific
/// sketch inside a `SectionExtraction`. The tuple `(section_id,
/// facet, sketch_index)` uniquely identifies a sketch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SketchRef {
    pub section_id: String,
    pub facet: Facet,
    /// Index into the facet's vector on the source `SectionExtraction`
    /// (e.g. `section.claims[sketch_index]`).
    pub sketch_index: usize,
}

/// One cluster of sketches sharing a facet.
///
/// The cluster carries its facet tag so Phase 3 naming can branch
/// on it without a second lookup. Consumers that want per-facet
/// partitions can `clusters.iter().filter(|c| c.facet == F)`; the
/// `Phase2AtlasOutput` methods expose that as a one-liner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasCluster {
    pub id: String,
    pub facet: Facet,
    pub refs: Vec<SketchRef>,
}

/// Top-level Phase 2 output for atlas pipelines. All facets live
/// in one `clusters` list tagged with `Facet`; `unclustered` is
/// the union of noise points across facets (each noise point
/// carries its own facet so downstream code can partition when
/// needed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase2AtlasOutput {
    pub schema_version: u32,
    pub pipeline_id: String,
    pub clusters: Vec<AtlasCluster>,
    pub unclustered: Vec<SketchRef>,
    pub written_at: String,
}

impl Phase2AtlasOutput {
    pub const SCHEMA_VERSION: u32 = 1;

    /// Iterate only the clusters of a given facet. Thin wrapper
    /// that keeps the common "give me the claim clusters" call a
    /// one-liner without collapsing the facet tag at storage time.
    pub fn clusters_by_facet(&self, facet: Facet) -> impl Iterator<Item = &AtlasCluster> {
        self.clusters.iter().filter(move |c| c.facet == facet)
    }
}

/// Phase 3 output — one named label per Phase 2 cluster, keyed by
/// cluster id. `metadata` carries facet-specific extensions (e.g.
/// `primary_arcs` on entity-state trajectory labels, `attributed_to`
/// on claim labels) without forcing the top-level schema to enumerate
/// every future field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedCluster {
    pub id: String,
    pub cluster_id: String,
    pub facet: Facet,
    pub label: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase3AtlasOutput {
    pub schema_version: u32,
    pub pipeline_id: String,
    pub named_clusters: Vec<NamedCluster>,
    pub written_at: String,
}

impl Phase3AtlasOutput {
    pub const SCHEMA_VERSION: u32 = 1;
}

/// Parse result from `parse_phase3_facet` — the fields a naming
/// prompt is expected to return. The runner combines this with the
/// cluster id + facet to build a `NamedCluster`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase3FacetParseResult {
    pub label: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

/// One sketch's content rendered for a Phase 3 naming prompt. The
/// CLI (or any runner) assembles a vector of these by walking a
/// cluster's `refs` and looking up each (section_id, facet,
/// sketch_index) in the Phase 1 cache. The prompt composer reads
/// the list without re-fetching source material — everything the
/// LLM needs to name the cluster is in the excerpt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SketchExcerpt {
    pub section_id: String,
    /// Facet-specific rendering of the sketch:
    /// - Question → the question content.
    /// - Claim → `"[discourse_act/epistemic_status] content"`; when
    ///   the claim is attributed, prefix with `"<entity>: "`.
    /// - EntityState → `"<entity>: <label>"`.
    /// - RelationState → `"<p1 × p2>: <label>"`.
    /// - Event → the event description.
    pub content: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub anchor: String,
}

// ── Phase 4: chunk clustering ─────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkCluster {
    pub id: String,
    pub chunk_ids: Vec<u64>,
    pub noise: bool,
    /// Mean embedding of this cluster's members. Used by phase 5 to
    /// align clusters to canonical concerns by cosine similarity
    /// before calling the model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub centroid: Vec<f32>,
}

/// A paragraph-level chunk the pipeline can read. Assembled by the
/// CLI's `corpus_io` helper from the source file + detected sections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRecord {
    pub id: u64,
    pub section_id: String,
    pub text: String,
}

/// Everything the phase runners need to reconstruct work from the
/// source file. The CLI builds one per subcommand; unit tests build
/// one inline.
#[derive(Debug, Clone)]
pub struct CorpusContext {
    pub chapters: Vec<ChapterInput>,
    pub chunks: Vec<ChunkRecord>,
    pub chapter_titles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase4Output {
    pub schema_version: u32,
    pub pipeline_id: String,
    pub clusters: Vec<ChunkCluster>,
    pub written_at: String,
}

impl Phase4Output {
    pub const SCHEMA_VERSION: u32 = 1;
}

// ── Phase 5: grounded position extraction ─────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub id: String,
    pub concern_id: String,
    pub chunk_cluster_id: String,
    pub position_text: String,
    pub grounding: Vec<Grounding>,
    /// Domain-specific extensions (spec §8.1: character_voice).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extensions: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grounding {
    pub chunk_id: u64,
    pub section_id: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase5Output {
    pub schema_version: u32,
    pub pipeline_id: String,
    pub positions: Vec<Position>,
    pub written_at: String,
}

impl Phase5Output {
    pub const SCHEMA_VERSION: u32 = 1;
}

// ── Phase 6: pairwise tension detection ───────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tension {
    pub id: String,
    pub position_a_id: String,
    pub position_b_id: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specific_disagreement: Option<String>,
    /// Domain-specific extension (spec §8.1: parallel_contrast,
    /// ironic_mirror, etc).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structural_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase6Output {
    pub schema_version: u32,
    pub pipeline_id: String,
    pub tensions: Vec<Tension>,
    pub written_at: String,
}

impl Phase6Output {
    pub const SCHEMA_VERSION: u32 = 1;
}

// ── Phase 7: gap detection ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gap {
    pub id: String,
    pub gap_text: String,
    pub evidence: String,
    /// Free-form "low / medium / high" plus rationale.
    pub significance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase7Output {
    pub schema_version: u32,
    pub pipeline_id: String,
    pub gaps: Vec<Gap>,
    pub written_at: String,
}

impl Phase7Output {
    pub const SCHEMA_VERSION: u32 = 1;
}

// ── Atlas: consolidated view used by `query` ──────────────────

/// Traversal-ready consolidation of phases 3/5/6/7.
///
/// Built lazily by `Atlas::from_cache` at query time. Not a cached
/// phase — it's derived from the phase caches.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Atlas {
    pub concerns: Vec<CanonicalConcern>,
    pub positions: Vec<Position>,
    pub tensions: Vec<Tension>,
    pub gaps: Vec<Gap>,
}

impl Atlas {
    pub fn position(&self, id: &str) -> Option<&Position> {
        self.positions.iter().find(|p| p.id == id)
    }

    pub fn concern(&self, id: &str) -> Option<&CanonicalConcern> {
        self.concerns.iter().find(|c| c.id == id)
    }

    pub fn positions_for_concern(&self, concern_id: &str) -> Vec<&Position> {
        self.positions
            .iter()
            .filter(|p| p.concern_id == concern_id)
            .collect()
    }

    pub fn tensions_for_position(&self, pos_id: &str) -> Vec<&Tension> {
        self.tensions
            .iter()
            .filter(|t| t.position_a_id == pos_id || t.position_b_id == pos_id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facet_stringification_matches_spec_taxonomy() {
        // Pins the wire format. The exemplar directory layout and
        // the Phase 3 prompt file names both read these strings,
        // so a rename here is load-bearing — tests fail loudly.
        assert_eq!(Facet::Question.as_str(), "question");
        assert_eq!(Facet::Claim.as_str(), "claim");
        assert_eq!(Facet::EntityState.as_str(), "entity_state");
        assert_eq!(Facet::RelationState.as_str(), "relation_state");
        assert_eq!(Facet::Event.as_str(), "event");

        assert_eq!(
            Facet::EntityState.prompt_suffix(),
            "entity_state_trajectory_naming"
        );
        assert_eq!(Facet::Event.prompt_suffix(), "event_thread_naming");
    }

    #[test]
    fn facet_enumeration_is_complete() {
        // Every variant must be in ALL — Phase 2 iterates over
        // Facet::ALL to drive clustering, so a missed entry means a
        // silent gap in coverage.
        let all_via_match: Vec<Facet> = vec![
            Facet::Question,
            Facet::Claim,
            Facet::EntityState,
            Facet::RelationState,
            Facet::Event,
        ];
        assert_eq!(all_via_match.len(), Facet::ALL.len());
        for f in &all_via_match {
            assert!(Facet::ALL.contains(f));
        }
    }

    #[test]
    fn facet_serializes_as_snake_case() {
        let json = serde_json::to_string(&Facet::EntityState).unwrap();
        assert_eq!(json, "\"entity_state\"");
        let back: Facet = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Facet::EntityState);
    }

    #[test]
    fn phase2_atlas_output_partitions_by_facet() {
        let output = Phase2AtlasOutput {
            schema_version: Phase2AtlasOutput::SCHEMA_VERSION,
            pipeline_id: "literary_atlas".into(),
            clusters: vec![
                AtlasCluster {
                    id: "cl_q_01".into(),
                    facet: Facet::Question,
                    refs: vec![SketchRef {
                        section_id: "sec_0001".into(),
                        facet: Facet::Question,
                        sketch_index: 0,
                    }],
                },
                AtlasCluster {
                    id: "cl_c_01".into(),
                    facet: Facet::Claim,
                    refs: Vec::new(),
                },
                AtlasCluster {
                    id: "cl_c_02".into(),
                    facet: Facet::Claim,
                    refs: Vec::new(),
                },
            ],
            unclustered: Vec::new(),
            written_at: "t".into(),
        };
        let claims: Vec<&AtlasCluster> = output.clusters_by_facet(Facet::Claim).collect();
        assert_eq!(claims.len(), 2);
        let questions: Vec<&AtlasCluster> =
            output.clusters_by_facet(Facet::Question).collect();
        assert_eq!(questions.len(), 1);
        let states: Vec<&AtlasCluster> =
            output.clusters_by_facet(Facet::EntityState).collect();
        assert!(states.is_empty());
    }

    #[test]
    fn named_cluster_metadata_roundtrips() {
        let mut md = HashMap::new();
        md.insert("attributed_to".into(), "Zosima".into());
        let nc = NamedCluster {
            id: "ncl_01".into(),
            cluster_id: "cl_c_01".into(),
            facet: Facet::Claim,
            label: "Active love costs more than dreamt love.".into(),
            metadata: md,
        };
        let json = serde_json::to_string(&nc).unwrap();
        let back: NamedCluster = serde_json::from_str(&json).unwrap();
        assert_eq!(back.facet, Facet::Claim);
        assert_eq!(back.metadata.get("attributed_to").unwrap(), "Zosima");
    }

    #[test]
    fn phase3_atlas_output_is_json_stable() {
        let out = Phase3AtlasOutput {
            schema_version: Phase3AtlasOutput::SCHEMA_VERSION,
            pipeline_id: "literary_atlas".into(),
            named_clusters: Vec::new(),
            written_at: "t".into(),
        };
        let json = serde_json::to_string(&out).unwrap();
        assert!(json.contains("\"pipeline_id\":\"literary_atlas\""));
        assert!(json.contains("\"named_clusters\":[]"));
    }

    #[test]
    fn phase_ordinals_are_sequential() {
        for (i, phase) in PipelinePhase::ALL.iter().enumerate() {
            assert_eq!(phase.ordinal() as usize, i);
        }
    }

    #[test]
    fn phase_ids_are_unique() {
        let ids: std::collections::HashSet<_> =
            PipelinePhase::ALL.iter().map(|p| p.id()).collect();
        assert_eq!(ids.len(), PipelinePhase::ALL.len());
    }

    #[test]
    fn phase_dependencies_point_backwards() {
        for phase in PipelinePhase::ALL {
            for dep in phase.dependencies() {
                assert!(
                    dep.ordinal() < phase.ordinal(),
                    "{:?} depends on {:?} but the latter is not upstream",
                    phase,
                    dep
                );
            }
        }
    }

    #[test]
    fn phase_from_str_accepts_aliases() {
        assert_eq!("extract".parse::<PipelinePhase>().unwrap(), PipelinePhase::Questions);
        assert_eq!(
            "cluster-questions".parse::<PipelinePhase>().unwrap(),
            PipelinePhase::QuestionClusters
        );
        assert_eq!("positions".parse::<PipelinePhase>().unwrap(), PipelinePhase::Positions);
        assert!("nonsense".parse::<PipelinePhase>().is_err());
    }

    #[test]
    fn atlas_traversal_filters_by_concern() {
        let atlas = Atlas {
            concerns: vec![CanonicalConcern {
                id: "cc_01".into(),
                cluster_id: "qc_01".into(),
                concern_text: "test".into(),
                scope: None,
                primary_arcs: Vec::new(),
            }],
            positions: vec![
                Position {
                    id: "pos_01".into(),
                    concern_id: "cc_01".into(),
                    chunk_cluster_id: "kc_01".into(),
                    position_text: "a".into(),
                    grounding: Vec::new(),
                    extensions: HashMap::new(),
                },
                Position {
                    id: "pos_02".into(),
                    concern_id: "cc_other".into(),
                    chunk_cluster_id: "kc_02".into(),
                    position_text: "b".into(),
                    grounding: Vec::new(),
                    extensions: HashMap::new(),
                },
            ],
            tensions: vec![Tension {
                id: "t_01".into(),
                position_a_id: "pos_01".into(),
                position_b_id: "pos_02".into(),
                description: "d".into(),
                specific_disagreement: None,
                structural_type: None,
            }],
            gaps: Vec::new(),
        };

        let for_cc = atlas.positions_for_concern("cc_01");
        assert_eq!(for_cc.len(), 1);
        assert_eq!(for_cc[0].id, "pos_01");

        let tensions = atlas.tensions_for_position("pos_01");
        assert_eq!(tensions.len(), 1);
    }

    #[test]
    fn extract_json_block_pure_object() {
        let s = r#"{"a":1,"b":"x"}"#;
        assert_eq!(extract_json_block(s), Some(s));
    }

    #[test]
    fn extract_json_block_with_prose_preamble() {
        let s = "Sure, here is the output:\n{\"ok\":true}";
        assert_eq!(extract_json_block(s), Some("{\"ok\":true}"));
    }

    #[test]
    fn extract_json_block_from_code_fence() {
        let s = "```json\n{\"ok\":true}\n```";
        assert_eq!(extract_json_block(s), Some("{\"ok\":true}"));
    }

    #[test]
    fn extract_json_block_from_generic_fence() {
        let s = "```\n{\"x\":1}\n```";
        assert_eq!(extract_json_block(s), Some("{\"x\":1}"));
    }

    #[test]
    fn extract_json_block_handles_braces_in_strings() {
        let s = r#"prelude {"text":"}}","n":1}"#;
        let got = extract_json_block(s).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(got).unwrap();
        assert_eq!(parsed["text"], "}}");
        assert_eq!(parsed["n"], 1);
    }

    #[test]
    fn strip_reasoning_tags_removes_complete_think_block() {
        let raw = "<think>considering the options</think>\n{\"questions\":[\"q1\"]}";
        let out = strip_reasoning_tags(raw);
        assert!(!out.contains("<think>"));
        assert!(out.contains("\"questions\""));
    }

    #[test]
    fn strip_reasoning_tags_drops_unclosed_think_tail() {
        // Truncated mid-thought: everything after `<think>` is dropped.
        let raw = "preamble <think>the model ran out of tokens before closing";
        let out = strip_reasoning_tags(raw);
        assert_eq!(out, "preamble ");
    }

    #[test]
    fn strip_reasoning_tags_passthrough_when_no_tag() {
        let raw = "{\"questions\":[\"q1\"]}";
        let out = strip_reasoning_tags(raw);
        assert_eq!(out, raw);
    }

    #[test]
    fn is_truncated_thinking_response_fires_whenever_think_is_unclosed() {
        assert!(is_truncated_thinking_response("<think>long reasoning without closure"));
        // A `{` inside the reasoning trace is a red herring; the
        // answer we care about is after </think>, which is missing.
        assert!(is_truncated_thinking_response(
            "<think>drafting {\"position_text\": …} but never closing"
        ));
        // Closed thinking tag + JSON → not truncated.
        assert!(!is_truncated_thinking_response("<think>done</think>{\"q\":1}"));
        // No think tag at all → not truncated.
        assert!(!is_truncated_thinking_response("{\"q\":1}"));
    }

    #[test]
    fn extract_json_block_none_when_absent() {
        assert!(extract_json_block("no json at all").is_none());
    }

    #[test]
    fn phase1_output_roundtrip() {
        let out = Phase1Output {
            schema_version: Phase1Output::SCHEMA_VERSION,
            pipeline_id: "literary".into(),
            questions_by_chapter: vec![ExtractedQuestion {
                chapter_id: "ch_0001".into(),
                questions: vec!["What is the question?".into()],
                reveals: Some("A framing line.".into()),
                thematic_carriers: vec!["Anna".into()],
                setting: Some("Moscow drawing-room, 1870s".into()),
                plot: Some("A letter arrives and is read aloud.".into()),
                section_extraction: None,
            }],
            failures: vec![Phase1Failure {
                chapter_id: "ch_0007".into(),
                reason: "parse error: missing questions".into(),
                raw_response_head: Some("I cannot help with that.".into()),
                failure_kind: PhaseFailureKind::ParseDrift,
            }],
            written_at: "2026-04-22T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&out).unwrap();
        let parsed: Phase1Output = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.questions_by_chapter.len(), 1);
        assert_eq!(parsed.pipeline_id, "literary");
        assert_eq!(parsed.questions_by_chapter[0].setting.as_deref(), Some("Moscow drawing-room, 1870s"));
        assert_eq!(parsed.questions_by_chapter[0].plot.as_deref(), Some("A letter arrives and is read aloud."));
        assert_eq!(parsed.failures.len(), 1);
        assert_eq!(parsed.failures[0].raw_response_head.as_deref(), Some("I cannot help with that."));
    }

    #[test]
    fn phase1_output_roundtrip_omits_empty_failures() {
        let out = Phase1Output {
            schema_version: Phase1Output::SCHEMA_VERSION,
            pipeline_id: "literary".into(),
            questions_by_chapter: vec![],
            failures: vec![],
            written_at: "2026-04-22T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&out).unwrap();
        assert!(!json.contains("failures"), "empty failures should be skipped; got {json}");
    }
}
