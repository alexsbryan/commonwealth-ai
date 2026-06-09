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
/// Returns `Ok(Some(info))` if an update is available, `Ok(None)` if the
/// app is up to date OR the endpoint is unreachable (the latter is soft-
/// failed deliberately — see module docs). The frontend should render
/// a "you're up to date" toast on `None` and an upgrade banner on `Some`.
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
                date: update.date.map(|d| d.to_string()),
                body: update.body.clone(),
            }))
        }
        Ok(None) => {
            tracing::debug!("updater: up to date");
            Ok(None)
        }
        Err(e) => {
            // Soft-fail. Network blips, GitHub rate limits, and the
            // "you've cut zero releases yet" 404 from svrnme.sh all
            // surface here; none of them should produce a scary
            // "Update check failed" dialog. The user can retry.
            tracing::warn!(error = %e, "updater: check failed (soft-failed to None)");
            Ok(None)
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
