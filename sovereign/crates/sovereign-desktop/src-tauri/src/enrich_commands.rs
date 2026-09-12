// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri command surface for atlas enrichment (read + install side).
//!
//! Enrichment BUILDS run IN-PROCESS in the daemon (tiered GliNER/RAPTOR
//! during ingest + the post-install structural-atlas hook), observed from
//! the UI by polling `lc_enrichment_status`. The old CLI-shell commands
//! were removed once every desktop surface migrated to that path —
//! `sovereign-cli` is not bundled with the desktop.
//!
//! What remains here is a THIN client of the daemon (thin-desktop order,
//! 2026-09-11) — every read below is one `TurnClient` call, every write is
//! the daemon's own install rail:
//!   - `enrich_list_corpora` — `GET /internal/corpus/enriched`, the
//!     DAEMON's enrichment store (this file read its own data root's tree
//!     until 2026-09-11, which on an attached boot is the wrong machine).
//!   - `install_starter_corpus` — `POST /internal/corpus/install` for the
//!     bundled `federalist-starter` recipe, then wait for the catalog to
//!     say `installed`. The HF repo, filename and sha256 live in
//!     `sovereign-recipes/federalist-starter/recipe.toml`'s `[prebuilt]`
//!     block — ONE copy, restored by `CorpusEngine::try_restore_prebuilt`
//!     like every other prebuilt corpus, instead of a second restore path
//!     in this process with the same three constants.
//!   - `enrich_get_starter_questions` —
//!     `GET /internal/corpus/{corpus}/starter-questions`; the ranker is
//!     `corpus_engine::enrichment::atlas::analysis::starter_questions`.
//!   - `is_first_run` / `mark_first_run_complete` — the onboarding marker,
//!     app-local by the campaign's closed set.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use sovereign_contracts::daemon_wire::{EnrichedCorpusSummary, StarterQuestion};
use sovereign_turn_client::TurnClient;
use tauri::{AppHandle, State};

use crate::commands::{install_failure, request_daemon_install, CorpusEntry};
use crate::state::AppState;

// ─── Command: enrich_list_corpora ────────────────────────────────────

/// Inventory of enrichment corpora in the DAEMON's store.
///
/// Errors flatten to a String for the Tauri boundary; an absent store is
/// `Ok(vec![])`, not an error, because the UI branches on length — that is
/// the route's rule, not a default applied here.
#[tauri::command]
pub async fn enrich_list_corpora(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<EnrichedCorpusSummary>, String> {
    TurnClient::new(state.client_base_url())
        .enriched_corpora::<EnrichedCorpusSummary>()
        .await
        .map_err(|e| format!("enrich_list_corpora: {e}"))
}

/// Result of [`install_starter_corpus`].
#[derive(Serialize)]
pub struct StarterInstallResult {
    pub corpus_id: String,
    /// True when the corpus was already present (no work done) — the
    /// onboarding flow skips the "restoring…" copy in that case.
    pub already_installed: bool,
}

/// The bundled starter recipe's `[corpus] id`
/// (`sovereign-recipes/federalist-starter/recipe.toml`, registered
/// `catalog_status = "hidden"` so it is not a row in Settings → Knowledge).
const STARTER_ID: &str = "federalist-starter";
/// How long a first-run install may take before this command gives up
/// waiting. The snapshot is ~162 KB; the bound is for a host that never
/// answers, and the install itself keeps running on the daemon.
const STARTER_INSTALL_WAIT: Duration = Duration::from_secs(15 * 60);

/// Install the "Federalist Papers" starter corpus — a pre-enriched snapshot,
/// no inference, a few seconds — through the daemon's install rail.
///
/// Idempotent: the catalog is asked first, and `installed` returns early.
/// Otherwise `POST /internal/corpus/install` (the same request the Knowledge
/// pane's Add button sends) and wait until the catalog says `installed` or
/// `/internal/corpus/status` records a `Failed` outcome, whose message is
/// the user's remedy (a 401 on the gated dataset, a sha mismatch, a full
/// disk) and is returned verbatim. Progress meanwhile rides the ordinary
/// `corpus-progress` events the status poller emits for every install.
///
/// Until 2026-09-11 this command downloaded and restored the archive
/// IN-PROCESS, into this process's own data root, with the HF coordinates
/// and sha256 hardcoded here — a second copy of a recipe and the one
/// restore that ran on the wrong machine on an attached boot. The
/// `SOVEREIGN_STARTER_SNAPSHOT` dev override went with it; the daemon's
/// `SOVEREIGN_RECIPES_DIR` is the knob for a local recipe now.
#[tauri::command]
pub async fn install_starter_corpus(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<StarterInstallResult, String> {
    if starter_installed(&state).await? {
        return Ok(StarterInstallResult {
            corpus_id: STARTER_ID.to_string(),
            already_installed: true,
        });
    }

    request_daemon_install(&app, state.inner(), STARTER_ID)
        .await
        .map_err(|e| format!("install_starter_corpus: {e}"))?;
    tracing::info!(
        corpus_id = STARTER_ID,
        "starter corpus: install requested of the daemon"
    );

    let started = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some(message) = install_failure(state.inner(), STARTER_ID).await? {
            tracing::warn!(corpus_id = STARTER_ID, %message, "starter corpus: install failed");
            return Err(format!("install_starter_corpus: {message}"));
        }
        if starter_installed(&state).await? {
            tracing::info!(
                corpus_id = STARTER_ID,
                elapsed_secs = started.elapsed().as_secs(),
                "starter corpus: installed by the daemon (prebuilt snapshot, no inference)"
            );
            return Ok(StarterInstallResult {
                corpus_id: STARTER_ID.to_string(),
                already_installed: false,
            });
        }
        if started.elapsed() > STARTER_INSTALL_WAIT {
            return Err(format!(
                "install_starter_corpus: the daemon has not finished installing `{STARTER_ID}` \
                 after {}s — it may still be running; check Settings → Knowledge",
                STARTER_INSTALL_WAIT.as_secs()
            ));
        }
    }
}

/// Whether the daemon's catalog lists the starter as `installed`.
async fn starter_installed(state: &State<'_, Arc<AppState>>) -> Result<bool, String> {
    let rows = TurnClient::new(state.client_base_url())
        .corpus_catalog::<CorpusEntry>()
        .await
        .map_err(|e| format!("install_starter_corpus: catalog: {e}"))?;
    Ok(rows
        .iter()
        .any(|r| r.id == STARTER_ID && r.status == "installed"))
}

// ─── Command: enrich_get_starter_questions ───────────────────────────

/// Return up to `limit` starter questions mined from the corpus's atlas,
/// by the DAEMON (`GET /internal/corpus/{corpus}/starter-questions`).
///
/// Returns an empty vec (NOT an error) when the corpus has no atlas — the
/// UI branches on vec length to decide whether to fall back to
/// excerpt-based starters. That is the host's 404 saying so, carried
/// through [`TurnClient::starter_questions_if_present`]; a host that is
/// unreachable, or that cannot read an atlas it HAS, is an `Err` (ARCH
/// principle 6). The ranking heuristic is documented on
/// `corpus_engine::enrichment::atlas::analysis::starter_questions`.
#[tauri::command]
pub async fn enrich_get_starter_questions(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    limit: usize,
) -> Result<Vec<StarterQuestion>, String> {
    match TurnClient::new(state.client_base_url())
        .starter_questions_if_present::<StarterQuestion>(&corpus_id, limit)
        .await
        .map_err(|e| format!("enrich_get_starter_questions: {e}"))?
    {
        Some(starters) => {
            tracing::debug!(
                corpus_id = %corpus_id,
                returned = starters.len(),
                "enrich_get_starter_questions"
            );
            Ok(starters)
        }
        None => {
            // Say what is KNOWN — the host answered 404 — not what it is
            // taken to mean. This line used to assert "has no atlas", and
            // for the life of the wrong-port bug above that assertion was
            // false: the 404 was a route that did not exist on the
            // listener being asked. A trace that states a conclusion it
            // cannot see is worse than no trace, because it is the line
            // someone greps to rule this branch out.
            tracing::debug!(
                corpus_id = %corpus_id,
                "enrich_get_starter_questions: host answered 404 — no atlas for \
                 this corpus, or no such route on the listener asked; either \
                 way the UI falls back to excerpt starters"
            );
            Ok(Vec::new())
        }
    }
}

// ─── Command: mark_first_run_complete / is_first_run ─────────────────

/// Marker file under `~/.svrnmesh/first_run_complete`. Absence
/// signals "user has not finished the onboarding corpus flow yet".
/// Content is an ISO-8601 timestamp so a future version can reason
/// about when onboarding completed (e.g. re-onboarding after a major
/// schema change).
fn first_run_marker_path() -> PathBuf {
    sovereign_contracts::rebrand::data_dir().join("first_run_complete")
}

#[tauri::command]
pub async fn is_first_run() -> Result<bool, String> {
    // Dev: SOVEREIGN_DEV_FORCE_FIRST_RUN replays the corpus onboarding
    // as a first launch (in-memory; the marker on disk is untouched).
    if crate::dev_flags::force_first_run() {
        return Ok(true);
    }
    Ok(!first_run_marker_path().exists())
}

#[tauri::command]
pub async fn mark_first_run_complete() -> Result<(), String> {
    let path = first_run_marker_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let ts = chrono::Utc::now().to_rfc3339();
    std::fs::write(&path, &ts).map_err(|e| format!("writing {}: {e}", path.display()))?;
    tracing::info!(path = %path.display(), "first_run_complete marker written");
    Ok(())
}
