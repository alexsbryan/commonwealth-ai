// SPDX-License-Identifier: AGPL-3.0-or-later
//! Referential atlas pipeline — third instance of the v2 atlas
//! `Pipeline` trait, alongside `literary_atlas` and `philosophy_atlas`.
//!
//! Targets the **class of referential corpora** — encyclopedias,
//! wikis, reference works — where a section describes entities,
//! events, and concepts in editorial third-person rather than
//! advancing an authorial argument. The same eight-phase machinery
//! applies; only the prompt assets at
//! `referential_atlas_prompts/*.md` are domain-tuned.
//!
//! The class boundary is "what does this section describe, what
//! does it claim, what would a reader come here to learn?" — the
//! Phase-1 atlas extraction shape is exactly that question.
//! Wikipedia, SEP, journal articles, API docs, manuals all fit.
//!
//! Wraps `LiteraryAtlasPipeline` as `inner` so every phase that
//! doesn't speak referential-specific language (parsers, schemas,
//! atom rendering, clustering tuning) delegates unchanged.

use super::super::atlas::{EntitySketch, SectionExtraction, SeedEntities};
use super::super::exemplar_bank::Exemplar;
use super::super::trait_def::Pipeline;
use super::super::types::*;
use super::literary::prepare_phase_json;
use super::literary_atlas::{
    parse_phase1b_coverage_response, phase1_section_extraction_schema,
    render_generic_phase3_exemplar, render_phase1_user_body, render_phase1b_user_body,
    LiteraryAtlasPipeline,
};
use crate::enrichment::domain::ClusteringConfig;
use crate::error::Result;

// ── Referential-specific prompt assets ───────────────────────

static PHASE1_ATLAS_SYSTEM: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "referential_atlas/phase1_system.md",
            include_str!("referential_atlas_prompts/phase1_system.md"),
        )
    });

static PHASE1_ATLAS_SYSTEM_TERSE: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "referential_atlas/phase1_system_terse.md",
            include_str!("referential_atlas_prompts/phase1_system_terse.md"),
        )
    });

static PHASE1A_SEED_SYSTEM: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "referential_atlas/phase1a_seed_system.md",
            include_str!("referential_atlas_prompts/phase1a_seed_system.md"),
        )
    });

static PHASE1B_ENTITY_COVERAGE: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "referential_atlas/phase1b_entity_coverage.md",
            include_str!("referential_atlas_prompts/phase1b_entity_coverage.md"),
        )
    });

static PHASE1B_CONCEPT_COVERAGE: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "referential_atlas/phase1b_concept_coverage.md",
            include_str!("referential_atlas_prompts/phase1b_concept_coverage.md"),
        )
    });

static PHASE3_QUESTION_NAMING: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "referential_atlas/phase3_question_naming.md",
            include_str!("referential_atlas_prompts/phase3_question_naming.md"),
        )
    });
static PHASE3_CLAIM_NAMING: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "referential_atlas/phase3_claim_naming.md",
            include_str!("referential_atlas_prompts/phase3_claim_naming.md"),
        )
    });
static PHASE3_ENTITY_STATE_NAMING: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "referential_atlas/phase3_entity_state_trajectory_naming.md",
            include_str!("referential_atlas_prompts/phase3_entity_state_trajectory_naming.md"),
        )
    });
static PHASE3_RELATION_STATE_NAMING: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "referential_atlas/phase3_relation_state_trajectory_naming.md",
            include_str!("referential_atlas_prompts/phase3_relation_state_trajectory_naming.md"),
        )
    });
static PHASE3_EVENT_NAMING: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "referential_atlas/phase3_event_thread_naming.md",
            include_str!("referential_atlas_prompts/phase3_event_thread_naming.md"),
        )
    });

/// Pipeline id exposed by the registry.
pub const PIPELINE_ID: &str = "referential_atlas";

/// Atlas pipeline tuned for referential corpora (encyclopedias,
/// wikis, reference works). Same atom schema as the literary and
/// philosophy variants; tuned prompts at every phase that speaks
/// domain language.
pub struct ReferentialAtlasPipeline {
    inner: LiteraryAtlasPipeline,
}

impl ReferentialAtlasPipeline {
    pub fn new() -> Self {
        Self {
            inner: LiteraryAtlasPipeline::new(),
        }
    }
}

impl Default for ReferentialAtlasPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipeline for ReferentialAtlasPipeline {
    fn id(&self) -> &'static str {
        PIPELINE_ID
    }

    fn name(&self) -> &'static str {
        "Referential — atlas atom graph"
    }

    fn vocabulary(&self) -> &Vocabulary {
        self.inner.vocabulary()
    }

    fn declaration(&self) -> crate::enrichment::ontology::OntologyPolicies {
        super::declaration::REFERENTIAL.clone()
    }

    // ── System preambles ──────────────────────────────────────

    fn phase1_system(&self) -> &'static str {
        *PHASE1_ATLAS_SYSTEM
    }

    fn phase3_system(&self) -> &'static str {
        self.inner.phase3_system()
    }

    fn phase5_system(&self) -> &'static str {
        self.inner.phase5_system()
    }

    fn phase6_system(&self) -> &'static str {
        self.inner.phase6_system()
    }

    fn phase7_system(&self) -> &'static str {
        self.inner.phase7_system()
    }

    // ── Phase 1 — atlas extraction ────────────────────────────

    fn compose_phase1(&self, chapter: &ChapterInput, exemplars: &[&Exemplar]) -> ChatPrompt {
        let user = render_phase1_user_body(
            chapter, exemplars, /*include_exemplars=*/ true, /*seed=*/ None,
        );
        ChatPrompt::new(self.phase1_system(), user)
            .with_response_schema(
                "phase1_section_extraction",
                phase1_section_extraction_schema(),
            )
            .with_phase_id("phase1")
    }

    fn compose_phase1_terse(&self, chapter: &ChapterInput) -> Option<ChatPrompt> {
        let user = render_phase1_user_body(
            chapter,
            /*exemplars=*/ &[],
            /*include_exemplars=*/ false,
            /*seed=*/ None,
        );
        Some(
            ChatPrompt::new(*PHASE1_ATLAS_SYSTEM_TERSE, user)
                .with_response_schema(
                    "phase1_section_extraction",
                    phase1_section_extraction_schema(),
                )
                .with_phase_id("phase1_terse"),
        )
    }

    // ── Phase 1b coverage check ────────────────────────────────

    fn compose_phase1b_entity_coverage(
        &self,
        chapter: &ChapterInput,
        existing: &SectionExtraction,
    ) -> Option<ChatPrompt> {
        let user = render_phase1b_user_body(chapter, existing);
        Some(
            ChatPrompt::new(*PHASE1B_ENTITY_COVERAGE, user)
                .with_phase_id("phase1b_entity")
                .with_max_output_tokens(512),
        )
    }

    fn compose_phase1b_concept_coverage(
        &self,
        chapter: &ChapterInput,
        existing: &SectionExtraction,
    ) -> Option<ChatPrompt> {
        let user = render_phase1b_user_body(chapter, existing);
        Some(
            ChatPrompt::new(*PHASE1B_CONCEPT_COVERAGE, user)
                .with_phase_id("phase1b_concept")
                .with_max_output_tokens(512),
        )
    }

    fn parse_phase1b_coverage(&self, response: &str) -> Result<Vec<EntitySketch>> {
        parse_phase1b_coverage_response(response)
    }

    fn compose_phase1_with_seed(
        &self,
        chapter: &ChapterInput,
        exemplars: &[&Exemplar],
        seed: Option<&SeedEntities>,
    ) -> ChatPrompt {
        let user =
            render_phase1_user_body(chapter, exemplars, /*include_exemplars=*/ true, seed);
        ChatPrompt::new(self.phase1_system(), user)
            .with_response_schema(
                "phase1_section_extraction",
                phase1_section_extraction_schema(),
            )
            .with_phase_id("phase1")
    }

    fn parse_phase1(&self, response: &str) -> Result<Phase1ChapterResult> {
        self.inner.parse_phase1(response)
    }

    // ── Stage 1a — seed extraction ─────────────────────────────
    //
    // Phase 1a's design assumes a single-text corpus where the first
    // section introduces canonical entities the whole work refers
    // back to. Referential corpora are multi-document — there is no
    // cross-document seed; each article's lead section serves as
    // its own seed if anything. Declare `SeedStrategy::None` so the
    // runner skips Phase 1a entirely. The *PHASE1A_SEED_SYSTEM asset
    // stays in the tree as documentation of the per-article seed
    // shape, but is no longer wired in.

    // ── Phase 3 — v1 delegate + atlas facet override ──────────

    fn compose_phase3(
        &self,
        cluster: &QuestionCluster,
        chapter_excerpts: &[&ChapterInput],
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        self.inner
            .compose_phase3(cluster, chapter_excerpts, exemplars)
    }

    fn parse_phase3(&self, response: &str) -> Result<Phase3ParseResult> {
        self.inner.parse_phase3(response)
    }

    fn compose_phase3_facet(
        &self,
        cluster: &AtlasCluster,
        facet: Facet,
        excerpts: &[SketchExcerpt],
        exemplars: &[&Exemplar],
    ) -> Option<ChatPrompt> {
        let system = match facet {
            Facet::Question => *PHASE3_QUESTION_NAMING,
            Facet::Claim => *PHASE3_CLAIM_NAMING,
            Facet::EntityState => *PHASE3_ENTITY_STATE_NAMING,
            Facet::RelationState => *PHASE3_RELATION_STATE_NAMING,
            Facet::Event => *PHASE3_EVENT_NAMING,
        };
        let mut user = String::new();

        if !exemplars.is_empty() {
            user.push_str("# Reference exemplars\n\n");
            for (i, e) in exemplars.iter().enumerate() {
                render_generic_phase3_exemplar(&mut user, i + 1, e);
            }
            user.push_str("---\n\n");
        }

        user.push_str(&format!(
            "# Cluster to name (id: {}, facet: {})\n\n",
            cluster.id,
            facet.as_str()
        ));
        user.push_str(&format!(
            "The following {} sketch(es) were grouped together by embedding \
             similarity and the facet's secondary signal:\n\n",
            excerpts.len()
        ));
        for (i, ex) in excerpts.iter().enumerate() {
            user.push_str(&format!("{}. [{}] {}", i + 1, ex.section_id, ex.content));
            if !ex.anchor.is_empty() {
                user.push_str(&format!("  (anchor: {:?})", ex.anchor));
            }
            user.push('\n');
        }
        user.push_str(
            "\n---\n\nRespond with a single JSON object per the schema in the system message.",
        );

        // Schema-constrained output. The Phase-3 naming run before
        // this was added saw a 5–28% per-facet parse-failure rate on
        // unconstrained Qwen3.5-4B output (claim facet was worst at
        // 28%): the model would emit invented keys, drop required
        // fields, or produce malformed JSON entirely. Each facet's
        // schema mirrors its prompt's documented shape and uses
        // `maxLength`/`maxItems` caps as runaway-prevention floors —
        // the same JsonConstraint enforcer that bounds Phase 1.
        let (schema_name, schema) = match facet {
            Facet::Question => ("phase3_question", phase3_question_schema()),
            Facet::Claim => ("phase3_claim", phase3_claim_schema()),
            Facet::EntityState => ("phase3_entity_state", phase3_entity_state_schema()),
            Facet::RelationState => ("phase3_relation_state", phase3_relation_state_schema()),
            Facet::Event => ("phase3_event", phase3_event_schema()),
        };

        Some(
            ChatPrompt::new(system, user)
                .with_response_schema(schema_name, schema)
                .with_phase_id("phase3_facet")
                .with_max_output_tokens(512),
        )
    }

    /// Parse a referential phase-3 facet response.
    ///
    /// The referential prompts (`phase3_question_naming.md` etc.) ask
    /// the model for facet-specific shapes — `canonical_question`,
    /// `canonical_claim`, `canonical_event`, `canonical_label` — not
    /// the generic `{label, metadata}` the literary parser expects.
    /// Mapping each facet's primary key onto `label` and threading the
    /// remaining fields through `metadata` is what lets phases 5+
    /// consume these results unchanged.
    ///
    /// We try the facet-specific shape first; if that fails, fall
    /// back to the literary `{label, metadata}` parser so a manually
    /// authored response (or a future prompt change) still works.
    fn parse_phase3_facet(&self, facet: Facet, response: &str) -> Result<Phase3FacetParseResult> {
        let cleaned = prepare_phase_json(response, "phase 3 (referential)")?;
        if let Some(parsed) = parse_referential_phase3_facet(facet, &cleaned) {
            return Ok(parsed);
        }
        // Fall back to the generic literary parser for backward
        // compatibility (and so the error message points at the
        // generic shape if both attempts fail).
        self.inner.parse_phase3_facet(facet, response)
    }

    // ── Phase 5/6/7 — delegate to v1 literary ─────────────────

    fn compose_phase5(
        &self,
        concern: &CanonicalConcern,
        cluster: &ChunkCluster,
        cluster_chunk_texts: &[(u64, String)],
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        self.inner
            .compose_phase5(concern, cluster, cluster_chunk_texts, exemplars)
    }

    fn parse_phase5(&self, response: &str) -> Result<Phase5ParseResult> {
        self.inner.parse_phase5(response)
    }

    fn compose_phase6(
        &self,
        pos_a: &Position,
        pos_b: &Position,
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        self.inner.compose_phase6(pos_a, pos_b, exemplars)
    }

    fn parse_phase6(&self, response: &str) -> Result<Option<Phase6ParseResult>> {
        self.inner.parse_phase6(response)
    }

    fn compose_phase7(
        &self,
        concerns: &[CanonicalConcern],
        positions: &[Position],
        chapter_titles: &[String],
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        self.inner
            .compose_phase7(concerns, positions, chapter_titles, exemplars)
    }

    fn parse_phase7(&self, response: &str) -> Result<Vec<Phase7ParseItem>> {
        self.inner.parse_phase7(response)
    }

    // ── Clustering tuning — delegate ──────────────────────────

    fn question_clustering_config(&self) -> ClusteringConfig {
        self.inner.question_clustering_config()
    }

    fn chunk_clustering_config(&self) -> ClusteringConfig {
        self.inner.chunk_clustering_config()
    }

    // ── Phase 8 — referential corpora skip configuration ──────
    //
    // Configurations are interpretive rollups (e.g. "is this work
    // best read as a tragedy or a comedy?"). Referential corpora
    // don't admit such rollups — there's no editorial position to
    // collapse. Inherits the trait default (`false`).
}

// ── Phase 3 facet output schemas ─────────────────────────────
//
// One JSON Schema per facet, matching the per-facet prompts under
// `referential_atlas_prompts/phase3_*_naming.md`. Each schema:
//
//  - lists the facet's required fields exactly as the prompt asks,
//  - uses an `enum` for `kind`/`discourse_act` so the model can
//    only pick a documented value,
//  - caps every string with `maxLength` and every array with
//    `maxItems` — same runaway-prevention floor we use for Phase 1,
//  - uses `additionalProperties: false` so off-schema keys (which
//    the parser would otherwise just discard) can't waste tokens.
//
// `JsonConstraint` enforces these via logit masking, so the model
// physically cannot emit a malformed response. Without the schema,
// the unconstrained Phase 3 pass had a 5–28% per-facet failure rate.

const PHASE3_QUESTION_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "canonical_question": { "type": "string", "maxLength": 400 },
    "kind": {
      "type": "string",
      "enum": ["factual", "definitional", "causal", "comparative", "procedural"]
    },
    "description": { "type": "string", "maxLength": 600 },
    "aliases": {
      "type": "array",
      "maxItems": 5,
      "items": { "type": "string", "maxLength": 200 }
    }
  },
  "required": ["canonical_question", "kind"]
}"##;

const PHASE3_CLAIM_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "canonical_claim": { "type": "string", "maxLength": 600 },
    "discourse_act": {
      "type": "string",
      "enum": ["assertion", "attribution", "position", "definition"]
    },
    "subject": { "type": "string", "maxLength": 200 },
    "attributed_to": {
      "anyOf": [
        { "type": "string", "maxLength": 200 },
        { "type": "null" }
      ]
    },
    "description": { "type": "string", "maxLength": 600 }
  },
  "required": ["canonical_claim", "discourse_act"]
}"##;

const PHASE3_ENTITY_STATE_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "entity_name": { "type": "string", "maxLength": 200 },
    "canonical_label": { "type": "string", "maxLength": 400 },
    "kind": {
      "type": "string",
      "enum": ["biographical", "structural", "physical", "intellectual"]
    },
    "description": { "type": "string", "maxLength": 600 }
  },
  "required": ["canonical_label"]
}"##;

const PHASE3_RELATION_STATE_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "participants": {
      "type": "array",
      "maxItems": 8,
      "items": { "type": "string", "maxLength": 200 }
    },
    "canonical_label": { "type": "string", "maxLength": 400 },
    "kind": {
      "type": "string",
      "enum": ["diplomatic", "political", "institutional", "biographical", "causal", "temporal"]
    },
    "description": { "type": "string", "maxLength": 600 }
  },
  "required": ["participants", "canonical_label"]
}"##;

const PHASE3_EVENT_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "canonical_event": { "type": "string", "maxLength": 400 },
    "kind": {
      "type": "string",
      "enum": ["historical", "biographical", "scientific", "cultural", "natural"]
    },
    "participants": {
      "type": "array",
      "maxItems": 8,
      "items": { "type": "string", "maxLength": 200 }
    },
    "time": {
      "anyOf": [
        { "type": "string", "maxLength": 200 },
        { "type": "null" }
      ]
    },
    "description": { "type": "string", "maxLength": 600 }
  },
  "required": ["canonical_event"]
}"##;

fn phase3_question_schema() -> serde_json::Value {
    serde_json::from_str(PHASE3_QUESTION_SCHEMA).expect("PHASE3_QUESTION_SCHEMA must be valid JSON")
}
fn phase3_claim_schema() -> serde_json::Value {
    serde_json::from_str(PHASE3_CLAIM_SCHEMA).expect("PHASE3_CLAIM_SCHEMA must be valid JSON")
}
fn phase3_entity_state_schema() -> serde_json::Value {
    serde_json::from_str(PHASE3_ENTITY_STATE_SCHEMA)
        .expect("PHASE3_ENTITY_STATE_SCHEMA must be valid JSON")
}
fn phase3_relation_state_schema() -> serde_json::Value {
    serde_json::from_str(PHASE3_RELATION_STATE_SCHEMA)
        .expect("PHASE3_RELATION_STATE_SCHEMA must be valid JSON")
}
fn phase3_event_schema() -> serde_json::Value {
    serde_json::from_str(PHASE3_EVENT_SCHEMA).expect("PHASE3_EVENT_SCHEMA must be valid JSON")
}

/// Parse a facet-shaped phase-3 naming response into the generic
/// `{label, metadata}` shape phases 5+ consume.
///
/// Returns `None` when the JSON doesn't match the expected facet
/// shape — caller falls back to the literary parser.
fn parse_referential_phase3_facet(facet: Facet, cleaned: &str) -> Option<Phase3FacetParseResult> {
    let value: serde_json::Value = serde_json::from_str(cleaned).ok()?;
    let obj = value.as_object()?;

    let primary_key = match facet {
        Facet::Question => "canonical_question",
        Facet::Claim => "canonical_claim",
        Facet::EntityState | Facet::RelationState => "canonical_label",
        Facet::Event => "canonical_event",
    };

    let label = obj
        .get(primary_key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !is_placeholder_literal(s))?;

    let mut metadata: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (key, val) in obj {
        if key == primary_key {
            continue;
        }
        let stringified = match val {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => continue,
            serde_json::Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
                .join(", "),
            other => other.to_string(),
        };
        let trimmed = stringified.trim().to_string();
        if trimmed.is_empty() || is_placeholder_literal(&trimmed) {
            continue;
        }
        metadata.insert(key.clone(), trimmed);
    }

    Some(Phase3FacetParseResult { label, metadata })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::pipeline::atlas::SeedStrategy;

    fn sample_section() -> ChapterInput {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("ordinal".to_string(), "1".to_string());
        let text = "Albert Einstein (1879-1955) was a German-born theoretical \
                    physicist who developed the theory of relativity. He received \
                    the 1921 Nobel Prize in Physics for his discovery of the law \
                    of the photoelectric effect."
            .to_string();
        let approx_tokens = text.split_whitespace().count();
        ChapterInput {
            chapter_id: "lead".into(),
            title: "Albert Einstein".into(),
            text,
            metadata,
            approx_tokens,
        }
    }

    #[test]
    fn referential_atlas_registers_expected_identity() {
        let p = ReferentialAtlasPipeline::new();
        assert_eq!(p.id(), "referential_atlas");
        assert!(p.name().to_lowercase().contains("referential"));
    }

    #[test]
    fn referential_atlas_skips_configuration_phase() {
        let p = ReferentialAtlasPipeline::new();
        assert!(
            !p.runs_configuration_phase(),
            "referential_atlas should not opt into Phase 8 — referential corpora \
             have no interpretive rollup to produce."
        );
    }

    #[test]
    fn referential_atlas_declares_none_seed_strategy() {
        let p = ReferentialAtlasPipeline::new();
        // Multi-document referential corpora have no cross-document
        // seed; each article's lead serves as its own. The runner
        // skips Phase 1a entirely on this strategy.
        assert!(matches!(p.seed_strategy(), SeedStrategy::None));
    }

    #[test]
    fn referential_atlas_compose_seed_prompt_returns_none() {
        // Trait default returns None when SeedStrategy::None — no
        // override, no Phase 1a invocation.
        let p = ReferentialAtlasPipeline::new();
        assert!(p.compose_seed_prompt(&sample_section()).is_none());
    }

    #[test]
    fn referential_atlas_phase1_system_is_referential_tuned() {
        let p = ReferentialAtlasPipeline::new();
        let sys = p.phase1_system();
        // Shibboleth: the referential preamble explicitly names the
        // domain class — encyclopedic / referential / wiki — so a
        // future regression that swaps in a literary or philosophy
        // asset trips here.
        let lower = sys.to_lowercase();
        assert!(
            lower.contains("referential")
                || lower.contains("encyclopedic")
                || lower.contains("encyclopedia")
                || lower.contains("reference work"),
            "referential phase1_system should name its domain; got first 200 chars: {sys:.200}"
        );
    }

    #[test]
    fn referential_atlas_compose_phase1_renders_a_prompt() {
        let p = ReferentialAtlasPipeline::new();
        let prompt = p.compose_phase1(&sample_section(), &[]);
        assert!(!prompt.system.is_empty());
        assert!(
            prompt.user.contains("Einstein")
                || prompt.user.contains("Albert")
                || prompt.user.contains("lead")
        );
    }

    #[test]
    fn parse_phase3_facet_question_uses_canonical_question_as_label() {
        let response = r#"{
            "canonical_question": "What caused the fall of Rome?",
            "kind": "causal",
            "description": "A common entry point for readers tracing late-antique decline.",
            "aliases": ["decline of Rome", "why Rome fell"]
        }"#;
        let p = ReferentialAtlasPipeline::new();
        let parsed = p
            .parse_phase3_facet(Facet::Question, response)
            .expect("referential question parse");
        assert_eq!(parsed.label, "What caused the fall of Rome?");
        assert_eq!(
            parsed.metadata.get("kind").map(String::as_str),
            Some("causal")
        );
        assert!(parsed.metadata.contains_key("description"));
        assert_eq!(
            parsed.metadata.get("aliases").map(String::as_str),
            Some("decline of Rome, why Rome fell"),
            "aliases array should flatten to comma-joined string"
        );
    }

    #[test]
    fn parse_phase3_facet_claim_uses_canonical_claim_as_label() {
        let response = r#"{
            "canonical_claim": "Antibiotic resistance is rising globally.",
            "discourse_act": "assertion",
            "subject": "antibiotic resistance",
            "attributed_to": null,
            "description": "A modern public-health claim."
        }"#;
        let p = ReferentialAtlasPipeline::new();
        let parsed = p
            .parse_phase3_facet(Facet::Claim, response)
            .expect("referential claim parse");
        assert_eq!(parsed.label, "Antibiotic resistance is rising globally.");
        assert_eq!(
            parsed.metadata.get("discourse_act").map(String::as_str),
            Some("assertion")
        );
        // Null fields should be filtered out.
        assert!(!parsed.metadata.contains_key("attributed_to"));
    }

    #[test]
    fn parse_phase3_facet_event_uses_canonical_event_as_label() {
        let response = r#"{
            "canonical_event": "Fall of the Berlin Wall",
            "kind": "historical",
            "participants": ["East Germany", "West Germany"],
            "time": "1989-11-09",
            "description": "Symbolic end of the Cold War in Europe."
        }"#;
        let p = ReferentialAtlasPipeline::new();
        let parsed = p
            .parse_phase3_facet(Facet::Event, response)
            .expect("referential event parse");
        assert_eq!(parsed.label, "Fall of the Berlin Wall");
        assert_eq!(
            parsed.metadata.get("kind").map(String::as_str),
            Some("historical")
        );
    }

    #[test]
    fn parse_phase3_facet_falls_back_to_literary_when_no_facet_key() {
        // If the model returns the literary-style {label, metadata}
        // shape instead of the referential per-facet shape, the
        // fallback should still produce a valid result.
        let response = r#"{
            "label": "Some thematic concern",
            "metadata": {"scope": "novel-wide"}
        }"#;
        let p = ReferentialAtlasPipeline::new();
        let parsed = p
            .parse_phase3_facet(Facet::Question, response)
            .expect("literary fallback parse");
        assert_eq!(parsed.label, "Some thematic concern");
    }

    #[test]
    fn parse_phase3_facet_rejects_placeholder_label() {
        let response = r#"{"canonical_question": "...", "kind": "factual"}"#;
        let p = ReferentialAtlasPipeline::new();
        // Placeholder triggers fallback, which itself errors.
        let err = p.parse_phase3_facet(Facet::Question, response);
        assert!(err.is_err());
    }

    #[test]
    fn phase3_facet_schemas_parse_as_valid_json() {
        // Each schema const must be parseable JSON; the helper
        // functions panic loudly otherwise.
        let _ = phase3_question_schema();
        let _ = phase3_claim_schema();
        let _ = phase3_entity_state_schema();
        let _ = phase3_relation_state_schema();
        let _ = phase3_event_schema();
    }

    #[test]
    fn phase3_facet_schemas_have_required_keys_per_prompt() {
        // Pin the required-keys list against each prompt's documented
        // shape. The actual JsonConstraint compile path is exercised
        // in `sovereign-inference`'s json_constraint tests; here we
        // just sanity-check that the schemas state what they should.
        let q = phase3_question_schema();
        let q_required: Vec<&str> = q
            .pointer("/required")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(q_required.contains(&"canonical_question"));
        assert!(q_required.contains(&"kind"));

        let c = phase3_claim_schema();
        let c_required: Vec<&str> = c
            .pointer("/required")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(c_required.contains(&"canonical_claim"));
        assert!(c_required.contains(&"discourse_act"));
    }

    fn sample_cluster() -> AtlasCluster {
        AtlasCluster {
            id: "test_cl_0001".into(),
            facet: Facet::Question,
            refs: vec![],
        }
    }

    #[test]
    fn compose_phase3_facet_attaches_question_schema() {
        let p = ReferentialAtlasPipeline::new();
        let prompt = p
            .compose_phase3_facet(&sample_cluster(), Facet::Question, &[], &[])
            .expect("question facet returns Some");
        let schema = prompt
            .response_schema
            .as_ref()
            .expect("question facet must attach a schema");
        // Pin the structural shape: required `canonical_question`
        // + `kind` keys with the documented enum.
        let required = schema
            .pointer("/required")
            .and_then(|v| v.as_array())
            .expect("required array");
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required_strs.contains(&"canonical_question"));
        assert!(required_strs.contains(&"kind"));
        // `kind` enum should be present and non-empty.
        let enum_vals = schema
            .pointer("/properties/kind/enum")
            .and_then(|v| v.as_array())
            .expect("kind.enum");
        assert!(enum_vals.iter().any(|v| v == "factual"));
    }

    #[test]
    fn compose_phase3_facet_attaches_per_facet_schema() {
        let p = ReferentialAtlasPipeline::new();
        let cases = [
            (Facet::Claim, "canonical_claim", "phase3_claim"),
            (Facet::EntityState, "canonical_label", "phase3_entity_state"),
            (
                Facet::RelationState,
                "canonical_label",
                "phase3_relation_state",
            ),
            (Facet::Event, "canonical_event", "phase3_event"),
        ];
        for (facet, expected_prop, expected_name) in cases {
            let prompt = p
                .compose_phase3_facet(&sample_cluster(), facet, &[], &[])
                .unwrap_or_else(|| panic!("{facet:?} returns Some"));
            assert_eq!(
                prompt.response_schema_name.as_deref(),
                Some(expected_name),
                "{facet:?}: schema name"
            );
            let schema = prompt
                .response_schema
                .as_ref()
                .unwrap_or_else(|| panic!("{facet:?}: schema attached"));
            assert!(
                schema
                    .pointer(&format!("/properties/{expected_prop}"))
                    .is_some(),
                "{facet:?}: property {expected_prop} must be in schema"
            );
        }
    }
}
