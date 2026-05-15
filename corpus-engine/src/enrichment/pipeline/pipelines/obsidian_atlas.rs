//! Obsidian-vault atlas pipeline — initial scaffolding.
//!
//! Heterogeneous personal-vault notes (daily journals, meeting notes,
//! project notes, zettel references, book highlights, idea drafts) are
//! a different beast from cleanly-authored literary prose: shorter
//! sections, frontmatter-heavy metadata, wikilink-dense, and full of
//! abbreviations the author understands but a Phase 1 atlas extractor
//! may not.
//!
//! v1 of this pipeline is a thin **forwarding wrapper** around
//! `literary_atlas` — every trait method delegates. The `obsidian_atlas`
//! id exists so the bench eval (`sovereign bench obsidian`) can score
//! literary-as-applied-to-a-vault as a baseline. When the bench surfaces
//! systematic gaps (frontmatter tags lost, wikilink graph absent,
//! daily-journal entries misclassified) the prompt divergence lands
//! HERE — not in `literary_atlas` — so the literary calibration stays
//! intact.
//!
//! Forking from day one (even as a no-op delegate) is cheaper than
//! forking later: a tuning commit only has to revise this file, never
//! risk perturbing literary's calibration. The cost is one extra file
//! + one extra registry entry. The benefit is unambiguous ownership of
//! "this prompt change is for vaults, not for books."

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

/// Pipeline id exposed by the registry. Must match the string used
/// by callers (the local-corpus manager's accepted-pipeline gate,
/// the bench harness, the obsidian-vault default `pipeline_id`).
pub const PIPELINE_ID: &str = "obsidian_atlas";

/// Vault-flavored Phase 1 system preamble. Diverges from
/// `literary_atlas`'s prompt in five places, each driven by a
/// baseline-bench failure observed against the author's vault:
///   1. "Author / first-person voice ≠ Person" replaces the
///      literary "narrator" clause — catches the `the author` FP.
///   2. "Years, dates, statute names are NOT Persons" — catches
///      the `1968` / `2009` / `ERISA` FPs.
///   3. "Acronyms are Institution/Concept, never Person" — catches
///      the `NFL` / `PBM` / `NVIDIA` FPs.
///   4. Domain-concept examples drawn from non-fiction (regulatory
///      capture, salary cap, EUV monopoly) instead of literary
///      motifs (the absurd, figure in the carpet). Targets the
///      8.3% concept recall on the baseline bench.
///   5. "Distinguish Concept from Claim" — flags the failure mode
///      where the model folds named mechanisms into the Claim
///      that *uses* them.
static PHASE1_OBSIDIAN_SYSTEM: ::std::sync::LazyLock<&'static str> =
    ::std::sync::LazyLock::new(|| crate::enrichment::pipeline::prompts::load_or_baked(
        "obsidian_atlas/phase1_system.md",
        include_str!("obsidian_atlas_prompts/phase1_system.md"),
    ));

/// Obsidian-vault atlas pipeline. Wraps a `LiteraryAtlasPipeline` and
/// delegates every trait method — for v1 the two are behaviourally
/// identical. The bench eval consumes the divergence headroom: when
/// fixture-vault scores show literary-style prompts underperform on
/// personal-vault notes, the prompt rewrites land in this module.
pub struct ObsidianAtlasPipeline {
    inner: Arc<LiteraryAtlasPipeline>,
}

impl ObsidianAtlasPipeline {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(LiteraryAtlasPipeline::new()),
        }
    }
}

impl Default for ObsidianAtlasPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipeline for ObsidianAtlasPipeline {
    // ── Identity — diverges from literary so the registry, telemetry,
    //               and bench reports can distinguish the two.

    fn id(&self) -> &'static str {
        PIPELINE_ID
    }

    fn name(&self) -> &'static str {
        "Obsidian vault — atlas atom graph"
    }

    fn vocabulary(&self) -> &Vocabulary {
        self.inner.vocabulary()
    }

    // ── System preambles. Phase 1 diverges; the rest still
    //    delegate to literary_atlas until the bench surfaces a
    //    specific tuning need at those stages.

    fn phase1_system(&self) -> &'static str {
        *PHASE1_OBSIDIAN_SYSTEM
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

    // ── Phase 1 composition + terse retry + 1b coverage. ─────────

    fn compose_phase1(
        &self,
        chapter: &ChapterInput,
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        // Build the user body via literary_atlas's renderer (the
        // user-message shape is identical — exemplar block + chapter
        // body + closing instruction), but pair it with OUR system
        // preamble. Without this override the call would be
        // literary's compose_phase1 via inner-delegation, which
        // resolves `self.phase1_system()` against the LiteraryAtlasPipeline
        // and silently bypasses every vault-specific rule the new
        // prompt encodes.
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

    fn compose_phase1_with_seed(
        &self,
        chapter: &ChapterInput,
        exemplars: &[&Exemplar],
        seed: Option<&SeedEntities>,
    ) -> ChatPrompt {
        // Same override rationale as `compose_phase1` — seed-aware
        // path must use the obsidian system preamble.
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

    fn compose_phase1_terse(&self, chapter: &ChapterInput) -> Option<ChatPrompt> {
        self.inner.compose_phase1_terse(chapter)
    }

    fn compose_phase1b_entity_coverage(
        &self,
        chapter: &ChapterInput,
        existing: &SectionExtraction,
    ) -> Option<ChatPrompt> {
        self.inner.compose_phase1b_entity_coverage(chapter, existing)
    }

    fn compose_phase1b_concept_coverage(
        &self,
        chapter: &ChapterInput,
        existing: &SectionExtraction,
    ) -> Option<ChatPrompt> {
        self.inner.compose_phase1b_concept_coverage(chapter, existing)
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
        self.inner.compose_phase3(cluster, chapter_excerpts, exemplars)
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
        self.inner.compose_phase3_facet(cluster, facet, excerpts, exemplars)
    }

    fn parse_phase3_facet(
        &self,
        facet: Facet,
        response: &str,
    ) -> Result<Phase3FacetParseResult> {
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

    // ── Phase 8 (Configuration) — vaults opt in via the literary path.
    //
    // Personal vaults DO have configurations worth surfacing (recurring
    // commitments, project arcs, the user's evolving stance on a topic
    // across journal entries). Keep the literary opt-in until the bench
    // tells us otherwise.

    fn runs_configuration_phase(&self) -> bool {
        self.inner.runs_configuration_phase()
    }

    fn compose_phase8_configuration(
        &self,
        atlas_summary: &crate::enrichment::atlas::analysis::AtlasSummary,
        exemplars: &[&Exemplar],
    ) -> Option<ChatPrompt> {
        self.inner.compose_phase8_configuration(atlas_summary, exemplars)
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
        let p = ObsidianAtlasPipeline::new();
        assert_eq!(p.id(), "obsidian_atlas");
        assert!(p.name().contains("Obsidian"));
        // Same opt-ins as literary_atlas (the bench measures whether
        // those opt-ins make sense for vaults; flipping them happens
        // here if it should ever happen).
        let lit = LiteraryAtlasPipeline::new();
        assert_eq!(p.runs_configuration_phase(), lit.runs_configuration_phase());
        assert_eq!(
            p.runs_phase6_atlas_classifier(),
            lit.runs_phase6_atlas_classifier()
        );
        assert_eq!(p.runs_phase6_holistic(), lit.runs_phase6_holistic());
    }

    #[test]
    fn phase1_diverges_from_literary_with_vault_specific_rules() {
        // The obsidian Phase 1 preamble names the four FP failure
        // modes the baseline bench surfaced (author-as-Person,
        // year-as-Person, hires-people-as-Institution, concept-
        // folded-into-claim). Pin the load-bearing rule strings so
        // a future prompt revision that removes them fails the test
        // loudly. The wording was rephrased after the initial
        // iteration; assertions track the current canonical phrasing.
        let p = ObsidianAtlasPipeline::new();
        let lit = LiteraryAtlasPipeline::new();
        assert_ne!(p.phase1_system(), lit.phase1_system());
        let p1 = p.phase1_system();
        assert!(p1.contains("narrator / author / first-person voice is NEVER a"));
        assert!(p1.contains("Years, dates, and statute names are NEVER Person atoms"));
        assert!(p1.contains("if the name describes\na thing that hires people, it is not a Person"));
        assert!(p1.contains("Distinguish Concept atoms from Claim atoms"));
    }

    #[test]
    fn phase3_and_phase5_still_delegate() {
        // Only Phase 1 has diverged so far. If a future iteration
        // forks Phase 3 or 5, update this test alongside the
        // divergence.
        let p = ObsidianAtlasPipeline::new();
        let lit = LiteraryAtlasPipeline::new();
        assert_eq!(p.phase3_system(), lit.phase3_system());
        assert_eq!(p.phase5_system(), lit.phase5_system());
    }
}
