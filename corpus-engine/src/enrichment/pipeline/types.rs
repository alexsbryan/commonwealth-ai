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

// Pure string helpers split out into `text_helpers` (ARCH §3.1). The
// historical `super::types::is_placeholder_literal` etc. paths still
// work via this re-export so the split is callsite-transparent.
pub use super::text_helpers::{
    extract_json_block, is_placeholder_literal, is_truncated_thinking_response,
    strip_reasoning_tags,
};

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
///
/// When `response_schema` is set, callers signal that the daemon
/// should run grammar-constrained generation against this JSON
/// Schema — an OpenAI-style `response_format: { type: "json_schema",
/// ... }` request. Used by Phase 1 to force the model into valid
/// JSON shape, eliminating the "missing comma / unclosed bracket"
/// failure mode observed on Gemma-31B for long structured outputs.
// `Eq` was dropped from the derive list when `temperature: Option<f32>`
// was added — `f32` doesn't implement `Eq` (NaN is not reflexive).
// `PartialEq` is sufficient for every test that compares ChatPrompts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatPrompt {
    pub system: String,
    pub user: String,
    /// JSON Schema for grammar-constrained generation. When `None`,
    /// the request runs without constraints (backwards-compatible).
    /// When `Some`, the chat client serialises this into the
    /// OpenAI-style `response_format.json_schema.schema` field; the
    /// server's adapter maps it to `CompletionRequest.structured_output`
    /// which `build_sampler` consumes via `LlamaSampler::llguidance`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<serde_json::Value>,
    /// Schema name for the OpenAI `response_format.json_schema.name`
    /// field. Only meaningful when `response_schema` is `Some`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema_name: Option<String>,
    /// Stable identifier of the pipeline phase that produced this
    /// prompt — `"phase1_seed"`, `"phase1_extract"`, `"phase3_name"`,
    /// `"phase5_tensions"`, `"phase7_configure"`, etc. Carried so the
    /// chat client can route the request to a phase-specific model
    /// when the operator has declared one (e.g. small/fast for bulk
    /// extraction, large/reasoning for synthesis). When the client has
    /// no per-phase override for this id (or this field is `None`),
    /// the client falls back to its default `chat_model`.
    ///
    /// The recipe-side compose functions are the source of truth for
    /// which phase id a prompt carries. The chat client side never
    /// invents one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    /// Output-token budget for this prompt. When set, the chat client
    /// forwards it as `InferenceRequirements.max_output_tokens` so the
    /// OICP scheduler can hard-gate against each candidate claim's
    /// `max_output` (per OICP-v0.3 §2.4). Used to route short-call
    /// phases (phase1b coverage, phase3 cluster naming, phase5
    /// positions, phase6 tensions) to a high-throughput batched
    /// FastShort claim and keep long-output Phase 1 on FastLong.
    /// `None` leaves the budget unconstrained — the client falls back
    /// to its model-default cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Sampling temperature (0.0–2.0). When set, the chat client
    /// forwards it as the request `temperature` field instead of the
    /// dispatcher / provider default. Phase composers attach this when
    /// the atlas operator has a per-phase override (e.g. `0.0` for
    /// classifier phases, `0.3` for interpretive Phase 8). `None`
    /// falls through to the provider's `default_temperature` and
    /// finally the dispatcher's hardcoded fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Reasoning / extended-thinking budget in tokens. Anthropic
    /// thinking models, OpenAI o1-class, and DeepSeek-reasoner consume
    /// this differently, but all interpret it as "spend up to N tokens
    /// in a hidden reasoning block before emitting the visible
    /// response". `Some(0)` disables thinking explicitly; `None`
    /// inherits the provider default. Per-phase overrides matter for
    /// Phase 1 (which benefits from reasoning when the article is
    /// dense or dialectical) versus the judge / classifier phases
    /// (which don't).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_tokens: Option<u32>,
}

impl ChatPrompt {
    pub fn new(system: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            user: user.into(),
            response_schema: None,
            response_schema_name: None,
            phase_id: None,
            max_output_tokens: None,
            temperature: None,
            thinking_tokens: None,
        }
    }

    /// Attach a JSON Schema for grammar-constrained generation.
    pub fn with_response_schema(
        mut self,
        name: impl Into<String>,
        schema: serde_json::Value,
    ) -> Self {
        self.response_schema_name = Some(name.into());
        self.response_schema = Some(schema);
        self
    }

    /// Tag this prompt with the pipeline phase that produced it. The
    /// chat client uses this to look up a phase-specific model in the
    /// operator's `chat_models` map and route the request there. See
    /// [`ChatPrompt::phase_id`] for the schema of the id strings.
    pub fn with_phase_id(mut self, phase_id: impl Into<String>) -> Self {
        self.phase_id = Some(phase_id.into());
        self
    }

    /// Cap the output-token budget for this prompt. The chat client
    /// forwards the value as `InferenceRequirements.max_output_tokens`
    /// so OICP scheduling can hard-gate (per v0.3 §2.4) against each
    /// candidate claim's `max_output`. Short-call phases set a small
    /// value (e.g. 512) to opt into the high-throughput FastShort
    /// claim; long-output phases either omit it or set it large.
    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }

    /// Override the sampling temperature for this prompt. See
    /// [`ChatPrompt::temperature`] for semantics.
    pub fn with_temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }

    /// Override the thinking-token budget for this prompt. See
    /// [`ChatPrompt::thinking_tokens`] for semantics.
    pub fn with_thinking_tokens(mut self, n: u32) -> Self {
        self.thinking_tokens = Some(n);
        self
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

// ── Phase 0: per-section classification (routed-Phase-1 prelude) ──
//
// The classifier reads each section's title, first ~500 words, and
// frontmatter tags, and assigns a `SectionType` that drives downstream
// Phase 1 dispatch. The classification is meta-shape, not content
// extraction — it answers "what kind of writing is this?" so the
// routed Phase 1 can pick a schema that fits the section's genre.
//
// Why it lives here: classification is a typed input/output the runner
// caches alongside Phase 1's `cache/questions.json`. The
// `SectionClassification` struct mirrors `Phase1ChapterResult` shape-
// wise (per-section, cacheable, schema-versioned).

/// Genre tag assigned to a section by the Phase 0 classifier.
///
/// The set is closed: a section that matches none well falls into
/// `Mixed` with `secondary_type` populated so the runner can fan out
/// to two Phase 1 schemas. Adding a new variant is a schema-version
/// bump on `SectionClassificationsFile` — all existing classified
/// caches stay readable via the `Unknown` fallback below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionType {
    /// Short stories, narrative fiction passages, novel chapters.
    /// The literary-atlas schema fits this case unchanged.
    Fiction,
    /// Long-form argumentative writing — econ / policy / social
    /// criticism. The current literary schema cramps these into a
    /// 10-cap Claims array; the routed Phase 1 will give them
    /// first-class `positions`, `mechanisms`, `oppositions`,
    /// `evidence_invocations`.
    ArgumentativeEssay,
    /// Literary, musical, film, or visual criticism — sections that
    /// name works and judge them. Routed Phase 1 gives them
    /// `works_discussed`, `formal_elements`, `evaluative_judgments`.
    Criticism,
    /// First-person daily journals, project journals, field notes.
    /// Author voice ≠ Person; people-encountered + decisions-made +
    /// observations + open-threads.
    Journal,
    /// Meeting transcripts / recaps. Attendees + agenda + decisions
    /// + action items.
    MeetingRecord,
    /// Zettel cards, definition notes, glossary entries. Dense
    /// concept + relationship payload, little narrative.
    Reference,
    /// Work-tracking notes — task lists, technical planning,
    /// status logs. Decisions + tasks + artifacts (incl. model
    /// names) + blockers.
    ProjectNote,
    /// Verse including prose poetry. Speaker ≠ Person; images,
    /// motifs, formal devices.
    Poetry,
    /// The section genuinely spans two genres (e.g., a journal
    /// entry that contains a long argumentative riff). The runner
    /// fans out to both `primary_type` and `secondary_type` Phase 1
    /// schemas and merges results.
    Mixed,
    /// Forward-compat fallback for deserialising classification
    /// caches written by future variants. Treated as `Mixed` with
    /// no secondary at dispatch time.
    #[serde(other)]
    Unknown,
}

impl SectionType {
    /// Stable short tag used in filenames, logs, prompt-asset paths.
    /// One change here ripples through everywhere — keep in sync
    /// with the `Deserialize` rename_all = "snake_case" attribute.
    pub fn tag(self) -> &'static str {
        match self {
            SectionType::Fiction => "fiction",
            SectionType::ArgumentativeEssay => "argumentative_essay",
            SectionType::Criticism => "criticism",
            SectionType::Journal => "journal",
            SectionType::MeetingRecord => "meeting_record",
            SectionType::Reference => "reference",
            SectionType::ProjectNote => "project_note",
            SectionType::Poetry => "poetry",
            SectionType::Mixed => "mixed",
            SectionType::Unknown => "unknown",
        }
    }

    /// All variants the classifier may emit. `Unknown` is excluded —
    /// it's a deserializer fallback, not a classifier output.
    pub const CLASSIFIER_OUTPUTS: &'static [SectionType] = &[
        SectionType::Fiction,
        SectionType::ArgumentativeEssay,
        SectionType::Criticism,
        SectionType::Journal,
        SectionType::MeetingRecord,
        SectionType::Reference,
        SectionType::ProjectNote,
        SectionType::Poetry,
        SectionType::Mixed,
    ];
}

// ── MECE classification vector (v2) ───────────────────────────
//
// The flat `SectionType` above is the v1 surface — pragmatic, but
// not MECE. `MeetingRecord` ⊂ `Reference`, `Criticism` ⊂
// `ArgumentativeEssay`, `Journal` collapses Narrative+Reflective,
// and `Mixed` is the taxonomy admitting it cannot carve cleanly.
//
// v2 replaces the single label with a vector over three orthogonal
// MECE axes (Discourse Mode, Epistemic Posture, Temporal Frame) plus
// an optional Audience axis. Atom shapes attach to axis VALUES, not
// labels, so routing becomes compositional: a section with
// `discourse_mode = {Argumentative @ 0.55, Narrative @ 0.45}` fans out
// to BOTH typed extensions above `ROUTING_THRESHOLD` (0.25) instead of
// collapsing into `Mixed` and getting dropped by the dispatcher.
//
// Back-compat: `SectionClassificationVector::legacy_section_type()`
// projects to the v1 `SectionType` enum for any caller that hasn't
// migrated. Conversely `SectionClassificationVector::from_legacy()`
// reads a v1 record as a degenerate vector (primary @ 1.0, no
// secondaries) so existing `cache/section_classifications.json` files
// stay readable across the bump.

/// Axis A — what is the section's language *doing*. Six MECE values.
/// Atom families produced per discourse mode:
/// - Narrative: Events, EntityStates, Relations, RelationStates, ParticipantArcs
/// - Argumentative: Positions, Mechanisms, Oppositions, EvidenceInvocations, Concessions
/// - Descriptive: Definitions, PropertyClaims, Relationships, Examples, Provenance
/// - Reflective: Interactions, Observations, OpenThreads, MoodShifts, Realisations
/// - Procedural: Tasks, Decisions, Artifacts, Dependencies, Blockers, StatusSignals
/// - Lyric: Images, Motifs, FormalDevices, VoiceShifts, TonalMovements
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscourseMode {
    Narrative,
    Argumentative,
    Descriptive,
    Reflective,
    Procedural,
    Lyric,
}

impl DiscourseMode {
    /// Stable short tag — filenames, logs, prompt-asset paths.
    pub fn tag(self) -> &'static str {
        match self {
            DiscourseMode::Narrative => "narrative",
            DiscourseMode::Argumentative => "argumentative",
            DiscourseMode::Descriptive => "descriptive",
            DiscourseMode::Reflective => "reflective",
            DiscourseMode::Procedural => "procedural",
            DiscourseMode::Lyric => "lyric",
        }
    }

    pub const ALL: &'static [DiscourseMode] = &[
        DiscourseMode::Narrative,
        DiscourseMode::Argumentative,
        DiscourseMode::Descriptive,
        DiscourseMode::Reflective,
        DiscourseMode::Procedural,
        DiscourseMode::Lyric,
    ];
}

/// Dispatcher fans out to every discourse mode whose weight ≥ this.
/// 0.25 chosen so a 0.55/0.45 hybrid fires both extensions but a
/// 0.85/0.15 single-mode-with-spoken-word-framing fires only the
/// primary. Tune at the call site if a corpus needs different
/// behaviour.
pub const DISCOURSE_ROUTING_THRESHOLD: f32 = 0.25;

/// Axis A weighted distribution. `primary` always has the largest
/// weight; `secondaries` is sorted by weight descending and capped
/// at 2 to bound dispatch fan-out at 3 modes per section.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiscourseModeDistribution {
    pub primary: DiscourseMode,
    pub primary_weight: f32,
    #[serde(default)]
    pub secondaries: Vec<(DiscourseMode, f32)>,
}

impl DiscourseModeDistribution {
    /// Single-mode-at-1.0 constructor — the shape v1 caches project
    /// into.
    pub fn solo(mode: DiscourseMode) -> Self {
        Self {
            primary: mode,
            primary_weight: 1.0,
            secondaries: Vec::new(),
        }
    }

    /// Sum of `primary_weight` + every secondary weight. Validator
    /// callers want this within `[0.99, 1.01]`; classifier output
    /// outside that range is suspect.
    pub fn weight_sum(&self) -> f32 {
        self.primary_weight + self.secondaries.iter().map(|(_, w)| *w).sum::<f32>()
    }

    /// `true` when the weights sum to 1.0 within ±0.01. Used by the
    /// classifier parser to reject malformed model output.
    pub fn weights_sum_to_one(&self) -> bool {
        let s = self.weight_sum();
        (0.99..=1.01).contains(&s)
    }

    /// Modes the dispatcher should fan out to. Always includes the
    /// primary; secondaries are included only when their weight is
    /// ≥ `threshold`. Returned in (mode, weight) pairs preserving
    /// the input order.
    pub fn active_modes(&self, threshold: f32) -> Vec<(DiscourseMode, f32)> {
        let mut out = Vec::with_capacity(1 + self.secondaries.len());
        out.push((self.primary, self.primary_weight));
        for &(mode, weight) in &self.secondaries {
            if weight >= threshold {
                out.push((mode, weight));
            }
        }
        out
    }
}

/// Axis B — section's relationship to actual-world truth. Four MECE
/// values. Modulates downstream Claim atoms via `apply_epistemic_modulator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpistemicPosture {
    Factual,
    Normative,
    Fictional,
    Hypothetical,
}

impl EpistemicPosture {
    pub fn tag(self) -> &'static str {
        match self {
            EpistemicPosture::Factual => "factual",
            EpistemicPosture::Normative => "normative",
            EpistemicPosture::Fictional => "fictional",
            EpistemicPosture::Hypothetical => "hypothetical",
        }
    }
}

/// Axis C — temporal anchor of the content. Three MECE values.
/// Modulates Event/Task atoms via `apply_temporal_modulator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalFrame {
    Episodic,
    Atemporal,
    Prospective,
}

impl TemporalFrame {
    pub fn tag(self) -> &'static str {
        match self {
            TemporalFrame::Episodic => "episodic",
            TemporalFrame::Atemporal => "atemporal",
            TemporalFrame::Prospective => "prospective",
        }
    }
}

/// Optional Axis D — who the section is *for*. Not load-bearing for
/// atom shape; affects rendering downstream (briefing tone, redaction
/// rules). Three MECE values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudienceRelation {
    PrivateFirstPerson,
    SpecificRecipient,
    PublicImpersonal,
}

/// The v2 classification record. Replaces `SectionClassification`'s
/// flat enum with a vector over three+ MECE axes. The dispatcher
/// reads `discourse_mode.active_modes(DISCOURSE_ROUTING_THRESHOLD)`
/// and fires one typed extension per active mode; the post-extraction
/// modulators apply Epistemic + Temporal tags to the extracted atoms.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SectionClassificationVector {
    pub section_id: String,
    pub discourse_mode: DiscourseModeDistribution,
    pub epistemic_posture: EpistemicPosture,
    pub temporal_frame: TemporalFrame,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience_relation: Option<AudienceRelation>,
    /// SHA-256 short hash (16 hex chars) of section text at classify
    /// time. Same role as v1's `SectionClassification::content_hash`.
    pub content_hash: String,
    pub classified_at_unix: u64,
    /// Short rationale the operator can audit. Routed to telemetry;
    /// not load-bearing for downstream phases.
    #[serde(default)]
    pub reasoning: String,
}

impl SectionClassificationVector {
    /// Project the vector back to a v1 `SectionType` for any caller
    /// that hasn't migrated. The mapping is necessarily lossy —
    /// secondaries and the Epistemic/Temporal axes collapse into the
    /// flat label.
    pub fn legacy_section_type(&self) -> SectionType {
        let primary = self.discourse_mode.primary;
        match (primary, self.epistemic_posture) {
            (DiscourseMode::Narrative, EpistemicPosture::Fictional) => SectionType::Fiction,
            (DiscourseMode::Narrative, _) => SectionType::Fiction,
            (DiscourseMode::Argumentative, _) => SectionType::ArgumentativeEssay,
            (DiscourseMode::Reflective, _) => SectionType::Journal,
            (DiscourseMode::Procedural, _) => SectionType::ProjectNote,
            (DiscourseMode::Lyric, _) => SectionType::Poetry,
            (DiscourseMode::Descriptive, _) => SectionType::Reference,
        }
    }

    /// Inverse — read a v1 `SectionClassification` as a degenerate
    /// vector (primary @ 1.0, no secondaries; epistemic + temporal
    /// guessed from the v1 label). Used by the cache-migration path
    /// in `SectionClassificationsFile::read_with_migration`.
    pub fn from_legacy(c: &SectionClassification) -> Self {
        let (mode, posture, frame) = match c.primary_type {
            SectionType::Fiction => (
                DiscourseMode::Narrative,
                EpistemicPosture::Fictional,
                TemporalFrame::Episodic,
            ),
            SectionType::ArgumentativeEssay => (
                DiscourseMode::Argumentative,
                EpistemicPosture::Normative,
                TemporalFrame::Atemporal,
            ),
            SectionType::Criticism => (
                DiscourseMode::Argumentative,
                EpistemicPosture::Normative,
                TemporalFrame::Atemporal,
            ),
            SectionType::Journal => (
                DiscourseMode::Reflective,
                EpistemicPosture::Factual,
                TemporalFrame::Episodic,
            ),
            SectionType::MeetingRecord => (
                DiscourseMode::Descriptive,
                EpistemicPosture::Factual,
                TemporalFrame::Episodic,
            ),
            SectionType::Reference => (
                DiscourseMode::Descriptive,
                EpistemicPosture::Factual,
                TemporalFrame::Atemporal,
            ),
            SectionType::ProjectNote => (
                DiscourseMode::Procedural,
                EpistemicPosture::Factual,
                TemporalFrame::Prospective,
            ),
            SectionType::Poetry => (
                DiscourseMode::Lyric,
                EpistemicPosture::Fictional,
                TemporalFrame::Atemporal,
            ),
            SectionType::Mixed | SectionType::Unknown => (
                DiscourseMode::Descriptive,
                EpistemicPosture::Factual,
                TemporalFrame::Atemporal,
            ),
        };
        let mut dist = DiscourseModeDistribution::solo(mode);
        // If the legacy record carried a secondary_type, project it as
        // a 0.50/0.50 split. Otherwise stay degenerate at 1.0.
        if let Some(secondary) = c.secondary_type {
            let secondary_mode = match secondary {
                SectionType::Fiction => DiscourseMode::Narrative,
                SectionType::ArgumentativeEssay | SectionType::Criticism => {
                    DiscourseMode::Argumentative
                }
                SectionType::Journal => DiscourseMode::Reflective,
                SectionType::MeetingRecord | SectionType::Reference => DiscourseMode::Descriptive,
                SectionType::ProjectNote => DiscourseMode::Procedural,
                SectionType::Poetry => DiscourseMode::Lyric,
                SectionType::Mixed | SectionType::Unknown => DiscourseMode::Descriptive,
            };
            if secondary_mode != mode {
                dist.primary_weight = 0.5;
                dist.secondaries.push((secondary_mode, 0.5));
            }
        }
        Self {
            section_id: c.section_id.clone(),
            discourse_mode: dist,
            epistemic_posture: posture,
            temporal_frame: frame,
            audience_relation: None,
            content_hash: c.content_hash.clone(),
            classified_at_unix: c.classified_at_unix,
            reasoning: c.reasoning.clone(),
        }
    }
}

/// Result of the Phase 0 classifier for one section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionClassification {
    pub section_id: String,
    pub primary_type: SectionType,
    /// Model self-reported certainty, `0.0..=1.0`. Low confidence
    /// (< 0.6) is a signal for the runner to also dispatch the
    /// `secondary_type` Phase 1 schema as a safety net. The raw
    /// number flows to telemetry so we can spot a classifier that's
    /// systematically over-confident on a particular genre.
    pub confidence: f32,
    /// Populated when `primary_type == Mixed` OR the model surfaced
    /// a credible second-best fit. The runner fans out to both
    /// primary + secondary Phase 1 schemas and merges atom sets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_type: Option<SectionType>,
    /// Short rationale (1-2 sentences) explaining the classifier's
    /// choice. Routed to telemetry + visible in `enrich classify`
    /// CLI output. Not load-bearing for downstream phases.
    #[serde(default)]
    pub reasoning: String,
    /// SHA-256 short hash (16 hex chars) of the section text seen
    /// at classify time. Used by the runner to invalidate the
    /// cached classification when the section content changes —
    /// without this, a stale classification persists across edits
    /// to a chapter and the routed Phase 1 keeps dispatching the
    /// wrong schema.
    pub content_hash: String,
    /// Unix seconds when the classification was written. Surfaced
    /// in the CLI so an operator can see how stale the cache is.
    pub classified_at_unix: u64,
}

/// On-disk shape of `cache/section_classifications.json`.
/// Schema-version-stamped per the existing cache convention.
///
/// **v2 (current).** `classifications` carries
/// `SectionClassificationVector` records — per-section MECE
/// classification across Discourse Mode / Epistemic Posture /
/// Temporal Frame axes.
///
/// **v1 (back-compat read).** Old `cache/section_classifications.json`
/// files carry the flat `SectionType` records under the same field
/// name. They deserialise via [`SectionClassificationsFile::from_json_with_migration`],
/// which peeks at `schema_version`, parses v1 into a side struct,
/// and projects each record into a degenerate vector (primary
/// discourse mode @ 1.0, no secondaries) before returning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionClassificationsFile {
    pub schema_version: u32,
    /// Producer-pipeline id (`obsidian_atlas`, …) so a cache produced
    /// by one pipeline doesn't get silently consumed by another with
    /// different routing rules.
    pub pipeline_id: String,
    /// v2 axis vectors.
    #[serde(default)]
    pub classifications: Vec<SectionClassificationVector>,
    pub written_at: chrono::DateTime<chrono::Utc>,
}

/// v1 on-disk shape, for migration reads only. Same JSON layout as
/// the pre-v2 `SectionClassificationsFile`. The migration helper
/// matches on `schema_version` and routes to this struct when it
/// reads `1`, then projects each record into a v2 vector. The
/// `schema_version` field is read by serde but otherwise unused
/// — the migration helper pre-routes on it via a `Value` probe.
#[derive(Debug, Clone, Deserialize)]
struct SectionClassificationsFileV1 {
    #[allow(dead_code)]
    schema_version: u32,
    pipeline_id: String,
    #[serde(default)]
    classifications: Vec<SectionClassification>,
    written_at: chrono::DateTime<chrono::Utc>,
}

impl SectionClassificationsFile {
    /// v2: classifications carry `SectionClassificationVector` over
    /// MECE axes. v1 files (flat `SectionType` records) are migrated
    /// transparently by `from_json_with_migration()`.
    pub const SCHEMA_VERSION: u32 = 2;

    /// Read + transparently migrate a classifications cache. Returns
    /// the file with `classifications` populated regardless of the
    /// on-disk version:
    ///
    /// - **v2 file** → parsed directly.
    /// - **v1 file** → parsed into [`SectionClassificationsFileV1`]
    ///   and each record projected via
    ///   [`SectionClassificationVector::from_legacy`] into a
    ///   degenerate vector.
    ///
    /// On migration, `schema_version` is bumped to the current
    /// `SCHEMA_VERSION` so a subsequent save writes the v2 shape.
    pub fn from_json_with_migration(raw: &str) -> Result<Self, serde_json::Error> {
        let probe: serde_json::Value = serde_json::from_str(raw)?;
        let version = probe
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        if version >= Self::SCHEMA_VERSION {
            serde_json::from_value(probe)
        } else {
            let v1: SectionClassificationsFileV1 = serde_json::from_value(probe)?;
            let classifications: Vec<SectionClassificationVector> = v1
                .classifications
                .iter()
                .map(SectionClassificationVector::from_legacy)
                .collect();
            Ok(SectionClassificationsFile {
                schema_version: Self::SCHEMA_VERSION,
                pipeline_id: v1.pipeline_id,
                classifications,
                written_at: v1.written_at,
            })
        }
    }
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

/// Structured classification of a phase failure. Widened from the
/// original Phase-1-only enum to cover every phase's failure modes —
/// the `sovereign enrich errors` aggregator groups by this kind
/// across phases so an operator sees one bucket per root cause
/// instead of one row per incident. A failure that doesn't fit here
/// yet uses `Other` and is flagged in the aggregator as
/// "unclassified", which is itself a signal that a new kind is
/// warranted.
///
/// Grouped by origin:
///
/// **LLM-driven extraction (Phase 1, 3-naming, 6, 8)** — share a
/// common failure vocabulary because the same underlying model
/// pattern (reasoning → JSON) fails in the same ways across phases:
/// - `ThinkTruncated` → retry with the terse prompt variant and/or a
///   larger `max_output_tokens` budget.
/// - `ParseDrift` → JSON was malformed; a plain retry often recovers.
/// - `ChatError` → transport-level failure; retry.
/// - `EmptyExtraction` → well-formed envelope, zero atoms; prompt
///   needs attention.
/// - `Skipped` → runner declined to dispatch (e.g. body too short).
///
/// **Deterministic resolution (Phase 3a/3b)** — named drops that
/// were silent before Landing 3.A and are now surfaced so the
/// operator can see how much evidence the resolver is losing:
/// - `UnresolvedEntityName` — a sketch references a name that the
///   fuzzy resolver couldn't snap to any Entity atom. The sketch is
///   dropped; Phase 1's entity-extraction pass likely missed the
///   name or the seed list diverged.
/// - `EntityMergeAmbiguous` — the fuzzy resolver found ≥ 2
///   candidates and refused to guess. Remediate by tightening the
///   seed list or by promoting this to LLM-driven disambiguation.
/// - `UnresolvedRelationParticipant` — a Relation sketch's
///   participant string didn't resolve to an Entity atom. The
///   participant drops out of the relation's participant_ids list.
/// - `UnresolvedClaimAttribution` — a Claim sketch's `attributed_to`
///   string didn't match any Entity atom; claim keeps content but
///   loses attribution.
///
/// **Clustering / naming (Phase 2, Phase 3)** — structural signals:
/// - `NoClusterableItems` — a facet had 0 sketches to cluster.
///   Silent before; now named so the operator sees which facet the
///   corpus isn't exercising.
/// - `ClusterNamingFailed` — per-cluster LLM naming call returned
///   but couldn't be parsed. Cluster keeps its id but loses its
///   human-readable label.
///
/// `Other` is the escape hatch; if a kind appears in the aggregator
/// frequently enough, promote it to its own variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseFailureKind {
    // LLM-driven extraction failures
    ThinkTruncated,
    ParseDrift,
    ChatError,
    DeadlineExceeded,
    EmptyExtraction,
    Skipped,

    // Phase 3a/3b resolution drops (previously silent)
    UnresolvedEntityName,
    EntityMergeAmbiguous,
    UnresolvedRelationParticipant,
    UnresolvedClaimAttribution,

    // Clustering / naming
    NoClusterableItems,
    ClusterNamingFailed,

    Other,
}

impl PhaseFailureKind {
    /// One-line remediation hint the aggregator prints next to a
    /// group of failures. Concrete and actionable: either names the
    /// exact retry command, points at a prompt/schema fix, or
    /// explains why the drop is structurally correct (so the
    /// operator doesn't chase it).
    ///
    /// Keep the text imperative and under ~140 chars — the CLI
    /// prints it indented after the group header, and the desktop
    /// error panel shows it inline with the failure count.
    pub const fn remediation_hint(&self) -> &'static str {
        match self {
            Self::ThinkTruncated => {
                "Retry with `sovereign enrich extract <corpus> --retry-failed --terse` — the terse variant doubles max_output_tokens."
            }
            Self::ParseDrift => {
                "Retry with `sovereign enrich extract <corpus> --retry-failed` — a plain retry recovers most parse-drift cases (temperature noise)."
            }
            Self::ChatError => {
                "Transport-level error. Verify the daemon at localhost:9741 is up (`sovereign doctor`), then retry with `--retry-failed`."
            }
            Self::DeadlineExceeded => {
                "Daemon inference deadline hit (slow chat slot vs. token budget). Retry with `--retry-failed --terse` — the terse variant uses a tighter prompt that fits in the deadline."
            }
            Self::EmptyExtraction => {
                "Model returned an envelope with no atoms. Inspect the prompt and exemplars for the failing phase; a schema-echo suggests the few-shot examples look like the target shape."
            }
            Self::Skipped => {
                "Section was too short or heading-only. Not a bug — either merge the section into its neighbor in the manifest or leave as-is."
            }
            Self::UnresolvedEntityName => {
                "Fuzzy resolver couldn't match the name to any Entity atom. Either add the name to seed list (`enrich seed <corpus> --force`) or accept the drop if the reference is truly fleeting."
            }
            Self::EntityMergeAmbiguous => {
                "Fuzzy resolver found ≥ 2 candidates and refused to guess. Tighten the seed list so the canonical name is unambiguous, or tune transliterate_cyrillic + Levenshtein thresholds."
            }
            Self::UnresolvedRelationParticipant => {
                "Relation participant string didn't match any Entity. Usually a Phase 1 entity-extraction miss — check whether the entity was introduced anywhere in the corpus."
            }
            Self::UnresolvedClaimAttribution => {
                "Claim `attributed_to` didn't match any Entity. The claim keeps its content; only attribution is lost. Same fix path as UnresolvedEntityName."
            }
            Self::NoClusterableItems => {
                "Facet had 0 sketches. Structural — not a bug. If unexpected, check whether the corpus genuinely exercises this facet (e.g. philosophical essays vs. narrative fiction)."
            }
            Self::ClusterNamingFailed => {
                "Per-cluster LLM naming parse failure. Re-run `sovereign enrich name-atlas-clusters <corpus>` — naming is idempotent and cluster ids are stable."
            }
            Self::Other => {
                "Unclassified failure. Check the `reason` + `raw_response_head` fields in the run file and consider promoting this to a named PhaseFailureKind variant."
            }
        }
    }
}

/// Subject of a failure — what was being worked on when things went
/// wrong. Free-form string with a prefix convention so CLI filters
/// and aggregators can group sensibly across phases:
///
/// | Prefix | Example | Produced by |
/// |---|---|---|
/// | `chapter:` | `chapter:sec_0017` | Phase 1 |
/// | `cluster:` | `cluster:claim:cluster-04` | Phase 2/3 |
/// | `sketch:` | `sketch:entity_state:sec_0003#7` | Resolution drops |
/// | `atom:` | `atom:claim-0042` | Phase 6/8 |
/// | `pair:` | `pair:claim-0017:state-0021` | Phase 6 tensions |
/// | `facet:` | `facet:entity_state` | Phase 2 clustering |
/// | `corpus:` | `corpus:dopesick_jesus` | Corpus-wide |
///
/// A string (not an enum) because new subject kinds show up
/// naturally as phases are added; the prefix convention keeps the
/// aggregator's grouping stable without schema churn.
pub type FailureSubject = String;

/// Universal phase-failure record. Every phase output carries a
/// `Vec<PhaseFailure>` (even when empty), and the
/// `sovereign enrich errors` aggregator walks every phase's cache to
/// group these by `(phase, kind)` and print remediation.
///
/// The shape is deliberately close to the legacy `Phase1Failure` so
/// the classifier that produces Phase 1 failures can migrate with a
/// one-line shim. The critical addition is the `phase` field: it's
/// what lets the aggregator tell a cluster-naming ParseDrift from a
/// Phase-1 ParseDrift without file-path inference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseFailure {
    /// Which phase emitted this failure. Enables cross-phase
    /// aggregation without reparsing the enclosing file path.
    pub phase: PipelinePhase,
    /// What the phase was working on — see `FailureSubject` for
    /// the prefix convention.
    pub subject: FailureSubject,
    pub kind: PhaseFailureKind,
    pub reason: String,
    /// First ~1 KiB of the raw model response when the failure was
    /// LLM-driven. `None` for deterministic failures (clustering,
    /// resolution drops) — those capture their evidence in `reason`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_response_head: Option<String>,
}

impl PhaseFailure {
    /// Helper for Phase 1 compat — builds a failure with the
    /// `chapter:` subject prefix from a bare chapter id.
    pub fn for_chapter(
        phase: PipelinePhase,
        chapter_id: impl Into<String>,
        kind: PhaseFailureKind,
        reason: impl Into<String>,
        raw_response_head: Option<String>,
    ) -> Self {
        Self {
            phase,
            subject: format!("chapter:{}", chapter_id.into()),
            kind,
            reason: reason.into(),
            raw_response_head,
        }
    }
}

/// Chapter-level failure record. Carried through `Phase1RunResult` for
/// live reporting and serialized into `Phase1Output.failures` so the
/// run file preserves a truncated head of whatever the model actually
/// said. Without this the only signal on a parse failure is the
/// exception text, which makes prompt debugging a guessing game.
///
/// Kept as the Phase-1-specific shape for backward compatibility with
/// cached `questions-*.json` files that predate the unified
/// `PhaseFailure` type. The `sovereign enrich errors` aggregator
/// adapts these into `PhaseFailure` via `to_phase_failure()`.
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

impl Phase1Failure {
    /// Adapt a legacy Phase-1 failure record into the unified
    /// `PhaseFailure` shape. Used by the aggregator to normalize
    /// cached files without re-writing them.
    pub fn to_phase_failure(&self) -> PhaseFailure {
        PhaseFailure::for_chapter(
            PipelinePhase::Questions,
            self.chapter_id.clone(),
            self.failure_kind,
            self.reason.clone(),
            self.raw_response_head.clone(),
        )
    }
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
    /// Structured failures surfaced by this phase. Empty for the
    /// common case. `#[serde(default)]` means cached files written
    /// before Landing 3.A deserialise cleanly — old runs simply
    /// expose no failures, which is indistinguishable from "ran
    /// clean" in the aggregator.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<PhaseFailure>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<PhaseFailure>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<PhaseFailure>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<PhaseFailure>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<PhaseFailure>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<PhaseFailure>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<PhaseFailure>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<PhaseFailure>,
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
            failures: Vec::new(),
            written_at: "t".into(),
        };
        let claims: Vec<&AtlasCluster> = output.clusters_by_facet(Facet::Claim).collect();
        assert_eq!(claims.len(), 2);
        let questions: Vec<&AtlasCluster> = output.clusters_by_facet(Facet::Question).collect();
        assert_eq!(questions.len(), 1);
        let states: Vec<&AtlasCluster> = output.clusters_by_facet(Facet::EntityState).collect();
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
            failures: Vec::new(),
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
        let ids: std::collections::HashSet<_> = PipelinePhase::ALL.iter().map(|p| p.id()).collect();
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
        assert_eq!(
            "extract".parse::<PipelinePhase>().unwrap(),
            PipelinePhase::Questions
        );
        assert_eq!(
            "cluster-questions".parse::<PipelinePhase>().unwrap(),
            PipelinePhase::QuestionClusters
        );
        assert_eq!(
            "positions".parse::<PipelinePhase>().unwrap(),
            PipelinePhase::Positions
        );
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
        assert!(is_truncated_thinking_response(
            "<think>long reasoning without closure"
        ));
        // A `{` inside the reasoning trace is a red herring; the
        // answer we care about is after </think>, which is missing.
        assert!(is_truncated_thinking_response(
            "<think>drafting {\"position_text\": …} but never closing"
        ));
        // Closed thinking tag + JSON → not truncated.
        assert!(!is_truncated_thinking_response(
            "<think>done</think>{\"q\":1}"
        ));
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
        assert_eq!(
            parsed.questions_by_chapter[0].setting.as_deref(),
            Some("Moscow drawing-room, 1870s")
        );
        assert_eq!(
            parsed.questions_by_chapter[0].plot.as_deref(),
            Some("A letter arrives and is read aloud.")
        );
        assert_eq!(parsed.failures.len(), 1);
        assert_eq!(
            parsed.failures[0].raw_response_head.as_deref(),
            Some("I cannot help with that.")
        );
    }

    #[test]
    fn chat_prompt_roundtrips_max_output_tokens() {
        // Regression marker for the OICP-routing wire-up: short-call
        // composers (phase1b/3/5/6) attach max_output_tokens so the
        // chat client can opt into FastShort/FastLong claim selection.
        // If this field is dropped from the struct or its serde
        // attributes change, the daemon's hard-gate routing falls
        // back to model-name routing and the speedup disappears.
        let p = ChatPrompt::new("sys", "user")
            .with_phase_id("phase1b_entity")
            .with_max_output_tokens(512);
        assert_eq!(p.max_output_tokens, Some(512));

        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"max_output_tokens\":512"), "got {json}");

        let back: ChatPrompt = serde_json::from_str(&json).unwrap();
        assert_eq!(back.max_output_tokens, Some(512));
        assert_eq!(back.phase_id.as_deref(), Some("phase1b_entity"));

        // Absent field round-trips as None, not 0.
        let no_cap = ChatPrompt::new("s", "u");
        let no_cap_json = serde_json::to_string(&no_cap).unwrap();
        assert!(!no_cap_json.contains("max_output_tokens"));
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
        assert!(
            !json.contains("failures"),
            "empty failures should be skipped; got {json}"
        );
    }
}
