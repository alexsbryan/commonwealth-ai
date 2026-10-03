// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recovery affordance for the daemon this app talks to.
//!
//! There used to be two more commands here — `supervisor_reconnect` and
//! `supervisor_active` — wrapping a `Supervisor` the desktop held over a
//! daemon CHILD it had spawned. Both are gone with the supervisor
//! (sv-surface svt-2): the app starts no daemon, so it has none to wake.
//! What survives is the one move a client can honestly make about a daemon
//! it does not own — ask the OS service manager that DOES own it to restart
//! it.

/// Attach-mode recovery: best-effort restart of the externally-owned daemon
/// via the OS service manager (`launchctl kickstart` / `systemctl --user
/// restart`). Backs the attach-down banner raised by `crate::attach_watch`
/// (DAEMON_RESILIENCE.md P0.2).
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
