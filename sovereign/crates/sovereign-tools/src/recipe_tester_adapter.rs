// SPDX-License-Identifier: AGPL-3.0-or-later
//! Monolith-side adapter implementing [`RecipeTester`] over the real
//! `corpus_engine::CorpusEngine`.
//!
//! Lives here (not in the recipe-author package) because it is the one piece
//! that must touch `corpus-engine`. The authoring tools depend only on the
//! `RecipeTester` contract; this adapter is injected at their construction
//! sites, exactly like [`crate::recipe_notes_adapter::NoteStoreRecipeNotes`].
//!
//! It is a faithful in-process stand-in for the future daemon test endpoint:
//! same recipe + params → the same diagnostics. It maps the engine's rich
//! `TestReport` onto the contract's [`RecipeTestOutcome`] field-for-field, so
//! the tools' output is unchanged from when they called `test_recipe` directly.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use corpus_engine::{CorpusEngine, EmbedFn, TestOptions};
use sovereign_contracts::recipe::testing::{
    ExtractionOutcome, RecipeTestOutcome, RecipeTestParams, RecipeTester, SectionMiss,
    ValidationOutcome,
};
use sovereign_core::error::{Error, Result};

/// `RecipeTester` backed by an in-process stub `CorpusEngine`.
#[derive(Default)]
pub struct CorpusEngineRecipeTester;

impl CorpusEngineRecipeTester {
    pub fn new() -> Self {
        Self
    }

    /// A `CorpusEngine` with a zero-vector stub embed function. The test
    /// harness never touches embeddings in the authoring flow (`embed = false`),
    /// but the constructor requires an `EmbedFn`. The temp dir is scratch space
    /// for the engine's corpus/db roots.
    fn build_stub_engine() -> CorpusEngine {
        let stub_embed: EmbedFn =
            Arc::new(|_text| Box::pin(async { Ok(vec![0f32; corpus_engine::DEFAULT_EMBED_DIM]) }));
        let tmp = std::env::temp_dir().join("sovereign-recipe-author-tester");
        CorpusEngine::new(tmp.clone(), tmp, stub_embed)
    }
}

#[async_trait]
impl RecipeTester for CorpusEngineRecipeTester {
    async fn test(
        &self,
        recipe_path: &Path,
        params: &RecipeTestParams,
    ) -> Result<RecipeTestOutcome> {
        let engine = Self::build_stub_engine();
        let options = TestOptions {
            sample_size: params.sample_size,
            embed: params.embed,
            offline: params.offline,
            parameters: params.parameters.clone(),
            ..Default::default()
        };
        let report = engine
            .test_recipe(recipe_path, &options)
            .await
            .map_err(|e| Error::InvalidInput(format!("{e}")))?;

        Ok(RecipeTestOutcome {
            passed: report.passed(),
            validation: ValidationOutcome {
                errors: report.validation.errors.clone(),
                warnings: report.validation.warnings.clone(),
            },
            extraction: report.extraction.as_ref().map(|e| ExtractionOutcome {
                records_attempted: e.records_attempted,
                records_succeeded: e.records_succeeded,
                extraction_rate: e.extraction_rate,
            }),
            section_misses: report
                .section_misses
                .iter()
                .map(|m| SectionMiss {
                    file: m.file.clone(),
                    section: m.section.clone(),
                    description: m.description.clone(),
                    nearby_text: m.nearby_text.clone(),
                })
                .collect(),
        })
    }
}
