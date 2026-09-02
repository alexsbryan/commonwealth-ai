// SPDX-License-Identifier: AGPL-3.0-or-later
//! Literary atlas pipeline — Step 1 of the v2.1 atlas schema rollout.
//!
//! Extends the `literary` pipeline's Phase 1 to emit a full
//! `SectionExtraction` record (entities / entity-states / relations /
//! relation-states / events / claims / questions) alongside the legacy
//! `questions` field. Phases 3–7 are delegated to the embedded
//! `LiteraryPipeline`; they continue to operate on the legacy
//! questions/concerns/positions flow while the atlas atom graph rides
//! along in each `ExtractedQuestion.section_extraction` slot.
//!
//! The atlas graph is not yet consumed by the downstream phases. When
//! a future landing rewrites Phase 3+ to traverse the atlas directly,
//! this pipeline keeps the same id and the Phase 1 output is already
//! in the right shape — no re-extraction needed.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::super::atlas::{
    EntitySketch, EntityType, SectionExtraction, SeedEntities, SeedEntity, SeedStrategy,
};
use super::super::exemplar_bank::{Exemplar, ExemplarKind};
use super::super::trait_def::Pipeline;
use super::super::types::*;
use super::literary::{prepare_phase_json, LiteraryPipeline};
use super::ontology_parse::null_or_empty_vec;
use crate::engine::CorpusEngine;
use crate::enrichment::atlas::{
    AtlasData, AtlasIngestion, AtlasIngestionConfig, AtlasIngestionRegistry,
};
use crate::enrichment::domain::ClusteringConfig;
use crate::enrichment::pipeline::atlas::EnrichmentDepth;
use crate::error::{Error, Result};
use crate::progress::ProgressCallback;
use crate::types::{EmbedFn, InferenceFn};
use serde::Deserialize;

pub(super) static PHASE1_ATLAS_SYSTEM: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "literary_atlas/phase1_system.md",
            include_str!("literary_atlas_prompts/phase1_system.md"),
        )
    });

static PHASE1B_ENTITY_COVERAGE: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "literary_atlas/phase1b_entity_coverage.md",
            include_str!("literary_atlas_prompts/phase1b_entity_coverage.md"),
        )
    });

static PHASE1B_CONCEPT_COVERAGE: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "literary_atlas/phase1b_concept_coverage.md",
            include_str!("literary_atlas_prompts/phase1b_concept_coverage.md"),
        )
    });

/// Terse Phase 1 preamble used when a default run failed with
/// `PhaseFailureKind::ThinkTruncated`. The asset drops the shape
/// example and prepends a "no reasoning trace" directive so the
/// model emits JSON directly instead of burning its output budget
/// on reflection.
pub(super) static PHASE1_ATLAS_SYSTEM_TERSE: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "literary_atlas/phase1_system_terse.md",
            include_str!("literary_atlas_prompts/phase1_system_terse.md"),
        )
    });

// Per-facet Phase 3 naming preambles. `compose_phase3_facet`
// selects among these by facet. Each targets the naming convention
// from spec §5.3 — question → thematic concern, claim → position
// family, entity-state → trajectory arc, relation-state →
// relational dynamic, event → narrative thread.
static PHASE3_QUESTION_NAMING: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "literary_atlas/phase3_question_naming.md",
            include_str!("literary_atlas_prompts/phase3_question_naming.md"),
        )
    });
static PHASE3_CLAIM_NAMING: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "literary_atlas/phase3_claim_naming.md",
            include_str!("literary_atlas_prompts/phase3_claim_naming.md"),
        )
    });
static PHASE3_ENTITY_STATE_NAMING: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "literary_atlas/phase3_entity_state_trajectory_naming.md",
            include_str!("literary_atlas_prompts/phase3_entity_state_trajectory_naming.md"),
        )
    });
static PHASE3_RELATION_STATE_NAMING: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "literary_atlas/phase3_relation_state_trajectory_naming.md",
            include_str!("literary_atlas_prompts/phase3_relation_state_trajectory_naming.md"),
        )
    });
static PHASE3_EVENT_NAMING: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "literary_atlas/phase3_event_thread_naming.md",
            include_str!("literary_atlas_prompts/phase3_event_thread_naming.md"),
        )
    });

static PHASE1A_SEED_SYSTEM: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "literary_atlas/phase1a_seed_system.md",
            include_str!("literary_atlas_prompts/phase1a_seed_system.md"),
        )
    });

/// Phase 8 configuration-detection preamble. The LLM reads the
/// atlas summary (not raw text) and emits 0–3 Configuration atoms
/// per spec §2.7, each with an `interpretive_note` articulating
/// alternative readings (the Ricoeur constraint per spec §1.2).
static PHASE8_CONFIGURATION_SYSTEM: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "literary_atlas/phase8_configuration.md",
            include_str!("literary_atlas_prompts/phase8_configuration.md"),
        )
    });

/// Phase 6 atlas Tension classifier preamble. The LLM reads one
/// resolved candidate (a claim+state pair sharing an entity) and
/// returns a verdict on whether they are in genuine structural
/// tension. Yes-verdicts promote to `EdgeType::Tension` records on
/// `edges.json` via `analysis::tension_classifier::classification_to_edge`.
static PHASE6_CLASSIFIER_SYSTEM: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "literary_atlas/phase6_classifier_system.md",
            include_str!("literary_atlas_prompts/phase6_classifier_system.md"),
        )
    });

/// Ontology-driven Phase-6 `tension` classifier template for custom-atlas
/// (recipe-ontology) corpora. The `{tension_term}`, `{position_term}`, and
/// `{guidance}` placeholders are filled per-corpus from the recipe's
/// `CustomOntology` at compose time, so the classifier judges conflicts in
/// the domain's own terms instead of the literary frame (Macbeth/Heathcliff
/// examples are the wrong unit of analysis for rule-sets and policies). See
/// `compose_phase6_atlas_classifier`.
static CUSTOM_PHASE6_CLASSIFIER_TEMPLATE: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "literary_atlas/custom_phase6_classifier_system.md",
            include_str!("literary_atlas_prompts/custom_phase6_classifier_system.md"),
        )
    });

/// Pipeline id exposed by the registry.
pub const PIPELINE_ID: &str = "literary_atlas";

/// Literary pipeline that extracts the full atlas atom graph in
/// Phase 1. Delegates Phases 3–7 to `LiteraryPipeline` unchanged.
pub struct LiteraryAtlasPipeline {
    inner: LiteraryPipeline,
    /// Which genre's ontology this pipeline runs. `id`/`name`/`vocabulary`, the
    /// Phase-1 extraction prompt and a handful of strategy choices come from
    /// here; Phases 3–7 are identical for every genre, because a genre is a
    /// Phase-1 ontology and not a pipeline. See [`super::genre::AtlasGenre`].
    genre: std::sync::Arc<dyn super::genre::AtlasGenre>,
}

impl LiteraryAtlasPipeline {
    pub fn new() -> Self {
        Self::with_genre(std::sync::Arc::new(super::genre::LiteraryGenre))
    }

    /// Build the atlas pipeline for one genre. The ONE constructor every genre
    /// goes through, prebuilt or recipe-driven — there is no second path that
    /// could acquire a different set of downstream phases.
    pub fn with_genre(genre: std::sync::Arc<dyn super::genre::AtlasGenre>) -> Self {
        Self {
            inner: LiteraryPipeline::new(),
            genre,
        }
    }

    /// Build a recipe-customized atlas pipeline from a custom ontology spec.
    /// Reports `id() = "custom_atlas"` and extracts Phase-1 atoms under the
    /// recipe's domain guidance (a neutral base prompt + the domain focus);
    /// downstream phases (3–7) are identical to every other genre.
    pub fn with_custom_ontology(spec: &super::configurable_atlas::CustomAtlasSpec) -> Self {
        Self::with_genre(std::sync::Arc::new(
            super::configurable_atlas::CustomOntology::build(spec),
        ))
    }
}

impl Default for LiteraryAtlasPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipeline for LiteraryAtlasPipeline {
    fn id(&self) -> &'static str {
        self.genre.id()
    }

    fn name(&self) -> &'static str {
        self.genre.name()
    }

    fn vocabulary(&self) -> &Vocabulary {
        match self.genre.vocabulary() {
            Some(v) => v,
            None => self.inner.vocabulary(),
        }
    }

    // ── Phase system preambles ────────────────────────────────
    //
    // Only Phase 1 diverges from `literary`; the rest reuse the same
    // prompt assets. When the atlas-native Phase 3+ lands we swap
    // those in here.

    fn phase1_system(&self) -> &'static str {
        self.genre.phase1_system()
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
        if let Some(p) = self.genre.compose_phase1(chapter, exemplars, None) {
            return p;
        }
        // Delegate to the seed-aware variant with no seed so the
        // seed-aware rendering path has a single call site. When a
        // seed is available the runner calls `compose_phase1_with_seed`
        // directly and gets the same body + an extra "known canonical
        // names" block at the top.
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

    /// Terse Phase 1 variant. Dispatched by the runner when a
    /// default-pass failure is classified as
    /// `PhaseFailureKind::ThinkTruncated`. Swaps the system preamble
    /// and drops the exemplar block to save tokens on a chapter that
    /// already blew past the output budget. Parser is shared with
    /// the default variant.
    fn compose_phase1_terse(&self, chapter: &ChapterInput) -> Option<ChatPrompt> {
        if let Some(p) = self.genre.compose_phase1_terse(chapter) {
            return Some(p);
        }
        let user = render_phase1_user_body(
            chapter,
            /*exemplars=*/ &[],
            /*include_exemplars=*/ false,
            /*seed=*/ None,
        );
        // Custom atlas retries with the SAME custom system prompt (just a lighter
        // body) so a failed chapter is re-extracted under the domain ontology,
        // not the literary terse prompt. Literary mode uses its terse system.
        let system = self.genre.phase1_terse_system();
        Some(
            ChatPrompt::new(system, user)
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
        // A genre whose atoms are not literary skips the literary-framed 1b
        // coverage top-up rather than being asked about characters.
        if !self.genre.runs_phase1b_coverage() {
            return None;
        }
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
        // A genre whose atoms are not literary skips the literary-framed 1b
        // coverage top-up rather than being asked about characters.
        if !self.genre.runs_phase1b_coverage() {
            return None;
        }
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

    // ── Stage 1a — seed extraction ─────────────────────────────

    fn seed_strategy(&self) -> SeedStrategy {
        self.genre.seed_strategy()
    }

    fn compose_seed_prompt(&self, first_section: &ChapterInput) -> Option<ChatPrompt> {
        let mut user = String::new();
        user.push_str("# Opening section\n\n");
        user.push_str(&format!("**Title:** {}\n", first_section.title));
        if let Some(ord) = first_section.metadata.get("ordinal") {
            user.push_str(&format!("**Position:** chapter {ord}\n"));
        }
        user.push_str("\n**Body:**\n\n");
        user.push_str(&first_section.text);
        user.push_str("\n\n---\n\n");
        user.push_str(
            "Respond with a single JSON object per the schema in the system \
             message. Entities only. No prose, no <think> block.",
        );
        Some(ChatPrompt::new(*PHASE1A_SEED_SYSTEM, user).with_phase_id("phase1_seed"))
    }

    fn parse_seed_response(&self, response: &str) -> Result<Vec<SeedEntity>> {
        let cleaned = prepare_phase_json(response, "stage 1a (seed)")?;

        #[derive(serde::Deserialize)]
        struct Raw {
            #[serde(default)]
            entries: Vec<Option<RawSeedEntry>>,
        }
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct RawSeedEntry {
            canonical_name: String,
            aliases: Vec<Option<String>>,
            entity_type: Option<EntityType>,
            description: String,
        }

        let raw: Raw = serde_json::from_str(&cleaned).map_err(|e| {
            Error::Serialization(format!("stage 1a (seed) response is not valid JSON: {e}"))
        })?;

        let mut entries: Vec<SeedEntity> = Vec::with_capacity(raw.entries.len());
        for item in raw.entries.into_iter().flatten() {
            let canonical = item.canonical_name.trim().to_string();
            if canonical.is_empty() || is_placeholder_literal(&canonical) {
                continue;
            }
            let description = item.description.trim().to_string();
            let description = if is_placeholder_literal(&description) {
                String::new()
            } else {
                description
            };
            let aliases: Vec<String> = item
                .aliases
                .into_iter()
                .flatten()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && !is_placeholder_literal(s))
                .collect();
            entries.push(SeedEntity {
                canonical_name: canonical,
                aliases,
                entity_type: item
                    .entity_type
                    .unwrap_or_else(|| EntityType::Other("unspecified".into())),
                description,
            });
        }
        if entries.is_empty() {
            return Err(Error::Serialization(
                "stage 1a (seed) response contained no valid entity entries — \
                 re-run the seed prompt; if the opening section genuinely has \
                 no named entities, declare SeedStrategy::None on the pipeline \
                 instead"
                    .into(),
            ));
        }
        Ok(entries)
    }

    fn compose_phase1_with_seed(
        &self,
        chapter: &ChapterInput,
        exemplars: &[&Exemplar],
        seed: Option<&SeedEntities>,
    ) -> ChatPrompt {
        if let Some(p) = self.genre.compose_phase1(chapter, exemplars, seed) {
            return p;
        }
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
        if let Some(r) = self.genre.parse_phase1(response) {
            return r;
        }
        super::ontology_parse::parse_phase1_section_extraction(
            response,
            &super::parse_policy::ParsePolicy::default(),
        )
    }

    // ── Phase 3 — delegate legacy path + atlas facet override ─

    fn compose_phase3(
        &self,
        cluster: &QuestionCluster,
        chapter_excerpts: &[&ChapterInput],
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        self.inner
            .compose_phase3(cluster, chapter_excerpts, exemplars)
            .with_phase_id("phase3")
            .with_max_output_tokens(1024)
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

        Some(
            ChatPrompt::new(system, user)
                .with_phase_id("phase3_facet")
                .with_max_output_tokens(512),
        )
    }

    fn parse_phase3_facet(&self, _facet: Facet, response: &str) -> Result<Phase3FacetParseResult> {
        let cleaned = prepare_phase_json(response, "phase 3 (atlas)")?;

        // Accept arbitrary JSON values inside `metadata` because the
        // per-facet prompts legitimately ask for arrays in some slots
        // (e.g. `participants: ["entity_a", "entity_b"]` in the
        // relation_state preamble). Flattening is centralised in
        // `phase3_metadata_value_to_string` so every facet shares the
        // same coercion rules — no facet-specific surprises.
        #[derive(serde::Deserialize)]
        struct Raw {
            label: Option<String>,
            #[serde(default)]
            metadata: Option<std::collections::HashMap<String, serde_json::Value>>,
        }
        let raw: Raw = serde_json::from_str(&cleaned)
            .map_err(|e| Error::Serialization(format!("phase 3 (atlas) JSON parse error: {e}")))?;
        let label = raw
            .label
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && !is_placeholder_literal(s))
            .ok_or_else(|| {
                Error::Serialization("phase 3 (atlas) response missing non-empty `label`".into())
            })?;
        let metadata: std::collections::HashMap<String, String> = raw
            .metadata
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(k, v)| {
                let s = phase3_metadata_value_to_string(v)?;
                let s = s.trim().to_string();
                if s.is_empty() || is_placeholder_literal(&s) {
                    None
                } else {
                    Some((k, s))
                }
            })
            .collect();
        Ok(Phase3FacetParseResult { label, metadata })
    }

    // ── Phase 5 — delegate ────────────────────────────────────

    fn compose_phase5(
        &self,
        concern: &CanonicalConcern,
        cluster: &ChunkCluster,
        cluster_chunk_texts: &[(u64, String)],
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        self.inner
            .compose_phase5(concern, cluster, cluster_chunk_texts, exemplars)
            .with_phase_id("phase5")
            .with_max_output_tokens(1024)
    }

    fn parse_phase5(&self, response: &str) -> Result<Phase5ParseResult> {
        self.inner.parse_phase5(response)
    }

    // ── Phase 6 — delegate ────────────────────────────────────

    fn compose_phase6(
        &self,
        pos_a: &Position,
        pos_b: &Position,
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        self.inner
            .compose_phase6(pos_a, pos_b, exemplars)
            .with_phase_id("phase6")
            .with_max_output_tokens(512)
    }

    fn parse_phase6(&self, response: &str) -> Result<Option<Phase6ParseResult>> {
        self.inner.parse_phase6(response)
    }

    // ── Phase 7 — delegate ────────────────────────────────────

    fn compose_phase7(
        &self,
        concerns: &[CanonicalConcern],
        positions: &[Position],
        chapter_titles: &[String],
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        self.inner
            .compose_phase7(concerns, positions, chapter_titles, exemplars)
            .with_phase_id("phase7")
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

    // ── Phase 8 (Configuration) — opt in ──────────────────────

    fn runs_configuration_phase(&self) -> bool {
        true
    }

    fn compose_phase8_configuration(
        &self,
        atlas_summary: &crate::enrichment::atlas::analysis::AtlasSummary,
        _exemplars: &[&Exemplar],
    ) -> Option<ChatPrompt> {
        let mut user = String::new();
        user.push_str("Atlas synopsis — structural view of the resolved atoms.\n");
        user.push_str("Use this to identify 0–3 configurations per the system prompt.\n\n");
        user.push_str(&format!("Sections: {}\n\n", atlas_summary.section_count));

        if !atlas_summary.entities.is_empty() {
            user.push_str("## Entities (by salience)\n\n");
            for e in &atlas_summary.entities {
                user.push_str(&format!(
                    "- `{}` **{}** (salience {:.2}) — {}\n",
                    e.id, e.canonical_name, e.salience, e.description
                ));
            }
            user.push('\n');
        }

        if !atlas_summary.relations.is_empty() {
            user.push_str("## Relations\n\n");
            for r in &atlas_summary.relations {
                user.push_str(&format!(
                    "- `{}` **{}** — between {}\n",
                    r.id,
                    r.label,
                    r.participants.join(" × ")
                ));
            }
            user.push('\n');
        }

        if !atlas_summary.trajectories.is_empty() {
            user.push_str("## Character trajectories (state chains in section order)\n\n");
            for t in &atlas_summary.trajectories {
                user.push_str(&format!(
                    "- `{}` **{}** — {}\n",
                    t.entity_id,
                    t.canonical_name,
                    t.state_labels.join(" → ")
                ));
            }
            user.push('\n');
        }

        if !atlas_summary.top_claims.is_empty() {
            user.push_str("## Top claims (by confidence)\n\n");
            for c in &atlas_summary.top_claims {
                let attrib = c
                    .attributed_to
                    .as_deref()
                    .map(|a| format!(" [attributed to **{a}**]"))
                    .unwrap_or_default();
                user.push_str(&format!(
                    "- `{}` ({}){} — {}\n",
                    c.id, c.discourse_act, attrib, c.content
                ));
            }
            user.push('\n');
        }

        if !atlas_summary.open_questions.is_empty() {
            user.push_str("## Open questions (unresolved by any claim)\n\n");
            for q in &atlas_summary.open_questions {
                user.push_str(&format!("- `{}` — {}\n", q.id, q.content));
            }
            user.push('\n');
        }

        if !atlas_summary.key_events.is_empty() {
            user.push_str("## Key events\n\n");
            for ev in &atlas_summary.key_events {
                user.push_str(&format!(
                    "- `{}` — {} (participants: {})\n",
                    ev.id,
                    ev.description,
                    ev.participants.join(", ")
                ));
            }
            user.push('\n');
        }

        user.push_str("\nReturn 0–3 configurations as strict JSON per the system prompt.");

        Some(
            ChatPrompt::new(*PHASE8_CONFIGURATION_SYSTEM, user)
                .with_phase_id("phase8_configuration"),
        )
    }

    fn parse_phase8_configuration(
        &self,
        response: &str,
    ) -> Result<Vec<crate::enrichment::atlas::analysis::Phase8ParseItem>> {
        parse_phase8_configuration_tolerant(response)
    }

    // ── Phase 6 atlas Tension classifier ─────────────────────────

    fn runs_phase6_atlas_classifier(&self) -> bool {
        true
    }

    fn tension_strategy(&self) -> crate::enrichment::atlas::analysis::TensionStrategy {
        self.genre.tension_strategy()
    }

    fn compose_phase6_atlas_classifier(
        &self,
        content: &crate::enrichment::atlas::analysis::CandidateContent,
    ) -> Option<ChatPrompt> {
        // A genre may judge conflicts in its own domain terms: the literary
        // frame below asks about narrative tension between characters, which
        // is the wrong unit of analysis for a rule-set or a policy document.
        if let Some(p) = self.genre.compose_phase6_classifier(content) {
            return Some(p);
        }
        Some(
            ChatPrompt::new(
                *PHASE6_CLASSIFIER_SYSTEM,
                render_phase6_classifier_user_body(content),
            )
            .with_response_schema(
                "phase6_classifier_response",
                crate::enrichment::atlas::analysis::phase6_classifier_response_schema(),
            )
            .with_phase_id("phase6_classifier")
            .with_max_output_tokens(256),
        )
    }
}

// ── Helpers ──────────────────────────────────────────────────

/// Coerce a Phase 3 metadata value into a flat string for the
/// `HashMap<String, String>` that downstream consumers expect.
///
/// Some per-facet preambles ask the model to emit arrays (e.g.
/// `participants: ["entity_a", "entity_b"]` for relation-state
/// trajectories). The downstream metadata bag is flat strings, so we
/// flatten arrays by joining string elements with ", ". Other shapes
/// are preserved by stringifying — better than dropping the slot
/// entirely. Returns `None` only for explicit nulls so the parser's
/// .filter_map drops them, matching the prior `Option<String>` shape.
pub(super) fn phase3_metadata_value_to_string(v: serde_json::Value) -> Option<String> {
    use serde_json::Value;
    match v {
        Value::Null => None,
        Value::String(s) => Some(s),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(items) => {
            let parts: Vec<String> = items
                .into_iter()
                .filter_map(phase3_metadata_value_to_string)
                .filter(|s| !s.trim().is_empty())
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(", "))
            }
        }
        Value::Object(map) => serde_json::to_string(&Value::Object(map)).ok(),
    }
}

/// Filter non-object items out of Phase 1 array fields whose schema
/// declares structs. Observed on Gemma-31B running sep-al-farabi
/// sec_0003: the model interleaved `"//"` comment-strings into
/// `entities_introduced` between actual entity objects, breaking
/// deserialization. Walks the seven known sketch arrays in the
/// section extraction and drops anything that isn't an Object or
/// Null. Idempotent — strings/numbers/booleans never legitimately
/// appear in these slots.
pub(super) fn sanitize_phase1_object_arrays(value: &mut serde_json::Value) {
    use serde_json::Value;
    let Value::Object(top) = value else { return };
    const OBJECT_ARRAY_FIELDS: &[&str] = &[
        "entities_introduced",
        "entities_developed",
        "relations_introduced",
        "relations_developed",
        "events",
        "claims",
        "questions_raised",
    ];
    for key in OBJECT_ARRAY_FIELDS {
        if let Some(Value::Array(items)) = top.get_mut(*key) {
            items.retain(|item| matches!(item, Value::Object(_) | Value::Null));
        }
    }
}

/// Pick the first event description from the extraction to fill the
/// legacy `plot` field. The atlas has richer event records; this is a
/// one-sentence back-compat summary.
/// Compose the user-message body for Phase 1. Shared by the
/// default, seed-aware, and terse variants so the body stays
/// identical across all three — only the system preamble +
/// whether exemplars + whether a seed block are included differ.
///
/// When `seed` is `Some`, a "Known canonical names in this corpus"
/// block is rendered at the top of the user message. Chapter-level
/// map calls use these to resolve pronouns and alias variants to
/// stable forms, which is the whole point of Stage 1a.
/// Render the Phase 6 classifier user body for one resolved
/// candidate. The system prompt covers the rule set; the user body
/// presents the two atoms verbatim with kind labels (Claim / State)
/// and the shared-entity name when present.
///
/// Format is deliberately minimal — this is a per-candidate call
/// dispatched many times per build (12 candidates for a 5-chapter
/// novel section, scaling with claims × states × shared entities),
/// so token efficiency matters more than narrative framing.
fn render_phase6_classifier_user_body(
    content: &crate::enrichment::atlas::analysis::CandidateContent,
) -> String {
    use crate::enrichment::atlas::analysis::TensionSide;
    let kind_label = |k: TensionSide| match k {
        TensionSide::Claim => "Claim",
        TensionSide::State => "State",
    };
    let mut user = String::new();
    user.push_str("# Candidate to classify\n\n");
    if let Some(name) = content.shared_entity_name.as_deref() {
        user.push_str(&format!("**Shared participant:** {name}\n\n"));
    } else {
        user.push_str("**Shared participant:** (not resolved by deterministic pass)\n\n");
    }
    user.push_str(&format!(
        "**A — {} ({}):** {}\n\n",
        kind_label(content.source_kind),
        content.source_atom.as_str(),
        content.source_text.trim()
    ));
    user.push_str(&format!(
        "**B — {} ({}):** {}\n\n",
        kind_label(content.target_kind),
        content.target_atom.as_str(),
        content.target_text.trim()
    ));
    user.push_str(
        "Classify whether A and B are in genuine structural tension. \
         Return one JSON object per the schema in the system message. \
         No prose, no `<think>` block, no markdown fences. Begin with `{`.\n",
    );
    user
}

/// Fill the ontology-driven Phase-6 classifier template from a custom
/// atlas's recipe data: the domain `guidance`, the `tension_term`, and the
/// `position_term`. Extracted from `compose_phase6_atlas_classifier` so the
/// "custom mode is ontology-driven, not literary" invariant is unit-testable
/// without constructing a full candidate.
pub(super) fn custom_phase6_classifier_system(
    guidance: &str,
    tension_term: &str,
    position_term: &str,
) -> String {
    CUSTOM_PHASE6_CLASSIFIER_TEMPLATE
        .replace("{tension_term}", tension_term)
        .replace("{position_term}", position_term)
        .replace("{guidance}", guidance)
}

/// Neutral user body for the ontology-driven custom-atlas Phase-6
/// classifier. Presents the two atoms plainly — no literary "shared
/// participant" framing — and asks the domain-specific question. The
/// system message (filled from the recipe ontology) carries the meaning
/// of a `{tension_term}`.
pub(super) fn render_custom_phase6_classifier_user_body(
    content: &crate::enrichment::atlas::analysis::CandidateContent,
    tension_term: &str,
) -> String {
    format!(
        "# Two atoms to compare\n\n**A:** {}\n\n**B:** {}\n\nDecide whether A and B are in a genuine {} per the system message. \
         Return one JSON object. No prose, no `<think>` block, no markdown fences. Begin with `{{`.\n",
        content.source_text.trim(),
        content.target_text.trim(),
        tension_term,
    )
}

pub(super) fn render_phase1_user_body(
    chapter: &ChapterInput,
    exemplars: &[&Exemplar],
    include_exemplars: bool,
    seed: Option<&SeedEntities>,
) -> String {
    let mut user = String::new();

    if let Some(seed) = seed {
        if !seed.entries.is_empty() {
            user.push_str("# Known canonical names in this corpus\n\n");
            user.push_str(
                "When a character, place, or other entity below appears in the \
                 chapter under any form (full name, patronymic, nickname, \
                 pronoun with clear antecedent), use the CANONICAL NAME from \
                 this list — not whatever form the chapter happened to use \
                 and not a translated/transliterated variant. If a name from \
                 the text is not in this list, treat it as a new entity and \
                 choose a canonical form of your own.\n\n",
            );
            for entry in &seed.entries {
                let aliases = if entry.aliases.is_empty() {
                    String::new()
                } else {
                    format!(" (aliases: {})", entry.aliases.join(", "))
                };
                let description = if entry.description.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", entry.description)
                };
                user.push_str(&format!(
                    "- **{}** [{}]{}{}\n",
                    entry.canonical_name,
                    entry.entity_type.as_str_repr(),
                    aliases,
                    description,
                ));
            }
            user.push_str("\n---\n\n");
        }
    }

    if include_exemplars && !exemplars.is_empty() {
        user.push_str("# Reference exemplars\n\n");
        user.push_str(
            "Each block shows the shape of a well-formed atlas \
             extraction. Produce your own analysis of the chapter \
             below; do NOT copy any exemplar's content.\n\n",
        );
        for (i, e) in exemplars.iter().enumerate() {
            render_atlas_exemplar(&mut user, i + 1, e);
        }
        user.push_str("---\n\n");
    }

    user.push_str("# Chapter to analyse\n\n");
    user.push_str(&format!("**Section id:** {}\n", chapter.chapter_id));
    user.push_str(&format!("**Title:** {}\n", chapter.title));
    if let Some(ord) = chapter.metadata.get("ordinal") {
        user.push_str(&format!("**Position:** chapter {ord}\n"));
    }
    user.push_str("\n**Body:**\n\n");
    user.push_str(&chapter.text);
    user.push_str("\n\n---\n\n");
    user.push_str(&format!(
        "Use `section_id = \"{}\"` in your response. Respond with \
         a single JSON object per the schema in the system message.",
        chapter.chapter_id
    ));

    user
}

pub(super) fn first_event_description(extraction: &SectionExtraction) -> Option<String> {
    extraction
        .events
        .first()
        .map(|e| e.description.trim().to_string())
        .filter(|s| !s.is_empty() && !is_placeholder_literal(s))
}

/// Pick the first unattributed (text-level) claim to fill the legacy
/// `reveals` field — that's typically the structural argument of the
/// section. Falls back to the first attributed claim if no text-level
/// claim is present.
pub(super) fn first_text_level_claim(extraction: &SectionExtraction) -> Option<String> {
    extraction
        .claims
        .iter()
        .find(|c| c.attributed_to.is_none())
        .or_else(|| extraction.claims.first())
        .map(|c| c.content.trim().to_string())
        .filter(|s| !s.is_empty() && !is_placeholder_literal(s))
}

/// Wipe placeholder-literal strings from a `SectionExtraction` so a
/// `"..."` that slipped past the prompt doesn't pollute downstream.
/// Strings that become empty after scrubbing stay empty — the skip-on-
/// empty serde attributes ensure they won't serialise.
pub(super) fn scrub_placeholder_strings(e: &mut SectionExtraction) {
    fn scrub(s: &mut String) {
        if is_placeholder_literal(s) {
            s.clear();
        }
    }
    /// A declared attribute whose value is schema echo (`"..."`, `"<value>"`)
    /// carries nothing; drop the key rather than store the echo. Numbers and
    /// snapped refs are untouched — only string values can be placeholders.
    fn scrub_attrs(attrs: &mut serde_json::Map<String, serde_json::Value>) {
        attrs.retain(|_, v| match v.as_str() {
            Some(s) => !is_placeholder_literal(s),
            None => true,
        });
    }
    for entity in &mut e.entities_introduced {
        scrub(&mut entity.canonical_name);
        entity.aliases.retain(|a| !is_placeholder_literal(a));
        scrub(&mut entity.description);
        scrub(&mut entity.anchor);
        scrub_attrs(&mut entity.attributes);
    }
    for state in &mut e.entities_developed {
        scrub(&mut state.entity_name);
        scrub(&mut state.label);
        scrub(&mut state.anchor);
    }
    for relation in &mut e.relations_introduced {
        relation.participants.retain(|p| !is_placeholder_literal(p));
        scrub(&mut relation.label);
        scrub(&mut relation.anchor);
        scrub_attrs(&mut relation.attributes);
    }
    for state in &mut e.relations_developed {
        state.participants.retain(|p| !is_placeholder_literal(p));
        scrub(&mut state.label);
        scrub(&mut state.anchor);
    }
    for event in &mut e.events {
        scrub(&mut event.description);
        event.participants.retain(|p| !is_placeholder_literal(p));
        scrub(&mut event.anchor);
        scrub_attrs(&mut event.attributes);
    }
    for claim in &mut e.claims {
        scrub(&mut claim.content);
        if let Some(a) = claim.attributed_to.as_mut() {
            scrub(a);
        }
        claim.attributed_to = claim.attributed_to.take().filter(|s| !s.is_empty());
        if let Some(s) = claim.subject.as_mut() {
            scrub(s);
        }
        claim.subject = claim.subject.take().filter(|s| !s.is_empty());
        scrub(&mut claim.anchor);
        scrub_attrs(&mut claim.attributes);
    }
    for q in &mut e.questions_raised {
        scrub(&mut q.content);
        scrub(&mut q.anchor);
    }
}

// ── Phase 1b coverage check (shared helpers) ────────────────
//
// Both literary_atlas and philosophy_atlas dispatch the same two
// coverage prompts (entity + concept) and parse the same response
// shape into `EntitySketch` entries. The user-body renderer and
// parser live here so philosophy can reuse them through the
// `pub(super)` re-export in the module hierarchy.

/// Build the user message for a Phase 1b audit. Lists what the
/// extractor already produced so the model's job is constrained
/// to NEW atoms.
pub(super) fn render_phase1b_user_body(
    chapter: &ChapterInput,
    existing: &SectionExtraction,
) -> String {
    let mut user = String::new();
    user.push_str("# Section\n\n");
    user.push_str(&format!("**Title:** {}\n\n", chapter.title));
    user.push_str("**Body:**\n\n");
    user.push_str(&chapter.text);
    user.push_str("\n\n# What the extractor already produced\n\n");
    user.push_str("**Entities:**\n");
    if existing.entities_introduced.is_empty() {
        user.push_str("  (none)\n");
    } else {
        for e in &existing.entities_introduced {
            user.push_str(&format!("  - {:?}: {}\n", e.entity_type, e.canonical_name));
        }
    }
    user.push_str(&format!(
        "\n**Other counts:** {} event(s), {} state(s), {} relation(s), \
         {} claim(s), {} question(s).\n\n",
        existing.events.len(),
        existing.entities_developed.len() + existing.relations_developed.len(),
        existing.relations_introduced.len(),
        existing.claims.len(),
        existing.questions_raised.len(),
    ));
    user.push_str(
        "# Your task\n\nList only what was missed. Return JSON per \
         the schema in the system message.\n",
    );
    user
}

/// Parse a Phase 1b response into `EntitySketch` entries. Accepts
/// either response shape — `missed_entities` (entity-coverage prompt)
/// or `missed_concepts` (concept-coverage prompt) — and treats the
/// concept variant as `entity_type: concept`. Drops entries with an
/// empty canonical name; passes the rest through with sensible
/// fallbacks.
pub(super) fn parse_phase1b_coverage_response(response: &str) -> Result<Vec<EntitySketch>> {
    let cleaned = prepare_phase_json(response, "phase 1b (coverage)")?;

    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Raw {
        #[serde(deserialize_with = "null_or_empty_vec")]
        missed_entities: Vec<Option<RawCoverageEntity>>,
        #[serde(deserialize_with = "null_or_empty_vec")]
        missed_concepts: Vec<Option<RawCoverageConcept>>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct RawCoverageEntity {
        canonical_name: String,
        entity_type: Option<EntityType>,
        description: String,
        anchor: String,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct RawCoverageConcept {
        canonical_name: String,
        description: String,
        anchor: String,
    }

    let raw: Raw = serde_json::from_str(&cleaned).map_err(|e| {
        Error::Serialization(format!(
            "phase 1b (coverage) response is not valid JSON: {e}"
        ))
    })?;

    let mut out: Vec<EntitySketch> = Vec::new();
    for item in raw.missed_entities.into_iter().flatten() {
        let name = item.canonical_name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        // A missing entity_type or one the schema can't classify
        // (Other(_)) is treated as a soft drop — the resolver
        // would discard it anyway. Better to skip here than to
        // pollute the section with un-typeable atoms.
        let entity_type = match item.entity_type {
            Some(EntityType::Other(_)) | None => continue,
            Some(et) => et,
        };
        out.push(EntitySketch {
            attributes: Default::default(),
            canonical_name: name,
            aliases: Vec::new(),
            entity_type,
            description: item.description.trim().to_string(),
            defining_quote: None,
            anchor: item.anchor.trim().to_string(),
        });
    }
    for item in raw.missed_concepts.into_iter().flatten() {
        let name = item.canonical_name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        out.push(EntitySketch {
            attributes: Default::default(),
            canonical_name: name,
            aliases: Vec::new(),
            entity_type: EntityType::Concept,
            description: item.description.trim().to_string(),
            defining_quote: None,
            anchor: item.anchor.trim().to_string(),
        });
    }
    Ok(out)
}

/// Render a Phase 3 facet-naming exemplar. The naming prompt
/// expects a small, shape-focused example — we surface the input
/// selector text + the target label/metadata. Keeps the naming
/// budget lean compared to the Phase 1 exemplar renderer.
pub(super) fn render_generic_phase3_exemplar(buf: &mut String, n: usize, e: &Exemplar) {
    buf.push_str(&format!("## Exemplar {n} ({:?})\n\n", e.kind));
    if let Some(input_text) = e.input.get("cluster_text").and_then(|v| v.as_str()) {
        buf.push_str(&format!("**Cluster snapshot:**\n{input_text}\n\n"));
    } else if let Some(selector) = e.selector_text.as_deref() {
        buf.push_str(&format!("**Cluster snapshot:**\n{selector}\n\n"));
    }
    match e.kind {
        ExemplarKind::Positive => {
            if let Some(out) = e.output.as_ref() {
                buf.push_str("**Target label:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(out).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
        }
        ExemplarKind::Corrected => {
            if let Some(m) = e.model_output.as_ref() {
                buf.push_str("**What the model produced:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(m).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
            let correction = e.corrected_output.as_ref().or(e.output.as_ref());
            if let Some(c) = correction {
                buf.push_str("**Corrected label:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(c).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
        }
        ExemplarKind::Negative => {
            if let Some(m) = e.model_output.as_ref() {
                buf.push_str("**Reject this label:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(m).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
        }
    }
    buf.push_str(&format!("**Why:** {}\n\n", e.rationale));
}

/// Render one atlas exemplar into the user-message buffer. Mirrors
/// `render_phase1_exemplar` in `literary.rs` but targets the atlas
/// input shape (chapter text + expected SectionExtraction).
fn render_atlas_exemplar(buf: &mut String, n: usize, e: &Exemplar) {
    buf.push_str(&format!("## Exemplar {n} ({:?})\n\n", e.kind));
    if let Some(title) = e.input.get("title").and_then(|v| v.as_str()) {
        buf.push_str(&format!("**Chapter:** {title}\n"));
    }
    if let Some(excerpt) = e.input.get("excerpt").and_then(|v| v.as_str()) {
        buf.push_str(&format!("**Excerpt:** {excerpt}\n\n"));
    }
    match e.kind {
        ExemplarKind::Positive => {
            if let Some(out) = e.output.as_ref() {
                buf.push_str("**Target output:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(out).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
        }
        ExemplarKind::Corrected => {
            if let Some(m) = e.model_output.as_ref() {
                buf.push_str("**Model produced:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(m).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
            let correction = e.corrected_output.as_ref().or(e.output.as_ref());
            if let Some(c) = correction {
                buf.push_str("**Corrected output:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(c).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
        }
        ExemplarKind::Negative => {
            if let Some(m) = e.model_output.as_ref() {
                buf.push_str("**Reject this shape:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(m).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
        }
    }
    buf.push_str(&format!("**Why:** {}\n\n", e.rationale));
}

// ── Phase 1 JSON Schema (for grammar-constrained generation) ─
//
// Lenient JSON Schema that mirrors `RawSectionExtraction`. Used by
// `phase1_section_extraction_schema()` so the daemon can pass it to
// `LlamaSampler::llguidance` and force the model to emit valid JSON.
// Strictness goal: eliminate the "invalid JSON syntax" failure mode
// (missing commas, unclosed brackets, duplicate keys) that recurs on
// long Phase 1 prompts. We do NOT enumerate enum strings — the
// `string_enum_with_other!` machinery already absorbs unknown values
// into `Other(String)`. We do NOT require most fields — the existing
// `Raw*::into_sketch()` drops sketches whose required fields are
// missing, so the parser stays the source of truth on completeness.
// Only `section_id` and `questions_raised` are required at the top
// level (mirroring the existing parser checks at parse_phase1).
// Note on `maxLength` annotations below: these are NOT data-quality
// caps — they are runaway-prevention caps. The in-house JSON-Schema
// constraint enforcer treats `maxLength` like `maxItems`: once the
// running code-point count reaches the cap, the only valid next
// byte is `"` (close-quote). Without these caps, a single unbounded
// string field can swallow the whole token budget — concrete
// 2026-05-04 repro: a 78-word LATIN lead burned 11337 generated
// tokens before the daemon's 300s deadline tripped, with the model
// elaborating into one long `description`/`content` string that
// the mask had no way to terminate. The numbers below are sized
// generously so legitimate output is never clipped:
//   - canonical_name / entity_name / label: 200 (entity names are
//     usually <50 chars; 200 leaves room for institutional names)
//   - description: 600 (one short paragraph)
//   - content (claim, question): 800 (one full sentence with caveats)
//   - anchor: 800 (a quoted span; sometimes a paragraph)
//   - discourse_act / epistemic_status: 200 (short labels)
//   - aliases / participants items: 200 (same as canonical names)
const PHASE1_SECTION_EXTRACTION_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "section_id": { "type": "string", "maxLength": 200 },
    "entities_introduced": {
      "type": "array",
      "maxItems": 15,
      "items": { "$ref": "#/$defs/entity_sketch" }
    },
    "entities_developed": {
      "type": "array",
      "maxItems": 10,
      "items": { "$ref": "#/$defs/entity_state_sketch" }
    },
    "relations_introduced": {
      "type": "array",
      "maxItems": 10,
      "items": { "$ref": "#/$defs/relation_sketch" }
    },
    "relations_developed": {
      "type": "array",
      "maxItems": 10,
      "items": { "$ref": "#/$defs/relation_state_sketch" }
    },
    "events": {
      "type": "array",
      "maxItems": 10,
      "items": { "$ref": "#/$defs/event_sketch" }
    },
    "claims": {
      "type": "array",
      "maxItems": 10,
      "items": { "$ref": "#/$defs/claim_sketch" }
    },
    "questions_raised": {
      "type": "array",
      "minItems": 1,
      "maxItems": 25,
      "items": { "$ref": "#/$defs/question_sketch" }
    },
    "argument_reconstructions": {
      "type": "array",
      "maxItems": 6,
      "items": { "$ref": "#/$defs/argument_reconstruction_sketch" }
    }
  },
  "required": ["section_id", "questions_raised"],
  "$defs": {
    "entity_sketch": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "canonical_name": { "type": "string", "maxLength": 200 },
        "aliases": { "type": "array", "maxItems": 5, "items": { "type": "string", "maxLength": 200 } },
        "entity_type": {
          "type": "string",
          "enum": ["person", "concept", "institution", "work", "place", "initiative"]
        },
        "description": { "type": "string", "maxLength": 600 },
        "defining_quote": {
          "anyOf": [
            { "type": "string", "maxLength": 220 },
            { "type": "null" }
          ]
        },
        "anchor": { "type": "string", "maxLength": 800 }
      },
      "required": ["canonical_name", "entity_type"]
    },
    "entity_state_sketch": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "entity_name": { "type": "string", "maxLength": 200 },
        "label": { "type": "string", "maxLength": 200 },
        "anchor": { "type": "string", "maxLength": 800 }
      },
      "required": ["entity_name", "label"]
    },
    "relation_sketch": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "participants": { "type": "array", "maxItems": 8, "items": { "type": "string", "maxLength": 200 } },
        "label": { "type": "string", "maxLength": 200 },
        "anchor": { "type": "string", "maxLength": 800 }
      },
      "required": ["participants", "label"]
    },
    "relation_state_sketch": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "participants": { "type": "array", "maxItems": 8, "items": { "type": "string", "maxLength": 200 } },
        "label": { "type": "string", "maxLength": 200 },
        "anchor": { "type": "string", "maxLength": 800 }
      },
      "required": ["participants", "label"]
    },
    "event_sketch": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "description": { "type": "string", "maxLength": 600 },
        "participants": { "type": "array", "maxItems": 8, "items": { "type": "string", "maxLength": 200 } },
        "anchor": { "type": "string", "maxLength": 800 }
      },
      "required": ["description"]
    },
    "claim_sketch": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "content": { "type": "string", "maxLength": 800 },
        "discourse_act": { "type": "string", "maxLength": 200 },
        "epistemic_status": { "type": "string", "maxLength": 200 },
        "attributed_to": {
          "anyOf": [
            { "type": "string", "maxLength": 200 },
            { "type": "array", "maxItems": 8, "items": { "type": "string", "maxLength": 200 } },
            { "type": "null" }
          ]
        },
        "quotable_excerpt": {
          "anyOf": [
            { "type": "string", "maxLength": 220 },
            { "type": "null" }
          ]
        },
        "anchor": { "type": "string", "maxLength": 800 }
      },
      "required": ["content", "discourse_act"]
    },
    "question_sketch": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "content": { "type": "string", "maxLength": 800 },
        "anchor": { "type": "string", "maxLength": 800 }
      },
      "required": ["content"]
    },
    "argument_reconstruction_sketch": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "name": { "type": "string", "maxLength": 200 },
        "proponent": {
          "anyOf": [
            { "type": "string", "maxLength": 200 },
            { "type": "null" }
          ]
        },
        "premises": {
          "type": "array",
          "maxItems": 8,
          "items": { "type": "string", "maxLength": 400 }
        },
        "conclusion": { "type": "string", "maxLength": 400 },
        "objections": {
          "type": "array",
          "maxItems": 6,
          "items": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
              "name": { "type": "string", "maxLength": 120 },
              "content": { "type": "string", "maxLength": 400 }
            },
            "required": ["name"]
          }
        },
        "anchor": { "type": "string", "maxLength": 800 }
      },
      "required": ["name", "premises", "conclusion"]
    }
  }
}"##;

/// Return the Phase 1 section-extraction JSON Schema as a parsed
/// `serde_json::Value`. Callers thread this through
/// `ChatPrompt::with_response_schema()` so the daemon's
/// grammar-constrained sampler forces the model into valid JSON.
///
/// The schema lives as a const string to avoid a `schemars` dep; the
/// const is unit-tested for parse-validity below so drift caught at
/// compile + test time, not at first runtime use.
pub fn phase1_section_extraction_schema() -> serde_json::Value {
    serde_json::from_str(PHASE1_SECTION_EXTRACTION_SCHEMA)
        .expect("PHASE1_SECTION_EXTRACTION_SCHEMA must be valid JSON")
}

/// Phase 1b coverage-pass schema. Used by both the entity-coverage and
/// the concept-coverage passes (the concept variant uses
/// `missed_concepts` instead of `missed_entities`, but the parser
/// accepts either, so a permissive union schema works for both). Both
/// arrays are optional and default to empty — the prompt instructs
/// "omit the key entirely if nothing was missed", and the parser
/// honours that.
const PHASE1B_COVERAGE_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "missed_entities": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "properties": {
          "canonical_name": {"type": "string"},
          "entity_type": {"type": "string"},
          "description": {"type": "string"},
          "anchor": {"type": "string"}
        },
        "required": ["canonical_name", "entity_type", "description", "anchor"]
      }
    },
    "missed_concepts": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "properties": {
          "canonical_name": {"type": "string"},
          "description": {"type": "string"},
          "anchor": {"type": "string"}
        },
        "required": ["canonical_name", "description", "anchor"]
      }
    }
  }
}"##;

pub fn phase1b_coverage_schema() -> serde_json::Value {
    serde_json::from_str(PHASE1B_COVERAGE_SCHEMA)
        .expect("PHASE1B_COVERAGE_SCHEMA must be valid JSON")
}

/// Phase 1a (seed) JSON schema. Same shape as `SeedEntity` —
/// downstream parser is `parse_seed_response`. The seed prompt asked
/// the model for "Entities only" but with no schema constraint,
/// dense first-sections (e.g. SEP `freewill` opening with one
/// 3000-word paragraph) caused the model to truncate prose-prefixed
/// JSON or emit unparseable output. Grammar-constrained generation
/// fixes the failure mode.
const PHASE1A_SEED_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "entries": {
      "type": "array",
      "minItems": 1,
      "items": {
        "type": "object",
        "additionalProperties": false,
        "properties": {
          "canonical_name": {"type": "string"},
          "aliases": {"type": "array", "items": {"type": "string"}},
          "entity_type": {"type": "string"},
          "description": {"type": "string"}
        },
        "required": ["canonical_name", "entity_type", "description"]
      }
    }
  },
  "required": ["entries"]
}"##;

pub fn phase1a_seed_schema() -> serde_json::Value {
    serde_json::from_str(PHASE1A_SEED_SCHEMA).expect("PHASE1A_SEED_SCHEMA must be valid JSON")
}

/// Parse a Phase 8 (`configurations`) response with tolerance for
/// three real-world model deviations observed during the SEP atlas
/// ingest:
///
/// 1. Strict shape: `{"configurations": [ {...}, {...} ]}` — the
///    documented format.
/// 2. Map-keyed shape: `{"configurations": {"key_a": {...},
///    "key_b": {...}}}` — DeepSeek-family models occasionally
///    interpret "0–3 configurations" as a keyed dict. Coerce values
///    into an array.
/// 3. Bare-array shape: `[ {...}, {...} ]` — model skips the wrapper
///    object.
/// 4. No JSON at all (e.g. an unclosed `<think>` trace or model
///    declined): return `Ok(vec![])`. Phase 8's prompt explicitly
///    allows "0–3 configurations" so zero configurations is a valid
///    answer, not a build failure.
pub(crate) fn parse_phase8_configuration_tolerant(
    response: &str,
) -> Result<Vec<crate::enrichment::atlas::analysis::Phase8ParseItem>> {
    let cleaned = match super::literary::prepare_phase_json(response, "phase 8 (configuration)") {
        Ok(c) => c,
        Err(Error::Serialization(msg)) if msg.contains("contained no recognisable JSON object") => {
            tracing::warn!(
                phase = "phase8_configuration",
                "model returned no JSON block; treating as 0 configurations"
            );
            return Ok(Vec::new());
        }
        Err(e) => return Err(e),
    };

    let root: serde_json::Value = serde_json::from_str(&cleaned).map_err(|e| {
        Error::Serialization(format!(
            "phase 8 (configuration) response is not valid JSON: {e}"
        ))
    })?;

    // Locate the items: either `root.configurations` or root itself
    // (when the model skipped the wrapper).
    let items_value = match &root {
        serde_json::Value::Object(map) => match map.get("configurations") {
            Some(v) => v.clone(),
            // No `configurations` key. If the object itself looks like
            // a single Phase8ParseItem (has `label` + `description`),
            // wrap it. Otherwise preserve the historical default of
            // "ignore unknown shape" and return empty.
            None => {
                if map.contains_key("label") && map.contains_key("description") {
                    serde_json::Value::Array(vec![root.clone()])
                } else {
                    return Ok(Vec::new());
                }
            }
        },
        serde_json::Value::Array(_) => root.clone(),
        _ => {
            return Err(Error::Serialization(format!(
                "phase 8 (configuration) response is not valid JSON: \
                 expected object or array at root, got {root}"
            )));
        }
    };

    // Coerce a map-of-configs into an array of values.
    let items_array = match items_value {
        serde_json::Value::Array(arr) => arr,
        serde_json::Value::Object(map) => map.into_iter().map(|(_, v)| v).collect(),
        other => {
            return Err(Error::Serialization(format!(
                "phase 8 (configuration) response is not valid JSON: \
                 `configurations` must be an array or object, got {other}"
            )));
        }
    };

    serde_json::from_value(serde_json::Value::Array(items_array)).map_err(|e| {
        Error::Serialization(format!(
            "phase 8 (configuration) response is not valid JSON: {e}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::pipeline::atlas::{DiscourseAct, EpistemicStatus};
    use std::collections::HashMap;

    #[test]
    fn custom_phase6_classifier_is_ontology_driven_not_literary() {
        let s = custom_phase6_classifier_system(
            "Rules about overnight guests, quiet hours, and chores.",
            "conflict",
            "rule",
        );
        // The domain's own term + guidance are present…
        assert!(s.contains("conflict"), "tension_term not filled");
        assert!(
            s.contains("Rules about overnight guests"),
            "guidance not injected"
        );
        // …no unfilled placeholders…
        assert!(!s.contains("{tension_term}"), "unfilled tension_term");
        assert!(!s.contains("{guidance}"), "unfilled guidance");
        // …and it is NOT the literary frame.
        assert!(!s.contains("Macbeth"), "leaked the literary classifier");
        // The literary classifier system, by contrast, IS character-framed —
        // proving the two paths are genuinely different prompts.
        assert!(PHASE6_CLASSIFIER_SYSTEM.contains("Macbeth"));
    }

    fn sample_chapter() -> ChapterInput {
        ChapterInput {
            chapter_id: "sec_0001".into(),
            title: "The Elder's Counsel".into(),
            text: "Zosima laid his hand upon Alyosha's head.".into(),
            metadata: HashMap::new(),
            approx_tokens: 10,
        }
    }

    #[test]
    fn seed_strategy_is_llm() {
        let p = LiteraryAtlasPipeline::new();
        assert_eq!(p.seed_strategy(), SeedStrategy::Llm);
    }

    // ── Phase 8 tolerant parser ─────────────────────────────────

    #[test]
    fn phase8_parses_strict_array_shape() {
        // The documented happy path: `{configurations: [...]}`.
        let response = r#"{
          "configurations": [
            {
              "label": "Developmental Framework",
              "description": "Reads the corpus as a chronological progression.",
              "interpretive_note": "Strongest in early sections.",
              "confidence": 0.85,
              "constituent_atoms": ["entity-0001"],
              "evidence_chunk_ids": ["sec_0001"]
            }
          ]
        }"#;
        let items = parse_phase8_configuration_tolerant(response).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "Developmental Framework");
        assert!((items[0].confidence - 0.85).abs() < 1e-6);
    }

    #[test]
    fn phase8_tolerates_map_keyed_configurations() {
        // Observed on SEP descartes ingest: model returned
        // `configurations` as a keyed dict instead of an array. The
        // tolerant parser coerces the dict values into a Vec.
        let response = r#"{
          "configurations": {
            "developmental": {
              "label": "Developmental Framework",
              "description": "Chronological reading.",
              "interpretive_note": "First half evidence."
            },
            "doctrinal": {
              "label": "Doctrinal Framework",
              "description": "Reads positions as a system.",
              "interpretive_note": "Holistic synthesis."
            }
          }
        }"#;
        let items = parse_phase8_configuration_tolerant(response).unwrap();
        assert_eq!(items.len(), 2);
        // Map iteration order is preserved (serde_json::Map is
        // backed by IndexMap when `preserve_order` is on; either way
        // both items should appear).
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Developmental Framework"));
        assert!(labels.contains(&"Doctrinal Framework"));
    }

    #[test]
    fn phase8_tolerates_bare_array_root() {
        // Model skipped the wrapper object entirely.
        let response = r#"[
          {
            "label": "Sole Reading",
            "description": "Only one plausible framing.",
            "interpretive_note": "Atoms cohere uniquely."
          }
        ]"#;
        let items = parse_phase8_configuration_tolerant(response).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "Sole Reading");
    }

    #[test]
    fn phase8_no_json_returns_empty_not_error() {
        // Observed on SEP culture-cogsci ingest: model emitted only
        // a `<think>` trace, never produced JSON. The prompt allows
        // "0–3 configurations" so zero is a valid answer — must not
        // fail the build step.
        let response = "<think>\nThis input is too ambiguous to fit a configuration.\n</think>";
        let items = parse_phase8_configuration_tolerant(response).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn phase8_empty_configurations_array_is_zero() {
        let response = r#"{ "configurations": [] }"#;
        let items = parse_phase8_configuration_tolerant(response).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn phase8_rejects_malformed_items() {
        // The response is well-formed JSON and routes through the
        // tolerant parser successfully up to the per-item deserialize
        // — but the items are missing required fields. That's a real
        // shape error worth surfacing (not the kind of model variance
        // we want to silently swallow).
        let response = r#"{"configurations": [{"label": "x"}]}"#;
        let err = parse_phase8_configuration_tolerant(response).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("phase 8 (configuration)"),
            "error must name the phase: {msg}"
        );
    }

    #[test]
    fn compose_seed_prompt_uses_seed_asset_and_first_section_body() {
        let p = LiteraryAtlasPipeline::new();
        let prompt = p
            .compose_seed_prompt(&sample_chapter())
            .expect("Llm strategy returns Some");
        // System preamble identifies itself as the seed pass.
        assert!(prompt.system.contains("seed entity extraction"));
        assert!(prompt.system.contains("seed entity list"));
        // User body carries the chapter text verbatim.
        assert!(prompt.user.contains("The Elder's Counsel"));
        assert!(prompt.user.contains("Zosima laid his hand"));
    }

    #[test]
    fn parse_seed_response_extracts_typed_entries() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "entries": [
            {
              "canonical_name": "Fyodor Pavlovich Karamazov",
              "aliases": ["Fyodor", "the father"],
              "entity_type": "person",
              "description": "Patriarch of the Karamazov household."
            },
            {
              "canonical_name": "Alyosha",
              "aliases": ["Alexei Fyodorovich", "Alyoshka"],
              "entity_type": "person",
              "description": "Youngest brother; novice at the monastery."
            }
          ]
        }"#;
        let entries = p.parse_seed_response(response).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].canonical_name, "Fyodor Pavlovich Karamazov");
        assert_eq!(entries[0].aliases.len(), 2);
        assert_eq!(entries[0].entity_type, EntityType::Person);
        assert_eq!(
            entries[1].description,
            "Youngest brother; novice at the monastery."
        );
    }

    #[test]
    fn parse_seed_response_strips_placeholder_literals_and_nulls() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "entries": [
            {
              "canonical_name": "Zossima",
              "aliases": [null, "Father Zossima", "..."],
              "entity_type": "person",
              "description": "..."
            },
            {
              "canonical_name": "...",
              "description": "placeholder"
            }
          ]
        }"#;
        let entries = p.parse_seed_response(response).unwrap();
        assert_eq!(entries.len(), 1, "placeholder canonical_name drops entry");
        assert_eq!(entries[0].canonical_name, "Zossima");
        assert_eq!(entries[0].aliases, vec!["Father Zossima".to_string()]);
        assert!(entries[0].description.is_empty());
    }

    #[test]
    fn parse_seed_response_errors_on_empty_entries() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{"entries": []}"#;
        let err = p.parse_seed_response(response).unwrap_err();
        assert!(format!("{err}").contains("no valid entity entries"));
    }

    #[test]
    fn parse_seed_response_strips_think_block() {
        let p = LiteraryAtlasPipeline::new();
        let response = "<think>considering the chapter's characters</think>\n\
            {\"entries\":[{\"canonical_name\":\"Fyodor\",\"entity_type\":\"person\",\"description\":\"x\"}]}";
        let entries = p.parse_seed_response(response).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].canonical_name, "Fyodor");
    }

    #[test]
    fn compose_phase1_with_seed_renders_canonical_names_block() {
        let p = LiteraryAtlasPipeline::new();
        let seed = SeedEntities {
            schema_version: SeedEntities::SCHEMA_VERSION,
            corpus_id: "brothers_karamazov".into(),
            origin: super::super::super::atlas::SeedOrigin::Llm,
            entries: vec![SeedEntity {
                canonical_name: "Alyosha".into(),
                aliases: vec!["Alexei Fyodorovich".into()],
                entity_type: EntityType::Person,
                description: "Youngest Karamazov brother.".into(),
            }],
            written_at: "t".into(),
        };
        let prompt = p.compose_phase1_with_seed(&sample_chapter(), &[], Some(&seed));
        assert!(prompt.user.contains("Known canonical names"));
        assert!(prompt.user.contains("**Alyosha**"));
        assert!(prompt.user.contains("Alexei Fyodorovich"));
        assert!(prompt.user.contains("Youngest Karamazov brother"));
        // Chapter body still present below the seed block.
        assert!(prompt.user.contains("The Elder's Counsel"));
    }

    #[test]
    fn compose_phase1_with_seed_none_matches_legacy_compose_phase1() {
        let p = LiteraryAtlasPipeline::new();
        let default_prompt = p.compose_phase1(&sample_chapter(), &[]);
        let seed_none_prompt = p.compose_phase1_with_seed(&sample_chapter(), &[], None);
        assert_eq!(default_prompt.user, seed_none_prompt.user);
        assert_eq!(default_prompt.system, seed_none_prompt.system);
    }

    #[test]
    fn default_pipeline_has_no_seed_strategy() {
        use crate::enrichment::pipeline::pipelines::literary::LiteraryPipeline;
        let p = LiteraryPipeline::new();
        assert_eq!(p.seed_strategy(), SeedStrategy::None);
        // compose_seed_prompt returns None — the pipeline doesn't
        // know how to produce a seed.
        assert!(p.compose_seed_prompt(&sample_chapter()).is_none());
    }

    #[test]
    fn pipeline_id_is_literary_atlas() {
        let p = LiteraryAtlasPipeline::new();
        assert_eq!(p.id(), "literary_atlas");
    }

    #[test]
    fn compose_phase1_terse_uses_terse_preamble_and_omits_exemplars() {
        // Terse variant swaps the system preamble AND drops the
        // exemplar block, since the whole point is to save tokens
        // on a chapter that already blew past the budget.
        let p = LiteraryAtlasPipeline::new();
        let prompt = p
            .compose_phase1_terse(&sample_chapter())
            .expect("literary_atlas always returns Some");
        // Pin the terse-specific directive from the asset.
        assert!(
            prompt.system.contains("Do NOT show your reasoning"),
            "expected terse directive in system preamble"
        );
        // Default preamble's shape example should be gone — terse
        // asset drops it to save tokens.
        assert!(!prompt.system.contains("EXAMPLE_ONLY_REPLACE_ME"));
        // User body still carries the chapter id + title so the
        // model has something to ground on.
        assert!(prompt.user.contains("sec_0001"));
        assert!(prompt.user.contains("The Elder's Counsel"));
        // No exemplar header — even if an exemplar bank existed,
        // the terse path wouldn't thread it through.
        assert!(!prompt.user.contains("# Reference exemplars"));
    }

    #[test]
    fn compose_phase1_terse_is_shorter_than_default_variant() {
        // Sanity: the whole reason this variant exists is to use
        // fewer tokens in the prompt so more are available for the
        // JSON answer. Pin that it's strictly smaller than the
        // default at an identical chapter input.
        let p = LiteraryAtlasPipeline::new();
        let default_prompt = p.compose_phase1(&sample_chapter(), &[]);
        let terse_prompt = p
            .compose_phase1_terse(&sample_chapter())
            .expect("literary_atlas always returns Some");
        let default_total = default_prompt.system.len() + default_prompt.user.len();
        let terse_total = terse_prompt.system.len() + terse_prompt.user.len();
        assert!(
            terse_total < default_total,
            "terse prompt should be smaller than default: terse={terse_total}, default={default_total}"
        );
    }

    #[test]
    fn compose_phase1_mentions_section_id_and_title() {
        let p = LiteraryAtlasPipeline::new();
        let prompt = p.compose_phase1(&sample_chapter(), &[]);
        assert!(prompt.user.contains("sec_0001"));
        assert!(prompt.user.contains("The Elder's Counsel"));
        // System preamble is the atlas one, not the legacy literary one.
        assert!(prompt.system.contains("atlas extraction"));
    }

    #[test]
    fn parse_phase1_roundtrips_slim_extraction() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "entities_introduced": [{
            "canonical_name": "Alyosha",
            "entity_type": "person",
            "description": "Youngest Karamazov, novice at the monastery.",
            "anchor": "Alyosha knelt at Zosima's feet"
          }],
          "entities_developed": [{
            "entity_name": "Alyosha",
            "label": "Unshaken faith meeting mortality",
            "anchor": "without Zosima in it"
          }],
          "events": [{
            "description": "Zosima instructs Alyosha to leave the monastery.",
            "participants": ["Zosima", "Alyosha"],
            "anchor": "go out into the world"
          }],
          "claims": [{
            "content": "Active love costs more than dreamt love.",
            "discourse_act": "argue",
            "epistemic_status": "confident",
            "attributed_to": "Zosima",
            "anchor": "love in dreams is greedy"
          }],
          "questions_raised": [{
            "content": "Can a faith formed in the cell survive the world outside?",
            "anchor": "faith in the cell"
          }]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();

        // Legacy fields are derived for back-compat with v1 Phase 2/3.
        assert_eq!(parsed.questions.len(), 1);
        assert!(parsed.questions[0].contains("faith"));
        // Plot = first event description.
        assert!(parsed
            .plot
            .as_deref()
            .unwrap()
            .contains("leave the monastery"));
        // Thematic carriers deduplicated across entity sketches.
        assert_eq!(parsed.thematic_carriers, vec!["Alyosha".to_string()]);
        // Atlas structure preserved intact.
        let extraction = parsed.section_extraction.expect("should carry atlas");
        assert_eq!(extraction.entities_introduced.len(), 1);
        assert_eq!(
            extraction.entities_introduced[0].entity_type,
            EntityType::Person
        );
        assert_eq!(extraction.claims[0].discourse_act, DiscourseAct::Argue);
        assert_eq!(
            extraction.claims[0].epistemic_status,
            EpistemicStatus::Confident
        );
        // Anchors preserved.
        assert_eq!(extraction.claims[0].anchor, "love in dreams is greedy");
    }

    #[test]
    fn parse_phase1_rejects_empty_extraction() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{"section_id": "sec_0001"}"#;
        let err = p.parse_phase1(response).unwrap_err();
        assert!(format!("{err}").contains("did not extract"), "got: {err}");
    }

    #[test]
    fn parse_phase1_rejects_extraction_with_no_questions() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "entities_introduced": [{
            "canonical_name": "Alyosha",
            "entity_type": "person"
          }]
        }"#;
        let err = p.parse_phase1(response).unwrap_err();
        assert!(format!("{err}").contains("questions"), "got: {err}");
    }

    #[test]
    fn parse_phase1_drops_unknown_entity_type_tag() {
        // The daemon's grammar-constrained sampler is a known no-op
        // (see sovereign-inference embedded.rs build_sampler comment),
        // so unknown entity_type tags reach the parser. Models hedge
        // borderline cases — "the narrator" gets typed as
        // "unspecified", a personified force gets typed as "deity",
        // an abstract group gets typed as "collective". Persisting
        // these as Other(_) atoms pollutes downstream phases (the
        // narrator becomes a forbidden_person hit; "deity" never
        // matches expected_concept_atoms because no golden lists it).
        // Drop the atom — if the model can't commit to one of the 5
        // named types, the atom isn't load-bearing enough to keep.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "entities_introduced": [{
            "canonical_name": "Grace",
            "entity_type": "deity",
            "description": "A personified force in the text"
          }],
          "questions_raised": [{"content": "Is grace earned or given?"}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let extraction = parsed.section_extraction.unwrap();
        assert!(
            extraction.entities_introduced.is_empty(),
            "entity with non-standard entity_type should be dropped, got: {:?}",
            extraction.entities_introduced
        );
    }

    #[test]
    fn parse_phase1_retypes_school_names_from_person_to_concept() {
        // Models occasionally type school names that appear repeatedly
        // ("virtue ethics", "situationism", "Neo-Aristotelianism") as
        // Person — they read as agents in dialectical prose. Retype
        // them on parse so the cross-position tension enumerator can
        // pair them as concepts.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "entities_introduced": [
            {"canonical_name": "virtue ethics", "entity_type": "person",
             "description": "A school", "anchor": "virtue ethics"},
            {"canonical_name": "situationism", "entity_type": "person",
             "description": "Another school", "anchor": "situationism"},
            {"canonical_name": "Neo-Aristotelianism", "entity_type": "person",
             "description": "Third school", "anchor": "Neo-Aristotelianism"},
            {"canonical_name": "deontology", "entity_type": "person",
             "description": "Field of ethics", "anchor": "deontology"},
            {"canonical_name": "Epicureans", "entity_type": "person",
             "description": "Plural school name", "anchor": "Epicureans"},
            {"canonical_name": "Neo-Aristotleians", "entity_type": "person",
             "description": "Plural with typo", "anchor": "Neo-Aristotleians"},
            {"canonical_name": "Aristotle", "entity_type": "person",
             "description": "Real philosopher", "anchor": "Aristotle"},
            {"canonical_name": "Christian", "entity_type": "person",
             "description": "A real first name (negative test)", "anchor": "Christian"}
          ],
          "questions_raised": [{"content": "What is virtue?", "anchor": "virtue"}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let ents = parsed.section_extraction.unwrap().entities_introduced;
        let by_name: std::collections::HashMap<_, _> = ents
            .iter()
            .map(|e| (e.canonical_name.clone(), e.entity_type.clone()))
            .collect();
        assert_eq!(by_name.get("virtue ethics"), Some(&EntityType::Concept));
        assert_eq!(by_name.get("situationism"), Some(&EntityType::Concept));
        assert_eq!(
            by_name.get("Neo-Aristotelianism"),
            Some(&EntityType::Concept)
        );
        assert_eq!(by_name.get("deontology"), Some(&EntityType::Concept));
        assert_eq!(by_name.get("Epicureans"), Some(&EntityType::Concept));
        assert_eq!(by_name.get("Neo-Aristotleians"), Some(&EntityType::Concept));
        assert_eq!(by_name.get("Aristotle"), Some(&EntityType::Person));
        // Negative test: a singular -ian name that's a real first name
        // should be left as Person.
        assert_eq!(by_name.get("Christian"), Some(&EntityType::Person));
    }

    #[test]
    fn parse_phase1_strips_think_block_before_parsing() {
        let p = LiteraryAtlasPipeline::new();
        let response = "<think>reasoning about what to extract…</think>\n\
            {\"section_id\":\"sec_0001\",\
             \"claims\":[{\"content\":\"c\",\"discourse_act\":\"enact\",\
                          \"epistemic_status\":\"confident\"}],\
             \"questions_raised\":[{\"content\":\"q?\"}]}";
        let parsed = p.parse_phase1(response).unwrap();
        assert_eq!(parsed.questions, vec!["q?".to_string()]);
    }

    #[test]
    fn parse_phase1_drops_malformed_claims_keeps_rest_of_extraction() {
        // A claim missing discourse_act is malformed — we drop it.
        // The other claim plus questions + entity survive.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "entities_introduced": [{
            "canonical_name": "Alyosha",
            "entity_type": "person"
          }],
          "claims": [
            {"content": "no discourse act here"},
            {"content": "a proper claim", "discourse_act": "argue", "epistemic_status": "confident"}
          ],
          "questions_raised": [{"content": "Why?"}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let extraction = parsed.section_extraction.unwrap();
        assert_eq!(
            extraction.claims.len(),
            1,
            "malformed claim should be dropped"
        );
        assert_eq!(extraction.claims[0].content, "a proper claim");
        assert_eq!(extraction.entities_introduced.len(), 1);
    }

    #[test]
    fn parse_phase1_defaults_epistemic_status_for_claims() {
        // Narrative prose claims default epistemic_status=Confident
        // when missing; only discourse_act is mandatory.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "claims": [{
            "content": "Passion outside the social order is self-destructive.",
            "discourse_act": "enact"
          }],
          "questions_raised": [{"content": "What becomes of unbound passion?"}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let c = &parsed.section_extraction.unwrap().claims[0];
        assert_eq!(c.discourse_act, DiscourseAct::Enact);
        assert_eq!(c.epistemic_status, EpistemicStatus::Confident);
    }

    #[test]
    fn sanitize_phase1_object_arrays_keeps_object_valued_fields() {
        // Ontology-v1 sketches carry an `attributes` OBJECT. The sanitizer
        // drops non-object ITEMS from the sketch arrays and must leave a
        // sketch's own object-valued fields intact.
        let mut value = serde_json::json!({
            "entities_introduced": [
                {
                    "canonical_name": "coin",
                    "entity_type": "coin",
                    "attributes": { "weight": 1.29, "mint": "London" }
                },
                "// stray comment string"
            ]
        });
        sanitize_phase1_object_arrays(&mut value);
        let items = value["entities_introduced"].as_array().unwrap();
        assert_eq!(items.len(), 1, "the stray string is dropped");
        assert_eq!(items[0]["attributes"]["weight"], 1.29);
        assert_eq!(items[0]["attributes"]["mint"], "London");
    }

    #[test]
    fn phase1_section_extraction_schema_parses_as_valid_json() {
        // Pin the schema-string-vs-JSON-validity contract so a typo
        // in the const fails at unit-test time rather than at first
        // grammar-constrained chat call. The helper itself
        // `expect()`s parse success, so this also asserts the
        // fallback path won't panic in production.
        let schema = phase1_section_extraction_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["section_id"].is_object());
        assert!(schema["properties"]["questions_raised"].is_object());
        // `$defs` carries the per-sketch object schemas — most likely
        // place to fluff a typo when adding a new sketch type.
        let defs = &schema["$defs"];
        for sketch in [
            "entity_sketch",
            "entity_state_sketch",
            "relation_sketch",
            "relation_state_sketch",
            "event_sketch",
            "claim_sketch",
            "question_sketch",
        ] {
            assert!(defs[sketch].is_object(), "missing $defs/{sketch}");
        }
    }

    #[test]
    fn compose_phase1_attaches_response_schema_for_grammar_constraint() {
        // Regression: every Phase 1 prompt path (default, with-seed,
        // terse) must carry the response_schema so the daemon's
        // grammar-constrained sampler engages. Without this the
        // schema is silently dropped and we're back to malformed
        // JSON drift on Gemma-31B / Qwopus-27B.
        let p = LiteraryAtlasPipeline::new();
        let chap = sample_chapter();
        let default_prompt = p.compose_phase1(&chap, &[]);
        assert_eq!(
            default_prompt.response_schema_name.as_deref(),
            Some("phase1_section_extraction")
        );
        assert!(default_prompt.response_schema.is_some());

        let terse_prompt = p.compose_phase1_terse(&chap).expect("terse variant");
        assert!(terse_prompt.response_schema.is_some());

        let seed_prompt = p.compose_phase1_with_seed(&chap, &[], None);
        assert!(seed_prompt.response_schema.is_some());
    }

    #[test]
    fn parse_phase1_keeps_last_value_on_duplicate_keys() {
        // Observed on Gemma-31B running sep-al-farabi sec_0003: the
        // model emitted the same `description` field twice on a
        // single entity, possibly from a self-correction mid-stream.
        // The Value-first parse path silently keeps the last value;
        // we lose the first description but keep the section.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "entities_introduced": [{
            "canonical_name": "kalâm",
            "entity_type": "concept",
            "description": "First description.",
            "description": "Replacement description.",
            "anchor": "kalam"
          }],
          "claims": [{"content": "X.", "discourse_act": "assert"}],
          "questions_raised": [{"content": "What is kalâm?"}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let e = &parsed.section_extraction.unwrap().entities_introduced[0];
        assert_eq!(e.description, "Replacement description.");
    }

    #[test]
    fn parse_phase1_filters_comment_strings_from_object_arrays() {
        // Observed on Gemma-31B running sep-al-farabi sec_0003 retry:
        // the model interleaved `"//"` literal strings between entity
        // objects, presumably as commentary. The pre-pass strips
        // those so the typed deserializer sees only valid struct or
        // null entries.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "entities_introduced": [
            "//",
            {
              "canonical_name": "Al-Fârâbî",
              "entity_type": "person",
              "description": "Philosopher.",
              "anchor": "Al-Farabi"
            },
            "// stray note from the model"
          ],
          "claims": [{"content": "Y.", "discourse_act": "assert"}],
          "questions_raised": [{"content": "Who is Al-Fârâbî?"}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let entities = &parsed.section_extraction.unwrap().entities_introduced;
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].canonical_name, "Al-Fârâbî");
    }

    #[test]
    fn parse_phase1_flattens_array_attributed_to_for_claims() {
        // Observed on Qwopus3.5-27B running sep-african-sage sec_0002:
        // the model emitted `attributed_to: ["Henry Oruka", "Kwasi
        // Wiredu"]` for a co-attributed claim. The schema asks for a
        // single string, but losing the whole claim over a stylistic
        // drift in attribution shape is too costly. The parser
        // flattens arrays via the same string-coercion adapter that
        // hardens Phase 3 metadata.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "claims": [{
            "content": "African sage philosophy admits both individual and collective authorship.",
            "discourse_act": "argue",
            "attributed_to": ["Henry Oruka", "Kwasi Wiredu"]
          }],
          "questions_raised": [{"content": "Who counts as a sage?"}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let c = &parsed.section_extraction.unwrap().claims[0];
        assert_eq!(
            c.attributed_to.as_deref(),
            Some("Henry Oruka, Kwasi Wiredu")
        );
    }

    #[test]
    fn parse_phase1_drops_relation_without_two_participants() {
        // Relations inherently involve at least two entities. A one-
        // participant relation is a schema echo or hallucination.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "relations_introduced": [
            {"participants": ["solo"], "label": "lonely bond"},
            {"participants": ["A", "B"], "label": "real bond"}
          ],
          "questions_raised": [{"content": "?"}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let rels = &parsed.section_extraction.unwrap().relations_introduced;
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].label, "real bond");
    }

    #[test]
    fn compose_phase3_facet_selects_right_preamble_per_facet() {
        // Pin the asset-per-facet routing. Each facet carries a
        // distinctive phrase from its Phase 3 prompt that we can
        // match against the returned ChatPrompt.system.
        let p = LiteraryAtlasPipeline::new();
        let cluster = AtlasCluster {
            id: "claim_cl_0001".into(),
            facet: Facet::Claim,
            refs: vec![],
        };
        let excerpts = vec![SketchExcerpt {
            section_id: "sec_0001".into(),
            content: "[enact/confident] love costs".into(),
            anchor: String::new(),
        }];
        let claim_prompt = p
            .compose_phase3_facet(&cluster, Facet::Claim, &excerpts, &[])
            .expect("atlas pipeline supports claim facet naming");
        assert!(claim_prompt.system.contains("position family"));

        let trajectory = AtlasCluster {
            id: "entity_state_cl_0001".into(),
            facet: Facet::EntityState,
            refs: vec![],
        };
        let es_prompt = p
            .compose_phase3_facet(&trajectory, Facet::EntityState, &excerpts, &[])
            .unwrap();
        assert!(es_prompt.system.contains("trajectory arc"));

        let relation = AtlasCluster {
            id: "relation_state_cl_0001".into(),
            facet: Facet::RelationState,
            refs: vec![],
        };
        let rs_prompt = p
            .compose_phase3_facet(&relation, Facet::RelationState, &excerpts, &[])
            .unwrap();
        assert!(rs_prompt.system.contains("relational"));

        let events = AtlasCluster {
            id: "event_cl_0001".into(),
            facet: Facet::Event,
            refs: vec![],
        };
        let ev_prompt = p
            .compose_phase3_facet(&events, Facet::Event, &excerpts, &[])
            .unwrap();
        assert!(ev_prompt.system.contains("narrative thread"));
    }

    #[test]
    fn parse_phase3_facet_roundtrips_label_and_metadata() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "label": "Jane's movement from self-protective observation to acknowledged love.",
          "metadata": {
            "entity_name": "Jane",
            "scope": "novel-wide"
          }
        }"#;
        let parsed = p.parse_phase3_facet(Facet::EntityState, response).unwrap();
        assert!(parsed.label.contains("Jane's movement"));
        assert_eq!(parsed.metadata.get("entity_name").unwrap(), "Jane");
    }

    #[test]
    fn parse_phase3_facet_rejects_empty_label() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{"label": ""}"#;
        let err = p.parse_phase3_facet(Facet::Claim, response).unwrap_err();
        assert!(format!("{err}").contains("label"));
    }

    #[test]
    fn parse_phase3_facet_flattens_array_metadata_values() {
        // The relation-state preamble explicitly asks for
        // `participants: ["a", "b"]`. The downstream metadata bag is
        // flat strings, so the parser flattens arrays by joining
        // string elements with ", " — model can be schema-faithful
        // without breaking the parser.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "label": "Frankfurt vs Fischer convergence on PAP rejection.",
          "metadata": {
            "participants": ["Harry Frankfurt", "John Martin Fischer"],
            "dynamic_type": "convergence"
          }
        }"#;
        let parsed = p
            .parse_phase3_facet(Facet::RelationState, response)
            .unwrap();
        assert!(parsed.label.contains("Frankfurt"));
        assert_eq!(
            parsed.metadata.get("participants").unwrap(),
            "Harry Frankfurt, John Martin Fischer"
        );
        assert_eq!(parsed.metadata.get("dynamic_type").unwrap(), "convergence");
    }

    #[test]
    fn parse_phase3_facet_tolerates_null_metadata_values() {
        // Qwen occasionally emits `{"scope": null}` when unsure —
        // the parser treats null the same as omit.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "label": "A real label.",
          "metadata": {"scope": null, "entity_name": "Alyosha"}
        }"#;
        let parsed = p.parse_phase3_facet(Facet::EntityState, response).unwrap();
        assert!(!parsed.metadata.contains_key("scope"));
        assert_eq!(parsed.metadata.get("entity_name").unwrap(), "Alyosha");
    }

    #[test]
    fn default_pipeline_trait_methods_return_none_for_facet_naming() {
        // v1 LiteraryPipeline inherits the trait default → None.
        // Runners that try atlas naming on v1 pipelines get a
        // clear "unsupported" signal instead of a silent fallback.
        use crate::enrichment::pipeline::pipelines::literary::LiteraryPipeline;
        let p = LiteraryPipeline::new();
        let cluster = AtlasCluster {
            id: "x".into(),
            facet: Facet::Claim,
            refs: vec![],
        };
        let out = p.compose_phase3_facet(&cluster, Facet::Claim, &[], &[]);
        assert!(out.is_none());
    }

    #[test]
    fn parse_phase1_scrubs_placeholder_fields() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "entities_introduced": [{
            "canonical_name": "Alyosha",
            "entity_type": "person",
            "description": "...",
            "anchor": "…"
          }],
          "questions_raised": [{"content": "A real question."}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let extraction = parsed.section_extraction.unwrap();
        assert!(extraction.entities_introduced[0].description.is_empty());
        assert!(extraction.entities_introduced[0].anchor.is_empty());
    }
}

// ── AtlasIngestion adapter ───────────────────────────────────
//
// Scaffolded during Step 1 back-fill so a future landing can wire
// the full 8-phase extraction-first flow through the
// `AtlasIngestion` trait without further module reshuffling. Today
// the adapter exists so the trait is exercised and the registry has
// `extraction_first` registered; the actual end-to-end ingestion is
// still driven by the per-phase CLI subcommands
// (`sovereign enrich extract`, `atlas-resolve`, etc.). A later step
// will consolidate the per-phase drivers into a single `ingest()`
// call that returns a populated `AtlasData`.

/// Adapter wrapping the `literary_atlas` extraction pipeline as the
/// canonical `AtlasIngestion` implementation for the
/// `extraction_first` strategy.
pub struct ExtractionFirstAdapter;

impl ExtractionFirstAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ExtractionFirstAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AtlasIngestion for ExtractionFirstAdapter {
    fn id(&self) -> &'static str {
        "extraction_first"
    }

    fn name(&self) -> &'static str {
        "Extraction-first (LLM per-section atlas)"
    }

    fn ingest<'a>(
        &'a self,
        _corpus: Arc<CorpusEngine>,
        _embed_fn: EmbedFn,
        _inference_fn: Option<InferenceFn>,
        _config: AtlasIngestionConfig,
        _progress: Arc<ProgressCallback>,
    ) -> Pin<Box<dyn Future<Output = Result<AtlasData>> + Send + 'a>> {
        // Scaffolded: returns an empty atlas bundle at `Extracted`
        // depth, pending the later landing that chains Phase 1-8
        // into one `ingest()` invocation. The adapter is registered
        // so the registry contract holds; callers that want real
        // atlas output drive the per-phase CLI subcommands today.
        Box::pin(async move {
            Ok(AtlasData {
                atoms: serde_json::json!([]),
                edges: serde_json::json!([]),
                trajectories: serde_json::json!({}),
                manifest: serde_json::json!({}),
                schema_validation: serde_json::json!({
                    "note": "ExtractionFirstAdapter::ingest is scaffolded \
                             for the Open/Closed surface; real atlas \
                             output is produced today by the per-phase \
                             CLI subcommands (extract, atlas-resolve, \
                             name-atlas-clusters, etc.)."
                }),
                dominant_depth: EnrichmentDepth::Extracted,
            })
        })
    }
}

/// Register the extraction-first strategy into an atlas-ingestion
/// registry. Called from
/// `enrichment::atlas::registry::AtlasIngestionRegistry::builtin`
/// so the registry file stays free of strategy-specific imports
/// beyond the trait.
pub fn register_extraction_first(registry: &mut AtlasIngestionRegistry) {
    registry.register("extraction_first", || {
        Arc::new(ExtractionFirstAdapter::new())
    });
}

#[cfg(test)]
mod adapter_tests {
    use super::*;

    #[test]
    fn extraction_first_adapter_identifies_as_extraction_first() {
        let a = ExtractionFirstAdapter::new();
        assert_eq!(a.id(), "extraction_first");
    }

    #[test]
    fn register_extraction_first_populates_registry() {
        let mut r = AtlasIngestionRegistry::new();
        register_extraction_first(&mut r);
        assert_eq!(r.strategy_ids(), vec!["extraction_first"]);
        assert!(r.get("extraction_first").is_some());
    }

    #[test]
    fn extraction_first_adapter_metadata_is_stable() {
        // Step-1 scaffold: the adapter identifies itself + name.
        // Pins the contract so the registry lookup + status output
        // remain consistent when the full ingest lands.
        let a = ExtractionFirstAdapter::new();
        assert_eq!(a.id(), "extraction_first");
        assert_eq!(a.name(), "Extraction-first (LLM per-section atlas)");
    }
}
