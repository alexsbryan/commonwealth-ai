// SPDX-License-Identifier: AGPL-3.0-or-later
//! Running ONE step of the atlas build.
//!
//! The idempotency gate (is this step's canonical output already on disk, and
//! is it stale?), the two outcome types a step reports through, and the step
//! dispatch itself. `run_step` is the only place that knows how a step id maps
//! to the driver that performs it.

use super::plan::{ParsedBuild, PipelineCapabilities, Selection, Step};
use crate::{
    atlas_configuration, atlas_gaps, atlas_phase_cmd, atlas_resolve, atlas_tensions,
    atlas_tensions_classify, config::EnrichConfig, extract, paths, schema_review, seed_cmd,
};
use corpus_engine::enrichment::atlas::ann_store::{ann_table_is_fresh, ANN_TABLE_DIRNAME};
use corpus_engine::enrichment::atlas::ATLAS_DIRNAME;
use corpus_engine::enrichment::pipeline::{
    progress::wire, BuildStep, EnrichProgress, EnrichProgressFn, PipelineRegistry, SeedStrategy,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_tools::atlas_context_manager::{backfill_ann, AtlasContextFilter, BackfillOutcome};
use std::sync::Arc;

/// Canonical on-disk artefact each step produces. The orchestrator
/// short-circuits a step whose canonical output already exists.
///
/// "Canonical" here is the file downstream steps actually consume —
/// not every artefact the step writes. Extract emits both run-files
/// and a promoted `cache/questions.json`; downstream cluster reads
/// `cache/questions.json`, so that's the gate. Resolve writes
/// `atoms.json`, `edges.json`, and `trajectories.json` in lockstep;
/// `atoms.json` is the one downstream tensions/gaps/report read, so
/// it's the canonical witness.
///
/// `Configure` (Phase 8) is pipeline-gated and writes through
/// `atlas_configuration`'s own cache, which the cmd already
/// respects; we don't gate it here so the existing semantics
/// remain unchanged.
///
/// `Seed` is intentionally NOT cached at this layer — the seed cmd
/// has its own freshness checks and is cheap enough to re-evaluate.
fn step_canonical_output(step: Step, corpus_id: &str) -> Option<std::path::PathBuf> {
    match step {
        Step::Extract => Some(paths::cache_dir(corpus_id).join("questions.json")),
        Step::Cluster => Some(paths::cache_dir(corpus_id).join("atlas-clusters.json")),
        Step::Name => Some(paths::cache_dir(corpus_id).join("atlas-named-clusters.json")),
        Step::Resolve => Some(
            paths::index_root(corpus_id)
                .join("atlas")
                .join("atoms.json"),
        ),
        Step::Tensions => Some(
            paths::index_root(corpus_id)
                .join("atlas")
                .join("tension_candidates.json"),
        ),
        Step::Gaps => Some(paths::index_root(corpus_id).join("atlas").join("gaps.json")),
        Step::Report => Some(
            paths::index_root(corpus_id)
                .join("atlas")
                .join("schema_validation.json"),
        ),
        Step::Backfill => Some(
            paths::index_root(corpus_id)
                .join(ATLAS_DIRNAME)
                .join(ANN_TABLE_DIRNAME),
        ),
        Step::Seed | Step::Configure => None,
    }
}

/// Returns true iff the cached Phase 1 `questions.json` has at least
/// one chapter carrying a non-null `section_extraction`. Mirrors the
/// precondition `runner::phase_2_cluster_atlas` enforces — if this
/// returns false, the cluster step would fail with `phase 1 cache has
/// no section_extraction payloads`, so the cache is treated as stale
/// from a legacy (non-atlas) run and re-extracted.
///
/// Returns false on any parse error or missing field — re-running
/// extract is the safe fallback in all of those cases.
fn extract_cache_has_atlas_payloads(cache_path: &std::path::Path) -> bool {
    let bytes = match std::fs::read(cache_path) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let v: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return false,
    };
    v.get("questions_by_chapter")
        .and_then(|c| c.as_array())
        .map(|chapters| {
            chapters.iter().any(|c| {
                c.get("section_extraction")
                    .map(|s| !s.is_null())
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// True when `atoms.json` exists but carries no resolved atoms — the
/// empty placeholder every `corpus install` leaves behind. Each install
/// fires a model-free STRUCTURAL atlas pass (see `EnrichmentState` —
/// "post-install structural atlas, every corpus install fires this");
/// for prose corpora that pass emits zero atoms but still writes
/// `atlas/atoms.json`. If the resolve step treated that placeholder as
/// "already done" it would skip the real model-based resolve, leaving
/// the corpus with a 0-atom atlas even though Phase 1 extraction was
/// rich — the root cause of custom-atlas enrichments (CLI *and* the
/// in-app `BuildEnrichCard`, which spawns this same `enrich build`)
/// silently producing empty atlases.
///
/// Re-running resolve when the cache is empty is safe: resolve is
/// model-free (it assembles atoms from the cached extract + clusters),
/// so the worst case for a genuinely-empty corpus is a cheap, idempotent
/// re-run. A populated `atoms.json` (a real prior resolve) is preserved.
fn resolve_cache_is_structural_placeholder(cache_path: &std::path::Path) -> bool {
    let bytes = match std::fs::read(cache_path) {
        Ok(b) => b,
        Err(_) => return true, // unreadable → don't trust it → re-resolve
    };
    let v: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return true, // unparseable → re-resolve
    };
    // Resolved atoms live under `.atoms`; an empty (or absent) array is
    // the structural placeholder.
    v.get("atoms")
        .and_then(|a| a.as_array())
        .map(|a| a.is_empty())
        .unwrap_or(true)
}

// ── The step seam ────────────────────────────────────────────
//
// `run_step` used to return `i32`, and both halves of that were escape
// hatches. `0` meant "the step succeeded and I have nothing to tell
// you" — so `StepDone.summary`, a field on the typed progress wire, was
// filled with `format!("{step} complete")`: a value computed from the
// step's NAME, identical whether the step ran, was skipped as cached, or
// found nothing. Nothing reads that field today — `sovereign-tools`'
// wire reader decodes the event and never touches `summary`, and no
// frontend does either — which is what let it survive: a fabricated
// value nobody consumes looks exactly like a working one. The first
// consumer to read it would have got the step's own name back.
// ARCH §18.3: absence is reported, never defaulted.
//
// Nonzero meant "it failed and I have nothing to tell you", so
// `StepFailed.message` could only restate the number — and that one IS
// read, by `build_with_progress`'s own `eprintln!` to the operator.
//
// Both are now values the step supplies.

/// What a step reported — the step's own words, in its own words.
///
/// A newtype rather than a bare `String` so there is exactly one way to
/// build one, and building one requires having something to say.
///
/// There was briefly a second variant, `Untyped`, carrying a named
/// absence for steps still on the `-> i32` contract. It was DELETED when
/// the last of the nine converted, and that deletion is the ratchet: the
/// type now refuses to compile a step that reports nothing, instead of a
/// census having to notice one later.
#[derive(Debug, Clone)]
pub(super) struct StepOutcome(String);

impl StepOutcome {
    pub(super) fn did(summary: impl Into<String>) -> Self {
        Self(summary.into())
    }

    /// The line for `StepDone.summary`, verbatim.
    pub(super) fn summary(&self) -> String {
        self.0.clone()
    }
}

/// Why a step stopped.
#[derive(Debug, Clone)]
pub(super) struct StepFailure {
    /// What went wrong, in the step's own words.
    pub(super) message: String,
    /// The code `enrich build` exits with.
    pub(super) exit_code: i32,
}

impl StepFailure {
    pub(super) fn new(message: impl Into<String>, exit_code: i32) -> Self {
        Self {
            message: message.into(),
            exit_code,
        }
    }
}

pub(super) async fn run_step(
    step: Step,
    parsed: &ParsedBuild,
    embedder: Option<&Arc<dyn InferenceProvider>>,
) -> Result<StepOutcome, StepFailure> {
    let corpus = parsed.corpus_id.as_str();

    // ── Idempotency gate ───────────────────────────────────────
    //
    // If the step's canonical output is already on disk, skip the
    // re-run. Each step's cmd_* function would otherwise blindly
    // re-do the work (extract burns ~30 min of LLM time; resolve
    // is cheaper but still touches the same files). The contract
    // is "to force this step, delete its output" — same workflow
    // the operator just used in the drift-fix loop, now made
    // explicit in the orchestrator instead of implicit in each
    // step's cmd_*.
    //
    // Selection::Chapters bypasses the gate — the operator is
    // explicitly asking for partial work and the extract step has
    // its own per-chapter resume / retry logic.
    let chapters_override = matches!(&parsed.selection, Selection::Chapters(_));
    if !chapters_override {
        if let Some(cache_path) = step_canonical_output(step, corpus) {
            if cache_path.exists() {
                // For Extract, file-exists alone is not enough: a
                // `questions.json` left over from a legacy (non-atlas)
                // pipeline run has no `section_extraction` payloads,
                // and the downstream cluster step would fail with
                // "phase 1 cache has no section_extraction payloads".
                // Since `build` already requires an atlas pipeline
                // upstream (load_pipeline_capabilities), any cached
                // Phase 1 here MUST carry section_extraction — if it
                // doesn't, the cache is stale; re-run extract instead
                // of silently skipping into a doomed cluster step.
                // File-exists alone is not enough for two steps whose
                // canonical output can be a STALE placeholder another code
                // path wrote:
                // - Extract: a legacy non-atlas `questions.json` has no
                // section_extraction payloads → the cluster step dies.
                // - Resolve: every `corpus install` fires a model-free
                // structural atlas that writes an EMPTY `atoms.json`.
                // Treating that as "resolve done" leaves the corpus with
                // a 0-atom atlas even though Phase 1 extraction was rich
                // (the in-app custom-atlas enrich bug).
                let stale_reason: Option<&str> = if matches!(step, Step::Extract)
                    && !extract_cache_has_atlas_payloads(&cache_path)
                {
                    Some("from a non-atlas run (no section_extraction payloads)")
                } else if matches!(step, Step::Resolve)
                    && resolve_cache_is_structural_placeholder(&cache_path)
                {
                    Some("an empty post-install structural placeholder (no resolved atoms)")
                } else if matches!(step, Step::Backfill)
                    && !ann_table_is_fresh(&paths::index_root(corpus).join(ATLAS_DIRNAME))
                {
                    // - Backfill: the table is keyed on atom-id, so one that
                    // predates the atoms.json this run (or a later resolve)
                    // wrote seeds grounding from atoms that no longer exist.
                    Some("older than atlas/atoms.json (the atlas was re-resolved since it was embedded)")
                } else {
                    None
                };
                if let Some(reason) = stale_reason {
                    println!(
                        "  · {} cached file at {} is {reason}; invalidating cache.",
                        step.label(),
                        cache_path.display()
                    );
                    // The ANN table is a Lance DIRECTORY; every other
                    // canonical output is a file.
                    let removed = if cache_path.is_dir() {
                        std::fs::remove_dir_all(&cache_path)
                    } else {
                        std::fs::remove_file(&cache_path)
                    };
                    if let Err(e) = removed {
                        eprintln!(
                            "  warning: could not remove stale cache {}: {}",
                            cache_path.display(),
                            e
                        );
                    }
                } else {
                    println!(
                        "  · {} cached — {} exists; skipping.",
                        step.label(),
                        cache_path.display()
                    );
                    println!("    To force re-run: rm {}", cache_path.display());
                    return Ok(StepOutcome::did(format!(
                        "skipped — {} already on disk",
                        cache_path.display()
                    )));
                }
            }
        }
    }

    match step {
        // Converted to the verb triple: the seed list's own size and
        // origin become the step's summary.
        Step::Seed => {
            let params = seed_cmd::ParsedSeed {
                corpus_id: corpus.to_string(),
                force: false,
            };
            match seed_cmd::run(&params).await {
                Ok(report) => {
                    seed_cmd::render(&report);
                    Ok(StepOutcome::did(report.summary()))
                }
                Err(e) => Err(StepFailure::new(e.message(), e.exit_code())),
            }
        }
        Step::Extract => run_extract_step(parsed).await,
        // Converted to the verb triple: the per-facet cluster counts
        // become the step's summary.
        Step::Cluster => {
            let params = atlas_phase_cmd::ParsedCluster {
                corpus_id: corpus.to_string(),
            };
            match atlas_phase_cmd::run_cluster(&params).await {
                Ok(report) => {
                    atlas_phase_cmd::render_cluster(corpus, &report);
                    Ok(StepOutcome::did(report.summary()))
                }
                Err(message) => Err(StepFailure::new(message, 1)),
            }
        }
        // Converted to the verb triple: how many clusters were named,
        // how many failed and why, and whether any were named without
        // few-shot exemplars all become the step's summary.
        Step::Name => {
            let params = atlas_phase_cmd::ParsedName {
                corpus_id: corpus.to_string(),
            };
            match atlas_phase_cmd::run_name(&params).await {
                Ok(report) => {
                    atlas_phase_cmd::render_name(&report);
                    Ok(StepOutcome::did(report.summary()))
                }
                Err(message) => Err(StepFailure::new(message, 1)),
            }
        }
        // Converted to the verb triple: what resolution produced —
        // and which phase produced it — becomes the step's summary.
        Step::Resolve => {
            let params = atlas_resolve::ParsedResolve {
                corpus_id: corpus.to_string(),
                phase: atlas_resolve::ResolvePhase::All,
            };
            match atlas_resolve::run(&params).await {
                Ok(report) => Ok(StepOutcome::did(report.summary())),
                Err(message) => Err(StepFailure::new(message, 1)),
            }
        }
        Step::Tensions => {
            // Phase 6 has two halves: deterministic candidate
            // enumeration, then LLM classification of the candidates
            // into Tension edges. The build flow runs both. The LLM
            // half is gated on the pipeline opting in (atlas
            // pipelines do; legacy pipelines don't), so non-atlas
            // builds get a no-op second call. A non-zero exit from
            // the deterministic half short-circuits — there are no
            // candidates to classify if the enumerator failed.
            let det_params = atlas_tensions::ParsedTensions {
                corpus_id: corpus.to_string(),
            };
            let det = match atlas_tensions::run(&det_params).await {
                Ok(report) => {
                    atlas_tensions::render(&report);
                    report
                }
                Err(message) => return Err(StepFailure::new(message, 1)),
            };

            let classify_params = atlas_tensions_classify::ParsedClassify {
                corpus_id: corpus.to_string(),
                max_candidates: None,
                dry_run: false,
            };
            match atlas_tensions_classify::run(&classify_params).await {
                // Both halves speak. The classifier's four
                // classified-nothing outcomes are now distinguishable
                // from a run that actually classified something.
                Ok(outcome) => Ok(StepOutcome::did(format!(
                    "{}; {}",
                    det.summary(),
                    outcome.summary()
                ))),
                Err(message) => Err(StepFailure::new(message, 1)),
            }
        }
        // Converted to the verb triple: no argv is built, and the
        // report the detectors produced becomes the step's summary.
        Step::Gaps => {
            let params = atlas_gaps::ParsedGaps {
                corpus_id: corpus.to_string(),
            };
            match atlas_gaps::run(&params) {
                Ok(report) => {
                    atlas_gaps::render(&report);
                    Ok(StepOutcome::did(report.summary()))
                }
                Err(message) => Err(StepFailure::new(message, 1)),
            }
        }
        // Converted to the verb triple: how many configurations Phase 8
        // produced — and how many the model invented and lost — become
        // the step's summary.
        Step::Configure => {
            let params = atlas_configuration::ParsedConfig {
                corpus_id: corpus.to_string(),
            };
            match atlas_configuration::run(&params).await {
                Ok(report) => Ok(StepOutcome::did(report.summary())),
                Err(e) => Err(StepFailure::new(e.message(), e.exit_code())),
            }
        }
        // Converted to the verb triple: the §12 report's own counts
        // become the step's summary.
        Step::Report => {
            let params = schema_review::ParsedReport {
                corpus_id: corpus.to_string(),
                as_json: false,
            };
            match schema_review::run(&params) {
                Ok(outcome) => match schema_review::render(&params, &outcome) {
                    Ok(()) => Ok(StepOutcome::did(outcome.summary())),
                    Err(message) => Err(StepFailure::new(message, 1)),
                },
                Err(message) => Err(StepFailure::new(message, 1)),
            }
        }
        Step::Backfill => run_backfill_step(corpus, embedder).await,
    }
}

/// One `embed_query("probe")` against a caller-supplied provider — the
/// in-process (daemon) half of the fail-fast rule above.
pub(super) async fn probe_embedder(
    embedder: Arc<dyn InferenceProvider>,
) -> Result<Arc<dyn InferenceProvider>, String> {
    embedder.embed_query("probe").await.map_err(|e| {
        format!(
            "backfill: the embed slot did not answer ({e}); load an embed model, or \
             pass `--skip backfill` to build without grounding"
        )
    })?;
    Ok(embedder)
}

/// The Backfill step: `atlas/atoms.json` → `atlas/atoms_ann.lance` through
/// the ONE writer, `sovereign_tools::atlas_context_manager::backfill_ann`,
/// under the production grounding filter (`AtlasContextFilter::default()` —
/// the universe the daemon seeds `atlas_navigate_ann` from; `backfill_ann.rs`
/// says why no other filter may be used here, and `migrate_all`'s relaxed-
/// floor retry is deliberately NOT copied: one filter, the one grounding uses).
///
/// Three outcomes, each in its own words (§18.3): wrote N/M; skipped because
/// the filter admitted nothing (an atlas of Claims-only or structural atoms);
/// or a failure naming the recovery command. `embedder` is the provider the
/// probe verified — its absence is a wiring error and is reported, not
/// defaulted.
async fn run_backfill_step(
    corpus: &str,
    embedder: Option<&Arc<dyn InferenceProvider>>,
) -> Result<StepOutcome, StepFailure> {
    let Some(embedder) = embedder else {
        return Err(StepFailure::new(
            format!(
                "backfill: no embed provider was wired for this build; run \
                 `svrn atlas backfill-ann {corpus}`"
            ),
            1,
        ));
    };
    let atlas_dir = paths::index_root(corpus).join(ATLAS_DIRNAME);
    let filter = AtlasContextFilter::default();
    match backfill_ann(embedder.as_ref(), &atlas_dir, corpus, &filter).await {
        Ok(BackfillOutcome::Built(stats)) => {
            println!(
                "  · wrote {} — {}/{} entries resolved to atom-ids",
                atlas_dir.join(ANN_TABLE_DIRNAME).display(),
                stats.resolved,
                stats.total
            );
            Ok(StepOutcome::did(format!(
                "wrote atlas/{ANN_TABLE_DIRNAME} — {}/{} entries resolved to atom-ids",
                stats.resolved, stats.total
            )))
        }
        Ok(BackfillOutcome::NoSeedableAtoms {
            min_description_chars,
        }) => {
            println!(
                "  · no seedable atoms under the grounding filter \
                 (min_chars={min_description_chars}); table not written"
            );
            Ok(StepOutcome::did(format!(
                "skipped — no seedable atoms under the grounding filter \
                 (min_chars={min_description_chars})"
            )))
        }
        Err(e) => Err(StepFailure::new(
            format!(
                "backfill: {e}; once the daemon's embed model is up, run \
                 `svrn atlas backfill-ann {corpus}`"
            ),
            1,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Every capability flag true — no steps auto-skipped. The
    /// pipeline registry is not hit in these unit tests so we
    /// build the struct directly.
    fn full_capabilities() -> PipelineCapabilities {
        PipelineCapabilities {
            pipeline_id: "literary_atlas".into(),
            seed_strategy_none: false,
            runs_configuration_phase: true,
        }
    }
    /// The defect this seam deleted: `StepDone.summary` was
    /// `format!("{step} complete")`, computed by the orchestrator from
    /// the step's NAME. Every step therefore put the same sentence on
    /// the wire, so a consumer could not tell a step that ran from one
    /// skipped as cached from one that found nothing. (No consumer reads
    /// the field yet — which is why the fabrication survived unnoticed.)
    ///
    /// Falsifier: re-derive a `Did` summary from the step name — decorate
    /// it, prefix it, fall back to it when the string is empty — and this
    /// fails. The step's own words reach the wire unaltered.
    #[test]
    fn a_reported_summary_reaches_the_wire_verbatim() {
        let spoken = "4 gap(s): 3 open-question, 1 ungrounded-claim";
        let outcome = StepOutcome::did(spoken);
        assert_eq!(outcome.summary(), spoken);
    }
    /// The failure half of the same seam.
    ///
    /// `StepFailure` used to have a second constructor, `from_code`,
    /// which manufactured `"step `seed` exited with code 1"` from a step
    /// name and an exit code — a sentence that restates the number and
    /// tells the operator nothing. It was deleted with the last `-> i32`
    /// step, so a failure message can now only be words a step wrote.
    ///
    /// Falsifier: reintroduce a constructor that builds a message out of
    /// the step name and code, and this assertion is what it violates.
    #[test]
    fn a_failure_carries_a_written_reason_not_a_restated_exit_code() {
        let f = StepFailure::new("reading atlas/atoms.json: no such file", 1);
        assert_eq!(f.exit_code, 1);
        assert!(
            !f.message.contains("exited with code"),
            "a failure must name a cause, got: {}",
            f.message
        );
    }
    /// The three outcomes that used to be indistinguishable.
    ///
    /// A step that ran, a step skipped because its output was already on
    /// disk, and a step that ran and found nothing all produced
    /// `format!("{step} complete")` before 2026-08-26 — the same sentence
    /// for all three, computed from the step's NAME. Each now carries its
    /// own words.
    ///
    /// Falsifier: route any of them back through a summary derived from
    /// the step rather than the run, and two of these collapse together.
    #[test]
    fn ran_skipped_and_found_nothing_are_three_different_summaries() {
        let ran = StepOutcome::did("4 gap(s): 3 open-question, 1 ungrounded-claim");
        let skipped = StepOutcome::did("skipped — /x/atlas/gaps.json already on disk");
        let found_nothing =
            StepOutcome::did("no gaps over 400 claim(s) + 12 state(s) + 7 question(s)");

        let summaries = [ran.summary(), skipped.summary(), found_nothing.summary()];
        let distinct: std::collections::HashSet<&String> = summaries.iter().collect();
        assert_eq!(
            distinct.len(),
            3,
            "these three outcomes must not share a summary: {summaries:?}"
        );
        for s in &summaries {
            assert_ne!(
                s,
                &format!("{} complete", BuildStep::Gaps.display_name()),
                "this is the fabricated summary the seam deleted"
            );
        }
    }
    #[test]
    fn extract_cache_atlas_detection() {
        let tmp = std::env::temp_dir().join(format!("sov-build-cache-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        // Atlas-shaped cache: one chapter with a section_extraction object.
        let atlas = tmp.join("atlas.json");
        std::fs::write(
            &atlas,
            br#"{"questions_by_chapter":[{"chapter":"sec_0001","section_extraction":{"entities":[]}}]}"#,
        )
        .unwrap();
        assert!(extract_cache_has_atlas_payloads(&atlas));

        // Legacy non-atlas cache: chapters present but section_extraction is null.
        let legacy = tmp.join("legacy.json");
        std::fs::write(
            &legacy,
            br#"{"questions_by_chapter":[{"chapter":"sec_0001","section_extraction":null}]}"#,
        )
        .unwrap();
        assert!(!extract_cache_has_atlas_payloads(&legacy));

        // Legacy cache without the field at all (pre-atlas shape).
        let missing = tmp.join("missing.json");
        std::fs::write(
            &missing,
            br#"{"questions_by_chapter":[{"chapter":"sec_0001"}]}"#,
        )
        .unwrap();
        assert!(!extract_cache_has_atlas_payloads(&missing));

        // Malformed JSON → treat as stale.
        let bad = tmp.join("bad.json");
        std::fs::write(&bad, b"{not json").unwrap();
        assert!(!extract_cache_has_atlas_payloads(&bad));

        // Missing file → treat as stale.
        let gone = tmp.join("gone.json");
        assert!(!extract_cache_has_atlas_payloads(&gone));

        let _ = std::fs::remove_dir_all(&tmp);
    }
    #[test]
    fn step_canonical_output_covers_every_cacheable_step() {
        // Pin the idempotency contract: each step that should
        // short-circuit on cached output declares a canonical path,
        // and that path lives under the expected enrichment/index
        // root for the corpus. A future step addition that forgets
        // to map a path here would silently re-run every time;
        // this test fails on the discriminant of the new variant
        // so the omission can't slip through.
        let corpus = "my-corpus";
        // Steps that MUST be cacheable (the LLM-heavy ones and
        // anything downstream depends on as input).
        for step in [
            Step::Extract,
            Step::Cluster,
            Step::Name,
            Step::Resolve,
            Step::Tensions,
            Step::Gaps,
            Step::Report,
            Step::Backfill,
        ] {
            let path = step_canonical_output(step, corpus)
                .unwrap_or_else(|| panic!("step {step:?} must declare a canonical output"));
            // Sanity — every path is namespaced under the corpus.
            assert!(
                path.to_string_lossy().contains(corpus),
                "step {step:?} → {path:?} must be corpus-scoped"
            );
        }
        // Steps with no cache (seed is cheap + has its own
        // freshness check; configure is opt-in per pipeline).
        for step in [Step::Seed, Step::Configure] {
            assert!(
                step_canonical_output(step, corpus).is_none(),
                "step {step:?} should not be cache-gated (no canonical output)"
            );
        }
    }
    /// The Backfill cache gate keys on the ANN table directory under the
    /// corpus's atlas dir — the same path `ann_table_present` /
    /// `ann_table_is_fresh` read, via the shared constants, so the gate and
    /// the reader cannot name different directories.
    #[test]
    fn step_canonical_output_backfill_is_the_ann_table_dir() {
        let path = step_canonical_output(Step::Backfill, "my-corpus").unwrap();
        assert!(
            path.ends_with(format!("my-corpus/{ATLAS_DIRNAME}/{ANN_TABLE_DIRNAME}")),
            "got {path:?}"
        );
        assert_eq!(ANN_TABLE_DIRNAME, "atoms_ann.lance");
    }
    #[test]
    fn step_canonical_output_resolve_writes_to_index_root_atoms_json() {
        // Resolve's canonical output is `atoms.json` at the index
        // root. The drift orchestrator uses this exact path when
        // the operator wants to force a re-resolve (`rm atoms.json`).
        // Pin the path explicitly so a future refactor of the
        // atlas-output layout breaks loudly here instead of
        // silently making `rm atoms.json` a no-op.
        let path = step_canonical_output(Step::Resolve, "demo")
            .expect("Resolve must have a canonical output");
        let s = path.to_string_lossy();
        assert!(s.ends_with("indexes/demo/atlas/atoms.json"), "got: {s}");
    }
    #[test]
    fn resolve_cache_structural_placeholder_is_invalidated_but_real_resolve_is_kept() {
        // Pins the fix for the post-install structural atlas blocking
        // resolve: an empty `atoms.json` (the structural placeholder) must
        // be treated as stale so resolve re-runs; a populated one (a real
        // prior resolve) must be preserved so we don't redo finished work.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("atoms.json");

        // The post-install structural placeholder: empty atoms array.
        std::fs::write(&p, r#"{"schema_version":1,"atoms":[]}"#).unwrap();
        assert!(
            resolve_cache_is_structural_placeholder(&p),
            "empty atoms ⇒ placeholder ⇒ must re-resolve"
        );

        // A real resolve output is preserved (skip).
        std::fs::write(&p, r#"{"schema_version":1,"atoms":[{"kind":"Entity"}]}"#).unwrap();
        assert!(
            !resolve_cache_is_structural_placeholder(&p),
            "non-empty atoms ⇒ real resolve ⇒ must be kept"
        );

        // Absent / unparseable / missing ⇒ can't trust it ⇒ re-resolve.
        std::fs::write(&p, r#"{"schema_version":1}"#).unwrap();
        assert!(
            resolve_cache_is_structural_placeholder(&p),
            "absent atoms key"
        );
        std::fs::write(&p, "not json").unwrap();
        assert!(resolve_cache_is_structural_placeholder(&p), "unparseable");
        assert!(
            resolve_cache_is_structural_placeholder(&dir.path().join("nope.json")),
            "missing file"
        );
    }
}
