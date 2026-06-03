//! Conversation-history atlas pipeline.
//!
//! Chat transcripts (claude.ai exports today; future: Slack, iMessage,
//! email threads via parallel extractors) are a different shape from
//! authored prose. Each chunk is a *turn-pair*:
//!
//! ```text
//! ### [2025-09-04 18:01] user
//! …user's question…
//! ### [2025-09-04 18:02] assistant
//! …assistant's reply…
//! ```
//!
//! Two structural facts drive every prompt divergence in this
//! pipeline:
//!
//! 1. **The user is the voice** — first-person `### [...] user`
//!    blocks. Their stances, decisions, plans, and questions are the
//!    load-bearing atoms; everything else exists to give them context.
//!    The user is NEVER a Person atom (same rule as obsidian_atlas's
//!    author-as-voice convention).
//! 2. **The assistant is a generation surface** — `### [...] assistant`
//!    blocks. The assistant is NEVER a Person atom. Its statements
//!    rarely matter for the user's atlas; when they do, they're
//!    tagged `attributed_to: "assistant"` so downstream filtering can
//!    isolate user-authored content.
//!
//! v1 is a forwarding wrapper over `literary_atlas`, diverging only at
//! Phase 1 — the layer where atom shape is decided. Phases 3-7 inherit
//! literary's calibration; we'll fork them lazily when the bench shows
//! they need it. Forking from day one (per obsidian_atlas's "cheaper
//! than forking later" rationale) keeps tuning commits scoped to this
//! file.
//!
//! Pipeline runs via `sovereign enrich init <corpus> --pipeline
//! conversation_atlas`. The recipe path (`conversations-anthropic`)
//! also drives in-line entity extraction via the `conversational`
//! domain; the two paths are complementary — in-line catches Person /
//! Org / Initiative quickly during ingest, the atlas pipeline runs
//! later for the full typed atom + edge graph.

use std::sync::Arc;

use super::super::atlas::{
    EntitySketch, SectionExtraction, SeedEntities, SeedEntity, SeedStrategy,
};
use super::super::exemplar_bank::Exemplar;
use super::super::trait_def::Pipeline;
use super::super::types::*;
use super::literary_atlas::{
    phase1_section_extraction_schema, render_phase1_user_body, LiteraryAtlasPipeline,
};
use crate::enrichment::domain::ClusteringConfig;
use crate::error::Result;

/// Pipeline id exposed by the registry. Stable; the recipe + CLI
/// pass this string.
pub const PIPELINE_ID: &str = "conversation_atlas";

/// Conversation-flavored Phase 1 system preamble. Diverges from
/// `literary_atlas` (and from `obsidian_atlas`) in these load-bearing
/// places, each driven by the structural facts of chat transcripts:
///   1. Turn-block format is named upfront so the model knows the
///      `### [ts] user` / `### [ts] assistant` markers are
///      structural, not content.
///   2. The user (the voice behind `### [...] user`) is NEVER a
///      Person atom — same shape as obsidian's author/narrator rule.
///   3. The assistant (Claude / GPT / "the model") is NEVER a Person
///      atom — assistants are generation surfaces, not humans.
///   4. Timestamps and IDs (`2025-09-04`, `q3-2025`, `PR-1234`,
///      `commit abc1234`) are NEVER Person atoms — covers
///      conversation-specific artifact shapes that wouldn't appear in
///      authored prose.
///   5. Decisions / commitments are Claims with
///      `discourse_act: "commit"` — the "decision atom" the bench
///      needs for "when did I decide X and why" questions.
///   6. Attribution rules are spelled out exhaustively: user's
///      claims omit `attributed_to`; claims the user attributes to a
///      third party carry the third party's name; rare
///      assistant-authored claims carry `attributed_to: "assistant"`.
///      Load-bearing for the bench runner's attribution_mode filter.
///   7. Concept atom examples are drawn from working-professional
///      conversation vocab (`runway`, `burn rate`, `tech debt`,
///      `OKR alignment`, `prompt overlay`) instead of literary or
///      vault-essay examples.
static PHASE1_CONVERSATION_SYSTEM: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| {
        crate::enrichment::pipeline::prompts::load_or_baked(
            "conversation_atlas/phase1_system.md",
            include_str!("conversation_atlas_prompts/phase1_system.md"),
        )
    });

/// Conversation-history atlas pipeline. Forwarding wrapper over
/// `LiteraryAtlasPipeline` with a divergent Phase 1 system prompt.
pub struct ConversationAtlasPipeline {
    inner: Arc<LiteraryAtlasPipeline>,
}

impl ConversationAtlasPipeline {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(LiteraryAtlasPipeline::new()),
        }
    }
}

impl Default for ConversationAtlasPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipeline for ConversationAtlasPipeline {
    // ── Identity. ────────────────────────────────────────────────

    fn id(&self) -> &'static str {
        PIPELINE_ID
    }

    fn name(&self) -> &'static str {
        "Conversation history — atlas atom graph"
    }

    fn vocabulary(&self) -> &Vocabulary {
        self.inner.vocabulary()
    }

    // ── System preambles. Phase 1 diverges; the rest delegate until
    //    bench data tells us otherwise.

    fn phase1_system(&self) -> &'static str {
        *PHASE1_CONVERSATION_SYSTEM
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

    // ── Phase 1 composition + terse retry + 1b coverage. Override
    //    `compose_phase1` / `compose_phase1_with_seed` so the
    //    user-body builder pairs with OUR system prompt; default
    //    delegation would re-resolve `phase1_system()` against the
    //    inner LiteraryAtlasPipeline and silently bypass every
    //    conversation-specific rule.

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

    fn compose_phase1_terse(&self, chapter: &ChapterInput) -> Option<ChatPrompt> {
        self.inner.compose_phase1_terse(chapter)
    }

    fn compose_phase1b_entity_coverage(
        &self,
        chapter: &ChapterInput,
        existing: &SectionExtraction,
    ) -> Option<ChatPrompt> {
        self.inner
            .compose_phase1b_entity_coverage(chapter, existing)
    }

    fn compose_phase1b_concept_coverage(
        &self,
        chapter: &ChapterInput,
        existing: &SectionExtraction,
    ) -> Option<ChatPrompt> {
        self.inner
            .compose_phase1b_concept_coverage(chapter, existing)
    }

    fn parse_phase1b_coverage(&self, response: &str) -> Result<Vec<EntitySketch>> {
        self.inner.parse_phase1b_coverage(response)
    }

    // ── Stage 1a — seed extraction. ──────────────────────────────

    fn seed_strategy(&self) -> SeedStrategy {
        self.inner.seed_strategy()
    }

    fn compose_seed_prompt(&self, first_section: &ChapterInput) -> Option<ChatPrompt> {
        self.inner.compose_seed_prompt(first_section)
    }

    fn parse_seed_response(&self, response: &str) -> Result<Vec<SeedEntity>> {
        self.inner.parse_seed_response(response)
    }

    fn extract_seed_structural(&self, ctx: &CorpusContext) -> Result<Vec<SeedEntity>> {
        self.inner.extract_seed_structural(ctx)
    }

    // ── Phase 3. ─────────────────────────────────────────────────

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
        self.inner
            .compose_phase3_facet(cluster, facet, excerpts, exemplars)
    }

    fn parse_phase3_facet(&self, facet: Facet, response: &str) -> Result<Phase3FacetParseResult> {
        self.inner.parse_phase3_facet(facet, response)
    }

    // ── Phase 5. ─────────────────────────────────────────────────

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

    // ── Phase 6. ─────────────────────────────────────────────────

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

    // ── Phase 7. ─────────────────────────────────────────────────

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

    // ── Clustering knobs. ────────────────────────────────────────

    fn question_clustering_config(&self) -> ClusteringConfig {
        self.inner.question_clustering_config()
    }

    fn chunk_clustering_config(&self) -> ClusteringConfig {
        self.inner.chunk_clustering_config()
    }

    // ── Phase 1 parser. ──────────────────────────────────────────

    fn parse_phase1(&self, response: &str) -> Result<Phase1ChapterResult> {
        self.inner.parse_phase1(response)
    }

    // ── Phase 8 — configuration patterns. Conversation history has
    //    real configurations to surface (the user's evolving stance
    //    on a topic, recurring open loops, project arcs across many
    //    chats). Keep the literary opt-in until the bench tells us
    //    otherwise.

    fn runs_configuration_phase(&self) -> bool {
        self.inner.runs_configuration_phase()
    }

    fn compose_phase8_configuration(
        &self,
        atlas_summary: &crate::enrichment::atlas::analysis::AtlasSummary,
        exemplars: &[&Exemplar],
    ) -> Option<ChatPrompt> {
        self.inner
            .compose_phase8_configuration(atlas_summary, exemplars)
    }

    fn parse_phase8_configuration(
        &self,
        response: &str,
    ) -> Result<Vec<crate::enrichment::atlas::analysis::Phase8ParseItem>> {
        self.inner.parse_phase8_configuration(response)
    }

    // ── Phase 6 atlas classifier. ────────────────────────────────

    fn runs_phase6_atlas_classifier(&self) -> bool {
        self.inner.runs_phase6_atlas_classifier()
    }

    fn compose_phase6_atlas_classifier(
        &self,
        content: &crate::enrichment::atlas::analysis::CandidateContent,
    ) -> Option<ChatPrompt> {
        self.inner.compose_phase6_atlas_classifier(content)
    }

    fn parse_phase6_atlas_classifier(
        &self,
        response: &str,
    ) -> Result<crate::enrichment::atlas::analysis::Phase6Classification> {
        self.inner.parse_phase6_atlas_classifier(response)
    }

    fn runs_phase6_holistic(&self) -> bool {
        self.inner.runs_phase6_holistic()
    }

    fn compose_phase6_holistic(
        &self,
        atoms: &crate::enrichment::atlas::atoms::AtomsFile,
    ) -> Option<ChatPrompt> {
        self.inner.compose_phase6_holistic(atoms)
    }

    fn parse_phase6_holistic(
        &self,
        response: &str,
    ) -> Result<Vec<crate::enrichment::atlas::analysis::HolisticTension>> {
        self.inner.parse_phase6_holistic(response)
    }

    fn top_k_exemplars(&self, phase: PipelinePhase) -> usize {
        self.inner.top_k_exemplars(phase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_name_diverge_from_literary() {
        let p = ConversationAtlasPipeline::new();
        assert_eq!(p.id(), "conversation_atlas");
        assert!(p.name().to_lowercase().contains("conversation"));
        let lit = LiteraryAtlasPipeline::new();
        // Same opt-ins as literary_atlas — flip here when bench
        // tells us conversations need different phase coverage.
        assert_eq!(p.runs_configuration_phase(), lit.runs_configuration_phase());
        assert_eq!(
            p.runs_phase6_atlas_classifier(),
            lit.runs_phase6_atlas_classifier()
        );
        assert_eq!(p.runs_phase6_holistic(), lit.runs_phase6_holistic());
    }

    #[test]
    fn phase1_diverges_from_literary_with_conversation_specific_rules() {
        // The conversation_atlas Phase 1 preamble names the seven
        // load-bearing divergences (see PHASE1_CONVERSATION_SYSTEM
        // comment block). Pin the rule strings so a future prompt
        // revision that removes them fails loudly.
        let p = ConversationAtlasPipeline::new();
        let lit = LiteraryAtlasPipeline::new();
        assert_ne!(p.phase1_system(), lit.phase1_system());
        let p1 = p.phase1_system();
        // 1. Turn-block format awareness.
        assert!(p1.contains("### [YYYY-MM-DD HH:MM] user"));
        assert!(p1.contains("### [YYYY-MM-DD HH:MM] assistant"));
        // 2. User-as-voice rule.
        assert!(p1.contains("user (the speaker behind `### [...] user` blocks) is NEVER a"));
        // 3. Assistant-as-non-person rule.
        assert!(p1.contains(
            "assistant (the speaker behind `### [...] assistant` blocks)\nis NEVER a Person atom"
        ));
        // 4. Timestamps / IDs.
        assert!(p1.contains("Years, dates, timestamps, and IDs are NEVER Person"));
        // 5. Decisions via discourse_act=commit.
        assert!(p1.contains("THIS IS THE DECISION ATOM"));
        // 6. Attribution rules.
        assert!(p1.contains("attributed_to: \"assistant\""));
        // 7. Working-professional concept vocab.
        assert!(p1.contains("runway"));
        assert!(p1.contains("burn rate"));
    }

    #[test]
    fn phase3_and_phase5_still_delegate_to_literary() {
        let p = ConversationAtlasPipeline::new();
        let lit = LiteraryAtlasPipeline::new();
        assert_eq!(p.phase3_system(), lit.phase3_system());
        assert_eq!(p.phase5_system(), lit.phase5_system());
    }
}
