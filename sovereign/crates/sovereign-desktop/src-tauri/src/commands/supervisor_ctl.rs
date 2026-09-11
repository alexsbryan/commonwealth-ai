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
        Some(daemon) => Ok(daemon.supervisor.request_reconnect()),
        None => {
            Err("no daemon supervisor in this session (in-process or attached daemon)".to_string())
        }
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
/// `systemctl --user restart`). Backs the attach-down banner raised by
/// `crate::attach_watch` (DAEMON_RESILIENCE.md P0.2).
///
/// The `Err` is the banner's text, so it is written to be read by a
/// person: when no service manager owns the daemon — or this platform
/// has no backend the app can call — the message names that fact and the
/// command that fixes it (`kickstart_daemon`'s refusals, ARCH principle
/// 6). It is never a raw `launchctl`/`systemctl` line, and it never
/// becomes a spawn: the app does not manage a daemon's lifecycle
/// (sv-surface `sv-no-daemon-management`), so "there is nothing here to
/// restart" is an answer, not a gap to fill.
#[tauri::command]
pub async fn attach_restart_daemon() -> Result<(), String> {
    tokio::task::spawn_blocking(super::config_setup::kickstart_daemon)
        .await
        .map_err(|e| format!("restart task failed: {e}"))?
}
