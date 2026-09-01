// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich build` — one-shot atlas pipeline driver.
//!
//! Runs the full atlas flow against a corpus in the expected order
//! with step-by-step progress:
//!
//!   1. seed               — Stage 1a entity list
//!   2. extract            — Phase 1 per-section atlas extraction
//!      (cached on a `--full` run so later phases can consume it;
//!       a subset run is promoted to cache in-place so downstream
//!       phases have inputs)
//!   3. cluster            — Phase 2 facet-typed clustering
//!   4. name               — Phase 3 per-facet cluster naming
//!   5. resolve            — Phase 3a/3b atoms + edges + trajectories
//!   6. tensions           — Phase 6 deterministic candidate selection
//!   7. gaps               — Phase 7 deterministic gap detection
//!   8. configure          — Phase 8 (LLM, opt-in per pipeline)
//!   9. report             — §12 schema validation table
//!  10. backfill           — embed the resolved atoms into the ANN seed
//!      table (`atlas/atoms_ann.lance`) so the corpus grounds at once
//!
//! Each step invokes the same underlying `cmd_*` function used by
//! the standalone CLI verbs, so orchestrated behaviour matches a
//! manual sequence exactly. A step's failure stops the flow and
//! returns its exit code.

use super::{
    atlas_configuration, atlas_gaps, atlas_phase_cmd, atlas_resolve, atlas_tensions,
    atlas_tensions_classify, config::EnrichConfig, extract, paths, schema_review, seed_cmd,
};
use crate::chat_cmd::bootstrap::{build_session, ChatSession};
use crate::chat_cmd::config::parse_globals;
use corpus_engine::enrichment::atlas::ann_store::{ann_table_is_fresh, ANN_TABLE_DIRNAME};
use corpus_engine::enrichment::atlas::ATLAS_DIRNAME;
use corpus_engine::enrichment::pipeline::{
    progress::wire, BuildStep, EnrichProgress, EnrichProgressFn, PipelineRegistry, SeedStrategy,
};
use sovereign_cli_shared::help::{self, Help, HelpSection};
use sovereign_core::traits::InferenceProvider;
use sovereign_tools::atlas_context_manager::{backfill_ann, AtlasContextFilter, BackfillOutcome};
use std::sync::Arc;

const HELP: Help = Help {
    command: "svrn enrich build",
    summary: "Run the full atlas enrichment flow for a corpus in one command.",
    sections: &[
        HelpSection::Usage(
            "svrn enrich build <corpus-id> [--chapters <ids> | --full] [--skip <step>...] [--dry-run]",
        ),
        HelpSection::Flags(&[
            (
                "--chapters <ids>",
                "Comma-separated chapter ids for Phase 1 (e.g. sec_0001,sec_0002). \
                 Subset runs promote the run output into cache so downstream steps \
                 have inputs. Default: --full.",
            ),
            (
                "--full",
                "Run Phase 1 on every section in the corpus manifest. Updates \
                 cache/questions.json directly.",
            ),
            (
                "--skip <step>",
                "Skip a step by name. Accepts: seed, extract, cluster, name, resolve, \
                 tensions, gaps, configure, report, backfill. Repeatable.",
            ),
            (
                "--dry-run",
                "Print the planned step sequence and exit without running anything.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "svrn enrich build brothers_karamazov --full",
                "Full end-to-end build on the whole corpus.",
            ),
            (
                "svrn enrich build process_philosophy --chapters sec_0001,sec_0002,sec_0003",
                "Subset build — useful for iterating on a tiny validation slice.",
            ),
            (
                "svrn enrich build bk --skip configure",
                "Skip the LLM Phase 8 configuration step (fastest path to resolved atlas + report).",
            ),
        ]),
        HelpSection::Notes(
            "Requires `svrn enrich init <corpus>` first. Phase 8 (configure) is \
             skipped automatically if the pipeline hasn't opted in via \
             `runs_configuration_phase()`. Any step failure stops the flow with that \
             step's exit code. The last step, `backfill`, embeds the resolved atoms \
             into `atlas/atoms_ann.lance` through the daemon's embed model so the \
             corpus grounds immediately; it is probed before the first step runs, \
             and `--skip backfill` builds without grounding (`svrn atlas backfill-ann \
             <corpus>` seeds it later).",
        ),
    ],
};

pub async fn cmd_build(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }

    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };

    // Two renderings of one event stream, and the parent picks.
    //
    // A human gets banners. A parent process that set
    // `SOVEREIGN_ENRICH_PROGRESS=json` gets one `@progress {…}` line per
    // event on stdout — the wire `corpus_engine::…::progress::wire` declares,
    // which is what `sovereign_tools::enrich` reads. Before 2026-08-26 there
    // was no second rendering and that runner regex-matched THESE banners, so
    // rewording one silently changed what the desktop believed about a running
    // enrichment (TOPOLOGY §9.3).
    //
    // The env var is read here, at the ONE place that decides how to render.
    let machine = std::env::var(wire::REQUEST_ENV)
        .map(|v| v == wire::REQUEST_VALUE)
        .unwrap_or(false);
    let progress: EnrichProgressFn = if machine {
        Arc::new(|evt: EnrichProgress| println!("{}", wire::encode(&evt)))
    } else {
        Arc::new(|evt: EnrichProgress| print_cli_event(&evt))
    };
    build_with_progress(&parsed, Some(progress)).await
}

/// Library entry point: run the full `enrich build` flow with an
/// optional streaming progress callback.
///
/// The callback receives typed `EnrichProgress` events in strict
/// order (`BuildStart` → step events → `Complete` or `Aborted`).
/// `None` runs silently — useful for integration tests that only
/// care about the exit code.
///
/// Returns the same exit code `cmd_build` would: 0 on success,
/// nonzero when any enabled step fails.
///
/// Shared by the CLI (`cmd_build`) and the desktop Tauri layer
/// (`sovereign-desktop/src-tauri/src/enrich_commands.rs`). Adding a
/// per-step side effect means editing here once rather than across
/// frontends.
pub async fn build_with_progress(parsed: &ParsedBuild, progress: Option<EnrichProgressFn>) -> i32 {
    let emit = |evt: EnrichProgress| {
        if let Some(cb) = progress.as_ref() {
            cb(evt);
        }
    };

    // Load pipeline capabilities once. Failure surfaces before any
    // progress event so an invalid corpus_id doesn't emit a
    // spurious BuildStart.
    let capabilities = match load_pipeline_capabilities(&parsed.corpus_id) {
        Ok(c) => c,
        Err((code, msg)) => {
            eprintln!("error: {msg}");
            return code;
        }
    };

    let plan = Plan::new(parsed, &capabilities);
    if parsed.dry_run {
        plan.print_dry_run();
        return 0;
    }

    // Fail fast for the LAST step. Backfill needs the daemon's embed slot;
    // proving it answers costs one embed call here, and discovering it does
    // not after thirty minutes of extraction would waste the run. The same
    // session is the step's provider, so the daemon is resolved once.
    let backfill_session = if plan.enabled.contains(&Step::Backfill) {
        match probe_backfill_session().await {
            Ok(s) => Some(s),
            Err(msg) => {
                eprintln!("error: {msg}");
                return 1;
            }
        }
    } else {
        None
    };
    let embedder: Option<&Arc<dyn InferenceProvider>> =
        backfill_session.as_ref().map(|s| &s.inference);

    emit(EnrichProgress::BuildStart {
        corpus_id: parsed.corpus_id.clone(),
        pipeline_id: capabilities.pipeline_id.clone(),
        steps: plan.enabled_steps().map(Step::to_build_step).collect(),
        auto_skipped: plan
            .auto_skipped
            .iter()
            .map(|s| s.to_build_step())
            .collect(),
    });

    let total = plan.enabled_steps().count();
    for (i, step) in plan.enabled_steps().enumerate() {
        let ordinal = i + 1;
        let build_step = step.to_build_step();
        emit(EnrichProgress::StepStart {
            corpus_id: parsed.corpus_id.clone(),
            step: build_step,
            ordinal,
            total,
        });
        let outcome = match run_step(step, parsed, embedder).await {
            Ok(o) => o,
            Err(failure) => {
                let code = failure.exit_code;
                eprintln!();
                eprintln!("error: {}. Build stopped.", failure.message);
                emit(EnrichProgress::StepFailed {
                    corpus_id: parsed.corpus_id.clone(),
                    step: build_step,
                    message: failure.message,
                    exit_code: code,
                });
                emit(EnrichProgress::Aborted {
                    corpus_id: parsed.corpus_id.clone(),
                    failed_step: build_step,
                    exit_code: code,
                });
                return code;
            }
        };
        emit(EnrichProgress::StepDone {
            corpus_id: parsed.corpus_id.clone(),
            step: build_step,
            // Supplied by the step, never by this loop. A step that
            // ran, a step that was skipped because its output was
            // cached, and a step that found nothing are three
            // different outcomes; until 2026-08-26 all three put the
            // same fabricated `"<step> complete"` on the wire.
            summary: outcome.summary(),
        });
    }

    emit(EnrichProgress::Complete {
        corpus_id: parsed.corpus_id.clone(),
        steps_completed: total,
    });
    0
}

/// Render a single progress event on the CLI (stdout) in the same
/// banner shape operators have seen since Landing 3. Desktop
/// callers don't use this — they emit the structured event
/// straight through.
fn print_cli_event(evt: &EnrichProgress) {
    match evt {
        EnrichProgress::BuildStart {
            corpus_id,
            pipeline_id,
            steps,
            auto_skipped,
        } => {
            println!("=== enrich build — {corpus_id} ===");
            if !auto_skipped.is_empty() {
                let labels: Vec<&str> = auto_skipped.iter().map(|s| s.id()).collect();
                println!(
                    "  pipeline `{pipeline_id}` auto-skips: {}",
                    labels.join(", ")
                );
            }
            println!("  {} step(s) planned", steps.len());
            for (i, s) in steps.iter().enumerate() {
                println!("    {}. {}", i + 1, s.id());
            }
            println!();
        }
        EnrichProgress::StepStart {
            step,
            ordinal,
            total,
            ..
        } => {
            println!("─── [{ordinal}/{total}] {} ───", step.id());
        }
        EnrichProgress::StepDone { .. } => {
            println!();
        }
        EnrichProgress::ChapterProgress { .. } | EnrichProgress::ChapterFailed { .. } => {
            // The extract step prints its own per-chapter lines;
            // avoid double-printing. These events exist for
            // non-CLI consumers.
        }
        EnrichProgress::StepFailed { .. } | EnrichProgress::Aborted { .. } => {
            // The caller already prints a detailed error to
            // stderr via eprintln! above; nothing to add here.
        }
        EnrichProgress::SpawnFailed { corpus_id, message } => {
            // This variant is emitted only by the desktop's
            // subprocess-spawn path — the CLI's own in-process
            // orchestration can't hit it. Print a line anyway so
            // this branch is exhaustive and future in-process
            // spawn scenarios (e.g. a sub-step that shells out)
            // would surface correctly.
            eprintln!("error: could not start build for {corpus_id}: {message}");
        }
        EnrichProgress::Cancelled { corpus_id, at_step } => {
            // Same reason as `SpawnFailed` above: the CLI can't
            // emit this today (no cancellation channel in the
            // in-process path), but the match is exhaustive so a
            // future CLI flag like `--cancel-after-step <N>`
            // wouldn't require a parser update.
            let step_label = at_step.map(|s| s.id()).unwrap_or("none");
            eprintln!("cancelled build for {corpus_id} (was at step: {step_label})");
        }
        EnrichProgress::Complete { corpus_id, .. } => {
            println!("=== build complete — {corpus_id} ===");
        }
    }
}

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
struct StepOutcome(String);

impl StepOutcome {
    fn did(summary: impl Into<String>) -> Self {
        Self(summary.into())
    }

    /// The line for `StepDone.summary`, verbatim.
    fn summary(&self) -> String {
        self.0.clone()
    }
}

/// Why a step stopped.
#[derive(Debug, Clone)]
struct StepFailure {
    /// What went wrong, in the step's own words.
    message: String,
    /// The code `enrich build` exits with.
    exit_code: i32,
}

impl StepFailure {
    fn new(message: impl Into<String>, exit_code: i32) -> Self {
        Self {
            message: message.into(),
            exit_code,
        }
    }
}

async fn run_step(
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

/// Build the daemon-backed session the Backfill step embeds through, and
/// prove its embed slot answers (`embed_query("probe")`). `parse_globals(&[])`
/// resolves the daemon exactly as `svrn atlas backfill-ann` does — the same
/// bootstrap, not a second one (ARCH §19).
async fn probe_backfill_session() -> Result<ChatSession, String> {
    let (globals, _) = parse_globals(&[])?;
    let session = build_session(&globals).await.map_err(|e| {
        format!(
            "backfill: could not reach the daemon ({e}); start it, or pass \
             `--skip backfill` to build without grounding"
        )
    })?;
    session.inference.embed_query("probe").await.map_err(|e| {
        format!(
            "backfill: the daemon's embed slot did not answer ({e}); load an embed \
             model, or pass `--skip backfill` to build without grounding"
        )
    })?;
    Ok(session)
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

/// Run the Extract step with an auto-retry pass on transient
/// failure kinds (ThinkTruncated + ParseDrift). The pattern:
///
/// 1. Run `cmd_extract --full` (or `--chapters …`). If clean,
/// done.
/// 2. If non-zero AND the run file shows ONLY retry-eligible
/// failures, invoke `--retry-failed --terse` once. A
/// different model seed + bumped output budget recovers
/// ~80-90% of drift cases (measured on the Dopesick Jesus
/// smoke before structured retries were wired).
/// 3. After retry, re-check the final failure set. If it's
/// empty or has only non-retriable kinds (ChatError,
/// Skipped), promote whatever we have and return 0; the
/// caller can still `enrich errors <corpus>` to surface
/// what's left.
/// 4. If retry-eligible failures remain, return 1 — the
/// operator gets the same glassbox output as before.
///
/// An operator can opt out of the auto-retry by setting
/// `SOVEREIGN_ENRICH_AUTO_RETRY=0`. Defaults to on. The
/// env-var (rather than a CLI flag) keeps the common case —
/// CI invocations + the desktop UI — from having to thread a
/// boolean through every orchestrator.
async fn run_extract_step(parsed: &ParsedBuild) -> Result<StepOutcome, StepFailure> {
    use corpus_engine::enrichment::pipeline::PhaseFailureKind;

    let corpus = parsed.corpus_id.as_str();

    // First pass — the standard extract run shaped by the
    // orchestrator's selection.
    let mut first_args: Vec<String> = vec![corpus.into()];
    match &parsed.selection {
        Selection::Full => first_args.push("--full".into()),
        Selection::Chapters(ids) => {
            first_args.push("--chapters".into());
            first_args.push(ids.join(","));
        }
    }
    let first_code = extract::cmd_extract(&first_args).await;

    // Whatever the first pass did, we still need to promote a
    // subset run's output to cache so downstream phases have
    // input. Do this now so the retry (if any) sees the same
    // state the promote would produce.
    if matches!(&parsed.selection, Selection::Chapters(_)) && first_code == 0 {
        if let Err(e) = promote_subset_to_cache(corpus) {
            return Err(StepFailure::new(
                format!("promoting subset run to cache: {e}"),
                1,
            ));
        }
        println!("  · promoted subset run → cache/questions.json");
    }

    if first_code == 0 {
        // `cmd_extract` is still on the `-> i32` contract, so the count
        // is not in its return value — but it IS in the run file it just
        // wrote. Reading it beats inventing one, and beats a summary
        // that says nothing.
        let summary = match extract::read_latest_run(&paths::runs_dir(corpus)) {
            Ok(Some(run)) => format!("{} chapter(s) extracted", run.extracted),
            Ok(None) => "extract reported success but wrote no run file".to_string(),
            Err(e) => format!("extract succeeded; its run file was unreadable ({e})"),
        };
        return Ok(StepOutcome::did(summary));
    }

    // The first pass failed. Decide whether to auto-retry.
    let auto_retry_enabled = std::env::var("SOVEREIGN_ENRICH_AUTO_RETRY")
        .map(|v| v != "0" && v.to_lowercase() != "false")
        .unwrap_or(true);
    if !auto_retry_enabled {
        return Err(StepFailure::new(
            format!(
                "extract failed (exit {first_code}); auto-retry is off (SOVEREIGN_ENRICH_AUTO_RETRY)"
            ),
            first_code,
        ));
    }

    let runs_dir = paths::runs_dir(corpus);
    let failures = match extract::read_latest_run(&runs_dir) {
        Ok(Some(run)) => run.failures,
        Ok(None) => {
            // No run file means extract never produced one —
            // probably a config error. Nothing to retry.
            return Err(StepFailure::new(
                format!(
                    "extract failed (exit {first_code}) and wrote no run file — nothing to auto-retry, which usually means a config error"
                ),
                first_code,
            ));
        }
        Err(msg) => {
            return Err(StepFailure::new(
                format!(
                    "extract failed (exit {first_code}); auto-retry skipped — reading latest run file: {msg}"
                ),
                first_code,
            ));
        }
    };

    let retriable_count = failures
        .iter()
        .filter(|(_, kind)| {
            matches!(
                kind,
                PhaseFailureKind::ThinkTruncated
                    | PhaseFailureKind::ParseDrift
                    | PhaseFailureKind::DeadlineExceeded
            )
        })
        .count();

    if retriable_count == 0 {
        // Nothing the terse retry can help with (chat errors,
        // empty extractions, etc.). Let the operator handle.
        return Err(StepFailure::new(
            format!(
                "extract failed (exit {first_code}) with {} failure(s), none of them retriable — see `svrn enrich errors {corpus}`",
                failures.len()
            ),
            first_code,
        ));
    }

    println!();
    println!(
        "  · auto-retry: {} retriable failure(s) (parse_drift / think_truncated / deadline_exceeded) — \
         running `--retry-failed --terse` once",
        retriable_count
    );
    println!();

    let retry_args: Vec<String> = vec![corpus.into(), "--retry-failed".into(), "--terse".into()];
    let retry_code = extract::cmd_extract(&retry_args).await;

    // After the retry, re-read the latest run file to decide
    // whether the orchestration can continue. If only
    // unrecoverable kinds remain (ChatError, Skipped,
    // EmptyExtraction), surface them to the operator but allow
    // the orchestrator to continue — those chapters just won't
    // be in the cache for downstream phases. If retriable kinds
    // are still present, treat the retry as having not resolved
    // the issue and return the original failure exit.
    let remaining = match extract::read_latest_run(&runs_dir) {
        Ok(Some(run)) => run.failures,
        Ok(None) => Vec::new(),
        Err(_) => Vec::new(),
    };
    let remaining_retriable = remaining
        .iter()
        .filter(|(_, kind)| {
            matches!(
                kind,
                PhaseFailureKind::ThinkTruncated
                    | PhaseFailureKind::ParseDrift
                    | PhaseFailureKind::DeadlineExceeded
            )
        })
        .count();

    if remaining_retriable == 0 {
        println!();
        println!(
            "  ✓ auto-retry resolved {} chapter(s). {} non-retriable failure(s) remain; \
             continuing build.",
            retriable_count - remaining_retriable,
            remaining.len()
        );
        // Promote the (now-complete) subset extract to cache so the
        // downstream phases have input. The first-pass promote above is
        // gated on `first_code == 0`; when auto-retry ran, that gate was
        // false, so the full subset never landed — and the terse-retry
        // path only merges the *retried* chapter's output, not the whole
        // subset. Promote the full subset explicitly here. (Observed
        // 2026-06-01: a single parse_drift triggered auto-retry, the
        // first-pass promote was skipped, and cluster then failed
        // "questions cache is missing".)
        if matches!(&parsed.selection, Selection::Chapters(_)) {
            if let Err(e) = promote_subset_to_cache(corpus) {
                return Err(StepFailure::new(
                    format!("promoting subset run to cache after retry: {e}"),
                    1,
                ));
            }
            println!("  · promoted subset run → cache/questions.json");
        }
        return Ok(StepOutcome::did(format!(
            "auto-retry recovered {} chapter(s); {} non-retriable failure(s) remain",
            retriable_count - remaining_retriable,
            remaining.len()
        )));
    }

    // Retry didn't recover everything. Surface the original
    // failure exit and let the operator intervene via
    // `enrich errors <corpus>`.
    Err(StepFailure::new(
        format!(
            "auto-retry left {remaining_retriable} retriable failure(s) unresolved — see `svrn enrich errors {corpus}`"
        ),
        retry_code,
    ))
}

/// Copy the most recent subset run into cache/questions.json so
/// cluster/name/resolve can proceed against a consistent input.
/// Mirrors what operators do by hand today.
fn promote_subset_to_cache(corpus_id: &str) -> std::io::Result<()> {
    let runs_dir = paths::runs_dir(corpus_id);
    let cache_dir = paths::cache_dir(corpus_id);
    let latest = find_latest_run(&runs_dir)?;
    let cache_path = cache_dir.join("questions.json");
    std::fs::create_dir_all(&cache_dir)?;
    std::fs::copy(&latest, &cache_path)?;
    Ok(())
}

fn find_latest_run(runs_dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(runs_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|s| s.starts_with("questions-") && s.ends_with(".json"))
                .unwrap_or(false)
        })
        .collect();
    if entries.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "no questions-*.json run files in {} — did extract succeed?",
                runs_dir.display()
            ),
        ));
    }
    entries.sort_by_key(|e| e.file_name());
    Ok(entries.last().unwrap().path())
}

// ── Plan + step enum ─────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Seed,
    Extract,
    Cluster,
    Name,
    Resolve,
    Tensions,
    Gaps,
    Configure,
    Report,
    /// Last, always: it reads the atlas every step before it wrote.
    Backfill,
}

impl Step {
    /// Cross-crate representation of this step for the progress
    /// event stream (`corpus_engine::enrichment::pipeline::BuildStep`).
    /// Keep in lockstep with `Step::label` — the canonical id string
    /// comes from `BuildStep::id` to avoid two sources of truth.
    pub(super) fn to_build_step(self) -> BuildStep {
        match self {
            Step::Seed => BuildStep::Seed,
            Step::Extract => BuildStep::Extract,
            Step::Cluster => BuildStep::Cluster,
            Step::Name => BuildStep::Name,
            Step::Resolve => BuildStep::Resolve,
            Step::Tensions => BuildStep::Tensions,
            Step::Gaps => BuildStep::Gaps,
            Step::Configure => BuildStep::Configure,
            Step::Report => BuildStep::Report,
            Step::Backfill => BuildStep::Backfill,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Step::Seed => "seed",
            Step::Extract => "extract",
            Step::Cluster => "cluster",
            Step::Name => "name",
            Step::Resolve => "resolve",
            Step::Tensions => "tensions",
            Step::Gaps => "gaps",
            Step::Configure => "configure",
            Step::Report => "report",
            Step::Backfill => "backfill",
        }
    }

    /// The `--skip` vocabulary, rendered from `all()` so the parser error and
    /// the desktop constructor cannot list different steps than the plan runs.
    /// (`HELP` is a `const` and repeats the list as a literal; a test below
    /// holds the two together.)
    fn valid_labels() -> String {
        Self::all()
            .iter()
            .map(|s| s.label())
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn from_label(s: &str) -> Option<Step> {
        match s {
            "seed" => Some(Step::Seed),
            "extract" => Some(Step::Extract),
            "cluster" => Some(Step::Cluster),
            "name" => Some(Step::Name),
            "resolve" => Some(Step::Resolve),
            "tensions" => Some(Step::Tensions),
            "gaps" => Some(Step::Gaps),
            "configure" => Some(Step::Configure),
            "report" => Some(Step::Report),
            "backfill" => Some(Step::Backfill),
            _ => None,
        }
    }

    fn all() -> &'static [Step] {
        &[
            Step::Seed,
            Step::Extract,
            Step::Cluster,
            Step::Name,
            Step::Resolve,
            Step::Tensions,
            Step::Gaps,
            Step::Configure,
            Step::Report,
            Step::Backfill,
        ]
    }
}

/// Pipeline-level capabilities the orchestrator needs to shape
/// the plan. Loaded once at build start from the corpus's
/// enrichment config + the pipeline registry.
pub(super) struct PipelineCapabilities {
    pub pipeline_id: String,
    pub seed_strategy_none: bool,
    pub runs_configuration_phase: bool,
}

fn load_pipeline_capabilities(corpus_id: &str) -> Result<PipelineCapabilities, (i32, String)> {
    let cfg = EnrichConfig::require(corpus_id).map_err(|e| {
        (
            1,
            format!("loading enrichment config for `{corpus_id}`: {e}"),
        )
    })?;
    let registry = PipelineRegistry::builtin();
    let pipeline = super::pipeline_resolve::resolve_pipeline(&cfg).ok_or_else(|| {
        (
            1,
            format!(
                "unknown pipeline `{}` in this corpus's config (known: {:?})",
                cfg.pipeline_id,
                registry.pipeline_ids()
            ),
        )
    })?;
    // The atlas flow presumes an atlas-shaped pipeline — Phases
    // 2-8 require `section_extraction` payloads that the legacy
    // `literary` pipeline doesn't emit. Fail loudly at start
    // with an actionable remediation rather than crashing
    // mid-flow.
    if !cfg.pipeline_id.ends_with("_atlas") {
        return Err((
            2,
            format!(
                "pipeline `{}` is a legacy (non-atlas) pipeline; `build` only supports \
                 atlas pipelines. Re-init with `svrn enrich reset {corpus_id} --full \
                 --yes` followed by `svrn enrich init {corpus_id} --source <path> \
                 --pipeline literary_atlas` (or `--pipeline philosophy_atlas`), then \
                 retry.",
                cfg.pipeline_id
            ),
        ));
    }
    Ok(PipelineCapabilities {
        pipeline_id: cfg.pipeline_id.clone(),
        seed_strategy_none: matches!(pipeline.seed_strategy(), SeedStrategy::None),
        runs_configuration_phase: pipeline.runs_configuration_phase(),
    })
}

struct Plan {
    enabled: Vec<Step>,
    /// Steps dropped because the pipeline explicitly opts out —
    /// e.g. a seed-less atlas variant, or a pipeline that doesn't
    /// run Phase 8. Surfaced in the banner so an operator sees
    /// the pipeline-driven subset without thinking the
    /// orchestrator silently lost steps.
    auto_skipped: Vec<Step>,
}

impl Plan {
    fn new(parsed: &ParsedBuild, caps: &PipelineCapabilities) -> Self {
        let mut auto_skipped: Vec<Step> = Vec::new();
        if caps.seed_strategy_none {
            auto_skipped.push(Step::Seed);
        }
        if !caps.runs_configuration_phase {
            auto_skipped.push(Step::Configure);
        }
        let enabled = Step::all()
            .iter()
            .copied()
            .filter(|s| !parsed.skipped.contains(s) && !auto_skipped.contains(s))
            .collect();
        Self {
            enabled,
            auto_skipped,
        }
    }

    fn enabled_steps(&self) -> impl Iterator<Item = Step> + '_ {
        self.enabled.iter().copied()
    }

    fn print_dry_run(&self) {
        if !self.auto_skipped.is_empty() {
            println!(
                "  auto-skipped (pipeline opts out): {}",
                self.auto_skipped
                    .iter()
                    .map(|s| s.label())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        println!("  planned steps ({}):", self.enabled.len());
        for (i, s) in self.enabled.iter().enumerate() {
            println!("    {}. {}", i + 1, s.label());
        }
    }
}

// ── Arg parsing ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Selection {
    Full,
    Chapters(Vec<String>),
}

/// Parsed `enrich build` invocation. Exposed publicly so external
/// callers (desktop app) can construct one without going through
/// argv parsing.
#[derive(Debug, Clone)]
pub struct ParsedBuild {
    pub corpus_id: String,
    pub selection: Selection,
    /// Step labels the caller explicitly asked to skip (via
    /// `--skip <label>` on the CLI, or by inserting values
    /// manually in the desktop path). Pipeline-capability auto
    /// skips land separately in `Plan::auto_skipped`.
    pub(super) skipped: Vec<Step>,
    pub dry_run: bool,
}

impl ParsedBuild {
    /// Construct a `ParsedBuild` without going through argv.
    /// Intended for the desktop Tauri layer, which receives typed
    /// inputs (a corpus id + an optional chapter list + a set of
    /// step-ids to skip).
    ///
    /// `skip_step_ids` accepts the step-id strings exposed by
    /// `BuildStep::id` (`seed`, `extract`, …). Unknown ids are
    /// rejected — silent ignore would be a footgun when a typo
    /// lets Phase 8 run on a corpus the operator meant to exclude.
    #[allow(dead_code)] // Used by the desktop enrich_commands layer once it lands.
    pub fn from_inputs(
        corpus_id: impl Into<String>,
        chapters: Option<Vec<String>>,
        skip_step_ids: &[String],
        dry_run: bool,
    ) -> Result<Self, String> {
        let selection = match chapters {
            Some(ids) if ids.is_empty() => {
                return Err("chapter list is empty".into());
            }
            Some(ids) => Selection::Chapters(ids),
            None => Selection::Full,
        };
        let mut skipped: Vec<Step> = Vec::new();
        for id in skip_step_ids {
            let step = Step::from_label(id).ok_or_else(|| {
                format!("unknown skip step `{id}` (valid: {})", Step::valid_labels())
            })?;
            if !skipped.contains(&step) {
                skipped.push(step);
            }
        }
        Ok(Self {
            corpus_id: corpus_id.into(),
            selection,
            skipped,
            dry_run,
        })
    }
}

fn parse_args(args: &[String]) -> Result<ParsedBuild, String> {
    let mut corpus_id: Option<String> = None;
    let mut chapters: Option<Vec<String>> = None;
    let mut full = false;
    let mut skipped: Vec<Step> = Vec::new();
    let mut dry_run = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--full" => {
                full = true;
                i += 1;
            }
            "--chapters" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--chapters requires a comma-separated id list".to_string())?;
                chapters = Some(
                    raw.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                );
                i += 2;
            }
            "--skip" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--skip requires a step name".to_string())?;
                let step = Step::from_label(raw).ok_or_else(|| {
                    format!(
                        "unknown step `{raw}` for --skip (valid: {})",
                        Step::valid_labels()
                    )
                })?;
                if !skipped.contains(&step) {
                    skipped.push(step);
                }
                i += 2;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_some() {
                    return Err(format!("unexpected positional argument: {other}"));
                }
                corpus_id = Some(other.to_string());
                i += 1;
            }
        }
    }
    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    let selection = match (full, chapters) {
        (true, Some(_)) => {
            return Err("use either --full or --chapters, not both".to_string());
        }
        (true, None) => Selection::Full,
        (false, Some(ids)) if ids.is_empty() => {
            return Err("--chapters list is empty".to_string());
        }
        (false, Some(ids)) => Selection::Chapters(ids),
        (false, None) => Selection::Full, // default
    };
    Ok(ParsedBuild {
        corpus_id,
        selection,
        skipped,
        dry_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn parse_defaults_to_full_selection() {
        let p = parse_args(&["bk".into()]).unwrap();
        assert!(matches!(p.selection, Selection::Full));
        assert!(!p.dry_run);
        assert!(p.skipped.is_empty());
    }

    #[test]
    fn parse_accepts_chapter_subset() {
        let p =
            parse_args(&["bk".into(), "--chapters".into(), "sec_0001,sec_0002".into()]).unwrap();
        match p.selection {
            Selection::Chapters(ids) => {
                assert_eq!(ids, vec!["sec_0001", "sec_0002"]);
            }
            _ => panic!("expected Chapters selection"),
        }
    }

    #[test]
    fn parse_rejects_both_full_and_chapters() {
        let err = parse_args(&[
            "bk".into(),
            "--full".into(),
            "--chapters".into(),
            "sec_0001".into(),
        ])
        .unwrap_err();
        assert!(err.contains("either --full or --chapters"));
    }

    #[test]
    fn parse_accepts_repeated_skip_flag() {
        let p = parse_args(&[
            "bk".into(),
            "--skip".into(),
            "configure".into(),
            "--skip".into(),
            "tensions".into(),
        ])
        .unwrap();
        assert!(p.skipped.contains(&Step::Configure));
        assert!(p.skipped.contains(&Step::Tensions));
    }

    #[test]
    fn parse_rejects_unknown_skip_name() {
        let err = parse_args(&["bk".into(), "--skip".into(), "banana".into()]).unwrap_err();
        assert!(err.contains("unknown step"));
    }

    #[test]
    fn parse_dry_run_flag() {
        let p = parse_args(&["bk".into(), "--dry-run".into()]).unwrap();
        assert!(p.dry_run);
    }

    #[test]
    fn parse_rejects_missing_corpus_id() {
        let err = parse_args(&[]).unwrap_err();
        assert!(err.contains("corpus-id"));
    }

    #[test]
    fn parse_rejects_unknown_flag() {
        let err = parse_args(&["bk".into(), "--nope".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
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

    /// `--skip <label>` must accept exactly the labels the plan runs, in
    /// both directions, for every step — a variant added to `all()` without
    /// a `from_label` arm would be un-skippable and this catches it.
    #[test]
    fn from_label_round_trips_every_step() {
        for step in Step::all() {
            assert_eq!(
                Step::from_label(step.label()),
                Some(*step),
                "label `{}` must parse back to {step:?}",
                step.label()
            );
        }
        assert_eq!(Step::from_label("backfill"), Some(Step::Backfill));
    }

    /// Backfill reads the atlas every other step wrote, so it is last in
    /// `all()` and last in every plan — including one whose pipeline
    /// auto-skips Seed and Configure — and `--skip backfill` removes it.
    #[test]
    fn backfill_is_the_last_step_in_every_plan() {
        assert_eq!(Step::all().last(), Some(&Step::Backfill));
        let caps = PipelineCapabilities {
            pipeline_id: "custom_atlas".into(),
            seed_strategy_none: true,
            runs_configuration_phase: false,
        };
        let parsed = ParsedBuild::from_inputs("bk", None, &[], false).unwrap();
        let plan = Plan::new(&parsed, &caps);
        assert_eq!(plan.enabled.last(), Some(&Step::Backfill));
        assert_eq!(plan.enabled.len(), Step::all().len() - 2);

        let skipped = ParsedBuild::from_inputs("bk", None, &["backfill".into()], false).unwrap();
        let plan = Plan::new(&skipped, &caps);
        assert!(!plan.enabled.contains(&Step::Backfill));
        assert_eq!(plan.enabled.last(), Some(&Step::Report));
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

    /// `HELP` is a `const`, so it repeats the `--skip` vocabulary as a
    /// literal; this holds that literal to `Step::all()`.
    #[test]
    fn help_names_every_skippable_step() {
        let flags = HELP
            .sections
            .iter()
            .find_map(|s| match s {
                HelpSection::Flags(entries) => Some(entries),
                _ => None,
            })
            .expect("HELP has a Flags section");
        let (_, skip_text) = flags
            .iter()
            .find(|(flag, _)| flag.starts_with("--skip"))
            .expect("HELP documents --skip");
        for step in Step::all() {
            assert!(
                skip_text.contains(step.label()),
                "HELP --skip text omits `{}`: {skip_text}",
                step.label()
            );
        }
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

    #[test]
    fn plan_respects_skip_filter() {
        let parsed = parse_args(&[
            "bk".into(),
            "--skip".into(),
            "configure".into(),
            "--skip".into(),
            "tensions".into(),
        ])
        .unwrap();
        let plan = Plan::new(&parsed, &full_capabilities());
        let labels: Vec<&str> = plan.enabled_steps().map(|s| s.label()).collect();
        assert!(!labels.contains(&"configure"));
        assert!(!labels.contains(&"tensions"));
        // And the ones that survive retain their canonical order.
        assert_eq!(labels[0], "seed");
        assert_eq!(labels[1], "extract");
    }

    #[test]
    fn plan_default_contains_every_step() {
        let parsed = parse_args(&["bk".into()]).unwrap();
        let plan = Plan::new(&parsed, &full_capabilities());
        assert_eq!(plan.enabled_steps().count(), Step::all().len());
    }

    #[test]
    fn plan_auto_skips_seed_when_pipeline_declares_none_strategy() {
        let parsed = parse_args(&["bk".into()]).unwrap();
        let caps = PipelineCapabilities {
            pipeline_id: "atlas_structural".into(),
            seed_strategy_none: true,
            runs_configuration_phase: true,
        };
        let plan = Plan::new(&parsed, &caps);
        let labels: Vec<&str> = plan.enabled_steps().map(|s| s.label()).collect();
        assert!(
            !labels.contains(&"seed"),
            "seed should be auto-skipped when seed_strategy is None"
        );
        assert!(plan.auto_skipped.contains(&Step::Seed));
    }

    #[test]
    fn plan_auto_skips_configure_when_pipeline_opts_out() {
        let parsed = parse_args(&["bk".into()]).unwrap();
        let caps = PipelineCapabilities {
            pipeline_id: "minimal_atlas".into(),
            seed_strategy_none: false,
            runs_configuration_phase: false,
        };
        let plan = Plan::new(&parsed, &caps);
        let labels: Vec<&str> = plan.enabled_steps().map(|s| s.label()).collect();
        assert!(
            !labels.contains(&"configure"),
            "configure should be auto-skipped when runs_configuration_phase is false"
        );
        assert!(plan.auto_skipped.contains(&Step::Configure));
    }

    #[test]
    fn plan_auto_skip_and_manual_skip_both_apply() {
        // Explicit --skip + capability-auto-skip compose —
        // neither hides the other, both land in their
        // respective categories.
        let parsed = parse_args(&["bk".into(), "--skip".into(), "tensions".into()]).unwrap();
        let caps = PipelineCapabilities {
            pipeline_id: "minimal_atlas".into(),
            seed_strategy_none: true,
            runs_configuration_phase: false,
        };
        let plan = Plan::new(&parsed, &caps);
        let labels: Vec<&str> = plan.enabled_steps().map(|s| s.label()).collect();
        assert!(!labels.contains(&"seed"));
        assert!(!labels.contains(&"tensions"));
        assert!(!labels.contains(&"configure"));
        assert!(plan.auto_skipped.contains(&Step::Seed));
        assert!(plan.auto_skipped.contains(&Step::Configure));
        // --skip tensions is NOT a capability-driven skip, so it
        // doesn't show up in auto_skipped.
        assert!(!plan.auto_skipped.contains(&Step::Tensions));
    }

    #[test]
    fn from_inputs_matches_parse_args_for_equivalent_cli() {
        // Desktop builds ParsedBuild via `from_inputs`; CLI
        // via `parse_args`. Equivalent inputs must produce
        // identical ParsedBuild shapes — otherwise the progress
        // stream diverges between the two frontends.
        let cli = parse_args(&[
            "bk".into(),
            "--chapters".into(),
            "sec_0001,sec_0002".into(),
            "--skip".into(),
            "configure".into(),
        ])
        .unwrap();
        let desktop = ParsedBuild::from_inputs(
            "bk",
            Some(vec!["sec_0001".into(), "sec_0002".into()]),
            &["configure".into()],
            false,
        )
        .unwrap();
        assert_eq!(cli.corpus_id, desktop.corpus_id);
        assert_eq!(cli.dry_run, desktop.dry_run);
        assert_eq!(cli.skipped, desktop.skipped);
        match (cli.selection, desktop.selection) {
            (Selection::Chapters(a), Selection::Chapters(b)) => assert_eq!(a, b),
            other => panic!("expected matching chapter selections, got {other:?}"),
        }
    }

    #[test]
    fn from_inputs_rejects_unknown_skip_id() {
        // Typos in the skip list are operator errors — surfacing
        // as an Err prevents a UI dialog that silently runs a
        // phase the operator thought it had excluded.
        let err = ParsedBuild::from_inputs("bk", None, &["configure".into(), "nope".into()], false)
            .unwrap_err();
        assert!(err.contains("nope"));
    }

    #[test]
    fn from_inputs_rejects_empty_chapter_list() {
        let err = ParsedBuild::from_inputs("bk", Some(vec![]), &[], false).unwrap_err();
        assert!(err.contains("empty"));
    }
}
