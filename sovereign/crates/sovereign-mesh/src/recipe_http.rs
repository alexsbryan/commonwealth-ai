// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe-registry writes and reads the desktop stops doing in-process
//! (thin-desktop order, 2026-09-11): importing an authored recipe into the
//! local registry, and reading a recipe's `[parameters]` for the install
//! form.
//!
//! Both go through the DAEMON's `CorpusEngine` and its `RecipeRegistry`
//! — the registry whose `overrides_dir` IS the recipes dir every install
//! resolves through. `recipe_commands.rs` used to build a stub engine in a
//! temp dir to validate, then write `~/.svrnmesh/recipes/<id>/recipe.toml`
//! and upsert `registry.toml` with a loop of its own (a second copy of the
//! CLI's `recipe publish` loop); both loops now call
//! `RecipeRegistry::install_local_recipe`, one decider (ARCH principle 8).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use corpus_engine::harness::verify_atoms_at;
use corpus_engine::testing::TestReport;
use corpus_engine::{CorpusEngine, ParameterKind, Recipe, TestOptions};
use sovereign_authoring_harness::{Declaration, HarnessRun};
use sovereign_contracts::daemon_wire::{
    HarnessRunCardView, ImportRecipeRequest, ImportRecipeResult, IngestJobAck,
    RecipeDryRunProgress, RecipeDryRunReport, RecipeDryRunRequest, RecipeHarnessProgress,
    RecipeHarnessRequest, RecipeJobState, RecipeParameter, RecipeParameterSchema,
};

use crate::daemon::EmbeddedDaemon;
use crate::http_response::{json_error, Absence};
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

/// The recipe-registry router. Mounted unconditionally beside
/// `corpus_catalog_http`; a daemon with no corpus engine answers 503
/// naming that, which is a different fact from an unmounted router's 404.
///
/// **The trailing `/progress` on the two job routes is load-bearing.** With
/// the progress routes spelled `.../recipes/test/{job}`, axum prefers the
/// STATIC `test` segment over `{corpus}`, so
/// `GET /internal/corpus/recipes/test/parameters` reached the dry-run handler
/// and answered "no recipe dry run `parameters`" — a recipe whose id is
/// `test` or `harness` could no longer have its install form rendered. Found
/// by `the_dry_run_progress_route_does_not_shadow_the_parameters_route`,
/// which went red on exactly that (svt-6). Six segments cannot collide with
/// the five-segment `{corpus}/parameters`, and it matches the sibling
/// convention `/internal/corpus/{corpus}/index/progress`.
pub fn recipe_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/internal/corpus/recipes/import", post(import))
        .route("/internal/corpus/recipes/test", post(dry_run))
        .route(
            "/internal/corpus/recipes/test/{job}/progress",
            get(dry_run_progress),
        )
        .route("/internal/corpus/recipes/harness", post(harness))
        .route(
            "/internal/corpus/recipes/harness/{job}/progress",
            get(harness_progress),
        )
        .route(
            "/internal/corpus/recipes/{corpus}/parameters",
            get(parameters),
        )
        .localhost_only_with(daemon)
}

/// POST `/internal/corpus/recipes/import` `{toml_text}` — validate the
/// recipe offline (`test_recipe`, sample size 0) and, when it passes,
/// install it into the daemon's recipes dir + local `registry.toml`.
/// Answers `ImportRecipeResult`: a recipe that fails validation is a 200
/// with `success: false` and the errors (the form renders them); a body
/// that is not a recipe at all, or a store the daemon cannot write, is
/// 400 / 500.
async fn import(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(body): Json<ImportRecipeRequest>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let recipe = Recipe::from_toml(&body.toml_text)
        .map_err(|e| Absence::invalid(format!("recipe TOML parse failed: {e}")))?;
    let corpus_id = recipe.corpus.id.clone();
    if corpus_id.is_empty() {
        return Err(Absence::invalid("recipe `[corpus] id` must not be empty"));
    }

    // The validation harness reads a file; stage the text beside the
    // engine's own recipes rather than in a shared system temp dir.
    let staging = engine
        .recipes_dir()
        .join("_import")
        .join(format!("{corpus_id}.toml"));
    if let Some(parent) = staging.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Absence::internal(format!("create {}: {e}", parent.display())))?;
    }
    std::fs::write(&staging, &body.toml_text)
        .map_err(|e| Absence::internal(format!("stage recipe for validation: {e}")))?;
    let options = TestOptions {
        sample_size: 0,
        embed: false,
        offline: true,
        ..Default::default()
    };
    let report = engine.test_recipe(&staging, &options).await;
    let _ = std::fs::remove_file(&staging);
    let report =
        report.map_err(|e| Absence::internal(format!("validation harness failed: {e}")))?;

    if !report.validation.errors.is_empty() {
        tracing::debug!(
            corpus_id = %corpus_id,
            errors = report.validation.errors.len(),
            "recipe_http: import refused by validation"
        );
        return Ok((
            StatusCode::OK,
            Json(ImportRecipeResult {
                success: false,
                corpus_id,
                recipe_path: String::new(),
                errors: report.validation.errors.clone(),
                warnings: report.validation.warnings.clone(),
            }),
        )
            .into_response());
    }

    let recipe_path = engine
        .registry()
        .install_local_recipe(&recipe, &body.toml_text)
        .map_err(|e| Absence::internal(format!("install recipe `{corpus_id}`: {e}")))?;
    tracing::info!(
        corpus_id = %corpus_id,
        path = %recipe_path.display(),
        warnings = report.validation.warnings.len(),
        "recipe_http: recipe imported into the local registry"
    );
    Ok((
        StatusCode::OK,
        Json(ImportRecipeResult {
            success: true,
            corpus_id,
            recipe_path: recipe_path.display().to_string(),
            errors: Vec::new(),
            warnings: report.validation.warnings.clone(),
        }),
    )
        .into_response())
}

/// GET `/internal/corpus/recipes/{corpus}/parameters` — the recipe's
/// declared `[parameters]`, resolved through the daemon's registry (local
/// override first, then the registry entry). Answers
/// `RecipeParameterSchema`; an unknown recipe is a 404 naming it.
async fn parameters(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let recipe = engine
        .registry()
        .fetch_recipe(&corpus)
        .await
        .map_err(|e| Absence::missing(format!("recipe `{corpus}`: {e}")))?;
    let parameters: Vec<RecipeParameter> = recipe
        .parameters
        .iter()
        .map(|(name, spec)| RecipeParameter {
            name: name.clone(),
            kind: parameter_kind_label(&spec.kind).to_string(),
            description: spec.description.clone(),
            required: spec.required,
            default: spec.default.as_ref().map(toml_to_json),
        })
        .collect();
    tracing::debug!(
        corpus_id = %corpus,
        parameters = parameters.len(),
        "recipe_http: parameter schema served"
    );
    Ok((
        StatusCode::OK,
        Json(RecipeParameterSchema {
            corpus_id: recipe.corpus.id,
            parameters,
        }),
    )
        .into_response())
}

// ─── The recipe dry run ────────────────────────────────────────

/// One sampled dry run's live state, keyed by JOB id in [`DRY_RUNS`].
///
/// In-process on purpose, like `corpus_catalog_http`'s index builds: the run
/// it narrates is this daemon's, and a log that outlived the daemon would
/// describe a run that may not have finished.
struct DryRun {
    recipe_id: String,
    /// `None` while running.
    outcome: Mutex<Option<Result<RecipeDryRunReport, String>>>,
}

static DRY_RUNS: OnceLock<Mutex<HashMap<String, Arc<DryRun>>>> = OnceLock::new();

fn dry_runs() -> &'static Mutex<HashMap<String, Arc<DryRun>>> {
    DRY_RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The job id of a dry run still running for this recipe, if any. The 409
/// is by RECIPE, not by job: two samples of one recipe write the same
/// download staging under `corpus-engine-test-downloads`.
///
/// A poisoned lock reads as "no live run", so the refusal is not offered and
/// a second run is accepted. That is the same trade `corpus_catalog_http`'s
/// `index_build_for` makes, deliberately: the lock is poisoned only when a
/// handler panicked, and refusing every later run on a daemon that is already
/// wrong is worse than allowing two harness runs to share a download dir.
fn live_dry_run_for(recipe_id: &str) -> Option<String> {
    let jobs = dry_runs().lock().ok()?;
    jobs.iter()
        .find(|(_, job)| {
            job.recipe_id == recipe_id && job.outcome.lock().ok().is_some_and(|o| o.is_none())
        })
        .map(|(job_id, _)| job_id.clone())
}

/// Stage the supplied TOML beside the engine's own recipes, under a name
/// nothing else is using, and hand back the path.
///
/// The nonce is what keeps three writers apart: `import` stages
/// `<id>.toml` and deletes it, a synchronous dry run of the same recipe
/// would delete the other's file mid-read, and two sampled runs would
/// clobber each other. One staging helper, one naming rule (ARCH
/// principle 8).
fn stage_recipe(
    engine: &CorpusEngine,
    recipe_id: &str,
    nonce: &str,
    toml_text: &str,
) -> Result<std::path::PathBuf, Absence> {
    let staging = engine
        .recipes_dir()
        .join("_import")
        .join(format!("{recipe_id}.{nonce}.toml"));
    if let Some(parent) = staging.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Absence::internal(format!("create {}: {e}", parent.display())))?;
    }
    std::fs::write(&staging, toml_text)
        .map_err(|e| Absence::internal(format!("stage recipe for the dry run: {e}")))?;
    Ok(staging)
}

/// THE projection from the engine's `TestReport` onto the wire. Both arms
/// of the route answer through this one function, so a sampled run and a
/// validation-only run cannot disagree about what a field means
/// (ARCH principle 8).
fn dry_run_report(report: &TestReport) -> RecipeDryRunReport {
    // `extraction`/`chunking` are `None` when the stage did not run — the
    // validation-only case. They collapse to zero here because that is the
    // wire contract the panel already reads (`RecipeTestResult` in types.ts
    // types both as plain numbers), and the field that says WHICH case it is
    // is `records_attempted == 0`, documented on the DTO. Widening these to
    // `Option` is a frontend change, not this commit's.
    let (records_attempted, records_succeeded, extraction_rate) = report
        .extraction
        .as_ref()
        .map(|e| (e.records_attempted, e.records_succeeded, e.extraction_rate))
        .unwrap_or((0, 0, 0.0));
    let (total_chunks, avg_chars) = report
        .chunking
        .as_ref()
        .map(|c| (c.total_chunks, c.avg_chars))
        .unwrap_or((0, 0.0));
    RecipeDryRunReport {
        passed: report.passed(),
        errors: report.validation.errors.clone(),
        warnings: report.warnings(),
        recipe_id: report.recipe_id.clone(),
        recipe_name: report.recipe_name.clone(),
        source_reachable: report.validation.source_reachable,
        records_attempted,
        records_succeeded,
        extraction_rate,
        total_chunks,
        avg_chars,
        report_markdown: report.to_markdown(),
    }
}

/// POST `/internal/corpus/recipes/test` `{toml_text, sample_size, offline}` —
/// run the recipe harness over the DAEMON's engine.
///
/// Two arms, split on whether anything is downloaded:
///
/// - `sample_size == 0` is validation-only — static checks plus, when
///   `offline` is false, one HTTP HEAD on the source URL — and answers
///   `RecipeDryRunReport` synchronously. A HEAD is a reachability probe, not
///   an acquisition, and the import route beside this one already runs the
///   same harness inline.
/// - `sample_size > 0` acquires a sample, so it is a JOB: `202` with an
///   [`IngestJobAck`] naming `GET /internal/corpus/recipes/test/{job}/progress`.
///   A second run of the same recipe while one is in flight is refused by
///   name, not queued.
///
/// `output` is never set: the engine writes no file, and the report the
/// author keeps is written beside THEIR recipe by the surface that owns that
/// directory.
async fn dry_run(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(body): Json<RecipeDryRunRequest>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let recipe = Recipe::from_toml(&body.toml_text)
        .map_err(|e| Absence::invalid(format!("recipe TOML parse failed: {e}")))?;
    let recipe_id = recipe.corpus.id.clone();
    if recipe_id.is_empty() {
        return Err(Absence::invalid("recipe `[corpus] id` must not be empty"));
    }

    if body.sample_size == 0 {
        let staging = stage_recipe(engine, &recipe_id, "validate", &body.toml_text)?;
        let options = TestOptions {
            sample_size: 0,
            embed: false,
            offline: body.offline,
            ..Default::default()
        };
        let report = engine.test_recipe(&staging, &options).await;
        let _ = std::fs::remove_file(&staging);
        let report =
            report.map_err(|e| Absence::internal(format!("validation harness failed: {e}")))?;
        tracing::debug!(
            recipe_id = %recipe_id,
            errors = report.validation.errors.len(),
            offline = body.offline,
            "recipe_http: dry run served inline (validation only)"
        );
        return Ok((StatusCode::OK, Json(dry_run_report(&report))).into_response());
    }

    if let Some(job_id) = live_dry_run_for(&recipe_id) {
        return Ok(json_error(
            StatusCode::CONFLICT,
            format!("`{recipe_id}` is already being tested (job {job_id})"),
        ));
    }

    let job_id = format!("recipe-test-{}", uuid::Uuid::new_v4());
    let staging = stage_recipe(engine, &recipe_id, &job_id, &body.toml_text)?;
    let job = Arc::new(DryRun {
        recipe_id: recipe_id.clone(),
        outcome: Mutex::new(None),
    });
    if let Ok(mut jobs) = dry_runs().lock() {
        jobs.insert(job_id.clone(), Arc::clone(&job));
    }
    tracing::info!(
        recipe_id = %recipe_id,
        job_id = %job_id,
        sample_size = body.sample_size,
        offline = body.offline,
        "recipe_http: recipe dry run accepted",
    );

    let engine = Arc::clone(engine);
    let sample_size = body.sample_size;
    let offline = body.offline;
    let spawn_recipe = recipe_id.clone();
    let spawn_job = job_id.clone();
    tokio::spawn(async move {
        let options = TestOptions {
            sample_size,
            embed: false,
            offline,
            ..Default::default()
        };
        let result = engine
            .test_recipe(&staging, &options)
            .await
            .map(|r| dry_run_report(&r))
            .map_err(|e| format!("recipe harness failed: {e}"));
        let _ = std::fs::remove_file(&staging);
        match &result {
            Ok(r) => tracing::info!(
                recipe_id = %spawn_recipe,
                job_id = %spawn_job,
                passed = r.passed,
                chunks = r.total_chunks,
                "recipe_http: recipe dry run complete",
            ),
            Err(e) => tracing::warn!(
                recipe_id = %spawn_recipe,
                job_id = %spawn_job,
                error = %e,
                "recipe_http: recipe dry run failed",
            ),
        }
        if let Ok(mut o) = job.outcome.lock() {
            *o = Some(result);
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(IngestJobAck {
            progress_route: format!("/internal/corpus/recipes/test/{job_id}/progress"),
            corpus_id: recipe_id,
            job_id,
            ok: true,
        }),
    )
        .into_response())
}

/// GET `/internal/corpus/recipes/test/{job}/progress` — where one sampled dry run
/// stands. A job id this daemon never minted is a 404 naming it: "no such
/// job" and "a job that has not started" ask the caller for different
/// things, so they are not collapsed into one shape (ARCH principle 6).
///
/// A finished run is readable more than once — the report stays in the map
/// for the lifetime of the daemon, because the panel polls and then renders
/// from the last answer.
async fn dry_run_progress(
    _: LocalOnly,
    Extension(_daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(job_id): Path<String>,
) -> Result<Response, Absence> {
    let Some(job) = dry_runs().lock().ok().and_then(|j| j.get(&job_id).cloned()) else {
        return Ok(json_error(
            StatusCode::NOT_FOUND,
            format!("no recipe dry run `{job_id}` on this daemon"),
        ));
    };
    let outcome = job.outcome.lock().ok().and_then(|o| o.clone());
    let progress = match outcome {
        None => RecipeDryRunProgress {
            recipe_id: job.recipe_id.clone(),
            job_id: job_id.clone(),
            state: RecipeJobState::Running,
            report: None,
            error: None,
        },
        Some(Ok(report)) => RecipeDryRunProgress {
            recipe_id: job.recipe_id.clone(),
            job_id: job_id.clone(),
            state: RecipeJobState::Complete,
            report: Some(report),
            error: None,
        },
        Some(Err(e)) => RecipeDryRunProgress {
            recipe_id: job.recipe_id.clone(),
            job_id: job_id.clone(),
            state: RecipeJobState::Error,
            report: None,
            error: Some(e),
        },
    };
    tracing::debug!(
        job_id = %job_id,
        state = ?progress.state,
        "recipe_http: recipe dry run progress served",
    );
    Ok(Json(progress).into_response())
}

// ─── The authoring harness ─────────────────────────────────────

/// The card this daemon serialises: the view at the concrete `HarnessRun`.
type HarnessCard = HarnessRunCardView<HarnessRun>;

/// One harness run's live state, keyed by JOB id in [`HARNESS_RUNS`].
struct HarnessJob {
    recipe_id: String,
    /// `None` while running.
    outcome: Mutex<Option<Result<HarnessCard, String>>>,
}

static HARNESS_RUNS: OnceLock<Mutex<HashMap<String, Arc<HarnessJob>>>> = OnceLock::new();

fn harness_runs() -> &'static Mutex<HashMap<String, Arc<HarnessJob>>> {
    HARNESS_RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The job id of a harness run still going for this recipe, if any. By
/// RECIPE, because the frozen-sample store under
/// `<data_dir>/harness/<recipe-id>` is per recipe and two runs would capture
/// into the same directory.
///
/// Same poisoned-lock trade as [`live_dry_run_for`] above.
fn live_harness_for(recipe_id: &str) -> Option<String> {
    let jobs = harness_runs().lock().ok()?;
    jobs.iter()
        .find(|(_, job)| {
            job.recipe_id == recipe_id && job.outcome.lock().ok().is_some_and(|o| o.is_none())
        })
        .map(|(job_id, _)| job_id.clone())
}

/// POST `/internal/corpus/recipes/harness` `{toml_text, sample_size, enrich}`
/// — the deterministic authoring harness over a frozen sample, as a JOB.
///
/// Always a job: rungs 1-5 are offline after the first run, but the FIRST
/// run captures, and a capture is the one networked step (I3). A Tauri
/// command never awaits a download.
///
/// The drive is `sovereign_authoring_harness::run_over_frozen_sample` — the
/// same function `svrn recipe test` calls, not a second copy of it. What
/// differs is rung 6, which is this daemon's own question: `verify_atoms_at`
/// over `<index_dir>/<recipe-id>`, the corpus this daemon actually installed
/// and enriched. `None` (no rung-6 verdict) when the corpus is not enriched
/// yet — reported as an absent rung rather than a passing one.
async fn harness(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(body): Json<RecipeHarnessRequest>,
) -> Result<Response, Absence> {
    let engine = engine_for(&daemon)?;
    let recipe = Recipe::from_toml(&body.toml_text)
        .map_err(|e| Absence::invalid(format!("recipe TOML parse failed: {e}")))?;
    let recipe_id = recipe.corpus.id.clone();
    if recipe_id.is_empty() {
        return Err(Absence::invalid("recipe `[corpus] id` must not be empty"));
    }
    if let Some(job_id) = live_harness_for(&recipe_id) {
        return Ok(json_error(
            StatusCode::CONFLICT,
            format!("`{recipe_id}` is already running the harness (job {job_id})"),
        ));
    }

    // The frozen-sample store is the DAEMON's, under the data root it owns.
    let harness_root = daemon.data_dir().join("harness").join(&recipe_id);
    let job_id = format!("recipe-harness-{}", uuid::Uuid::new_v4());
    let job = Arc::new(HarnessJob {
        recipe_id: recipe_id.clone(),
        outcome: Mutex::new(None),
    });
    if let Ok(mut jobs) = harness_runs().lock() {
        jobs.insert(job_id.clone(), Arc::clone(&job));
    }
    tracing::info!(
        recipe_id = %recipe_id,
        job_id = %job_id,
        sample_size = body.sample_size,
        enrich = body.enrich,
        "recipe_http: authoring harness accepted",
    );

    let engine = Arc::clone(engine);
    let sample_size = body.sample_size;
    let enrich = body.enrich;
    let index_dir = engine.index_dir().join(&recipe_id);
    let spawn_recipe = recipe_id.clone();
    let spawn_job = job_id.clone();
    tokio::spawn(async move {
        let result = run_harness_job(
            &engine,
            &recipe,
            &harness_root,
            sample_size,
            enrich,
            &index_dir,
            &spawn_job,
        )
        .await;
        match &result {
            Ok(card) => tracing::info!(
                recipe_id = %spawn_recipe,
                job_id = %spawn_job,
                green = card.green,
                frozen_docs = card.frozen_docs,
                "recipe_http: authoring harness complete",
            ),
            Err(e) => tracing::warn!(
                recipe_id = %spawn_recipe,
                job_id = %spawn_job,
                error = %e,
                "recipe_http: authoring harness failed",
            ),
        }
        if let Ok(mut o) = job.outcome.lock() {
            *o = Some(result);
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(IngestJobAck {
            progress_route: format!("/internal/corpus/recipes/harness/{job_id}/progress"),
            corpus_id: recipe_id,
            job_id,
            ok: true,
        }),
    )
        .into_response())
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
    job_id: &str,
) -> Result<HarnessCard, String> {
    let frozen_run = sovereign_authoring_harness::run_over_frozen_sample(
        engine,
        recipe,
        harness_root,
        sample_size,
        false,
        &|m| tracing::info!(job_id, "recipe_http: harness {m}"),
    )
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
        ran_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        run,
    })
}

/// GET `/internal/corpus/recipes/harness/{job}/progress` — where one harness run
/// stands. A job id this daemon never minted is a 404 naming it.
async fn harness_progress(
    _: LocalOnly,
    Extension(_daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(job_id): Path<String>,
) -> Result<Response, Absence> {
    let Some(job) = harness_runs()
        .lock()
        .ok()
        .and_then(|j| j.get(&job_id).cloned())
    else {
        return Ok(json_error(
            StatusCode::NOT_FOUND,
            format!("no harness run `{job_id}` on this daemon"),
        ));
    };
    let outcome = job.outcome.lock().ok().and_then(|o| o.clone());
    let (state, card, error) = match outcome {
        None => (RecipeJobState::Running, None, None),
        Some(Ok(card)) => (RecipeJobState::Complete, Some(card), None),
        Some(Err(e)) => (RecipeJobState::Error, None, Some(e)),
    };
    tracing::debug!(
        job_id = %job_id,
        state = ?state,
        "recipe_http: authoring harness progress served",
    );
    Ok(Json(RecipeHarnessProgress {
        recipe_id: job.recipe_id.clone(),
        job_id,
        state,
        card,
        error,
    })
    .into_response())
}

fn engine_for(daemon: &Arc<EmbeddedDaemon>) -> Result<&Arc<CorpusEngine>, Absence> {
    daemon
        .corpus_engine()
        .ok_or_else(|| Absence::unavailable("corpus engine not initialised"))
}

/// The recipe's `type` label for a parameter, as the form keys on it.
pub fn parameter_kind_label(k: &ParameterKind) -> &'static str {
    match k {
        ParameterKind::String => "string",
        ParameterKind::Int => "int",
        ParameterKind::Date => "date",
        ParameterKind::List => "list",
    }
}

/// A TOML default rendered as JSON for the form.
pub fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    match v {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(*i),
        toml::Value::Float(f) => serde_json::json!(*f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => {
            let mut map = serde_json::Map::new();
            for (k, vv) in table {
                map.insert(k.clone(), toml_to_json(vv));
            }
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_kind_labels_round_trip() {
        assert_eq!(parameter_kind_label(&ParameterKind::String), "string");
        assert_eq!(parameter_kind_label(&ParameterKind::Int), "int");
        assert_eq!(parameter_kind_label(&ParameterKind::Date), "date");
        assert_eq!(parameter_kind_label(&ParameterKind::List), "list");
    }

    #[test]
    fn toml_to_json_handles_arrays_and_strings() {
        let v = toml::Value::Array(vec![
            toml::Value::String("NVDA".into()),
            toml::Value::String("MSFT".into()),
        ]);
        assert_eq!(toml_to_json(&v), serde_json::json!(["NVDA", "MSFT"]));
    }
}
