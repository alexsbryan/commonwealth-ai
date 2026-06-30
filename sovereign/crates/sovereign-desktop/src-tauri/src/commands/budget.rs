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

use crate::error::DesktopError;
use crate::state::{self, AppState, DesktopConfig};

// ─── Ingest budget + mesh quiesce ──────────────────────────────
//
// Both controls live behind `/internal/*` daemon endpoints; the
// Settings panel pokes them to share the machine over long ingests
// without forcing a restart.
//
//   - `throttle_factor` ∈ (0.0, 1.0]: 1.0 = full speed (default),
//     0.5 ≈ duty-cycle 50% (sleep equal to embed wall time after
//     each batch). Use the corpus pause route to fully stop a
//     corpus — 0.0 is rejected by the daemon.
//   - `mesh_quiesced` bool: when true, this node neither pulls
//     peer-assigned work nor dispatches its own queue. The
//     SOVEREIGN_DISABLE_AUTO_COLLAB env var seeds the same atomic
//     at boot, so flipping at runtime via this command is reversible
//     without a daemon restart.

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct IngestBudgetState {
    pub throttle_factor: f32,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct MeshQuiesceState {
    pub quiesced: bool,
}

#[tauri::command]
pub async fn get_ingest_budget(
    state: State<'_, Arc<AppState>>,
) -> Result<IngestBudgetState, DesktopError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/ingest/budget");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET /internal/ingest/budget: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(DesktopError::upstream(format!(
            "daemon /internal/ingest/budget returned {status}: {body}"
        )));
    }
    resp.json::<IngestBudgetState>()
        .await
        .map_err(|e| DesktopError::upstream(format!("decode /internal/ingest/budget: {e}")))
}

#[tauri::command]
pub async fn set_ingest_budget(
    state: State<'_, Arc<AppState>>,
    throttle_factor: f32,
) -> Result<IngestBudgetState, DesktopError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/ingest/budget");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "throttle_factor": throttle_factor }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/ingest/budget: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(DesktopError::upstream(format!(
            "daemon /internal/ingest/budget returned {status}: {body}"
        )));
    }
    resp.json::<IngestBudgetState>()
        .await
        .map_err(|e| DesktopError::upstream(format!("decode /internal/ingest/budget: {e}")))
}

#[tauri::command]
pub async fn get_mesh_quiesced(
    state: State<'_, Arc<AppState>>,
) -> Result<MeshQuiesceState, DesktopError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/mesh/quiesce");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET /internal/mesh/quiesce: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(DesktopError::upstream(format!(
            "daemon /internal/mesh/quiesce returned {status}: {body}"
        )));
    }
    resp.json::<MeshQuiesceState>()
        .await
        .map_err(|e| DesktopError::upstream(format!("decode /internal/mesh/quiesce: {e}")))
}

#[tauri::command]
pub async fn set_mesh_quiesced(
    state: State<'_, Arc<AppState>>,
    quiesced: bool,
) -> Result<MeshQuiesceState, DesktopError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/mesh/quiesce");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "quiesced": quiesced }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/mesh/quiesce: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(DesktopError::upstream(format!(
            "daemon /internal/mesh/quiesce returned {status}: {body}"
        )));
    }
    resp.json::<MeshQuiesceState>()
        .await
        .map_err(|e| DesktopError::upstream(format!("decode /internal/mesh/quiesce: {e}")))
}

// ── Storage budget ───────────────────────────────────────────
//
// Mirror of `commonwealth_api::routes_internal::mesh_admin::
// StorageBudgetState`. Defined here as a flat serde struct so the
// desktop crate doesn't depend on commonwealth-api types just for
// this round-trip — keeps the TypeScript bridge simple. The wire
// shape must stay byte-compatible with the daemon's response.

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct StorageBudgetState {
    /// `None` means "no budget configured — gossiped free_storage_gb
    /// reports raw free disk and nothing is clamped".
    pub budget_bytes: Option<u64>,
    /// Sum of `index_size_bytes` across installed corpora as of the
    /// last gossip tick.
    pub used_bytes: u64,
    /// Free disk across all mounted volumes, in bytes. Same number
    /// the gossip path reports (modulo budget clamp).
    pub free_disk_bytes: u64,
    /// Suggested baseline the desktop's "Use recommended" affordance
    /// applies. Computed server-side from current free disk so a
    /// user with 250 GiB free sees a 100 GiB recommendation while a
    /// user with 60 GiB free sees a 30 GiB one.
    pub recommended_bytes: u64,
}

/// Read the daemon's current budget snapshot. Also seeds two
/// stateful defaults so the user never sees a blank "no budget"
/// state without an explicit choice:
///
///  1. If the persisted `desktop.toml` has a budget, push it to the
///     daemon (covers daemon restart while desktop kept running, or
///     first read after the desktop survived a launchd-restarted
///     daemon).
///  2. If neither the config nor the daemon has a budget, apply the
///     daemon's recommended baseline AND persist it. The user can
///     still override after — this just ensures svrnmesh starts
///     out as a respectful tenant of the disk on first launch
///     instead of silently having no ceiling.
///
/// Returns whatever the daemon reports after these reconciliations.
#[tauri::command]
pub async fn get_storage_budget(
    state: State<'_, Arc<AppState>>,
) -> Result<StorageBudgetState, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/storage/budget");

    let fetch = || async {
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("GET /internal/storage/budget: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "daemon /internal/storage/budget returned {status}: {body}"
            ));
        }
        resp.json::<StorageBudgetState>()
            .await
            .map_err(|e| format!("decode /internal/storage/budget: {e}"))
    };

    let snapshot = fetch().await?;
    let persisted = state.config.read().await.storage_budget_bytes;

    // Reconciliation case 1: config has a value, daemon doesn't.
    // Push the persisted value forward.
    if let (Some(persisted_bytes), None) = (persisted, snapshot.budget_bytes) {
        let resp = client
            .post(&url)
            .json(&serde_json::json!({ "budget_bytes": persisted_bytes }))
            .send()
            .await
            .map_err(|e| format!("rehydrate storage budget: {e}"))?;
        if !resp.status().is_success() {
            tracing::warn!(
                status = %resp.status(),
                "get_storage_budget: rehydrate POST failed; the daemon's atomic stays at no-budget"
            );
        } else {
            return resp
                .json::<StorageBudgetState>()
                .await
                .map_err(|e| format!("decode rehydrate response: {e}"));
        }
    }

    // Reconciliation case 2: nobody has a value. Adopt the
    // recommended baseline AND persist it so the choice survives
    // restart. If the disk is too small for the recommendation to
    // be meaningful (under the AppState 1 GiB floor), skip — the
    // user will see the "Use recommended" affordance in Settings
    // and can apply it explicitly.
    if persisted.is_none() && snapshot.budget_bytes.is_none() {
        const MIN_BUDGET: u64 = 1_073_741_824;
        if snapshot.recommended_bytes >= MIN_BUDGET {
            let resp = client
                .post(&url)
                .json(&serde_json::json!({
                    "budget_bytes": snapshot.recommended_bytes
                }))
                .send()
                .await
                .map_err(|e| format!("seed recommended storage budget: {e}"))?;
            if resp.status().is_success() {
                let applied: StorageBudgetState = resp
                    .json()
                    .await
                    .map_err(|e| format!("decode seed response: {e}"))?;
                let mut cfg = state.config.write().await;
                cfg.storage_budget_bytes = applied.budget_bytes;
                if let Err(e) = cfg.save() {
                    tracing::warn!("get_storage_budget: seed persist failed: {e}");
                }
                tracing::info!(
                    budget_bytes = ?applied.budget_bytes,
                    free_disk_bytes = applied.free_disk_bytes,
                    "storage_budget: seeded recommended baseline on first launch"
                );
                return Ok(applied);
            }
            tracing::warn!(
                status = %resp.status(),
                "get_storage_budget: seed POST failed; user will see no-budget state"
            );
        }
    }

    Ok(snapshot)
}

/// Push a new budget to the daemon. `budget_bytes = None` clears the
/// budget. Also rewrites the persisted `desktop.toml` so the choice
/// survives a restart — the daemon's atomic is runtime state, the
/// config file is the source of truth on next boot.
#[tauri::command]
pub async fn set_storage_budget(
    state: State<'_, Arc<AppState>>,
    budget_bytes: Option<u64>,
) -> Result<StorageBudgetState, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/storage/budget");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "budget_bytes": budget_bytes }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/storage/budget: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/storage/budget returned {status}: {body}"
        ));
    }
    let applied: StorageBudgetState = resp
        .json()
        .await
        .map_err(|e| format!("decode /internal/storage/budget: {e}"))?;

    // Persist into desktop.toml. Best-effort: if the disk write
    // fails the daemon already has the new value in its atomic, so
    // the runtime experience is correct; only the next-boot default
    // would revert. Log and surface the error so the UI can show it.
    {
        let mut cfg = state.config.write().await;
        cfg.storage_budget_bytes = applied.budget_bytes;
        if let Err(e) = cfg.save() {
            tracing::warn!("set_storage_budget: config save failed: {e}");
            return Err(format!("daemon updated but config save failed: {e}"));
        }
    }

    Ok(applied)
}

/// Return health details for a single installed corpus (claim/relationship
/// counts, article profiles flag). Loaded on demand so `list_corpora` stays
/// fast — the frontend calls this only when the user expands the detail panel.
#[tauri::command]
pub async fn get_corpus_health(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Option<CorpusHealthDetail>, String> {
    let engine_guard = state.corpus_engine.read().await;
    let engine = match engine_guard.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Ok(None),
    };
    drop(engine_guard);

    let index = match engine.open_index_for_corpus(&corpus_id).await {
        Ok(idx) => idx,
        Err(_) => return Ok(None),
    };

    // Count skeleton parse failures from the NDJSON log file.
    let failures_path = index.path().join("_skeleton_failures.ndjson");
    let parse_failure_count = if failures_path.exists() {
        std::fs::read_to_string(&failures_path)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count() as u64)
            .unwrap_or(0)
    } else {
        0
    };

    // Check if a field skeleton exists (indicates enrichment ran).
    let has_skeleton = index.load_field_skeleton().ok().flatten().is_some();
    let skeleton_questions = if has_skeleton {
        index
            .load_field_skeleton()
            .ok()
            .flatten()
            .map(|s| s.canonical_questions.len() as u64)
            .unwrap_or(0)
    } else {
        0
    };

    Ok(Some(CorpusHealthDetail {
        corpus_id: corpus_id.clone(),
        claims_count: skeleton_questions,
        relationships_count: 0,
        has_article_profiles: has_skeleton,
        parse_failure_count,
    }))
}

/// Re-parse stored skeleton extraction failures using the improved repair
/// parser (unquoted-string fix, truncation repair, quality filter).
/// Does not re-run inference — only the saved raw responses are re-processed.
/// Salvaged questions are merged into the existing field_skeleton.json.
/// Returns the number of newly recovered questions.
#[tauri::command]
pub async fn retry_enrichment_failures(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<u64, String> {
    let engine_guard = state.corpus_engine.read().await;
    let engine = match engine_guard.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    drop(engine_guard);

    let index = engine
        .open_index_for_corpus(&corpus_id)
        .await
        .map_err(|e| format!("Failed to open index for '{corpus_id}': {e}"))?;

    let (salvaged, still_failed) = corpus_engine::reprocess_skeleton_failures(&index)
        .map_err(|e| format!("Reprocessing failed: {e}"))?;

    tracing::info!(
        corpus_id = %corpus_id,
        salvaged = salvaged,
        still_failed = still_failed,
        "Skeleton failure reprocessing complete"
    );

    Ok(salvaged as u64)
}
