//! Philosophy atlas pipeline — Phase C Step 7 of the v2.1 rollout.
//!
//! Parallel to [`super::literary_atlas::LiteraryAtlasPipeline`]:
//! same Rust scaffolding, same atom schema, same clustering
//! machinery. The only divergence is in the **prompt assets** —
//! Phase 1, Phase 1a seed, Phase 1 terse, Phase 3 facet-naming
//! (all five facets), and Phase 8 configuration all have
//! philosophy-tuned preambles at `philosophy_atlas_prompts/*.md`.
//!
//! The acceptance test for this pipeline (spec §8.2): the same
//! 8-phase runner, clustering engine, and traversal primitives
//! handle a Stanford Encyclopedia of Philosophy article with no
//! domain-specific code branches. All the domain knowledge lives
//! in the markdown prompt assets.
//!
//! Wraps `LiteraryAtlasPipeline` as `inner` so every phase the
//! pipeline doesn't need to tune (Phases 5, 6, 7 on the v1 path;
//! the parsers that are schema-driven rather than domain-tuned)
//! delegates to the identical implementation.

use super::super::atlas::{
    EntitySketch, SectionExtraction, SeedEntities, SeedEntity, SeedStrategy,
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

// ── Philosophy-specific prompt assets ────────────────────────

const PHASE1_ATLAS_SYSTEM: &str =
    include_str!("philosophy_atlas_prompts/phase1_system.md");

const PHASE1_ATLAS_SYSTEM_TERSE: &str =
    include_str!("philosophy_atlas_prompts/phase1_system_terse.md");

const PHASE1A_SEED_SYSTEM: &str =
    include_str!("philosophy_atlas_prompts/phase1a_seed_system.md");

const PHASE1B_ENTITY_COVERAGE: &str =
    include_str!("philosophy_atlas_prompts/phase1b_entity_coverage.md");

const PHASE1B_CONCEPT_COVERAGE: &str =
    include_str!("philosophy_atlas_prompts/phase1b_concept_coverage.md");

const PHASE3_QUESTION_NAMING: &str =
    include_str!("philosophy_atlas_prompts/phase3_question_naming.md");
const PHASE3_CLAIM_NAMING: &str =
    include_str!("philosophy_atlas_prompts/phase3_claim_naming.md");
const PHASE3_ENTITY_STATE_NAMING: &str =
    include_str!("philosophy_atlas_prompts/phase3_entity_state_trajectory_naming.md");
const PHASE3_RELATION_STATE_NAMING: &str =
    include_str!("philosophy_atlas_prompts/phase3_relation_state_trajectory_naming.md");
const PHASE3_EVENT_NAMING: &str =
    include_str!("philosophy_atlas_prompts/phase3_event_thread_naming.md");

const PHASE8_CONFIGURATION_SYSTEM: &str =
    include_str!("philosophy_atlas_prompts/phase8_configuration.md");

const PHASE6_HOLISTIC_SYSTEM: &str =
    include_str!("philosophy_atlas_prompts/phase6_holistic_system.md");

/// Pipeline id exposed by the registry.
pub const PIPELINE_ID: &str = "philosophy_atlas";

/// Philosophy-domain atlas pipeline. Same atom schema as the
/// literary variant; tuned prompts across every phase that speaks
/// domain language.
pub struct PhilosophyAtlasPipeline {
    /// Reused for (a) every phase the philosophy pipeline doesn't
    /// tune (5, 6, 7 on the v1 path) and (b) every schema-driven
    /// parser where the parsing logic is identical across domains.
    inner: LiteraryAtlasPipeline,
}

impl PhilosophyAtlasPipeline {
    pub fn new() -> Self {
        Self {
            inner: LiteraryAtlasPipeline::new(),
        }
    }
}

impl Default for PhilosophyAtlasPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipeline for PhilosophyAtlasPipeline {
    fn id(&self) -> &'static str {
        PIPELINE_ID
    }

    fn name(&self) -> &'static str {
        "Philosophy — atlas atom graph"
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
    //
    // Reuses the literary_atlas user-body renderer and parser; the
    // only domain-specific bits are the system preamble strings.

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
        // Delegate to the inner atlas parser — the schema is
        // domain-agnostic (six facets, same field names). Only the
        // prompt that produced the response changes per domain.
        self.inner.parse_phase1(response)
    }

    // ── Stage 1a — seed extraction ─────────────────────────────

    fn seed_strategy(&self) -> SeedStrategy {
        SeedStrategy::Llm
    }

    fn compose_seed_prompt(&self, first_section: &ChapterInput) -> Option<ChatPrompt> {
        let mut user = String::new();
        user.push_str("# Opening section\n\n");
        user.push_str(&format!("**Title:** {}\n", first_section.title));
        if let Some(ord) = first_section.metadata.get("ordinal") {
            user.push_str(&format!("**Position:** section {ord}\n"));
        }
        user.push_str("\n**Body:**\n\n");
        user.push_str(&first_section.text);
        user.push_str("\n\n---\n\n");
        user.push_str(
            "Respond with a single JSON object per the schema in the system \
             message. Entities only. No prose, no <think> block.",
        );
        Some(ChatPrompt::new(PHASE1A_SEED_SYSTEM, user).with_phase_id("phase1_seed"))
    }

    fn parse_seed_response(&self, response: &str) -> Result<Vec<SeedEntity>> {
        // Delegate — the seed schema is the same JSON shape
        // regardless of domain; only the prompt that produces it
        // differs.
        self.inner.parse_seed_response(response)
    }

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
        // Delegate — schema-driven parser, identical across domains.
        self.inner.parse_phase3_facet(facet, response)
    }

    // ── Phase 5/6/7 — delegate to v1 literary for now ─────────

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

    // ── Phase 8 (Configuration) — opt in ──────────────────────

    fn runs_configuration_phase(&self) -> bool {
        true
    }

    fn compose_phase8_configuration(
        &self,
        atlas_summary: &crate::enrichment::atlas::analysis::AtlasSummary,
        _exemplars: &[&Exemplar],
    ) -> Option<ChatPrompt> {
        // The user-message body is identical to the literary
        // path — it's a mechanical atlas-summary render. The
        // system preamble switches to the philosophy-tuned asset.
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
            user.push_str("## Trajectories (state chains in section order)\n\n");
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

        Some(ChatPrompt::new(PHASE8_CONFIGURATION_SYSTEM, user).with_phase_id("phase8_configuration"))
    }

    fn parse_phase8_configuration(
        &self,
        response: &str,
    ) -> Result<Vec<crate::enrichment::atlas::analysis::Phase8ParseItem>> {
        // Reuse the literary atlas parser — the Phase 8 response
        // schema is domain-agnostic. Only the prompt that produces
        // the response differs.
        self.inner.parse_phase8_configuration(response)
    }

    // ── Phase 6 atlas Tension classifier ─────────────────────────
    //
    // Philosophy uses the *holistic* fault-line classifier (one call
    // per corpus) instead of the per-pair classifier. Per-pair was
    // empirically rejecting every cross-position candidate on
    // philosophy benches (0/81 acceptance on stoic): a fault line
    // between two whole positions is not visible in any single
    // claim-pair slice the per-pair frame considers. The holistic
    // call sees all positions and identifies between-position fault
    // lines as a unit. Literary keeps per-pair (literary tensions
    // are typically within-character — stated-vs-enacted — which the
    // per-pair frame fits well).

    fn runs_phase6_atlas_classifier(&self) -> bool {
        false
    }

    fn runs_phase6_holistic(&self) -> bool {
        true
    }

    fn compose_phase6_holistic(
        &self,
        atoms: &crate::enrichment::atlas::atoms::AtomsFile,
    ) -> Option<ChatPrompt> {
        let user_body = crate::enrichment::atlas::analysis::render_holistic_user_body(atoms);
        Some(
            ChatPrompt::new(PHASE6_HOLISTIC_SYSTEM, user_body)
                .with_phase_id("phase6_holistic"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_section() -> ChapterInput {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("ordinal".to_string(), "1".to_string());
        let text = "Compatibilism is the thesis that free will is compatible with \
                    causal determinism. Its opponents divide into hard determinists \
                    (who reject free will) and libertarians (who reject determinism)."
            .to_string();
        let approx_tokens = text.split_whitespace().count();
        ChapterInput {
            chapter_id: "intro".into(),
            title: "1. Introduction".into(),
            text,
            metadata,
            approx_tokens,
        }
    }

    #[test]
    fn philosophy_atlas_registers_expected_identity() {
        let p = PhilosophyAtlasPipeline::new();
        assert_eq!(p.id(), "philosophy_atlas");
        assert!(p.name().to_lowercase().contains("philosophy"));
    }

    #[test]
    fn philosophy_atlas_opts_into_configuration_phase() {
        let p = PhilosophyAtlasPipeline::new();
        assert!(
            p.runs_configuration_phase(),
            "philosophy_atlas must opt into Phase 8 configuration detection"
        );
    }

    #[test]
    fn philosophy_atlas_declares_llm_seed_strategy() {
        let p = PhilosophyAtlasPipeline::new();
        assert!(matches!(p.seed_strategy(), SeedStrategy::Llm));
    }

    #[test]
    fn philosophy_atlas_phase1_system_is_philosophy_tuned() {
        let p = PhilosophyAtlasPipeline::new();
        let sys = p.phase1_system();
        // The philosophy phase-1 preamble explicitly names
        // argumentative prose — the critical shibboleth that
        // distinguishes it from the literary path.
        assert!(
            sys.contains("philosophy") || sys.contains("argumentative"),
            "philosophy phase1_system should name its domain; got: {sys:.200}"
        );
    }

    #[test]
    fn philosophy_atlas_compose_phase1_renders_a_prompt() {
        // Structural round-trip: compose_phase1 produces a
        // non-empty user body that includes the section id. This
        // is the minimum viable proof the same runner path works
        // against a philosophy pipeline.
        let p = PhilosophyAtlasPipeline::new();
        let prompt = p.compose_phase1(&sample_section(), &[]);
        assert!(!prompt.system.is_empty());
        assert!(prompt.user.contains("intro") || prompt.user.contains("Introduction"));
    }

    #[test]
    fn philosophy_atlas_compose_seed_prompt_uses_seed_asset() {
        let p = PhilosophyAtlasPipeline::new();
        let prompt = p
            .compose_seed_prompt(&sample_section())
            .expect("seed prompt should be available for LLM seed strategy");
        // System asset mentions 'seed' + 'philosophy'.
        assert!(prompt.system.to_lowercase().contains("seed"));
        assert!(prompt.system.to_lowercase().contains("philosoph"));
    }

    #[test]
    fn philosophy_atlas_compose_phase3_facet_selects_by_facet() {
        let p = PhilosophyAtlasPipeline::new();
        let cluster = AtlasCluster {
            id: "cl_0001".into(),
            facet: Facet::Claim,
            refs: vec![],
        };
        let prompt = p
            .compose_phase3_facet(&cluster, Facet::Claim, &[], &[])
            .expect("phase3_facet should compose when facet is claim");
        // The claim-naming prompt explicitly uses 'position' as
        // its philosophy-tuned label-shape.
        assert!(
            prompt.system.to_lowercase().contains("position"),
            "claim-facet system should be philosophy-tuned; got: {:.200}",
            prompt.system
        );
    }

    #[test]
    fn philosophy_atlas_compose_phase8_uses_ricoeur_constrained_prompt() {
        use crate::enrichment::atlas::analysis::AtlasSummary;
        let p = PhilosophyAtlasPipeline::new();
        let summary = AtlasSummary {
            section_count: 3,
            entities: vec![],
            relations: vec![],
            trajectories: vec![],
            top_claims: vec![],
            open_questions: vec![],
            key_events: vec![],
        };
        let prompt = p
            .compose_phase8_configuration(&summary, &[])
            .expect("phase8 must compose when pipeline opts in");
        // The Ricoeur constraint is the load-bearing difference
        // vs a generic configuration prompt.
        assert!(
            prompt.system.contains("Ricoeur") || prompt.system.contains("alternative reading"),
            "phase8 system must carry the Ricoeur constraint"
        );
    }

    #[test]
    fn philosophy_atlas_vocabulary_delegates_to_inner_without_crashing() {
        let p = PhilosophyAtlasPipeline::new();
        // Just exercise the method — inner uses the literary
        // vocabulary which is a reasonable default until a
        // philosophy vocabulary is separately tuned.
        let _ = p.vocabulary();
    }

    #[test]
    fn philosophy_atlas_id_differs_from_literary_atlas() {
        let phil = PhilosophyAtlasPipeline::new();
        let lit = LiteraryAtlasPipeline::new();
        assert_ne!(phil.id(), lit.id());
    }

    /// The utility of this pipeline hinges on the Phase 1 prompt
    /// recognising philosophy's vocabulary. A quick sanity pin:
    /// the asset must name philosopher/concept/work entity types
    /// (the philosophy twist on the six-facet schema).
    #[test]
    fn philosophy_phase1_prompt_lists_philosophy_entity_types() {
        let sys = PhilosophyAtlasPipeline::new().phase1_system();
        assert!(sys.contains("concept"));
        assert!(sys.contains("work"));
        assert!(sys.to_lowercase().contains("philosopher") || sys.contains("person"));
    }
}
