// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atlas-vs-tiered enrichment dispatch for a local corpus.
//!
//! `enrich_now` is the one-shot "enrich this corpus" entry point; which BUILD
//! that means is decided by the corpus recipe's `[enrichment] type`, resolved
//! through `corpus_engine::enrichment::pass::EnrichmentPassRegistry` — the
//! one decider every other site uses (ARCH §10.6; this module carried its own
//! `enrich_route` copy until 2026-09-03, and it disagreed with ingest about
//! unknown types). The `atlas` pass runs the atlas orchestrator in-process (a
//! shipped desktop carries no CLI to shell out to); everything else, and every
//! recipe-less folder corpus, keeps the tiered RAPTOR + entity build.
//!
//! [`atlas_progress_to_state`] is the other half: it routes the orchestrator's
//! progress events into the corpus's `_enrichment_state.json` — the surface
//! `lc_enrichment_status` already polls — so an in-process atlas build is as
//! visible to the UI as a tiered one.
//!
//! Carved out of `manager.rs` 2026-09-01. The two methods stay `impl
//! LocalCorpusManager` because they are the manager's API, which is why
//! `engine`, `enrichment_driver` and `engine_index_dir` widened from private
//! to `pub(super)` there: the narrowest visibility that lets this module see
//! them, and no wider.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::enrichment::pass::{self, EnrichmentPassRegistry};
use corpus_engine::enrichment::pipeline::EnrichProgress;
use corpus_engine::enrichment::state::{EnrichmentPhase, EnrichmentStateFile};
use sovereign_core::error::{Error, Result};

use super::manager::LocalCorpusManager;

impl LocalCorpusManager {
    /// One-shot tiered enrichment for a folder corpus (vault, watched, or
    /// a one-shot document folder) — no pipeline id needed. This is "the
    /// watched folder's first sweep, without the watching": it runs the
    /// same tiered build (`start_tiered_build`) the enable path uses, so a
    /// drag-drop import gets the RAPTOR + entity atlas a watched folder
    /// gets. The pipeline id is a formality the tiered path ignores.
    pub async fn enrich_now(&self, corpus_id: &str) -> Result<String> {
        // ontology-v1 P0.4 — the recipe's `[enrichment] type` decides which
        // build this is. `tiered` (and every recipe-less folder corpus) keeps
        // today's path; `atlas` runs the CLI's `enrich build` orchestrator
        // in-process, so a shipped desktop (no CLI on PATH) can build a
        // recipe-driven / custom-ontology atlas at all.
        let recipe_type = match self.engine.load_recipe(corpus_id).await {
            Ok(r) => r.enrichment.map(|e| e.enrichment_type),
            Err(e) => {
                tracing::debug!(
                    corpus_id = %corpus_id,
                    error = %e,
                    "enrich_now: no recipe for corpus; tiered path"
                );
                None
            }
        };
        let pass = recipe_type
            .as_deref()
            .and_then(|t| EnrichmentPassRegistry::builtin().get(t));
        let is_atlas = pass.as_ref().is_some_and(|p| p.id() == pass::ATLAS);
        tracing::info!(
            corpus_id = %corpus_id,
            recipe_type = recipe_type.as_deref().unwrap_or("<none>"),
            pass = pass.as_ref().map(|p| p.id()).unwrap_or("<none>"),
            route = if is_atlas { "atlas" } else { "tiered" },
            "enrich_now: route decided"
        );
        if is_atlas {
            self.start_atlas_build(corpus_id).await
        } else {
            self.enable_enrichment(corpus_id, "referential_atlas").await
        }
    }

    /// The in-process atlas build for an `[enrichment] type = "atlas"`
    /// recipe corpus. Refuses — naming the command — when the corpus has no
    /// enrichment config yet: the orchestrator would otherwise fail before
    /// its first progress event and the UI would see only an exit code
    /// (§18.3). Progress lands in the corpus's `_enrichment_state.json`, the
    /// file `lc_enrichment_status` already reads.
    async fn start_atlas_build(&self, corpus_id: &str) -> Result<String> {
        match sovereign_enrichment_catalog::config::EnrichConfig::load(corpus_id) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(Error::Execution(format!(
                    "corpus '{corpus_id}' has an atlas recipe but no enrichment config — \
                     run `svrn enrich init {corpus_id} --from-corpus {corpus_id}` first"
                )))
            }
            Err(e) => {
                return Err(Error::Execution(format!(
                    "read enrichment config for '{corpus_id}': {e}"
                )))
            }
        }
        let index_dir = self.engine_index_dir().join(corpus_id);
        let sink = atlas_progress_to_state(index_dir, corpus_id.to_string());
        self.enrichment_driver
            .start_atlas_build(corpus_id, sink)
            .await
    }
}

/// Route the atlas orchestrator's progress events into the corpus's
/// `_enrichment_state.json` — the surface `lc_enrichment_status` polls — so an
/// in-process build is as visible as a tiered one. Steps stamp
/// `AtomExtraction` (the closest phase the file's enum has to "building the
/// atlas") with `<ordinal>/<total>` and the step id as message; `Complete`
/// stamps `Complete`; a failed, aborted or cancelled build stamps `Failed`
/// with the step's own words. Stamp failures are warned, never fatal.
fn atlas_progress_to_state(
    index_dir: PathBuf,
    corpus_id: String,
) -> crate::enrich::EnrichProgressFn {
    let counters = std::sync::Mutex::new((0u64, 0u64)); // (current, total)
    Arc::new(move |evt: EnrichProgress| {
        let mut c = counters.lock().unwrap_or_else(|p| p.into_inner());
        let stamp = |phase: EnrichmentPhase, cur: u64, total: u64, msg: &str| {
            EnrichmentStateFile::stamp(
                &index_dir,
                &corpus_id,
                Some("atlas"),
                phase,
                cur,
                total,
                Some(msg),
            )
            .map(drop)
        };
        let fail = |msg: &str| EnrichmentStateFile::fail(&index_dir, &corpus_id, msg).map(drop);
        let result = match evt {
            EnrichProgress::BuildStart { steps, .. } => {
                *c = (0, steps.len() as u64);
                stamp(
                    EnrichmentPhase::AtomExtraction,
                    0,
                    c.1,
                    "atlas build: planned",
                )
            }
            EnrichProgress::StepStart {
                step,
                ordinal,
                total,
                ..
            } => {
                *c = (ordinal as u64, total as u64);
                stamp(EnrichmentPhase::AtomExtraction, c.0, c.1, step.id())
            }
            EnrichProgress::StepDone { step, summary, .. } => stamp(
                EnrichmentPhase::AtomExtraction,
                c.0,
                c.1,
                &format!("{}: {summary}", step.id()),
            ),
            EnrichProgress::StepFailed { step, message, .. } => {
                fail(&format!("{}: {message}", step.id()))
            }
            EnrichProgress::Aborted {
                failed_step,
                exit_code,
                ..
            } => fail(&format!(
                "atlas build aborted at `{}` (exit {exit_code})",
                failed_step.id()
            )),
            EnrichProgress::Cancelled { at_step, .. } => fail(&format!(
                "atlas build cancelled{}",
                at_step
                    .map(|s| format!(" before `{}`", s.id()))
                    .unwrap_or_default()
            )),
            EnrichProgress::SpawnFailed { message, .. } => {
                fail(&format!("atlas build could not start: {message}"))
            }
            EnrichProgress::Complete {
                steps_completed, ..
            } => stamp(
                EnrichmentPhase::Complete,
                steps_completed as u64,
                steps_completed as u64,
                "atlas build complete",
            ),
            EnrichProgress::ChapterProgress { .. } | EnrichProgress::ChapterFailed { .. } => Ok(()),
        };
        if let Err(e) = result {
            tracing::warn!(
                corpus_id = %corpus_id,
                error = %e,
                "atlas build: enrichment state stamp failed"
            );
        }
    })
}

#[cfg(test)]
mod atlas_dispatch_tests {
    use super::*;
    use corpus_engine::enrichment::pipeline::BuildStep;

    /// The progress → state mapping the UI reads: a step in flight shows
    /// `<ordinal>/<total>` under a non-terminal phase, `Complete` closes the
    /// run, and a failed step lands as `Failed` carrying the step's words.
    #[test]
    fn atlas_progress_stamps_the_enrichment_state_file() {
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("c1");
        std::fs::create_dir_all(&index_dir).unwrap();
        let sink = atlas_progress_to_state(index_dir.clone(), "c1".into());

        sink(EnrichProgress::BuildStart {
            corpus_id: "c1".into(),
            pipeline_id: "custom_atlas".into(),
            steps: vec![BuildStep::Extract, BuildStep::Resolve, BuildStep::Backfill],
            auto_skipped: vec![],
        });
        sink(EnrichProgress::StepStart {
            corpus_id: "c1".into(),
            step: BuildStep::Resolve,
            ordinal: 2,
            total: 3,
        });
        let st = EnrichmentStateFile::read(&index_dir).unwrap().unwrap();
        assert_eq!(st.phase, EnrichmentPhase::AtomExtraction);
        assert_eq!((st.step_current, st.step_total), (2, 3));
        assert_eq!(st.message.as_deref(), Some("resolve"));
        assert!(!st.phase.is_terminal());

        sink(EnrichProgress::Complete {
            corpus_id: "c1".into(),
            steps_completed: 3,
        });
        let st = EnrichmentStateFile::read(&index_dir).unwrap().unwrap();
        assert_eq!(st.phase, EnrichmentPhase::Complete);
        assert!(st.completed_at.is_some());

        sink(EnrichProgress::StepFailed {
            corpus_id: "c1".into(),
            step: BuildStep::Backfill,
            message: "the embed slot did not answer".into(),
            exit_code: 1,
        });
        let st = EnrichmentStateFile::read(&index_dir).unwrap().unwrap();
        assert_eq!(st.phase, EnrichmentPhase::Failed);
        assert!(
            st.error
                .as_deref()
                .unwrap_or("")
                .contains("backfill: the embed slot did not answer"),
            "got {:?}",
            st.error
        );
    }
}
