// SPDX-License-Identifier: AGPL-3.0-or-later
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
        BootstrapMode::Attach { client_port, .. } => *client_port,
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

// ─── Request types ───────────────────────────────────────────────
//
// Imported, not re-declared — the same rule the response types below
// already follow. This is the config the user's choices travel in, so
// it MUST be the daemon's own type.
//
// Until 2026-08-21 this was a hand-copied five-field `WatchedFolderConfigWire`
// (follow_symlinks, deletion_guard, sweep_interval_secs,
// soft_delete_grace_secs, exclude_globs). `src/lib/types.ts` declares ten
// fields NON-OPTIONAL and `WatchedFolderRegisterFlow.svelte` binds five of the
// missing ones to live controls, so serde dropped them on the way through this
// command and `RegisterRequest.config`'s per-field `#[serde(default)]` filled
// them back in with defaults on the daemon side. The user's sensitive toggle,
// sync-mode radio, OCR checkbox, additional-roots picker and enrichment choice
// were all inert, silently.
pub use sovereign_tools::local_corpus::config::{DeletionGuardConfig, WatchedFolderConfig};

// ─── Wire types ──────────────────────────────────────────────────
//
// Imported, not re-declared: these ARE the daemon's response types from
// `/internal/corpus/watch/*`, so a field rename on the server side is now a
// compile error here instead of a runtime deserialization failure. Until
// 2026-08-21 (nc-21) this file carried seven hand-copied mirrors that had
// already drifted — `ListEntry` was missing `sync_mode`, `sensitive` and
// `additional_roots_count`, and typed the nested payloads as
// `serde_json::Value`. The commands below only pass these through to the
// frontend; nothing here reads a field.
pub use sovereign_mesh::corpus_watch_http::{
    AckResponse, IncompleteJobsResponse, ListEntry, ListResponse, RegisterResponse, StateResponse,
    StatusResponse,
};

// ─── Commands ────────────────────────────────────────────────────

#[tauri::command]
pub async fn lc_watch_register(
    state: State<'_, Arc<AppState>>,
    path: PathBuf,
    display_name: Option<String>,
    config: Option<WatchedFolderConfig>,
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
pub async fn lc_watch_list(state: State<'_, Arc<AppState>>) -> Result<ListResponse, String> {
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

/// Folder-ingest v1 §3.5: trigger a Manual-mode sweep. The corpus
/// must already be registered with `sync_mode = "manual"`; this
/// command is a no-op (server returns 409) for Continuous corpora.
#[tauri::command]
pub async fn lc_watch_sync_now(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<AckResponse, String> {
    let url = format!(
        "{}/internal/corpus/watch/sync-now/{corpus_id}",
        base_url(&state)
    );
    post_json(&url, json!({})).await
}

/// Folder-ingest v1 §3.1: layer an additional root onto an existing
/// watched corpus. The next scheduler tick walks it.
#[tauri::command]
pub async fn lc_watch_add_root(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    path: String,
) -> Result<AckResponse, String> {
    let url = format!(
        "{}/internal/corpus/watch/{corpus_id}/roots",
        base_url(&state)
    );
    post_json(&url, json!({ "path": path })).await
}

/// Folder-ingest v1 §3.1: detach an additional root by index.
#[tauri::command]
pub async fn lc_watch_remove_root(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    idx: u32,
) -> Result<AckResponse, String> {
    let url = format!(
        "{}/internal/corpus/watch/{corpus_id}/roots/{idx}",
        base_url(&state)
    );
    delete_json(&url).await
}

/// Folder-ingest v1 §3.3: enable atlas enrichment on a watched
/// folder. Returns immediately with a job_id; the build runs in a
/// daemon-side subprocess. Progress events surface on the
/// `enrich://progress/<job_id>` Tauri channel.
#[tauri::command]
pub async fn lc_watch_enrich_enable(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    pipeline_id: String,
) -> Result<serde_json::Value, String> {
    let url = format!(
        "{}/internal/corpus/watch/{corpus_id}/enrich/enable",
        base_url(&state)
    );
    post_json(&url, json!({ "pipeline_id": pipeline_id })).await
}

/// Folder-ingest v1 §3.3: disable atlas enrichment. Cancels any
/// in-flight build, tears down the atlas dir, resets to Off.
#[tauri::command]
pub async fn lc_watch_enrich_disable(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<AckResponse, String> {
    let url = format!(
        "{}/internal/corpus/watch/{corpus_id}/enrich/disable",
        base_url(&state)
    );
    post_json(&url, json!({})).await
}

/// Folder-ingest v1 §3.3: rebuild the atlas using the
/// previously-configured pipeline.
#[tauri::command]
pub async fn lc_watch_enrich_rebuild(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<serde_json::Value, String> {
    let url = format!(
        "{}/internal/corpus/watch/{corpus_id}/enrich/rebuild",
        base_url(&state)
    );
    post_json(&url, json!({})).await
}

/// Folder-ingest v1 §3.7: per-folder glassbox digest. Heavier than
/// `lc_watch_state`; the desktop fetches this once when the user
/// opens the detail panel, not on every poll tick. Returns the
/// `DetailsResponse` shape from sovereign-mesh's
/// `corpus_watch_http`: format counts, skipped-by-extension,
/// failed-files, sync mode, sensitivity, enrichment status,
/// tombstones.
#[tauri::command]
pub async fn lc_watch_details(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<serde_json::Value, String> {
    let url = format!(
        "{}/internal/corpus/watch/details/{corpus_id}",
        base_url(&state)
    );
    get_json(&url).await
}

/// Folder-ingest v1 §3.7: per-document inspection digest. Returns
/// the `DocumentResponse` shape: file metadata, chunk count,
/// first chunk preview, atom contributions (empty until Phase E).
#[tauri::command]
pub async fn lc_watch_document(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    doc_id: String,
) -> Result<serde_json::Value, String> {
    // doc_id can contain slashes (relative path) and other URL-
    // hostile characters; percent-encode every byte that isn't
    // an unreserved path character per RFC 3986.
    let encoded = url_encode_segment(&doc_id);
    let url = format!(
        "{}/internal/corpus/watch/document/{corpus_id}/{encoded}",
        base_url(&state)
    );
    get_json(&url).await
}

fn url_encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

#[tauri::command]
pub async fn lc_watch_remove(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<AckResponse, String> {
    let url = format!("{}/internal/corpus/watch/{corpus_id}", base_url(&state));
    delete_json(&url).await
}

#[tauri::command]
pub async fn lc_watch_incomplete_jobs(
    state: State<'_, Arc<AppState>>,
) -> Result<IncompleteJobsResponse, String> {
    let url = format!("{}/internal/corpus/watch/incomplete-jobs", base_url(&state));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact payload `WatchedFolderRegisterFlow.svelte` builds, with
    /// every control moved OFF its default so a dropped field is visible
    /// as a wrong value rather than a coincidental match.
    fn register_flow_payload() -> serde_json::Value {
        json!({
            "follow_symlinks": true,
            "deletion_guard": {
                "absolute_threshold": 7,
                "fractional_threshold": 0.5,
                "enabled": false
            },
            "sweep_interval_secs": 900,
            "soft_delete_grace_secs": 172_800,
            "exclude_globs": ["*.tmp"],
            "with_ocr": true,
            "sync_mode": "manual",
            "sensitive": true,
            "additional_roots": [{ "path": "/tmp/extra", "added_at_unix": 1_787_000_000 }],
            "enrichment": { "kind": "off" }
        })
    }

    /// Every field the register flow sets must survive the Tauri command
    /// boundary — deserialize into the config type, then re-serialize into
    /// the HTTP body exactly as `lc_watch_register` does.
    ///
    /// This is the guard for the 2026-08-21 defect: the command took a
    /// hand-copied five-field mirror, so `with_ocr`, `sync_mode`,
    /// `sensitive`, `additional_roots` and `enrichment` were silently
    /// dropped here and then re-defaulted by `RegisterRequest`'s per-field
    /// `#[serde(default)]` on the daemon side. Against that mirror this
    /// test fails on all five; it cannot be satisfied by anything short of
    /// carrying the daemon's own type.
    #[test]
    fn register_config_survives_the_command_boundary() {
        let cfg: WatchedFolderConfig = serde_json::from_value(register_flow_payload())
            .expect("the register flow's payload deserializes into the daemon's config type");

        // What `lc_watch_register` actually puts on the wire.
        let body = json!({ "config": cfg });
        let sent = &body["config"];

        // The five fields the fork dropped.
        assert_eq!(sent["with_ocr"], serde_json::json!(true));
        assert_eq!(sent["sync_mode"], serde_json::json!("manual"));
        assert_eq!(sent["sensitive"], serde_json::json!(true));
        assert_eq!(
            sent["additional_roots"][0]["path"],
            serde_json::json!("/tmp/extra")
        );
        assert_eq!(sent["enrichment"]["kind"], serde_json::json!("off"));

        // The five it carried, so this test also pins the fork's own surface.
        assert_eq!(sent["follow_symlinks"], serde_json::json!(true));
        assert_eq!(sent["sweep_interval_secs"], serde_json::json!(900));
        assert_eq!(sent["soft_delete_grace_secs"], serde_json::json!(172_800));
        assert_eq!(sent["exclude_globs"][0], serde_json::json!("*.tmp"));
        assert_eq!(
            sent["deletion_guard"]["absolute_threshold"],
            serde_json::json!(7)
        );
    }

    /// `config: None` must still be the daemon's defaults, not a local
    /// re-statement of them. The fork carried its own `Default` impl with
    /// hand-copied constants (120s / 7d / absolute 100 / fractional 0.25);
    /// those now come from one place.
    #[test]
    fn absent_config_defaults_to_the_daemon_type() {
        let cfg = Option::<WatchedFolderConfig>::None.unwrap_or_default();
        let sent = json!(cfg);
        assert_eq!(sent["sync_mode"], serde_json::json!("continuous"));
        assert_eq!(sent["sensitive"], serde_json::json!(false));
        assert_eq!(sent["with_ocr"], serde_json::json!(false));
        assert_eq!(sent["enrichment"]["kind"], serde_json::json!("off"));
    }
}
