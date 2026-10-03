// SPDX-License-Identifier: AGPL-3.0-or-later
//! The harness DRIVE — capture-if-needed, load the frozen sample, run the
//! deterministic rungs.
//!
//! # Why this is a function and not a comment saying "same as the CLI"
//!
//! Three surfaces ran this sequence verbatim: `svrn recipe test`
//! (`sovereign-cli-llm/src/recipe_cmd.rs`), the desktop's
//! `recipe_run_harness`, and — once the desktop became a client — the daemon
//! route that replaced it. N sites running one sequence share one bug (ARCH
//! principle 8), and the sequence has an ordering constraint worth holding in
//! one place: the capture is the ONE networked step (I3), so a run that skips
//! it must still load the sample the earlier capture wrote, and a `capture.json`
//! that is missing AFTER a capture is a refusal rather than an empty run.
//!
//! What genuinely differs between the callers is rung 6, and that stays
//! theirs: the CLI ingests the frozen sample through a daemon-backed engine
//! and verifies the atoms THAT produced; the daemon verifies the atoms its own
//! installed corpus already holds. Neither is a mode of the other, so the
//! difference is a parameter to [`FrozenRun::verdicts`], not a flag inside the
//! drive.

use std::path::Path;

use corpus_engine::harness::{capture, EnrichOutput, FrozenSample, HarnessRunner, StageOutputs};
use corpus_engine::{CorpusEngine, Recipe};

use crate::{run_deterministic, Declaration, HarnessRun};

/// One deterministic pass over a frozen sample: the sample itself, what the
/// stages produced, and whether THIS call performed the capture.
pub struct FrozenRun {
    pub frozen: FrozenSample,
    pub outputs: StageOutputs,
    /// True when this call ran the one networked step — what lets a surface
    /// say "froze N docs" the first time and "offline" thereafter.
    pub captured_now: bool,
}

impl FrozenRun {
    /// Documents in the frozen sample.
    pub fn frozen_docs(&self) -> usize {
        self.frozen.manifest.docs.len()
    }

    /// When the sample was captured (the sidecar's timestamp — never an
    /// input to a verdict, I1).
    pub fn captured_at(&self) -> i64 {
        self.frozen.manifest.captured_at
    }

    /// The verdict ladder. `enrich` is rung 6 and is the caller's: `None`
    /// means the rung did not run, which is reported as an absent rung
    /// rather than a passing one.
    pub fn verdicts(
        &self,
        recipe: &Recipe,
        enrich: Option<&EnrichOutput>,
        declaration: &Declaration,
    ) -> HarnessRun {
        run_deterministic(
            &self.frozen.manifest,
            recipe,
            &self.outputs,
            enrich,
            declaration,
        )
    }
}

/// Capture the frozen sample if there is not one (or `recapture` forces it),
/// then run rungs 1-5 over it.
///
/// `notice` is the caller's glassbox sink — the CLI writes to stderr, a daemon
/// writes a tracing event. It is called only around the networked step, which
/// is the only part a user waits on without knowing why.
///
/// Errors are `String` because every caller renders them to a human: a CLI
/// line, a job's `error` field, a Tauri `Err`. Each message names the step it
/// failed in, so one spelling serves all three.
pub async fn run_over_frozen_sample(
    engine: &CorpusEngine,
    recipe: &Recipe,
    harness_root: &Path,
    sample_size: usize,
    recapture: bool,
    notice: &(dyn Fn(&str) + Sync),
) -> Result<FrozenRun, String> {
    let mut captured_now = false;
    if recapture || !harness_root.join("capture.json").exists() {
        if recapture {
            // Best-effort, and NOT a silent downgrade of the recapture: what
            // this clears is orphaned blobs, while `capture` below re-acquires
            // and rewrites `capture.json` whether or not the directory went.
            // A recapture that could not delete still recaptures.
            let _ = std::fs::remove_dir_all(harness_root);
        }
        notice("capturing a frozen sample (the one networked step)");
        let manifest = capture(engine, recipe, harness_root, sample_size)
            .await
            .map_err(|e| format!("frozen-sample capture failed: {e}"))?;
        notice(&format!(
            "froze {} docs from {} — sample {}",
            manifest.docs.len(),
            manifest.acquirer,
            manifest.sample_id
        ));
        captured_now = true;
    }

    // A missing sample AFTER a capture is a refusal, not an empty run: the
    // rungs below would all report zero and read as a clean pass (ARCH
    // principle 6).
    let frozen = FrozenSample::load(harness_root)
        .map_err(|e| format!("load frozen sample: {e}"))?
        .ok_or_else(|| "no frozen sample found after capture".to_string())?;

    let work_dir = std::env::temp_dir().join(format!("harness-run-{}", recipe.corpus.id));
    let outputs = HarnessRunner::new(engine, recipe, &frozen)
        .run(&work_dir, sample_size)
        .await
        .map_err(|e| format!("harness run failed: {e}"))?;

    Ok(FrozenRun {
        frozen,
        outputs,
        captured_now,
    })
}
