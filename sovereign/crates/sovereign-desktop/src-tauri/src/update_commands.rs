// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri commands that wrap `tauri-plugin-updater`.
//!
//! Two surfaces:
//!
//! - `check_for_update`  -> the UI's "Check for updates" button. Soft-fails
//!   to `None` on any endpoint glitch so users see "you're up to date"
//!   rather than a scary stack trace when the updater backend hiccups.
//!
//! - `install_update`    -> downloads + verifies + installs + restarts.
//!   Only callable after `check_for_update` returned `Some`.
//!
//! The endpoint + signing pubkey live in `tauri.conf.json` under
//! `plugins.updater`. See RELEASING.md for the operational flow.

use serde::Serialize;
use tauri::{AppHandle, Runtime};
use tauri_plugin_updater::UpdaterExt;

#[derive(Debug, Serialize)]
pub struct UpdateInfo {
    /// Version available on the server (e.g. "0.2.0").
    pub version: String,
    /// Version the running app reports (e.g. "0.1.0").
    pub current_version: String,
    /// ISO-8601 publish date of the release, if the server provides one.
    pub date: Option<String>,
    /// Release notes from the server. Already stripped of markdown
    /// by the manifest endpoint; safe to display as plain text.
    pub body: Option<String>,
}

/// Check the configured updater endpoint for a newer version.
///
/// Three distinct outcomes, deliberately NOT conflated (see the `Err` arm):
///   - `Ok(Some(info))` — an update is available; frontend shows the banner.
///   - `Ok(None)`       — genuinely up to date (endpoint returned 204);
///                        frontend shows a "you're up to date" toast.
///   - `Err(msg)`       — the check FAILED (offline, endpoint 4xx/5xx, bad
///                        manifest); frontend shows a calm, retryable notice.
///                        This is NOT the same as being current.
#[tauri::command]
pub async fn check_for_update<R: Runtime>(app: AppHandle<R>) -> Result<Option<UpdateInfo>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(update)) => {
            tracing::info!(
                version = %update.version,
                current = %update.current_version,
                "updater: update available"
            );
            Ok(Some(UpdateInfo {
                version: update.version.clone(),
                current_version: update.current_version.clone(),
                // Emit RFC3339 (JS `new Date()` parses it). `update.date` is a
                // time::OffsetDateTime whose Display (`to_string()`) is
                // `2026-07-15 10:45:13.0 +00:00:00` — space-separated, NOT
                // ISO-8601 — which the frontend rendered as "Invalid Date".
                // Convert via unix timestamp using chrono (already a dep) so we
                // don't pull time's `formatting` feature.
                date: update.date.and_then(|d| {
                    chrono::DateTime::from_timestamp(d.unix_timestamp(), d.nanosecond())
                        .map(|dt| dt.to_rfc3339())
                }),
                body: update.body.clone(),
            }))
        }
        Ok(None) => {
            tracing::debug!("updater: up to date");
            Ok(None)
        }
        Err(e) => {
            // Surface the failure — do NOT collapse it into `Ok(None)`.
            //
            // This command previously soft-failed every error to "up to date"
            // to avoid scary dialogs. That masking hid THREE real updater bugs
            // for weeks (2026-07-15): an OS-only-target 400, a wrong-version
            // pick, and a bad date — all invisible because a broken check was
            // indistinguishable from being current. A failed check is NOT the
            // same as being up to date, and the glassbox principle says the
            // user (and our logs) must be able to tell them apart.
            //
            // We keep it non-scary at the UI layer (a calm, retryable notice —
            // see UpdatesSection.svelte), and log the technical detail here
            // while returning a plain-language message. `check` is only ever
            // triggered by an explicit "Check for updates" click (no silent
            // auto-poll), so surfacing the failure is exactly what the user
            // asked for.
            tracing::warn!(error = %e, "updater: check failed");
            Err(format!(
                "Couldn't reach the update service. Check your connection and try again. ({e})"
            ))
        }
    }
}

/// Download, verify, install, and restart into the available update.
///
/// Errors propagate to the frontend as strings; the UI should surface a
/// dialog and offer to retry. Restart is unconditional on successful
/// install — there is no "install on next launch" path in v1.
#[tauri::command]
pub async fn install_update<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no update available".to_string())?;

    let target_version = update.version.clone();
    tracing::info!(version = %target_version, "updater: downloading");

    let mut downloaded: u64 = 0;
    update
        .download_and_install(
            move |chunk_len, content_len| {
                downloaded += chunk_len as u64;
                tracing::debug!(
                    downloaded,
                    content_len = content_len.unwrap_or(0),
                    "updater: chunk"
                );
            },
            || {
                tracing::info!("updater: download complete, installing");
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    tracing::info!(version = %target_version, "updater: installed, restarting");
    app.restart();
}
