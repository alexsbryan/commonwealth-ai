// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ingest's implementation of the [`RecipeTester`] contract over the real
//! [`CorpusEngine`], and the [`RecipeAuthorSeams`] recipe authoring takes
//! from ingest.
//!
//! Moved from sovereign-tools' `recipe_tester_adapter` (pb-ingest-dial-tools):
//! the authoring tools depend only on the contract, and the host that
//! composes ingest hands these in, exactly like the note port svrn's store
//! implements.
//!
//! It is a faithful in-process stand-in for the future daemon test endpoint:
//! same recipe + params → the same diagnostics. It maps the engine's rich
//! `TestReport` onto the contract's [`RecipeTestOutcome`] field-for-field, so
//! the tools' output is unchanged from when they called `test_recipe` directly.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::{CorpusEngine, TestOptions};
use corpus_index::types::EmbedFn;
use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::recipe::testing::{
    ExtractionOutcome, RecipeAuthorSeams, RecipeTestOutcome, RecipeTestParams, RecipeTester,
    SectionMiss, ValidationOutcome,
};

/// What recipe authoring takes from ingest: the tester over this engine, the
/// recipe variant-catalog descriptor and the bundled registry snapshot.
pub fn recipe_author_seams() -> RecipeAuthorSeams {
    RecipeAuthorSeams {
        tester: Arc::new(CorpusEngineRecipeTester::new()),
        descriptor_json: crate::recipe_schema::RECIPE_SCHEMA_DESCRIPTOR_JSON,
        registry_toml: crate::registry::BUNDLED_REGISTRY_TOML,
    }
}

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
        let stub_embed: EmbedFn = Arc::new(|_text| {
            Box::pin(async { Ok(vec![0f32; corpus_index::types::DEFAULT_EMBED_DIM]) })
        });
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
