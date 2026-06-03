//! Cross-platform service registration for the sovereign daemon.
//!
//! On macOS, writes a launchd plist to `~/Library/LaunchAgents/` and
//! invokes `launchctl load`. On Linux, writes a systemd user unit to
//! `~/.config/systemd/user/` and invokes `systemctl --user
//! daemon-reload` + `enable --now`.
//!
//! Templates are bundled at compile time via `include_str!` — see
//! `contrib/launchd/` and `contrib/systemd/`. `{BINARY}` and `{HOME}`
//! placeholders are substituted with absolute paths.
//!
//! All install/uninstall operations are best-effort: failures return
//! `Err(String)` with a human-readable reason but never panic. Setup
//! should treat these as warnings rather than fatal errors — the user
//! can always start the daemon manually with `sovereign daemon run`.

use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
const LAUNCHD_TEMPLATE: &str = include_str!("../../../contrib/launchd/com.sovereign.daemon.plist");

#[cfg(target_os = "linux")]
const SYSTEMD_TEMPLATE: &str = include_str!("../../../contrib/systemd/sovereign.service");

/// Install and enable the sovereign daemon service for the current
/// user. `bin_path` must be absolute and point to the `sovereign`
/// binary that should be invoked by the service manager.
///
/// Returns `Ok(())` when the service is registered and started.
/// Returns `Err(String)` on any failure — setup should treat this as
/// a warning and continue.
pub fn install_service(bin_path: &Path) -> Result<(), String> {
    let bin_path = canonicalize_binary(bin_path)?;

    #[cfg(target_os = "macos")]
    {
        install_launchd(&bin_path)
    }

    #[cfg(target_os = "linux")]
    {
        install_systemd(&bin_path)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = bin_path;
        Err(format!(
            "service registration is not supported on this platform \
             (os={}). Run `sovereign daemon run` manually.",
            std::env::consts::OS
        ))
    }
}

/// Stop the running sovereign daemon without unregistering it from
/// the service manager. A subsequent `sovereign daemon restart` (or
/// `launchctl start`) will bring it back.
///
/// On macOS, sends SIGTERM via `launchctl stop`. Because the plist
/// sets `KeepAlive.SuccessfulExit = false`, a clean exit (status 0)
/// does NOT trigger an automatic restart, so the daemon stays down
/// until explicitly started again.
///
/// Returns `Ok(())` when the stop command was accepted. The process
/// may still be winding down — the daemon drains in-flight requests
/// before it exits.
pub fn stop_service() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let out = Command::new("launchctl")
            .args(["stop", "com.sovereign.daemon"])
            .output()
            .map_err(|e| format!("spawn launchctl: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("No such process") || stderr.contains("not running") {
                // Already stopped — treat as success.
                return Ok(());
            }
            // macOS launchctl returns exit 3 with empty stderr when the
            // service is not registered. That's the pidfile-only-install
            // case: there's no service to stop because the user never
            // ran `sovereign setup`. Treat as already-stopped so the
            // caller (`stop_daemon` → `restart_daemon`) doesn't bail
            // before starting a fresh daemon.
            if out.status.code() == Some(3) && stderr.trim().is_empty() {
                return Ok(());
            }
            return Err(format!(
                "launchctl stop com.sovereign.daemon failed: {}\n\
                 hint: if the daemon isn't registered, run `sovereign setup` first.",
                stderr.trim()
            ));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let out = Command::new("systemctl")
            .args(["--user", "stop", "sovereign.service"])
            .output()
            .map_err(|e| format!("spawn systemctl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "systemctl --user stop sovereign failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(format!(
            "service stop is not supported on this platform (os={}). \
             Send SIGTERM to the `sovereign daemon run` process manually.",
            std::env::consts::OS
        ))
    }
}

/// Stop + unregister the service. Idempotent — returns `Ok(())` if
/// the service was never installed.
pub fn uninstall_service() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        uninstall_launchd()
    }

    #[cfg(target_os = "linux")]
    {
        uninstall_systemd()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok(())
    }
}

/// Ensure the binary path is absolute and exists. Expanding
/// `current_exe()` into an absolute path is what `sovereign setup`
/// should pass in so the service manager doesn't need `PATH` lookup.
fn canonicalize_binary(bin_path: &Path) -> Result<PathBuf, String> {
    let abs = if bin_path.is_absolute() {
        bin_path.to_path_buf()
    } else {
        std::fs::canonicalize(bin_path)
            .map_err(|e| format!("cannot resolve binary path {}: {e}", bin_path.display()))?
    };
    if !abs.exists() {
        return Err(format!("binary not found at {}", abs.display()));
    }
    Ok(abs)
}

// ─── macOS (launchd) ──────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn launchd_plist_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "cannot resolve home directory".to_string())?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join("com.sovereign.daemon.plist"))
}

#[cfg(target_os = "macos")]
fn install_launchd(bin_path: &Path) -> Result<(), String> {
    let home = dirs::home_dir().ok_or_else(|| "cannot resolve home directory".to_string())?;
    let plist_path = launchd_plist_path()?;

    // Make sure logs directory exists — launchd refuses to start if
    // StandardOutPath's parent is missing.
    let logs_dir = home.join(".sovereign").join("logs");
    std::fs::create_dir_all(&logs_dir)
        .map_err(|e| format!("create {}: {e}", logs_dir.display()))?;

    let content = LAUNCHD_TEMPLATE
        .replace("{BINARY}", &bin_path.to_string_lossy())
        .replace("{HOME}", &home.to_string_lossy());

    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&plist_path, content)
        .map_err(|e| format!("write {}: {e}", plist_path.display()))?;

    // If the agent is already loaded (re-setup), unload first so load picks
    // up the fresh binary path. Ignore errors — might not be loaded yet.
    let _ = std::process::Command::new("launchctl")
        .args(["unload", plist_path.to_string_lossy().as_ref()])
        .output();

    let out = std::process::Command::new("launchctl")
        .args(["load", plist_path.to_string_lossy().as_ref()])
        .output()
        .map_err(|e| format!("launchctl load: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("launchctl load failed: {}", stderr.trim()));
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_launchd() -> Result<(), String> {
    let plist_path = launchd_plist_path()?;
    if !plist_path.exists() {
        return Ok(());
    }

    // Best-effort unload; failure is not fatal (might already be unloaded).
    let _ = std::process::Command::new("launchctl")
        .args(["unload", plist_path.to_string_lossy().as_ref()])
        .output();

    std::fs::remove_file(&plist_path)
        .map_err(|e| format!("remove {}: {e}", plist_path.display()))?;

    Ok(())
}

// ─── Linux (systemd --user) ────────────────────────────────────────

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> Result<PathBuf, String> {
    let config =
        dirs::config_dir().ok_or_else(|| "cannot resolve user config directory".to_string())?;
    Ok(config
        .join("systemd")
        .join("user")
        .join("sovereign.service"))
}

#[cfg(target_os = "linux")]
fn install_systemd(bin_path: &Path) -> Result<(), String> {
    let unit_path = systemd_unit_path()?;
    let content = SYSTEMD_TEMPLATE.replace("{BINARY}", &bin_path.to_string_lossy());

    if let Some(parent) = unit_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&unit_path, content)
        .map_err(|e| format!("write {}: {e}", unit_path.display()))?;

    // Reload the systemd user daemon so it picks up the new unit.
    let out = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()
        .map_err(|e| format!("systemctl daemon-reload: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("systemctl daemon-reload failed: {}", stderr.trim()));
    }

    let out = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "sovereign.service"])
        .output()
        .map_err(|e| format!("systemctl enable --now: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("systemctl enable --now failed: {}", stderr.trim()));
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_systemd() -> Result<(), String> {
    let unit_path = systemd_unit_path()?;
    if !unit_path.exists() {
        return Ok(());
    }

    // Disable + stop first so systemd forgets about it.
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", "sovereign.service"])
        .output();

    std::fs::remove_file(&unit_path).map_err(|e| format!("remove {}: {e}", unit_path.display()))?;

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();

    Ok(())
}
