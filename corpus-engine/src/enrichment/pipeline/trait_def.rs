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

    fn compose_phase1(
        &self,
        chapter: &ChapterInput,
        exemplars: &[&Exemplar],
    ) -> ChatPrompt;

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

    // ── Selection tuning ──────────────────────────────────────

    /// How many exemplars to inject per call. Default 5 across all
    /// phases. Override per phase when a domain learns that some
    /// phases need more steering than others.
    fn top_k_exemplars(&self, _phase: PipelinePhase) -> usize {
        5
    }
}
