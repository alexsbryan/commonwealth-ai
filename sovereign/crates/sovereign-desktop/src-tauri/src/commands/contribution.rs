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

// ─── Contribution controls (W3) ──────────────────────────────
//
// Tauri wrappers around the daemon's `/internal/contribution/*` HTTP
// routes (commonwealth-api::routes_internal::mesh_admin). The Svelte
// settings panel + tray menu call these; the daemon process owns the
// authoritative state.
//
// Local DTOs mirror the daemon shapes byte-for-byte so the desktop
// crate doesn't depend on commonwealth-api just for these types
// (same pattern as MeshQuiesceState above).

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContributionStatus {
    pub ceiling: usize,
    pub in_flight: usize,
    pub paused_until: Option<i64>,
    pub pause_remaining_secs: Option<u64>,
    pub yield_peers_to_foreground: bool,
    pub yielding_secs_remaining: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LedgerEventDto {
    /// `node_id` is hex-string-encoded by serde (NodeId derives that)
    /// so the frontend can do friendly-name lookup without parsing.
    pub node_id: serde_json::Value,
    pub timestamp: u64,
    /// Wire kind: serialized with `#[serde(tag = "type")]` so the
    /// frontend can branch on `kind.type`.
    pub kind: serde_json::Value,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RecentContributionsResp {
    pub events: Vec<LedgerEventDto>,
}

#[tauri::command]
pub async fn get_contribution_status(
    state: State<'_, Arc<AppState>>,
) -> Result<ContributionStatus, String> {
    get_contribution_status_at(&state.internal_base_url()).await
}

/// HTTP implementation behind `get_contribution_status`, with the
/// daemon's internal base URL passed explicitly so the tray poller
/// (which holds an `AppHandle`, not a `tauri::State`) can call it.
pub(crate) async fn get_contribution_status_at(
    base_url: &str,
) -> Result<ContributionStatus, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let url = format!("{base_url}/internal/contribution/status");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET /internal/contribution/status: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/contribution/status returned {status}: {body}"
        ));
    }
    resp.json::<ContributionStatus>()
        .await
        .map_err(|e| format!("decode /internal/contribution/status: {e}"))
}

#[tauri::command]
pub async fn set_contribution_ceiling(
    state: State<'_, Arc<AppState>>,
    max: Option<usize>,
) -> Result<ContributionStatus, String> {
    set_contribution_ceiling_at(&state.internal_base_url(), max).await
}

/// HTTP implementation behind `set_contribution_ceiling`, with the
/// daemon's internal base URL passed explicitly. Split out so the
/// non-command boot/consent callers (which hold an `AppState` rather
/// than a `tauri::State`) can resolve the URL via
/// `AppState::internal_base_url()` and share the same request path.
pub(crate) async fn set_contribution_ceiling_at(
    base_url: &str,
    max: Option<usize>,
) -> Result<ContributionStatus, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let url = format!("{base_url}/internal/contribution/ceiling");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "max": max }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/contribution/ceiling: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/contribution/ceiling returned {status}: {body}"
        ));
    }
    resp.json::<ContributionStatus>()
        .await
        .map_err(|e| format!("decode /internal/contribution/ceiling: {e}"))
}

#[tauri::command]
pub async fn pause_contributions(
    state: State<'_, Arc<AppState>>,
    duration_secs: u64,
) -> Result<ContributionStatus, String> {
    pause_contributions_at(&state.internal_base_url(), duration_secs).await
}

/// HTTP implementation behind `pause_contributions`, with the daemon's
/// internal base URL passed explicitly so the tray menu (which holds
/// an `AppHandle`, not a `tauri::State`) can call it.
pub(crate) async fn pause_contributions_at(
    base_url: &str,
    duration_secs: u64,
) -> Result<ContributionStatus, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let url = format!("{base_url}/internal/contribution/pause");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "duration_secs": duration_secs }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/contribution/pause: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/contribution/pause returned {status}: {body}"
        ));
    }
    resp.json::<ContributionStatus>()
        .await
        .map_err(|e| format!("decode /internal/contribution/pause: {e}"))
}

#[tauri::command]
pub async fn resume_contributions(
    state: State<'_, Arc<AppState>>,
) -> Result<ContributionStatus, String> {
    resume_contributions_at(&state.internal_base_url()).await
}

/// HTTP implementation behind `resume_contributions`, with the daemon's
/// internal base URL passed explicitly so the tray menu (which holds
/// an `AppHandle`, not a `tauri::State`) can call it.
pub(crate) async fn resume_contributions_at(
    base_url: &str,
) -> Result<ContributionStatus, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let url = format!("{base_url}/internal/contribution/resume");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|e| format!("POST /internal/contribution/resume: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/contribution/resume returned {status}: {body}"
        ));
    }
    resp.json::<ContributionStatus>()
        .await
        .map_err(|e| format!("decode /internal/contribution/resume: {e}"))
}

#[tauri::command]
pub async fn get_recent_contributions(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
) -> Result<Vec<LedgerEventDto>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/contribution/recent");
    let mut req = client.get(&url);
    if let Some(n) = limit {
        req = req.query(&[("limit", n.to_string())]);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("GET /internal/contribution/recent: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/contribution/recent returned {status}: {body}"
        ));
    }
    resp.json::<RecentContributionsResp>()
        .await
        .map(|r| r.events)
        .map_err(|e| format!("decode /internal/contribution/recent: {e}"))
}

// ─── Activity ledger (Activity & Sharing surface) ────────────
//
// Three reads behind the "all on this machine" totals + feed:
//  - get_activity_summary  → daemon /internal/activity/summary
//    (embeddings served, chunks ingested/enriched, local serving,
//     plus this node's folded-in peer contribution).
//  - get_activity_recent   → daemon /internal/activity/recent feed.
//  - get_chat_activity     → the in-process Runtime's own chat usage,
//    derived from persisted message provenance (the daemon never sees
//    desktop chat, so this slice is read locally from the store).
// The first two return raw JSON; the Svelte side owns the typed shape.

#[tauri::command]
pub async fn get_activity_summary(
    state: State<'_, Arc<AppState>>,
    window_days: Option<u32>,
) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/activity/summary");
    let mut req = client.get(&url);
    if let Some(d) = window_days {
        req = req.query(&[("window_days", d.to_string())]);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("GET /internal/activity/summary: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/activity/summary returned {status}: {body}"
        ));
    }
    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| format!("decode /internal/activity/summary: {e}"))
}

#[tauri::command]
pub async fn get_activity_recent(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
) -> Result<Vec<serde_json::Value>, String> {
    #[derive(serde::Deserialize)]
    struct Resp {
        events: Vec<serde_json::Value>,
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/activity/recent");
    let mut req = client.get(&url);
    if let Some(n) = limit {
        req = req.query(&[("limit", n.to_string())]);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("GET /internal/activity/recent: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/activity/recent returned {status}: {body}"
        ));
    }
    resp.json::<Resp>()
        .await
        .map(|r| r.events)
        .map_err(|e| format!("decode /internal/activity/recent: {e}"))
}

#[tauri::command]
pub async fn get_chat_activity(
    state: State<'_, Arc<AppState>>,
    window_days: Option<u32>,
) -> Result<serde_json::Value, String> {
    let window_secs = (window_days.unwrap_or(7).max(1) as i64) * 86_400;
    let store = {
        let guard = state.sqlite_store.read().await;
        guard.as_ref().cloned()
    };
    let Some(store) = store else {
        return Err("chat store not ready".into());
    };
    let summary = store
        .summarize_chat_activity(window_secs)
        .await
        .map_err(|e| format!("summarize_chat_activity: {e}"))?;
    serde_json::to_value(summary).map_err(|e| format!("serialize chat activity: {e}"))
}

// ─── First-mesh consent (W4) ─────────────────────────────────
//
// One-time dialog shown after setup completes, before the user
// joins a multi-peer mesh: "Share idle GPU with the mesh?" The
// answer drives the daemon's peer-inflight ceiling and persists in
// DesktopConfig so we don't re-prompt on every launch.
//
// The Svelte side calls get_first_mesh_consent on boot; None means
// "show the modal". After the user decides, record_first_mesh_consent
// persists the choice AND applies the ceiling at the daemon, so the
// next peer-served inference request is gated correctly.

#[tauri::command]
pub async fn get_first_mesh_consent(
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
) -> Result<Option<crate::state::FirstMeshConsent>, String> {
    Ok(state.config.read().await.first_mesh_consent.clone())
}

#[tauri::command]
pub async fn record_first_mesh_consent(
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    share_gpu: bool,
) -> Result<crate::state::FirstMeshConsent, String> {
    // 1 concurrent peer request is the "Yes, share idle GPU" default
    // — matches the plan's 25% bucket. The user can lift this later
    // in Settings without re-prompting consent.
    let ceiling = if share_gpu { 1 } else { 0 };
    let recorded_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let decision = crate::state::FirstMeshConsent {
        share_gpu,
        ceiling,
        recorded_at_unix,
    };

    // Persist FIRST so even if the daemon call fails the decision
    // survives a restart (and we won't re-prompt the user).
    {
        let mut cfg = state.config.write().await;
        cfg.first_mesh_consent = Some(decision.clone());
        cfg.save()
            .map_err(|e| format!("save desktop config: {e}"))?;
    }

    // Apply the ceiling at the daemon. Best-effort: if the daemon
    // isn't reachable yet (early-boot race), the cfg already records
    // the user's intent and a follow-up apply_first_mesh_consent at
    // boot can re-issue. For v1 we just log + continue.
    if let Err(e) = set_contribution_ceiling_at(&state.internal_base_url(), Some(ceiling)).await {
        tracing::warn!(
            error = %e,
            ceiling,
            "consent recorded but daemon ceiling apply failed; \
             will be re-applied on next boot"
        );
    }

    Ok(decision)
}

// ─── Crash report (W6) ───────────────────────────────────────
//
// Bundles the latest supervisor-written crash log + redacted config
// into a single markdown file on Desktop, returns the project's
// GitHub Issues URL the frontend opens via tauri-plugin-shell. NO
// auto-upload — the user reads the file and attaches it to an issue
// they open. See crash_bundle.rs.

#[derive(Debug, serde::Serialize)]
pub struct CrashReportInfo {
    /// Absolute path of the report file on disk. UI shows this so
    /// the user can copy/open it.
    pub report_path: String,
    /// The project's GitHub Issues URL. Frontend passes this to
    /// `tauri-plugin-shell.open(url)`; the user attaches the report.
    pub issues_url: String,
}

#[tauri::command]
pub async fn prepare_crash_report() -> Result<CrashReportInfo, String> {
    let cfg = sovereign_core::setup_config::SetupConfig::load().ok();
    let app_version = env!("CARGO_PKG_VERSION");
    let data_dir = cfg
        .as_ref()
        .map(|c| c.data.dir.clone())
        .or_else(|| dirs::home_dir().map(|h| h.join(".sovereign")))
        .ok_or_else(|| "could not resolve data dir".to_string())?;
    let prepared = crate::crash_bundle::prepare_report(&data_dir, cfg.as_ref(), app_version)?;
    Ok(CrashReportInfo {
        report_path: prepared.report_path.to_string_lossy().into_owned(),
        issues_url: prepared.issues_url,
    })
}
