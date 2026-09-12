// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-split from the former monolithic `commands.rs` (PR5). Tauri
//! command handlers grouped by concern; re-exported through
//! `commands/mod.rs` so `commands::<name>` paths in `main.rs`'s
//! `generate_handler!` stay valid.
//!
//! # These three ran a CorpusEngine in this process until svt-6
//!
//! `recipe_validate`, `recipe_test` and `recipe_run_harness` each built a
//! stub `CorpusEngine` over a system temp dir and ran the real harness here;
//! `recipe_run_harness`'s rung 6 then reached `state.corpus_engine` — the
//! LAST reader of that slot — to verify atoms in the shared indexes root. A
//! client that opens the knowledge engine to answer a question is the
//! duplicate this campaign exists to remove (ARCH principle 12), and the
//! duplicate was measurable: the frozen sample landed under the CLIENT's
//! `~/.svrnmesh/harness`, and the atoms verified were reached through an
//! engine this process partitioned for itself.
//!
//! All three are TurnClient calls now —
//! `POST /internal/corpus/recipes/test` and
//! `POST /internal/corpus/recipes/harness` (`sovereign_mesh::recipe_http`) —
//! and the daemon runs the same `corpus_engine::harness` code over ITS engine
//! and ITS data root. The two result shapes below are unchanged, so
//! `RecipeTestingPanel.svelte` and `HarnessLadderCard.svelte` are untouched.
use super::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use sovereign_contracts::daemon_wire::{
    IngestJobAck, RecipeDryRunProgress, RecipeDryRunReport, RecipeDryRunRequest,
    RecipeHarnessProgress, RecipeHarnessRequest, RecipeJobState,
};
use sovereign_turn_client::TurnClient;

use crate::state::AppState;

/// How often a daemon-side recipe job is polled, and how many CONSECUTIVE
/// poll failures end the wait with an error.
///
/// Same shape as the local-corpus ingest follow: one failure is a hiccup,
/// ten in a row is a daemon that is not coming back, and a panel that spins
/// forever is worse than one that says so (ARCH principle 6).
const JOB_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(750);
const JOB_POLL_MAX_FAILURES: u32 = 10;

/// Read the recipe the author picked in their own file dialog. The TOML
/// travels to the daemon; the PATH does not, because a daemon that reads
/// client-supplied paths is a different surface from one that reads its own
/// data root.
fn read_recipe(recipe_path: &Path) -> Result<String, String> {
    std::fs::read_to_string(recipe_path)
        .map_err(|e| format!("read recipe {}: {e}", recipe_path.display()))
}

// ─── Recipe Testing ──────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct RecipeValidateResult {
    pub passed: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub corpus_id: String,
    pub corpus_name: String,
    pub source_reachable: Option<bool>,
}

#[derive(Serialize)]
pub struct RecipeTestResult {
    pub passed: bool,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub recipe_id: String,
    pub recipe_name: String,
    pub records_attempted: usize,
    pub records_succeeded: usize,
    pub extraction_rate: f32,
    pub total_chunks: usize,
    pub avg_chars: f32,
    pub report_path: String,
    pub report_markdown: String,
}

/// Validate a recipe's fields without downloading any data.
///
/// `sample_size: 0` on the daemon's dry-run route: static checks plus, when
/// `offline` is false, one HTTP HEAD on the source URL. Answered inline —
/// nothing is acquired, so there is no job to follow.
///
/// `passed` here is "no validation ERRORS", deliberately weaker than the
/// harness's `passed`: this affordance answers "is the recipe well-formed?",
/// not "would it produce a usable corpus?".
#[tauri::command]
pub async fn recipe_validate(
    state: State<'_, Arc<AppState>>,
    recipe_path: String,
    offline: bool,
) -> Result<RecipeValidateResult, String> {
    let toml_text = read_recipe(Path::new(&recipe_path))?;
    let report: RecipeDryRunReport = TurnClient::new(state.client_base_url())
        .recipe_dry_run(&RecipeDryRunRequest {
            toml_text,
            sample_size: 0,
            offline,
        })
        .await
        .map_err(|e| format!("recipe_validate `{recipe_path}`: {e}"))?;

    Ok(RecipeValidateResult {
        passed: report.errors.is_empty(),
        errors: report.errors,
        warnings: report.warnings,
        corpus_id: report.recipe_id,
        corpus_name: report.recipe_name,
        source_reachable: report.source_reachable,
    })
}

/// Run the full recipe test harness: validate → acquire sample → extract →
/// chunk, on the DAEMON, then write `TEST_REPORT.md` beside the author's
/// recipe.
///
/// The report FILE is written here and not by the daemon, and that is an
/// ownership line rather than an accident: the recipe directory is the
/// author's own, reached through the file dialog they opened, while the
/// daemon writes only inside the root it owns. The markdown itself is the
/// daemon's — `report_markdown` verbatim, one renderer (ARCH principle 8).
///
/// Embedding is not available in this path — the embed phase is always
/// skipped, as it was in-process.
#[tauri::command]
pub async fn recipe_test(
    state: State<'_, Arc<AppState>>,
    recipe_path: String,
    sample_size: usize,
    offline: bool,
) -> Result<RecipeTestResult, String> {
    let path = PathBuf::from(&recipe_path);
    let output_path = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("TEST_REPORT.md");
    let toml_text = read_recipe(&path)?;
    let client = TurnClient::new(state.client_base_url());
    let request = RecipeDryRunRequest {
        toml_text,
        sample_size,
        offline,
    };

    // Two arms on one route, split on whether anything is acquired. A sample
    // is a download, and a Tauri command never awaits a download — it
    // follows the daemon's job (campaign ambiguity policy, 2026-09-12).
    let report = if sample_size == 0 {
        client
            .recipe_dry_run::<_, RecipeDryRunReport>(&request)
            .await
            .map_err(|e| format!("recipe_test `{recipe_path}`: {e}"))?
    } else {
        let ack: IngestJobAck = client
            .recipe_dry_run(&request)
            .await
            .map_err(|e| format!("recipe_test `{recipe_path}`: {e}"))?;
        follow_dry_run(&client, &ack.job_id).await?
    };

    let markdown = report.report_markdown;
    if let Err(e) = std::fs::write(&output_path, &markdown) {
        tracing::warn!(
            "Failed to write TEST_REPORT.md to {}: {e}",
            output_path.display()
        );
    }

    Ok(RecipeTestResult {
        passed: report.passed,
        warnings: report.warnings,
        errors: report.errors,
        recipe_id: report.recipe_id,
        recipe_name: report.recipe_name,
        records_attempted: report.records_attempted,
        records_succeeded: report.records_succeeded,
        extraction_rate: report.extraction_rate,
        total_chunks: report.total_chunks,
        avg_chars: report.avg_chars,
        report_path: output_path.to_string_lossy().into_owned(),
        report_markdown: markdown,
    })
}

/// Follow a daemon-side dry run to its report.
///
/// A run that ends in `Error` is an `Err` here, never an empty report: a
/// zero-chunk zero-record report and a harness that could not run look
/// identical on a panel and ask the author for opposite things (ARCH
/// principle 6).
async fn follow_dry_run(client: &TurnClient, job_id: &str) -> Result<RecipeDryRunReport, String> {
    let mut failures = 0u32;
    loop {
        tokio::time::sleep(JOB_POLL_INTERVAL).await;
        let progress: RecipeDryRunProgress = match client.recipe_dry_run_progress(job_id).await {
            Ok(p) => {
                failures = 0;
                p
            }
            Err(e) => {
                failures += 1;
                tracing::warn!(job_id, failures, "recipe_test: progress poll failed: {e}");
                if failures >= JOB_POLL_MAX_FAILURES {
                    return Err(format!(
                        "lost contact with the daemon while testing recipe job \
                         `{job_id}` ({failures} consecutive failures): {e}"
                    ));
                }
                continue;
            }
        };
        match progress.state {
            RecipeJobState::Running => {}
            RecipeJobState::Complete => {
                return progress.report.ok_or_else(|| {
                    format!("recipe job `{job_id}` reported complete with no report")
                })
            }
            RecipeJobState::Error => {
                return Err(progress
                    .error
                    .unwrap_or_else(|| format!("recipe job `{job_id}` failed without a reason")))
            }
        }
    }
}

// `HarnessRunCard` stood here and is GONE (svt-6). Its `run` field was
// `sovereign_authoring_harness::HarnessRun` — a type this process cannot name
// once it stops linking that crate, and the LAST thing holding the
// dependency. The daemon serialises the same card through
// `sovereign_contracts::daemon_wire::HarnessRunCardView<HarnessRun>`, whose
// six keys `sovereign-mesh/tests/main/recipe_surface_e2e.rs` pins by name and
// whose `types.ts` twin (`HarnessRunCard`) is unchanged.

/// Run the deterministic authoring harness over a frozen sample and return
/// the per-stage verdict ladder. Rungs 1–5 (Acquire→Extract→Filter→Chunk→
/// Index) are model-free and offline after the first run — the sample is
/// captured once under the DAEMON's `<data_dir>/harness/<recipe-id>/`, then
/// byte-identical (I1).
///
/// `enrich` adds rung 6, and it does NOT stand up a parallel enrichment
/// path: the daemon verifies the atoms its own ingest/enrich already wrote
/// for this corpus, in the index it serves retrieval from. No rung-6 verdict
/// comes back when the corpus is not enriched yet — install/enrich it through
/// the normal flow first.
///
/// Returns the daemon's card VERBATIM, as `serde_json::Value`: `HarnessRun`
/// is defined in `sovereign-authoring-harness`, a crate this process no
/// longer links. The bytes are one definition's and the shape is pinned on
/// the serving side.
#[tauri::command]
pub async fn recipe_run_harness(
    state: State<'_, Arc<AppState>>,
    recipe_path: String,
    sample_size: usize,
    enrich: bool,
) -> Result<serde_json::Value, String> {
    let toml_text = read_recipe(Path::new(&recipe_path))?;
    let client = TurnClient::new(state.client_base_url());
    let ack: IngestJobAck = client
        .recipe_harness(&RecipeHarnessRequest {
            toml_text,
            sample_size,
            enrich,
        })
        .await
        .map_err(|e| format!("recipe_run_harness `{recipe_path}`: {e}"))?;

    let mut failures = 0u32;
    loop {
        tokio::time::sleep(JOB_POLL_INTERVAL).await;
        let progress: RecipeHarnessProgress =
            match client.recipe_harness_progress(&ack.job_id).await {
                Ok(p) => {
                    failures = 0;
                    p
                }
                Err(e) => {
                    failures += 1;
                    tracing::warn!(
                        job_id = %ack.job_id,
                        failures,
                        "recipe_run_harness: progress poll failed: {e}"
                    );
                    if failures >= JOB_POLL_MAX_FAILURES {
                        return Err(format!(
                            "lost contact with the daemon while running the harness \
                         (job `{}`, {failures} consecutive failures): {e}",
                            ack.job_id
                        ));
                    }
                    continue;
                }
            };
        match progress.state {
            RecipeJobState::Running => {}
            RecipeJobState::Complete => {
                return progress.card.ok_or_else(|| {
                    format!(
                        "harness job `{}` reported complete with no card",
                        ack.job_id
                    )
                })
            }
            RecipeJobState::Error => {
                return Err(progress.error.unwrap_or_else(|| {
                    format!("harness job `{}` failed without a reason", ack.job_id)
                }))
            }
        }
    }
}

/// Kick off background installs for every corpus in the given tier.
/// Used by the setup wizard's "install tier" affordance.
pub(crate) async fn start_tier_installs(
    app_handle: &tauri::AppHandle,
    state: &Arc<AppState>,
    tier: &str,
) {
    // sv-surface D9b. Two things were wrong here and they are the same
    // shape: this process answering a question the daemon owns.
    //
    // The TIER lookup read `state.corpus_engine`'s built-in catalogue and
    // ran `tiers_for` locally; the catalogue row carries `tiers` (the
    // daemon holds `tiers_for` since 2a9a9e91e), so tier membership here
    // and the picker's tier chips can no longer disagree.
    //
    // The INSTALL ran `CorpusEngine::ingest` INLINE, in this process,
    // "duplicating the spawn pattern" of `install_corpus` — which does not
    // ingest at all, it asks the daemon. So the wizard's tier install and
    // the picker's single install were two different pipelines writing the
    // same index directory, and in attach the wizard's was the one the
    // daemon never heard about. Both go through `request_daemon_install`
    // now: idempotent, and the existing `corpus-progress` poller narrates
    // whichever ingests the daemon is actually running.
    let rows: Vec<CorpusEntry> =
        match sovereign_turn_client::TurnClient::new(state.client_base_url())
            .corpus_catalog::<CorpusEntry>()
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    tier,
                    "start_tier_installs: the daemon catalogue did not answer"
                );
                return;
            }
        };

    for row in rows.iter().filter(|r| r.tiers.iter().any(|t| t == tier)) {
        tracing::info!(tier, corpus_id = %row.id, "start_tier_installs: queuing install");
        if let Err(e) = crate::commands::corpus_install::request_daemon_install(
            app_handle,
            state.as_ref(),
            &row.id,
        )
        .await
        {
            tracing::warn!(
                tier,
                corpus_id = %row.id,
                error = %e,
                "start_tier_installs: the daemon refused the install request"
            );
        }
    }
}
