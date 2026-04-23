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
    Questions,
    QuestionClusters,
    Concerns,
    ChunkClusters,
    Positions,
    Tensions,
    Gaps,
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
    ];

    pub const fn ordinal(&self) -> u8 {
        match self {
            Self::Ingest => 0,
            Self::Questions => 1,
            Self::QuestionClusters => 2,
            Self::Concerns => 3,
            Self::ChunkClusters => 4,
            Self::Positions => 5,
            Self::Tensions => 6,
            Self::Gaps => 7,
        }
    }

    /// Stable short id used in file names (`phase<N>.json`) and CLI flags.
    pub const fn id(&self) -> &'static str {
        match self {
            Self::Ingest => "ingest",
            Self::Questions => "questions",
            Self::QuestionClusters => "question-clusters",
            Self::Concerns => "concerns",
            Self::ChunkClusters => "chunk-clusters",
            Self::Positions => "positions",
            Self::Tensions => "tensions",
            Self::Gaps => "gaps",
        }
    }

    pub const fn display_name(&self) -> &'static str {
        match self {
            Self::Ingest => "Ingest",
            Self::Questions => "Extract per-chapter questions",
            Self::QuestionClusters => "Cluster questions",
            Self::Concerns => "Name canonical concerns",
            Self::ChunkClusters => "Cluster chunks",
            Self::Positions => "Extract grounded positions",
            Self::Tensions => "Detect pairwise tensions",
            Self::Gaps => "Detect gaps",
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
            Self::Questions => &[Self::Ingest],
            Self::QuestionClusters => &[Self::Questions],
            Self::Concerns => &[Self::QuestionClusters],
            Self::ChunkClusters => &[Self::Ingest],
            Self::Positions => &[Self::Concerns, Self::ChunkClusters],
            Self::Tensions => &[Self::Positions],
            Self::Gaps => &[Self::Concerns, Self::Positions, Self::Tensions],
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
            }],
            failures: vec![Phase1Failure {
                chapter_id: "ch_0007".into(),
                reason: "parse error: missing questions".into(),
                raw_response_head: Some("I cannot help with that.".into()),
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
