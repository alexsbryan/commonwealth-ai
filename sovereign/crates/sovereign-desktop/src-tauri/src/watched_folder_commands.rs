//! Tauri command surface for watched-folder corpora.
//!
//! Mirrors the daemon's `/internal/corpus/watch/*` HTTP routes (see
//! `sovereign-mesh::corpus_watch_http`). Both Attach and Local modes
//! work through the same HTTP path because the desktop's embedded
//! daemon (Local mode) installs the same router as the standalone
//! `sovereign daemon` (Attach mode) — both bind 127.0.0.1:9741 by
//! convention.
//!
//! Why HTTP-proxy instead of direct manager calls: the daemon owns
//! the `WatchedFolderRegistry` + scheduler. Going through HTTP keeps
//! the desktop a thin client over a single source of truth, the same
//! way `mesh_commands` does for mesh state.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::State;

use crate::state::AppState;

const DEFAULT_CLIENT_PORT: u16 = 9741;

fn daemon_port(state: &AppState) -> u16 {
    // BootstrapMode::Attach carries the port the standalone daemon
    // bound; Local mode binds 9741 by convention. Both routes hit
    // the same router.
    use crate::bootstrap::BootstrapMode;
    match &state.bootstrap_mode {
        BootstrapMode::Attach { client_port } => *client_port,
        BootstrapMode::Local { .. } => DEFAULT_CLIENT_PORT,
    }
}

fn base_url(state: &AppState) -> String {
    format!("http://127.0.0.1:{}", daemon_port(state))
}

fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client builds")
}

// ─── Wire types — Deserialize-only mirrors of the daemon DTOs ────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedFolderConfigWire {
    #[serde(default = "default_follow_symlinks")]
    pub follow_symlinks: bool,
    #[serde(default)]
    pub deletion_guard: DeletionGuardConfigWire,
    #[serde(default = "default_sweep_interval")]
    pub sweep_interval_secs: u64,
    #[serde(default = "default_grace")]
    pub soft_delete_grace_secs: u64,
    #[serde(default)]
    pub exclude_globs: Vec<String>,
}

impl Default for WatchedFolderConfigWire {
    fn default() -> Self {
        Self {
            follow_symlinks: false,
            deletion_guard: DeletionGuardConfigWire::default(),
            sweep_interval_secs: 120,
            soft_delete_grace_secs: 7 * 86_400,
            exclude_globs: Vec::new(),
        }
    }
}

fn default_follow_symlinks() -> bool { false }
fn default_sweep_interval() -> u64 { 120 }
fn default_grace() -> u64 { 7 * 86_400 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletionGuardConfigWire {
    pub absolute_threshold: usize,
    pub fractional_threshold: f32,
    pub enabled: bool,
}

impl Default for DeletionGuardConfigWire {
    fn default() -> Self {
        Self {
            absolute_threshold: 100,
            fractional_threshold: 0.25,
            enabled: true,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RegisterResponse {
    pub corpus_id: String,
    pub display_name: String,
    pub initial_sweep: serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ListResponse {
    pub corpora: Vec<ListEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ListEntry {
    pub corpus_id: String,
    pub display_name: String,
    pub root_path: PathBuf,
    pub status: serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct StatusResponse {
    pub corpus_id: String,
    pub status: serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct StateResponse {
    pub corpus_id: String,
    pub status: serde_json::Value,
    pub skipped_by_extension: std::collections::HashMap<String, usize>,
    pub failed_files: Vec<serde_json::Value>,
    pub tombstones: usize,
    pub live_entries: usize,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AckResponse {
    pub corpus_id: String,
    pub ok: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IncompleteJobsResponse {
    pub jobs: Vec<serde_json::Value>,
}

// ─── Commands ────────────────────────────────────────────────────

#[tauri::command]
pub async fn lc_watch_register(
    state: State<'_, Arc<AppState>>,
    path: PathBuf,
    display_name: Option<String>,
    config: Option<WatchedFolderConfigWire>,
    sync_initial: Option<bool>,
) -> Result<RegisterResponse, String> {
    let body = json!({
        "path": path,
        "display_name": display_name,
        "config": config.unwrap_or_default(),
        "sync_initial": sync_initial.unwrap_or(false),
    });
    let url = format!("{}/internal/corpus/watch/register", base_url(&state));
    post_json(&url, body).await
}

#[tauri::command]
pub async fn lc_watch_list(
    state: State<'_, Arc<AppState>>,
) -> Result<ListResponse, String> {
    let url = format!("{}/internal/corpus/watch/list", base_url(&state));
    get_json(&url).await
}

#[tauri::command]
pub async fn lc_watch_status(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<StatusResponse, String> {
    let url = format!(
        "{}/internal/corpus/watch/status/{corpus_id}",
        base_url(&state)
    );
    get_json(&url).await
}

#[tauri::command]
pub async fn lc_watch_state(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<StateResponse, String> {
    let url = format!(
        "{}/internal/corpus/watch/state/{corpus_id}",
        base_url(&state)
    );
    get_json(&url).await
}

#[tauri::command]
pub async fn lc_watch_pause(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    reason: Option<String>,
) -> Result<AckResponse, String> {
    let url = format!(
        "{}/internal/corpus/watch/pause/{corpus_id}",
        base_url(&state)
    );
    post_json(&url, json!({ "reason": reason })).await
}

#[tauri::command]
pub async fn lc_watch_resume(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<AckResponse, String> {
    let url = format!(
        "{}/internal/corpus/watch/resume/{corpus_id}",
        base_url(&state)
    );
    post_json(&url, json!({})).await
}

#[tauri::command]
pub async fn lc_watch_confirm_deletion(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<AckResponse, String> {
    let url = format!(
        "{}/internal/corpus/watch/confirm-deletion/{corpus_id}",
        base_url(&state)
    );
    post_json(&url, json!({})).await
}

#[tauri::command]
pub async fn lc_watch_remove(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<AckResponse, String> {
    let url = format!(
        "{}/internal/corpus/watch/{corpus_id}",
        base_url(&state)
    );
    delete_json(&url).await
}

#[tauri::command]
pub async fn lc_watch_incomplete_jobs(
    state: State<'_, Arc<AppState>>,
) -> Result<IncompleteJobsResponse, String> {
    let url = format!(
        "{}/internal/corpus/watch/incomplete-jobs",
        base_url(&state)
    );
    get_json(&url).await
}

// ─── HTTP helpers ────────────────────────────────────────────────

async fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, String> {
    let resp = build_client()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("HTTP request to {url}: {e}"))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("read response from {url}: {e}"))?;
    if !status.is_success() {
        let body = String::from_utf8_lossy(&bytes);
        return Err(format!("daemon rejected the request ({status}): {body}"));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| format!("daemon returned an unparseable response from {url}: {e}"))
}

async fn post_json<T: serde::de::DeserializeOwned>(
    url: &str,
    body: serde_json::Value,
) -> Result<T, String> {
    let resp = build_client()
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("HTTP request to {url}: {e}"))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("read response from {url}: {e}"))?;
    if !status.is_success() {
        let body = String::from_utf8_lossy(&bytes);
        return Err(format!("daemon rejected the request ({status}): {body}"));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| format!("daemon returned an unparseable response from {url}: {e}"))
}

async fn delete_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, String> {
    let resp = build_client()
        .delete(url)
        .send()
        .await
        .map_err(|e| format!("HTTP request to {url}: {e}"))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("read response from {url}: {e}"))?;
    if !status.is_success() {
        let body = String::from_utf8_lossy(&bytes);
        return Err(format!("daemon rejected the request ({status}): {body}"));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| format!("daemon returned an unparseable response from {url}: {e}"))
}
