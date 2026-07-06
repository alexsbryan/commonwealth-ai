// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe-test seam.
//!
//! `RecipeValidateTool` and `RecipeTestTool` drive a recipe through the corpus
//! engine's test harness (`CorpusEngine::test_recipe`) — a runtime dependency
//! the extractable authoring package cannot carry.
//!
//! [`RecipeTester`] is the contract they depend on instead: run a recipe over a
//! sample and return the diagnostics the tools render. A monolith-side adapter
//! implements it over the real engine; the package sees only this trait.
//!
//! The outcome is a *rich* projection — every field the tools render today —
//! not the lossy `oicp_types::RecipeTestReport` wire shape. Rendering the tools
//! from the wire projection would drop the validation error/warning split, the
//! extraction rate, the structured section misses, and the nudge, changing tool
//! output. Preserving behavior (the B:P4 mandate) means the in-process tester
//! returns everything; the wire shape enters only at the HTTP cutover, which is
//! an explicit behavior change.

use std::collections::BTreeMap;
use std::path::Path;

use async_trait::async_trait;

use crate::error::Result;

/// Knobs for a recipe test run. Mirrors the used subset of the engine's
/// `TestOptions`.
#[derive(Debug, Clone, Default)]
pub struct RecipeTestParams {
    /// Source records to sample. `0` = validation-only (no download/extract).
    pub sample_size: usize,
    /// Embed chunks and run a search test. The authoring tools never enable
    /// this (no model is loaded in-tool) but the field is carried for fidelity.
    pub embed: bool,
    /// Skip the HTTP HEAD-check on the source URL.
    pub offline: bool,
    /// Install-time parameter values, validated against the recipe's parameter
    /// schema before acquisition.
    pub parameters: BTreeMap<String, toml::Value>,
}

/// Schema / regex / placeholder validation outcome.
#[derive(Debug, Clone, Default)]
pub struct ValidationOutcome {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Extraction-phase counts. `None` when `sample_size == 0` or acquisition
/// failed.
#[derive(Debug, Clone)]
pub struct ExtractionOutcome {
    pub records_attempted: usize,
    pub records_succeeded: usize,
    pub extraction_rate: f32,
}

/// One section the `html_sections` extractor expected but did not find.
#[derive(Debug, Clone)]
pub struct SectionMiss {
    pub file: String,
    pub section: String,
    pub description: String,
    /// 200-char snippet near where the section was expected; `None`/empty on
    /// empty inputs.
    pub nearby_text: Option<String>,
}

/// Everything the authoring tools render from a test run.
#[derive(Debug, Clone)]
pub struct RecipeTestOutcome {
    pub validation: ValidationOutcome,
    pub extraction: Option<ExtractionOutcome>,
    pub section_misses: Vec<SectionMiss>,
    /// Precomputed `TestReport::passed()` — the merge-ready verdict (no
    /// validation errors AND extraction rate ≥ 0.80 AND no over-limit chunks
    /// AND every test query hit). `RecipeTestTool` reports this; note that
    /// `RecipeValidateTool` uses its own weaker `errors.is_empty()` verdict.
    pub passed: bool,
}

/// Run a recipe through the test harness. A monolith-side adapter implements
/// this over `corpus_engine::CorpusEngine::test_recipe`.
///
/// Takes the resolved recipe **path**, not the TOML source: the harness reads a
/// `_section_misses.json` sidecar relative to the recipe directory, so staging
/// the TOML to a throwaway file would silently empty `section_misses`. The
/// eventual HTTP tester (which must ship TOML over the wire) accepts that as a
/// documented behavior change.
#[async_trait]
pub trait RecipeTester: Send + Sync {
    async fn test(
        &self,
        recipe_path: &Path,
        params: &RecipeTestParams,
    ) -> Result<RecipeTestOutcome>;
}
