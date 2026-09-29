// SPDX-License-Identifier: AGPL-3.0-or-later
//! The harness as svrn's daemon reaches it: [`RecipeHarnessPort`] over the
//! engine, so the daemon's harness route names no engine
//! (pb-ingest-dial-daemon-ports). The job body moved here from the daemon's
//! `recipe_http.rs`; the drive it calls is the one `svrn recipe test` calls.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use corpus_engine::harness::verify_atoms_at;
use corpus_engine::{CorpusEngine, Recipe};
use corpus_index::ingest_port::daemon::{HarnessRunCardView, RecipeHarnessPort};

use crate::Declaration;

/// The card at the concrete `HarnessRun`, before it crosses as JSON.
type HarnessCard = HarnessRunCardView<crate::HarnessRun>;

/// The engine's harness: the one [`RecipeHarnessPort`] implementor.
pub struct EngineHarness {
    engine: Arc<CorpusEngine>,
}

impl EngineHarness {
    /// The harness over `engine`.
    pub fn new(engine: Arc<CorpusEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl RecipeHarnessPort for EngineHarness {
    async fn run_recipe_harness(
        &self,
        recipe_toml: &str,
        harness_root: &Path,
        sample_size: usize,
        enrich: bool,
        index_dir: &Path,
        notice: &(dyn for<'s> Fn(&'s str) + Sync),
    ) -> Result<HarnessRunCardView, String> {
        let recipe =
            Recipe::from_toml(recipe_toml).map_err(|e| format!("recipe TOML parse failed: {e}"))?;
        let card = run_harness_job(
            &self.engine,
            &recipe,
            harness_root,
            sample_size,
            enrich,
            index_dir,
            notice,
        )
        .await?;
        // The same bytes the daemon serialised at the concrete type.
        let run = serde_json::to_value(&card.run)
            .map_err(|e| format!("harness run did not serialise: {e}"))?;
        Ok(HarnessRunCardView {
            green: card.green,
            run,
            ran_at_unix: card.ran_at_unix,
            frozen_docs: card.frozen_docs,
            frozen_captured_at: card.frozen_captured_at,
            frozen_captured_now: card.frozen_captured_now,
        })
    }
}

/// The body of the spawned harness job, lifted out so the spawn reads as one
/// call and the `?` chain is not hand-unrolled (ARCH principle 8 — the drive
/// itself is shared with the CLI; this is only the daemon's rung 6 around it).
async fn run_harness_job(
    engine: &CorpusEngine,
    recipe: &Recipe,
    harness_root: &std::path::Path,
    sample_size: usize,
    enrich: bool,
    index_dir: &std::path::Path,
    notice: &(dyn Fn(&str) + Sync),
) -> Result<HarnessCard, String> {
    let frozen_run =
        crate::run_over_frozen_sample(engine, recipe, harness_root, sample_size, false, notice)
            .await?;

    // Rung 6 (opt-in): verify the atoms the DAEMON's own ingest+enrich
    // already wrote for this corpus. Not a parallel enrichment pipeline —
    // the same index every retrieval reads.
    let enrich_out = if enrich {
        verify_atoms_at(index_dir)
            .await
            .map_err(|e| format!("enrich verify failed: {e}"))?
    } else {
        None
    };

    let run = frozen_run.verdicts(recipe, enrich_out.as_ref(), &Declaration::default());
    Ok(HarnessRunCardView {
        green: run.green(),
        frozen_docs: frozen_run.frozen_docs(),
        frozen_captured_at: frozen_run.captured_at(),
        frozen_captured_now: frozen_run.captured_now,
        ran_at_unix: sovereign_time::unix_now_u64(),
        run,
    })
}
