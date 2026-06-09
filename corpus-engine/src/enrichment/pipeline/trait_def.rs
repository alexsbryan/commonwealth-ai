// SPDX-License-Identifier: AGPL-3.0-or-later
//! The v2 enrichment pipeline trait.
//!
//! `Pipeline` is a sibling of the v1 `Domain` trait, not an extension.
//! It splits the monolithic 5-phase `FieldModelEngine` shape into the
//! 7 LLM + clustering phases the admin CLI iterates on:
//!
//!   1. per-chapter question extraction
//!   2. question clustering  (HDBSCAN — no trait method)
//!   3. canonical concern naming
//!   4. chunk clustering     (HDBSCAN — no trait method)
//!   5. grounded position extraction
//!   6. pairwise tension detection
//!   7. gap detection
//!
//! Each prompt-bearing phase exposes three trait hooks:
//!
//! - `phaseN_system()` — the stable system preamble (domain language
//!   that rarely changes). Loaded from an `include_str!` markdown
//!   asset so prompts live as data, not Rust string literals.
//! - `compose_phaseN(input, exemplars)` — builds the `ChatPrompt`
//!   that gets sent to the daemon. The runner hands in the top-K
//!   exemplars for this call.
//! - `parse_phaseN(response)` — validates the model's JSON output
//!   against the expected schema.

use super::atlas::{EntitySketch, SectionExtraction, SeedEntities, SeedEntity, SeedStrategy};
use super::exemplar_bank::Exemplar;
use super::types::*;
use crate::enrichment::domain::ClusteringConfig;
use crate::error::Result;

/// A v2 enrichment pipeline. One per target domain (literary,
/// philosophical, journal, codebase, …).
///
/// Object-safe. Held as `Arc<dyn Pipeline>` in the runtime. All
/// methods take `&self`. The trait is intentionally generic over
/// input/output struct shapes defined in `types.rs` — adding a new
/// pipeline is a single impl, not a match-arm in a dispatcher.
pub trait Pipeline: Send + Sync + 'static {
    // ── Identity ──────────────────────────────────────────────

    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn vocabulary(&self) -> &Vocabulary;

    // ── System preambles (stable language, loaded from assets) ─

    fn phase1_system(&self) -> &'static str;
    fn phase3_system(&self) -> &'static str;
    fn phase5_system(&self) -> &'static str;
    fn phase6_system(&self) -> &'static str;
    fn phase7_system(&self) -> &'static str;

    // ── Prompt composition ────────────────────────────────────
    //
    // The runner calls these once per input, passing the top-K
    // exemplars it selected via `ExemplarBank::select_top_k`.

    fn compose_phase1(&self, chapter: &ChapterInput, exemplars: &[&Exemplar]) -> ChatPrompt;

    /// Variant of `compose_phase1` that threads a seed entity list
    /// into the user message. The default impl ignores the seed
    /// and delegates to `compose_phase1`, so pipelines without a
    /// seed strategy (or callers that haven't produced a seed
    /// yet) behave exactly as before.
    ///
    /// Pipelines that declare `SeedStrategy::Llm` or
    /// `SeedStrategy::Structural` should override this to render
    /// the canonical-names block the seed buys them. The seed is
    /// passed as `Option` so a pipeline that supports seeds can
    /// still be run without one during development.
    fn compose_phase1_with_seed(
        &self,
        chapter: &ChapterInput,
        exemplars: &[&Exemplar],
        seed: Option<&SeedEntities>,
    ) -> ChatPrompt {
        let _ = seed;
        self.compose_phase1(chapter, exemplars)
    }

    // ── Stage 1a — seed extraction surface ────────────────────
    //
    // Three trait methods, all defaulted, that together let a
    // pipeline declare HOW it produces a seed list (if at all).
    // The runner dispatches on `seed_strategy()` and calls exactly
    // one of the action methods. Pipelines that don't need seeds
    // inherit the defaults and Stage 1a becomes a no-op for them.

    /// Which seed-production strategy this pipeline uses. Default
    /// is `None` — the pipeline runs Stage 1b map calls without
    /// seed context. Override to `Llm` or `Structural` to opt
    /// into Stage 1a.
    fn seed_strategy(&self) -> SeedStrategy {
        SeedStrategy::None
    }

    /// Compose the Stage 1a prompt for `SeedStrategy::Llm`
    /// pipelines. Takes the corpus's first section; returns a
    /// `ChatPrompt` the runner hands to the chat closure. The
    /// default returns `None`; pipelines that claim the `Llm`
    /// strategy must override this or the runner errors with a
    /// clear "strategy says Llm but compose_seed_prompt returned
    /// None" message.
    fn compose_seed_prompt(&self, _first_section: &ChapterInput) -> Option<ChatPrompt> {
        None
    }

    /// Parse a Stage 1a LLM response into the entries that will
    /// back `SeedEntities`. The runner stamps `corpus_id` +
    /// `origin` + `written_at` on the returned entries; this
    /// method only needs to produce the typed list.
    ///
    /// Default returns a clear "not implemented" error — paired
    /// with `compose_seed_prompt`, pipelines that support the Llm
    /// strategy must override both.
    fn parse_seed_response(&self, _response: &str) -> Result<Vec<SeedEntity>> {
        Err(crate::error::Error::Serialization(
            "pipeline does not implement parse_seed_response — either \
             override it alongside compose_seed_prompt or declare \
             SeedStrategy::None / SeedStrategy::Structural"
                .into(),
        ))
    }

    /// Produce a seed list from structural signals for
    /// `SeedStrategy::Structural` pipelines. The argument gives
    /// full corpus context (chapters + chunks + metadata) so the
    /// impl can walk wikilinks, infoboxes, or whatever structural
    /// signal the pipeline uses. Default errors for strategies
    /// other than `Structural`.
    fn extract_seed_structural(&self, _ctx: &CorpusContext) -> Result<Vec<SeedEntity>> {
        Err(crate::error::Error::Serialization(
            "pipeline does not implement extract_seed_structural — \
             declare SeedStrategy::Llm or SeedStrategy::None if no \
             structural signal is available"
                .into(),
        ))
    }

    /// Optional terse variant of Phase 1 composition, used by the
    /// runner when a default-variant run failed with
    /// `PhaseFailureKind::ThinkTruncated`. A pipeline that returns
    /// `Some(prompt)` opts into terse-retry recovery; the default
    /// `None` means "this pipeline has no terse variant; a terse
    /// retry should fail early with a clear error rather than
    /// silently reuse the default prompt."
    ///
    /// Implementations typically drop the reasoning preamble and
    /// any exemplar block to save tokens on chapters that already
    /// blew past the output budget. The parser is shared with the
    /// default variant — terse output follows the same schema.
    fn compose_phase1_terse(&self, _chapter: &ChapterInput) -> Option<ChatPrompt> {
        None
    }

    // ── Phase 1b — coverage check (opt-in, recall booster) ─────
    //
    // After Phase 1 succeeds for a chapter, the runner can issue a
    // second-pass audit prompt that asks the model "what did you
    // miss?" against its own extraction. The pattern is split into
    // two narrow prompts — one for missed entities (persons, works,
    // institutions, places), one for missed thematic concepts —
    // because a single broad audit underperforms a focused one
    // (validated against dubliners-test, +24.9 F1).
    //
    // A pipeline opts in by returning `Some(prompt)` from the
    // compose methods. The runner treats Phase 1b as best-effort:
    // a chat or parse failure logs a warning and the chapter
    // proceeds with its original Phase 1 atoms unchanged.

    /// Compose the entity-coverage audit prompt for one chapter.
    /// Receives the chapter and the just-completed Phase 1 extraction
    /// so the prompt can list "what was already lifted" (the model's
    /// job is to surface only NEW atoms). Default returns `None` —
    /// pipelines that don't want a coverage pass run Phase 1 alone.
    fn compose_phase1b_entity_coverage(
        &self,
        _chapter: &ChapterInput,
        _existing: &SectionExtraction,
    ) -> Option<ChatPrompt> {
        None
    }

    /// Compose the concept-coverage audit prompt for one chapter.
    /// Same shape as the entity variant but narrowed to abstract /
    /// thematic terms — the failure mode the broad entity audit
    /// underperformed on. Default `None`.
    fn compose_phase1b_concept_coverage(
        &self,
        _chapter: &ChapterInput,
        _existing: &SectionExtraction,
    ) -> Option<ChatPrompt> {
        None
    }

    /// Parse a Phase 1b response into additional `EntitySketch`
    /// entries the runner will append to the chapter's
    /// `entities_introduced` (deduping by canonical name).
    /// Default `Err` so a pipeline that composes a coverage prompt
    /// without parsing it surfaces a clear contract error.
    fn parse_phase1b_coverage(&self, _response: &str) -> Result<Vec<EntitySketch>> {
        Err(crate::error::Error::Serialization(
            "pipeline does not implement parse_phase1b_coverage — \
             override it alongside compose_phase1b_*_coverage, or \
             leave both compose methods returning None"
                .into(),
        ))
    }

    /// Compose a Phase 3 naming prompt for an atlas cluster of a
    /// specific facet. Returns `None` when the pipeline doesn't
    /// implement atlas-style naming — the caller can then fall back
    /// to the legacy `compose_phase3` flow or error out.
    ///
    /// `excerpts` is the flattened list of sketches the cluster
    /// covers, rendered per facet (see `SketchExcerpt::content`).
    /// The prompt composer reads them directly; it doesn't re-touch
    /// the Phase 1 cache.
    fn compose_phase3_facet(
        &self,
        _cluster: &AtlasCluster,
        _facet: Facet,
        _excerpts: &[SketchExcerpt],
        _exemplars: &[&Exemplar],
    ) -> Option<ChatPrompt> {
        None
    }

    /// Parse a Phase 3 facet-naming response. Returns the label +
    /// optional metadata the naming prompt asks for (spec §5.3 +
    /// `literary_atlas_prompts/phase3_<facet>_naming.md`).
    ///
    /// Default `Err` so pipelines that implement the compose half
    /// without the parse half produce a clear contract error
    /// rather than a silent empty result.
    fn parse_phase3_facet(&self, _facet: Facet, _response: &str) -> Result<Phase3FacetParseResult> {
        Err(crate::error::Error::Serialization(
            "pipeline does not implement parse_phase3_facet — call compose_phase3_facet \
             first to confirm support, or use the v1 name-concerns flow"
                .into(),
        ))
    }

    fn compose_phase3(
        &self,
        cluster: &QuestionCluster,
        chapter_excerpts: &[&ChapterInput],
        exemplars: &[&Exemplar],
    ) -> ChatPrompt;

    fn compose_phase5(
        &self,
        concern: &CanonicalConcern,
        cluster: &ChunkCluster,
        cluster_chunk_texts: &[(u64, String)],
        exemplars: &[&Exemplar],
    ) -> ChatPrompt;

    fn compose_phase6(
        &self,
        pos_a: &Position,
        pos_b: &Position,
        exemplars: &[&Exemplar],
    ) -> ChatPrompt;

    fn compose_phase7(
        &self,
        concerns: &[CanonicalConcern],
        positions: &[Position],
        chapter_titles: &[String],
        exemplars: &[&Exemplar],
    ) -> ChatPrompt;

    // ── Clustering (pure HDBSCAN — no LLM) ────────────────────

    fn question_clustering_config(&self) -> ClusteringConfig;
    fn chunk_clustering_config(&self) -> ClusteringConfig;

    // ── Response parsers ──────────────────────────────────────
    //
    // Each returns `Err` with a descriptive message when the model's
    // response does not match the expected schema. The runner logs
    // the full response + the parse error before retrying.

    fn parse_phase1(&self, response: &str) -> Result<Phase1ChapterResult>;
    fn parse_phase3(&self, response: &str) -> Result<Phase3ParseResult>;
    fn parse_phase5(&self, response: &str) -> Result<Phase5ParseResult>;
    fn parse_phase6(&self, response: &str) -> Result<Option<Phase6ParseResult>>;
    fn parse_phase7(&self, response: &str) -> Result<Vec<Phase7ParseItem>>;

    // ── Phase 8 (Configuration) — opt-in per pipeline ─────────

    /// Whether this pipeline runs the Phase 8 Configuration pass.
    /// Default `false` — most pipelines (code indices, transcript
    /// archives) have nothing configurational to say. Authored
    /// literary and philosophical pipelines opt in.
    ///
    /// The `atlas-configuration` subcommand checks this gate
    /// before calling `compose_phase8_configuration`; a pipeline
    /// that returns `false` here can leave the other Phase 8
    /// methods unimplemented.
    fn runs_configuration_phase(&self) -> bool {
        false
    }

    /// Build the Phase 8 prompt. The runner hands in a compact
    /// `AtlasSummary` — the atoms the LLM will reason over — plus
    /// the top-K Phase 8 exemplars. Return `None` when the
    /// pipeline is not opted in; the runner silently skips.
    ///
    /// Opted-in pipelines MUST override this and return
    /// `Some(prompt)`. The default is a conservative no-op so
    /// non-atlas pipelines never accidentally dispatch a
    /// configuration call.
    fn compose_phase8_configuration(
        &self,
        _atlas_summary: &crate::enrichment::atlas::analysis::AtlasSummary,
        _exemplars: &[&Exemplar],
    ) -> Option<ChatPrompt> {
        None
    }

    /// Parse the Phase 8 response into the shape the CLI will
    /// stamp with ids + drop unknown-atom references. Default
    /// errors with a descriptive message so a pipeline that
    /// opts into Phase 8 but forgets to implement the parser
    /// fails fast with an actionable message.
    fn parse_phase8_configuration(
        &self,
        _response: &str,
    ) -> Result<Vec<crate::enrichment::atlas::analysis::Phase8ParseItem>> {
        Err(crate::error::Error::Serialization(
            "pipeline does not implement parse_phase8_configuration — \
             either override it or return false from runs_configuration_phase"
                .into(),
        ))
    }

    // ── Phase 6 atlas classifier — opt-in per pipeline ────────────
    //
    // Distinct from `compose_phase6` (which is the v1 questions/
    // positions/tensions pass). The atlas-pipeline Phase 6 has two
    // halves: the deterministic candidate enumerator
    // (`atlas::analysis::tensions::select_candidates`) and this LLM
    // classifier that promotes accepted candidates to `Tension`
    // edges on `edges.json`.

    /// Whether this pipeline runs the Phase 6 atlas Tension
    /// classifier. Default `false` — non-atlas pipelines don't
    /// produce `tension_candidates.json` and have no candidates to
    /// classify. Atlas pipelines (literary, philosophy) opt in.
    fn runs_phase6_atlas_classifier(&self) -> bool {
        false
    }

    /// Build the Phase 6 atlas classifier prompt for one resolved
    /// candidate. Return `None` when the pipeline is not opted in;
    /// the runner silently skips. Opted-in pipelines MUST override
    /// this and return `Some(prompt)` per the schema in
    /// `atlas::analysis::tension_classifier::phase6_classifier_response_schema`.
    fn compose_phase6_atlas_classifier(
        &self,
        _content: &crate::enrichment::atlas::analysis::CandidateContent,
    ) -> Option<ChatPrompt> {
        None
    }

    /// Parse the Phase 6 atlas classifier response. Default delegates
    /// to the shared parser in `analysis::tension_classifier` which
    /// handles `<think>` blocks, code-fence stripping, and serde
    /// shape validation. Pipelines may override to add domain-specific
    /// post-processing (none today).
    fn parse_phase6_atlas_classifier(
        &self,
        response: &str,
    ) -> Result<crate::enrichment::atlas::analysis::Phase6Classification> {
        crate::enrichment::atlas::analysis::parse_phase6_classifier_response(response).map_err(
            |e| {
                crate::error::Error::Serialization(format!(
                    "phase 6 atlas classifier response parse failed: {e}"
                ))
            },
        )
    }

    // ── Phase 6 *holistic* classifier — opt-in alternative to per-pair ──
    //
    // The per-pair classifier asks "is THIS pair of atoms in tension?",
    // which is the wrong unit of analysis for *between-position* fault
    // lines (a property of two whole positions, not of any single
    // claim pair). The holistic classifier asks the model to read the
    // corpus's positions and surface the fault lines naturalistically
    // — one chat turn, all positions in scope. Pipelines opt in via
    // `runs_phase6_holistic` and override compose / parse.
    //
    // The runner uses *either* per-pair *or* holistic, not both, on
    // the assumption that a pipeline's domain has one right unit of
    // tension. Philosophy opts into holistic; literary keeps per-pair
    // (literary tensions are typically within-character —
    // stated-vs-enacted — and the per-pair frame fits).

    /// Whether this pipeline runs the Phase 6 *holistic* classifier
    /// (a single corpus-level pass) instead of the per-pair one.
    /// Default `false`.
    fn runs_phase6_holistic(&self) -> bool {
        false
    }

    /// Build the holistic Phase 6 prompt. Sees the entire resolved
    /// atom inventory. Returns `None` for pipelines that don't opt
    /// into holistic. Opted-in pipelines must override and return
    /// `Some(prompt)` whose response will be parsed by
    /// `parse_phase6_holistic`.
    fn compose_phase6_holistic(
        &self,
        _atoms: &crate::enrichment::atlas::atoms::AtomsFile,
    ) -> Option<ChatPrompt> {
        None
    }

    /// Parse the holistic-classifier response. Default delegates to
    /// `analysis::holistic_classifier::parse_holistic_response`,
    /// which handles chain-of-thought preamble + trailing JSON,
    /// `<think>` blocks, and the `fault_lines` / `tensions` key
    /// alias.
    fn parse_phase6_holistic(
        &self,
        response: &str,
    ) -> Result<Vec<crate::enrichment::atlas::analysis::HolisticTension>> {
        crate::enrichment::atlas::analysis::parse_holistic_response(response)
    }

    // ── Selection tuning ──────────────────────────────────────

    /// How many exemplars to inject per call. Default 5 across all
    /// phases. Override per phase when a domain learns that some
    /// phases need more steering than others.
    fn top_k_exemplars(&self, _phase: PipelinePhase) -> usize {
        5
    }
}
