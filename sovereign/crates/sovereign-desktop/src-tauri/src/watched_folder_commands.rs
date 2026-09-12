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

// ─── Wire types: none named here ─────────────────────────────────
//
// Every command below is a PASS-THROUGH: the request body the webview
// hands over goes to the route, the route's answer goes back to the
// webview, and nothing in this file reads a field of either. So the
// types cross as `serde_json::Value` — the daemon's own bytes, forwarded —
// rather than as the route's Rust types, which are
// `sovereign_mesh::corpus_watch_http`'s and close over
// `sovereign_tools::local_corpus` (`WatchedFolderStatus`, `FailedFile`,
// `WatchedIncompleteJob`, `WatchedFolderConfig`). Naming them here cost a
// `sovereign-desktop -> sovereign-mesh` layer edge, and a thin client
// does not link the daemon to forward its answer (sv-surface svt-3).
//
// This is NOT the hand-copied-mirror shape that drifted before 2026-08-21
// (seven local structs, `ListEntry` missing three fields, the register
// config a five-field twin that silently dropped the user's sensitive
// toggle, sync mode, OCR choice, extra roots and enrichment choice on the
// way through). A forwarded `Value` carries every field the webview sent
// and every field the route answered; there is no second declaration to
// fall behind. The Svelte-facing shape (`src/lib/types.ts`) is unchanged:
// it was always the route's bytes.
//
// `config` on register: `None` OMITS the key, and `RegisterRequest.config`
// is `#[serde(default)]` on the daemon — the same `WatchedFolderConfig::
// default()` the old `config.unwrap_or_default()` serialised client-side.
// An explicit `null` would NOT do: `serde(default)` fills an absent key,
// not a null one.

// ─── Commands ────────────────────────────────────────────────────

/// The `POST /internal/corpus/watch/register` body, exactly as sent. A
/// function so the test below exercises the same code the command runs.
fn register_body(
    path: PathBuf,
    display_name: Option<String>,
    config: Option<serde_json::Value>,
    sync_initial: Option<bool>,
) -> serde_json::Value {
    let mut body = json!({
        "path": path,
        "display_name": display_name,
        "sync_initial": sync_initial.unwrap_or(false),
    });
    if let Some(config) = config {
        body["config"] = config;
    }
    body
}

#[tauri::command]
pub async fn lc_watch_register(
    state: State<'_, Arc<AppState>>,
    path: PathBuf,
    display_name: Option<String>,
    config: Option<serde_json::Value>,
    sync_initial: Option<bool>,
) -> Result<serde_json::Value, String> {
    let body = register_body(path, display_name, config, sync_initial);
    let url = format!("{}/internal/corpus/watch/register", base_url(&state));
    post_json(&url, body).await
}

#[tauri::command]
pub async fn lc_watch_list(state: State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    let url = format!("{}/internal/corpus/watch/list", base_url(&state));
    get_json(&url).await
}

#[tauri::command]
pub async fn lc_watch_status(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<serde_json::Value, String> {
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
) -> Result<serde_json::Value, String> {
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
) -> Result<serde_json::Value, String> {
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
) -> Result<serde_json::Value, String> {
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
) -> Result<serde_json::Value, String> {
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
) -> Result<serde_json::Value, String> {
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
) -> Result<serde_json::Value, String> {
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
) -> Result<serde_json::Value, String> {
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
) -> Result<serde_json::Value, String> {
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
) -> Result<serde_json::Value, String> {
    let url = format!("{}/internal/corpus/watch/{corpus_id}", base_url(&state));
    delete_json(&url).await
}

#[tauri::command]
pub async fn lc_watch_incomplete_jobs(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
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
    /// boundary — the body `lc_watch_register` sends carries the webview's
    /// `config` byte-for-byte.
    ///
    /// This is the guard for the 2026-08-21 defect: the command took a
    /// hand-copied five-field mirror, so `with_ocr`, `sync_mode`,
    /// `sensitive`, `additional_roots` and `enrichment` were silently
    /// dropped here and then re-defaulted by `RegisterRequest`'s per-field
    /// `#[serde(default)]` on the daemon side. Against that mirror this
    /// test fails on all five; against a forwarded `Value` it cannot fail
    /// on ANY field, present or future, which is the point of forwarding.
    #[test]
    fn register_config_survives_the_command_boundary() {
        let payload = register_flow_payload();
        let body = register_body(
            PathBuf::from("/tmp/root"),
            Some("Root".into()),
            Some(payload.clone()),
            Some(false),
        );
        let sent = &body["config"];
        assert_eq!(
            sent, &payload,
            "the body must carry the webview's config unchanged"
        );

        // The five fields the fork dropped, named so a regression reads.
        assert_eq!(sent["with_ocr"], serde_json::json!(true));
        assert_eq!(sent["sync_mode"], serde_json::json!("manual"));
        assert_eq!(sent["sensitive"], serde_json::json!(true));
        assert_eq!(
            sent["additional_roots"][0]["path"],
            serde_json::json!("/tmp/extra")
        );
        assert_eq!(sent["enrichment"]["kind"], serde_json::json!("off"));
    }

    /// `config: None` must be the DAEMON's defaults, not a local
    /// re-statement of them. Structurally: the key is absent from the
    /// body, so `RegisterRequest.config`'s `#[serde(default)]` supplies
    /// `WatchedFolderConfig::default()` on the daemon — the one place those
    /// constants live. An explicit `null` would be a 422, not a default,
    /// which is why this pins ABSENCE and not a null.
    #[test]
    fn absent_config_is_omitted_so_the_daemon_defaults_it() {
        let body = register_body(PathBuf::from("/tmp/root"), None, None, None);
        assert!(
            body.get("config").is_none(),
            "config must be absent, not null: {body}"
        );
        assert_eq!(body["sync_initial"], serde_json::json!(false));
    }
}
