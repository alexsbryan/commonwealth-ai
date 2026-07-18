// SPDX-License-Identifier: AGPL-3.0-or-later
//! Supervisor control commands (DAEMON_RESILIENCE.md P0.2).
//!
//! The ReconnectBanner's button previously called nothing — there was
//! no command wrapping `Supervisor::request_reconnect`, so a
//! crash-loop-latched daemon could only be revived by restarting the
//! whole app. These commands are the missing Rust half.

use std::sync::Arc;

use tauri::State;

use crate::state::AppState;

/// Wake a `Failed`-latched (or restarting) supervisor for another
/// spawn attempt. Returns `true` when a reconnect was actually
/// requested; `Err` when this session runs no supervisor (in-process
/// or Attach mode) so the frontend can say so instead of spinning.
#[tauri::command]
pub async fn supervisor_reconnect(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    let guard = state.supervisor.read().await;
    match guard.as_ref() {
        Some(sup) => Ok(sup.request_reconnect()),
        None => Err(
            "no daemon supervisor in this session (in-process or attached daemon)".to_string(),
        ),
    }
}

/// Whether this session runs a supervised child daemon. Lets the
/// frontend decide which recovery affordance to render (reconnect
/// button vs. "restart the app" guidance).
#[tauri::command]
pub async fn supervisor_active(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.supervisor.read().await.is_some())
}

/// Attach-mode recovery: best-effort restart of the EXTERNALLY-owned
/// daemon via the OS service manager (`launchctl kickstart` /
/// `systemctl --user restart`). Errors when no service is registered —
/// the banner then tells the user to run `svrn daemon restart`
/// themselves. Backs the attach-down banner raised by
/// `crate::attach_watch` (DAEMON_RESILIENCE.md P0.2).
#[tauri::command]
pub async fn attach_restart_daemon() -> Result<(), String> {
    tokio::task::spawn_blocking(super::config_setup::kickstart_daemon)
        .await
        .map_err(|e| format!("restart task failed: {e}"))?
}
