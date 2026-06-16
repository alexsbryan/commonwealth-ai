// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-split from the former monolithic `commands.rs` (PR5). Tauri
//! command handlers grouped by concern; re-exported through
//! `commands/mod.rs` so `commands::<name>` paths in `main.rs`'s
//! `generate_handler!` stay valid.
#![allow(unused_imports)]
use super::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::io::AsyncWriteExt;

use crate::state::{self, AppState, DesktopConfig};

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
/// Returns immediately — performs only static checks and an optional
/// HTTP HEAD request to the source URL.
#[tauri::command]
pub async fn recipe_validate(
    recipe_path: String,
    offline: bool,
) -> Result<RecipeValidateResult, String> {
    let path = PathBuf::from(&recipe_path);
    let engine = recipe_stub_engine();
    let options = corpus_engine::TestOptions {
        sample_size: 0,
        embed: false,
        offline,
        ..Default::default()
    };

    let report = engine
        .test_recipe(&path, &options)
        .await
        .map_err(|e| e.to_string())?;

    Ok(RecipeValidateResult {
        passed: report.validation.errors.is_empty(),
        errors: report.validation.errors.clone(),
        warnings: report.warnings(),
        corpus_id: report.recipe_id.clone(),
        corpus_name: report.recipe_name.clone(),
        source_reachable: report.validation.source_reachable,
    })
}

/// Run the full recipe test harness: validate → acquire sample →
/// extract → chunk → write TEST_REPORT.md.
///
/// Embedding is not available in this code path — the embed phase is
/// always skipped. The report is written to `<recipe_dir>/TEST_REPORT.md`.
#[tauri::command]
pub async fn recipe_test(
    recipe_path: String,
    sample_size: usize,
    offline: bool,
) -> Result<RecipeTestResult, String> {
    let path = PathBuf::from(&recipe_path);
    let output_path = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("TEST_REPORT.md");

    let engine = recipe_stub_engine();
    let options = corpus_engine::TestOptions {
        sample_size,
        embed: false,
        offline,
        output: Some(output_path.clone()),
        ..Default::default()
    };

    let report = engine
        .test_recipe(&path, &options)
        .await
        .map_err(|e| e.to_string())?;

    let markdown = report.to_markdown();

    if let Err(e) = std::fs::write(&output_path, &markdown) {
        tracing::warn!(
            "Failed to write TEST_REPORT.md to {}: {e}",
            output_path.display()
        );
    }

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

    Ok(RecipeTestResult {
        passed: report.passed(),
        warnings: report.warnings(),
        errors: report.validation.errors.clone(),
        recipe_id: report.recipe_id.clone(),
        recipe_name: report.recipe_name.clone(),
        records_attempted,
        records_succeeded,
        extraction_rate,
        total_chunks,
        avg_chars,
        report_path: output_path.to_string_lossy().into_owned(),
        report_markdown: markdown,
    })
}

/// The deterministic verdict ladder, flattened for the UI. Mirrors the CLI's
/// rendered ladder (Acquire→Extract→Filter→Chunk→Index) but as structured data
/// the frontend renders as green/red/amber cards with expandable evidence.
#[derive(Serialize)]
pub struct HarnessRunCard {
    /// Roll-up: all stages pass → green; any fail → red. Warns never gate.
    pub green: bool,
    /// The full per-stage verdict ladder (serializable `HarnessRun`).
    pub run: sovereign_authoring_harness::HarnessRun,
    pub ran_at_unix: u64,
    /// Frozen-sample provenance — surfaced as a "❄ Frozen: N docs" chip.
    pub frozen_docs: usize,
    pub frozen_captured_at: i64,
    /// True when THIS call performed the one networked capture step — lets the
    /// UI say "froze N docs" the first time and "offline" thereafter.
    pub frozen_captured_now: bool,
}

/// Run the deterministic authoring harness over a frozen sample and return the
/// per-stage verdict ladder. Rungs 1–5 (Acquire→Extract→Filter→Chunk→Index) are
/// model-free + offline after the first run (the sample is captured once under
/// `~/.sovereign/harness/<recipe-id>/`, then byte-identical, I1).
///
/// `enrich` adds rung 6 — but it does NOT stand up a parallel enrichment path.
/// It REUSES the atoms the desktop's existing ingest/enrich flow already wrote
/// for this corpus (via the shared `state.corpus_engine` — the daemon-backed
/// OICP `InferenceProvider` seam) and only VERIFIES their integrity with
/// `verify_atoms_at`. Returns no rung-6 verdict when the corpus isn't enriched
/// yet — install/enrich it through the normal flow first.
#[tauri::command]
pub async fn recipe_run_harness(
    state: State<'_, Arc<AppState>>,
    recipe_path: String,
    sample_size: usize,
    enrich: bool,
) -> Result<HarnessRunCard, String> {
    use corpus_engine::harness::{capture, verify_atoms_at, FrozenSample, HarnessRunner};
    use sovereign_authoring_harness::{run_deterministic, Declaration};

    let path = PathBuf::from(&recipe_path);
    let recipe =
        corpus_engine::Recipe::from_file(&path).map_err(|e| format!("load recipe: {e}"))?;
    let engine = recipe_stub_engine();

    // Frozen sample under ~/.sovereign/harness/<recipe-id>/ — capture once
    // (network), iterate offline thereafter. Same store the CLI uses.
    let harness_root = dirs::home_dir()
        .ok_or_else(|| "no home directory".to_string())?
        .join(".sovereign")
        .join("harness")
        .join(&recipe.corpus.id);

    let mut frozen_captured_now = false;
    if !harness_root.join("capture.json").exists() {
        capture(&engine, &recipe, &harness_root, sample_size)
            .await
            .map_err(|e| format!("frozen-sample capture failed: {e}"))?;
        frozen_captured_now = true;
    }
    let frozen = FrozenSample::load(&harness_root)
        .map_err(|e| format!("load frozen sample: {e}"))?
        .ok_or_else(|| "no frozen sample found after capture".to_string())?;

    let work_dir = std::env::temp_dir().join(format!("harness-run-{}", recipe.corpus.id));
    let outputs = HarnessRunner::new(&engine, &recipe, &frozen)
        .run(&work_dir, sample_size)
        .await
        .map_err(|e| format!("harness run failed: {e}"))?;

    // Rung 6 (opt-in): verify the atoms the desktop's existing ingest/enrich
    // flow already produced for this corpus — REUSE the shared daemon-backed
    // `state.corpus_engine`, not a parallel enrichment pipeline. `None` (no
    // rung-6 verdict) when the corpus isn't enriched yet.
    let enrich_out = if enrich {
        let daemon_engine = state
            .corpus_engine
            .read()
            .await
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| "corpus engine not ready (is the daemon connected?)".to_string())?;
        verify_atoms_at(&daemon_engine.index_dir().join(&recipe.corpus.id))
            .await
            .map_err(|e| format!("enrich verify failed: {e}"))?
    } else {
        None
    };

    let run = run_deterministic(
        &frozen.manifest,
        &recipe,
        &outputs,
        enrich_out.as_ref(),
        &Declaration::default(),
    );

    Ok(HarnessRunCard {
        green: run.green(),
        frozen_docs: frozen.manifest.docs.len(),
        frozen_captured_at: frozen.manifest.captured_at,
        frozen_captured_now,
        ran_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        run,
    })
}

/// Build a `CorpusEngine` with a stub embed function for recipe testing.
/// The stub is never called because the embed phase is always disabled.
fn recipe_stub_engine() -> corpus_engine::CorpusEngine {
    let stub: corpus_engine::EmbedFn = std::sync::Arc::new(|_| {
        Box::pin(async { Ok(vec![0f32; corpus_engine::DEFAULT_EMBED_DIM]) })
    });
    let tmp = std::env::temp_dir().join("sovereign-recipe-test");
    corpus_engine::CorpusEngine::new(tmp.clone(), tmp, stub)
}

/// Kick off background installs for every corpus in the given tier.
/// Used by the setup wizard's "install tier" affordance.
pub(crate) async fn start_tier_installs(
    app_handle: &tauri::AppHandle,
    state: &Arc<AppState>,
    tier: &str,
) {
    let engine_guard = state.corpus_engine.read().await;
    let engine = match engine_guard.as_ref() {
        Some(e) => Arc::clone(e),
        None => {
            tracing::warn!("start_tier_installs: corpus engine not initialized");
            return;
        }
    };
    drop(engine_guard);

    let builtins = engine.builtin_corpora();
    for b in &builtins {
        if !tiers_for(&b.id).iter().any(|t| t == tier) {
            continue;
        }
        tracing::info!("Queuing corpus install for tier '{tier}': {}", b.id);
        // Reuse the install command's spawn-and-emit logic by calling it
        // directly. Each install runs in its own task; they don't block
        // each other but compete for download bandwidth.
        let app = app_handle.clone();
        let state_clone = Arc::clone(state);
        let cid = b.id.clone();
        // Synthesize a State<'_, Arc<AppState>> isn't possible here; just
        // duplicate the spawn pattern inline.
        tokio::spawn(async move {
            let engine_guard = state_clone.corpus_engine.read().await;
            let engine = match engine_guard.as_ref() {
                Some(e) => Arc::clone(e),
                None => return,
            };
            drop(engine_guard);

            let progress_cid = cid.clone();
            let progress_handle = app.clone();
            let progress_state = Arc::clone(&state_clone);
            let progress_cb: corpus_engine::ProgressCallback = Box::new(move |p| {
                let payload = ingest_progress_to_payload(&progress_cid, &p);
                if let Ok(mut map) = progress_state.install_progress.try_write() {
                    map.insert(payload.corpus_id.clone(), payload.clone());
                }
                let _ = progress_handle.emit("corpus-progress", payload);
            });

            let spec = corpus_engine::CorpusSpec::Builtin(cid.clone());
            if let Err(e) = engine.ingest(&spec, Some(progress_cb)).await {
                tracing::warn!("Tier install for '{cid}' failed: {e}");
            }
        });
    }
}
