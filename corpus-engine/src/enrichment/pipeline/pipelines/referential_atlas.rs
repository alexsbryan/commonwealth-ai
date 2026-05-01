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

use super::super::atlas::{
    EntitySketch, SectionExtraction, SeedEntities, SeedStrategy,
};
use super::super::exemplar_bank::Exemplar;
use super::super::trait_def::Pipeline;
use super::super::types::*;
use super::literary_atlas::{
    parse_phase1b_coverage_response, phase1_section_extraction_schema,
    render_generic_phase3_exemplar, render_phase1_user_body,
    render_phase1b_user_body, LiteraryAtlasPipeline,
};
use crate::enrichment::domain::ClusteringConfig;
use crate::error::Result;

// ── Referential-specific prompt assets ───────────────────────

const PHASE1_ATLAS_SYSTEM: &str =
    include_str!("referential_atlas_prompts/phase1_system.md");

const PHASE1_ATLAS_SYSTEM_TERSE: &str =
    include_str!("referential_atlas_prompts/phase1_system_terse.md");

const PHASE1A_SEED_SYSTEM: &str =
    include_str!("referential_atlas_prompts/phase1a_seed_system.md");

const PHASE1B_ENTITY_COVERAGE: &str =
    include_str!("referential_atlas_prompts/phase1b_entity_coverage.md");

const PHASE1B_CONCEPT_COVERAGE: &str =
    include_str!("referential_atlas_prompts/phase1b_concept_coverage.md");

const PHASE3_QUESTION_NAMING: &str =
    include_str!("referential_atlas_prompts/phase3_question_naming.md");
const PHASE3_CLAIM_NAMING: &str =
    include_str!("referential_atlas_prompts/phase3_claim_naming.md");
const PHASE3_ENTITY_STATE_NAMING: &str =
    include_str!("referential_atlas_prompts/phase3_entity_state_trajectory_naming.md");
const PHASE3_RELATION_STATE_NAMING: &str =
    include_str!("referential_atlas_prompts/phase3_relation_state_trajectory_naming.md");
const PHASE3_EVENT_NAMING: &str =
    include_str!("referential_atlas_prompts/phase3_event_thread_naming.md");

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

    // ── System preambles ──────────────────────────────────────

    fn phase1_system(&self) -> &'static str {
        PHASE1_ATLAS_SYSTEM
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

    fn compose_phase1(
        &self,
        chapter: &ChapterInput,
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        let user = render_phase1_user_body(
            chapter,
            exemplars,
            /*include_exemplars=*/ true,
            /*seed=*/ None,
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
            ChatPrompt::new(PHASE1_ATLAS_SYSTEM_TERSE, user)
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
            ChatPrompt::new(PHASE1B_ENTITY_COVERAGE, user)
                .with_phase_id("phase1b_entity"),
        )
    }

    fn compose_phase1b_concept_coverage(
        &self,
        chapter: &ChapterInput,
        existing: &SectionExtraction,
    ) -> Option<ChatPrompt> {
        let user = render_phase1b_user_body(chapter, existing);
        Some(
            ChatPrompt::new(PHASE1B_CONCEPT_COVERAGE, user)
                .with_phase_id("phase1b_concept"),
        )
    }

    fn parse_phase1b_coverage(
        &self,
        response: &str,
    ) -> Result<Vec<EntitySketch>> {
        parse_phase1b_coverage_response(response)
    }

    fn compose_phase1_with_seed(
        &self,
        chapter: &ChapterInput,
        exemplars: &[&Exemplar],
        seed: Option<&SeedEntities>,
    ) -> ChatPrompt {
        let user = render_phase1_user_body(
            chapter,
            exemplars,
            /*include_exemplars=*/ true,
            seed,
        );
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
    // runner skips Phase 1a entirely. The PHASE1A_SEED_SYSTEM asset
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
            Facet::Question => PHASE3_QUESTION_NAMING,
            Facet::Claim => PHASE3_CLAIM_NAMING,
            Facet::EntityState => PHASE3_ENTITY_STATE_NAMING,
            Facet::RelationState => PHASE3_RELATION_STATE_NAMING,
            Facet::Event => PHASE3_EVENT_NAMING,
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

        Some(ChatPrompt::new(system, user).with_phase_id("phase3_facet"))
    }

    fn parse_phase3_facet(
        &self,
        facet: Facet,
        response: &str,
    ) -> Result<Phase3FacetParseResult> {
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

#[cfg(test)]
mod tests {
    use super::*;

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

}
