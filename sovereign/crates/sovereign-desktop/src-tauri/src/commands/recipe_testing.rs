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

/// Build a `CorpusEngine` with a stub embed function for recipe testing.
/// The stub is never called because the embed phase is always disabled.
fn recipe_stub_engine() -> corpus_engine::CorpusEngine {
    let stub: corpus_engine::EmbedFn =
        std::sync::Arc::new(|_| Box::pin(async { Ok(vec![0f32; 768]) }));
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
