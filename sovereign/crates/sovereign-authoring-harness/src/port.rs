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

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine half of sovereign-daemon's recipe_surface harness case
    /// (pb-ingest-dial-daemon-tests-reads): the daemon test drives
    /// `RecipeHarnessDouble` and asserts the roots it passes; the card's
    /// own values are this implementor's. The first run over a root
    /// captures its one local file there, the second reuses it, and
    /// `enrich: false` leaves rung 6 out of the ladder.
    #[tokio::test]
    async fn the_first_run_captures_and_the_second_reuses_the_frozen_sample() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source.txt");
        std::fs::write(
            &source,
            "Alpha paragraph, long enough to survive a chunker.\n\n\
             Beta paragraph, likewise long enough to be kept.\n",
        )
        .unwrap();
        let recipe = format!(
            r#"
[corpus]
id = "dry-run-test"
name = "Dry run test"
description = "a recipe tested over a local file"
license = "Public Domain"
size_compressed_gb = 0.001
size_indexed_gb = 0.001

[acquire]
type = "local_file"
path = "{}"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
max_chars = 2048
overlap_chars = 0

[index]
fts = true
vector = false
"#,
            source.display()
        );
        let embed: corpus_index::types::EmbedFn =
            Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.0_f32; 8]) }));
        let harness = EngineHarness::new(Arc::new(CorpusEngine::new(
            tmp.path().join("recipes"),
            tmp.path().join("indexes"),
            embed,
        )));
        let root = tmp.path().join("harness").join("dry-run-test");
        let index_dir = tmp.path().join("indexes").join("dry-run-test");
        let run = || harness.run_recipe_harness(&recipe, &root, 5, false, &index_dir, &|_| {});

        let first = run().await.unwrap();
        assert!(first.frozen_captured_now, "the first run captures");
        assert_eq!(first.frozen_docs, 1, "one local file, one doc");
        let stages = first.run["stages"].as_array().expect("a stage ladder");
        assert!(!stages.is_empty(), "{:#?}", first.run);
        assert!(
            !stages.iter().any(|s| s["stage"] == "enrich"),
            "rung 6 did not run and must not appear: {:#?}",
            first.run
        );
        assert!(
            root.join("capture.json").exists(),
            "the frozen sample lands under the root it was given"
        );

        let second = run().await.unwrap();
        assert!(!second.frozen_captured_now, "the second run reuses it");
    }
}
