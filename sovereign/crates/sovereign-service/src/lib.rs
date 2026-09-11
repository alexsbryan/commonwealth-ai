// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cross-platform service registration for the svrn daemon — the one
//! decider for "is this daemon owned by the operating system, and how do I
//! ask it to start, stop or restart".
//!
//! # Why this is a crate and not a module in the daemon
//!
//! sv-surface's `sv-no-daemon-management` bar: the desktop must not be able
//! to BECOME a daemon, which means it must not link one. But it does need to
//! register one — the app installs the service at setup instead of spawning
//! a child of itself. Those two facts only fit together if registration
//! lives somewhere neither the daemon nor a surface owns.
//!
//! Hence the dependency list: `sovereign-contracts` for the data-root SSOT,
//! `dirs` for the user directories, `libc` on unix for the launchd domain's
//! uid. Nothing that can serve a turn, hold a store or assemble a Runtime. A
//! crate a thin client can depend on stops being one the moment it pulls in
//! the thing it was extracted to avoid, so additions here are reviewed
//! against that sentence rather than against convenience.
//!
//! # The three backends
//!
//! Every one is PER-USER and needs no elevation, because the daemon serves
//! one person's data root and a machine-wide service would be the wrong
//! grain:
//!
//! - **macOS** — a launchd LaunchAgent in `~/Library/LaunchAgents/`, loaded
//!   with `launchctl`.
//! - **Linux** — a systemd user unit in `~/.config/systemd/user/`, enabled
//!   with `systemctl --user`, plus `loginctl enable-linger` so the user
//!   manager survives logout and starts at boot (see `ensure_linger` —
//!   without it a user unit means "at login", not "at boot", and a
//!   headless node is absent for exactly the window it was installed to
//!   cover).
//! - **Windows** — a Scheduled Task registered with `schtasks /Create /XML`.
//!   Not a Windows Service: the SCM requires administrator rights and
//!   registers machine-wide, which is the wrong grain twice over. A logon-
//!   triggered task is the per-user analogue of the other two, and Task
//!   Scheduler's own restart policy is the analogue of `KeepAlive` /
//!   `Restart=on-failure`.
//!
//! Templates are bundled at compile time via `include_str!` — see
//! `contrib/launchd/` and `contrib/systemd/`. `{BINARY}` and `{HOME}`
//! placeholders are substituted with absolute paths.
//!
//! All install/uninstall operations are best-effort: failures return
//! `Err(String)` with a human-readable reason but never panic. Setup
//! should treat these as warnings rather than fatal errors — the user
//! can always start the daemon manually with `svrn daemon run`.

use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
const LAUNCHD_TEMPLATE: &str = include_str!("../../../contrib/launchd/com.svrnmesh.daemon.plist");

#[cfg(target_os = "linux")]
const SYSTEMD_TEMPLATE: &str = include_str!("../../../contrib/systemd/svrnmesh.service");

#[cfg(target_os = "windows")]
const SCHTASKS_TEMPLATE: &str = include_str!("../../../contrib/windows/SvrnmeshDaemon.xml");

/// The Scheduled Task name on Windows — the analogue of the launchd label
/// and the systemd unit name, and it must match `CANDIDATE_SERVICES`.
#[cfg(target_os = "windows")]
const TASK_NAME: &str = "SvrnmeshDaemon";

/// Install and enable the svrn daemon service for the current
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

    #[cfg(target_os = "windows")]
    {
        install_schtasks(&bin_path)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = bin_path;
        Err(format!(
            "service registration is not supported on this platform \
             (os={}). Run `svrn daemon run` manually.",
            std::env::consts::OS
        ))
    }
}

/// Stop the running svrn daemon without unregistering it from
/// the service manager. A subsequent `svrn daemon restart` (or
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
            .args(["stop", "com.svrnmesh.daemon"])
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
            // ran `svrn setup`. Treat as already-stopped so the
            // caller (`stop_daemon` → `restart_daemon`) doesn't bail
            // before starting a fresh daemon.
            if out.status.code() == Some(3) && stderr.trim().is_empty() {
                return Ok(());
            }
            return Err(format!(
                "launchctl stop com.svrnmesh.daemon failed: {}\n\
                 hint: if the daemon isn't registered, run `svrn setup` first.",
                stderr.trim()
            ));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let out = Command::new("systemctl")
            .args(["--user", "stop", "svrnmesh.service"])
            .output()
            .map_err(|e| format!("spawn systemctl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "systemctl --user stop svrnmesh failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        // `schtasks /End` asks the task to stop its running instance. A task
        // that is not running answers with an error we read as success, the
        // same shape the launchd arm above gives "No such process" — "it is
        // already stopped" is the outcome the caller asked for, not a
        // failure to report (§18.3 cuts the other way here: reporting an
        // error would be the substitution).
        use std::process::Command;
        let out = Command::new("schtasks")
            .args(["/End", "/TN", TASK_NAME])
            .output()
            .map_err(|e| format!("spawn schtasks: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let combined = format!("{stderr}{}", String::from_utf8_lossy(&out.stdout));
            let lowered = combined.to_lowercase();
            if lowered.contains("not running") || lowered.contains("cannot find") {
                return Ok(());
            }
            return Err(format!(
                "schtasks /End {TASK_NAME} failed: {}\n\
                 hint: if the daemon isn't registered, run `svrn install-service` first.",
                combined.trim()
            ));
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err(format!(
            "service stop is not supported on this platform (os={}). \
             Send SIGTERM to the `svrn daemon run` process manually.",
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

    #[cfg(target_os = "windows")]
    {
        uninstall_schtasks()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Ok(())
    }
}

/// Ensure the binary path is absolute and exists. Expanding
/// `current_exe()` into an absolute path is what `svrn setup`
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

/// Remove a leftover *legacy*-branded service registration
/// (`com.sovereign.daemon` / `sovereign.service`) before installing the
/// rebranded one. Without this, the old launchd job / systemd unit stays
/// registered and a second daemon crash-loops against the API port the new
/// one binds. Best-effort and idempotent — a no-op once the legacy files are
/// gone.
#[cfg(target_os = "macos")]
#[allow(clippy::disallowed_methods)] // real $HOME: ~/Library/LaunchAgents is an OS-mandated path, not our data root
fn migrate_legacy_service() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let legacy = home
        .join("Library")
        .join("LaunchAgents")
        .join("com.sovereign.daemon.plist");
    if legacy.exists() {
        let _ = std::process::Command::new("launchctl")
            .args(["unload", legacy.to_string_lossy().as_ref()])
            .output();
        let _ = std::fs::remove_file(&legacy);
        eprintln!("svrnmesh: removed legacy launchd service com.sovereign.daemon");
    }
}

/// Windows had NO service backend before sv-surface, so there is no
/// pre-rebrand registration anywhere to clean up. An empty body that says
/// why beats omitting the function and letting the call site grow a `cfg`.
#[cfg(target_os = "windows")]
fn migrate_legacy_service() {}

#[cfg(target_os = "linux")]
fn migrate_legacy_service() {
    let Some(config) = dirs::config_dir() else {
        return;
    };
    let legacy = config
        .join("systemd")
        .join("user")
        .join("sovereign.service");
    if legacy.exists() {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", "sovereign.service"])
            .output();
        let _ = std::fs::remove_file(&legacy);
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output();
        eprintln!("svrnmesh: removed legacy systemd unit sovereign.service");
    }
}

// ─── macOS (launchd) ──────────────────────────────────────────────

#[cfg(target_os = "macos")]
#[allow(clippy::disallowed_methods)] // real $HOME: ~/Library/LaunchAgents is an OS-mandated path, not our data root
fn launchd_plist_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "cannot resolve home directory".to_string())?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join("com.svrnmesh.daemon.plist"))
}

/// PATH as seen by the shell that ran `install-service` — the one
/// environment where the operator's toolchain (SCIP exporters, node,
/// llama-server) is known to resolve. Service managers start daemons with
/// a minimal PATH, which silently severed the SCIP exporters: every
/// git_poll rebuild exported 0 symbols while every status surface stayed
/// green (live incident 2026-08-06). Capturing at install time generalizes
/// across toolchain managers instead of enumerating their directories.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn captured_path() -> String {
    std::env::var("PATH")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".to_string())
}

// Both XML-carrying backends need this: the launchd plist and the Windows
// task definition break identically on a path containing `&`. Gated to
// macOS alone until the Windows backend landed, and the cross-compile gate
// is what said so rather than a Windows user.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(target_os = "macos")]
#[allow(clippy::disallowed_methods)] // real $HOME: fills the launchd plist's {HOME} placeholder
fn install_launchd(bin_path: &Path) -> Result<(), String> {
    // Remove any leftover legacy (com.sovereign.daemon) registration first so an
    // upgrading user doesn't end up with two daemons fighting over the API port.
    migrate_legacy_service();

    let home = dirs::home_dir().ok_or_else(|| "cannot resolve home directory".to_string())?;
    let plist_path = launchd_plist_path()?;

    // Resolve the data root (prefers ~/.svrnmesh, falls back to a populated
    // legacy ~/.sovereign). Using the resolved root for the daemon's logs +
    // working dir means we never pre-create an empty ~/.svrnmesh here, which
    // would otherwise defeat the startup data-dir migration's existence guard.
    let data_dir = sovereign_contracts::rebrand::svrnmesh_root();

    // Make sure logs directory exists — launchd refuses to start if
    // StandardOutPath's parent is missing.
    let logs_dir = data_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)
        .map_err(|e| format!("create {}: {e}", logs_dir.display()))?;

    let content = LAUNCHD_TEMPLATE
        .replace("{BINARY}", &bin_path.to_string_lossy())
        .replace("{DATA_DIR}", &data_dir.to_string_lossy())
        .replace("{HOME}", &home.to_string_lossy())
        .replace("{PATH}", &xml_escape(&captured_path()));

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
    Ok(config.join("systemd").join("user").join("svrnmesh.service"))
}

#[cfg(target_os = "linux")]
fn install_systemd(bin_path: &Path) -> Result<(), String> {
    // Remove any leftover legacy (sovereign.service) registration first.
    migrate_legacy_service();

    let unit_path = systemd_unit_path()?;
    // `%` is a systemd unit-file specifier; escape it so a PATH containing
    // one survives the round-trip.
    let content = SYSTEMD_TEMPLATE
        .replace("{BINARY}", &bin_path.to_string_lossy())
        .replace("{PATH}", &captured_path().replace('%', "%%"));

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
        .args(["--user", "enable", "--now", "svrnmesh.service"])
        .output()
        .map_err(|e| format!("systemctl enable --now: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("systemctl enable --now failed: {}", stderr.trim()));
    }

    // Enabled is not the same as SURVIVING LOGOUT — see `ensure_linger`.
    // Deliberately not part of the `?` chain above: a host whose polkit
    // refuses lingering still has a correctly installed service, and
    // failing the install would be the wrong verdict. It warns instead,
    // naming the consequence (ARCH principle 6).
    ensure_linger();

    Ok(())
}

// ─── Lingering: the difference between a service and a NODE ───────
//
// A systemd USER unit lives inside `user@<uid>.service`, and systemd stops
// that manager when the user's last session ends. So `WantedBy=default.target`
// means "at login", not "at boot": without lingering the daemon dies at logout
// and does not come back until someone logs in again — precisely the window a
// headless box in the corner is supposed to be contributing to the mesh.
// `loginctl enable-linger <user>` is what makes the user manager start at boot
// and outlive every session; it is also the only part of this install that a
// distro's polkit may refuse, so it is a warning and never a fatal error.
//
// `uninstall_service` deliberately does NOT turn it back off: lingering is a
// property of the USER, not of this unit, and another of their units may be
// living on it. Enabling it is our business; revoking it is not.
//
// The argv deciders and the output parse are pure, and compiled under `test`
// on every platform for the same reason `lifecycle_argv` is: the interesting
// part is a string, and a string is checkable from whatever host the suite
// runs on.

/// Argv that asks logind "does this user's manager survive logout?".
///
/// `show-user --property=Linger` answers for a user with no active session
/// too, which is the state an install run from a remote shell is in.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn linger_probe_argv(user: &str) -> Vec<String> {
    vec!["show-user".into(), user.into(), "--property=Linger".into()]
}

/// Argv that turns lingering on for one user. No `--now`, no elevation:
/// enabling it for ONESELF is what default polkit rules permit.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn linger_enable_argv(user: &str) -> Vec<String> {
    vec!["enable-linger".into(), user.into()]
}

/// Is lingering already on, per `loginctl show-user` output?
///
/// Whole-line equality rather than `contains`: the block this parses also
/// carries the word in its negative form, so a laxer `contains("Linger")`
/// reads `Linger=no` as a yes and the install then skips the only step
/// that was the point.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn linger_is_enabled(show_user_output: &str) -> bool {
    show_user_output
        .lines()
        .any(|line| line.trim() == "Linger=yes")
}

/// What the operator is told when lingering could not be turned on.
///
/// Reports the absence in the words of its consequence, not in logind's
/// (ARCH principle 6): the manager's own stderr is kept, but the sentence
/// that matters is what the user loses and the one command that fixes it.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn linger_refusal_warning(user: &str, detail: &str) -> String {
    let detail = detail.trim();
    let because = if detail.is_empty() {
        String::new()
    } else {
        format!(" ({detail})")
    };
    format!(
        "svrnmesh: WARNING — could not enable lingering for {user}{because}.\n\
         svrnmesh:   The daemon will STOP AT LOGOUT: systemd tears down your user\n\
         svrnmesh:   manager when your last session ends, and it does not come back\n\
         svrnmesh:   until you log in again. Peers see this node as ABSENT from the\n\
         svrnmesh:   mesh for that whole window, not as busy.\n\
         svrnmesh:   Fix it with:  loginctl enable-linger {user}\n\
         svrnmesh:   (some distros' polkit rules require an administrator to run it)."
    )
}

/// Whose manager we are asking logind to keep alive.
///
/// Shelled out rather than adding a `whoami`/`users` dependency — one string
/// does not buy a crate here (module doc: every dependency is a decision).
/// `$USER` is set by every interactive shell; `id -un` covers the install
/// run from something that is not one.
#[cfg(target_os = "linux")]
fn current_username() -> Option<String> {
    for key in ["USER", "LOGNAME"] {
        if let Ok(name) = std::env::var(key) {
            let name = name.trim().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    let out = std::process::Command::new("id").arg("-un").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Make this user's systemd manager outlive their sessions, so the daemon
/// stays on the mesh across logout and comes back at boot.
///
/// Idempotent, like everything else in this module's install path: the
/// probe runs first and an already-lingering user is left alone rather
/// than re-asked (a repeat `enable-linger` is harmless but a repeat
/// polkit prompt on a desktop host is not).
#[cfg(target_os = "linux")]
fn ensure_linger() {
    let Some(user) = current_username() else {
        eprintln!(
            "{}",
            linger_refusal_warning("$USER", "cannot resolve the current username")
        );
        return;
    };

    if let Ok(out) = std::process::Command::new("loginctl")
        .args(linger_probe_argv(&user))
        .output()
    {
        if out.status.success() && linger_is_enabled(&String::from_utf8_lossy(&out.stdout)) {
            return;
        }
    }

    match std::process::Command::new("loginctl")
        .args(linger_enable_argv(&user))
        .output()
    {
        Ok(out) if out.status.success() => {
            eprintln!(
                "svrnmesh: enabled lingering for {user} — the daemon now survives \
                 logout and starts at boot without a login"
            );
        }
        Ok(out) => eprintln!(
            "{}",
            linger_refusal_warning(&user, &String::from_utf8_lossy(&out.stderr))
        ),
        Err(e) => eprintln!(
            "{}",
            linger_refusal_warning(&user, &format!("spawn loginctl: {e}"))
        ),
    }
}

/// Where the task XML is staged before `schtasks /Create /XML` reads it.
///
/// A real file rather than stdin: `schtasks` has no stdin form for `/XML`,
/// and the XML must be UTF-16LE with a BOM — Task Scheduler rejects UTF-8
/// with a bare "The task XML is malformed", which names nothing useful.
#[cfg(target_os = "windows")]
fn schtasks_xml_path() -> Result<PathBuf, String> {
    let dir = dirs::data_local_dir()
        .ok_or_else(|| "cannot resolve local app-data directory".to_string())?;
    Ok(dir.join("svrnmesh").join("SvrnmeshDaemon.xml"))
}

/// Encode as UTF-16LE with a byte-order mark. `schtasks /Create /XML`
/// requires it; the declaration in the template says `encoding="UTF-16"`
/// and Task Scheduler believes the BOM over the declaration.
#[cfg(target_os = "windows")]
fn utf16le_with_bom(s: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// XML-escape for the task file. The same five entities the launchd plist
/// needs — `xml_escape` is shared rather than re-derived, because a path
/// with an `&` in it breaks both files identically (§10.6).
#[cfg(target_os = "windows")]
fn install_schtasks(bin_path: &Path) -> Result<(), String> {
    // Same first move as the other two: clear any pre-rebrand registration
    // before adding this one, so an upgrading user does not end up with two
    // daemons fighting over the client port.
    migrate_legacy_service();

    let data_dir = sovereign_contracts::rebrand::svrnmesh_root();
    // The daemon writes its own logs under the data root; unlike launchd
    // there is no StandardOutPath to point at a file, so Task Scheduler
    // discards the child's stdio and the daemon's log files are the record.
    let logs_dir = data_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)
        .map_err(|e| format!("create {}: {e}", logs_dir.display()))?;

    let content = SCHTASKS_TEMPLATE
        .replace("{BINARY}", &xml_escape(&bin_path.to_string_lossy()))
        .replace("{WORKDIR}", &xml_escape(&data_dir.to_string_lossy()));

    let xml_path = schtasks_xml_path()?;
    if let Some(parent) = xml_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&xml_path, utf16le_with_bom(&content))
        .map_err(|e| format!("write {}: {e}", xml_path.display()))?;

    // `/F` overwrites an existing registration, which makes install
    // IDEMPOTENT the way `launchctl load` after `unload` and `systemctl
    // enable --now` already are — and is what an app upgrade needs when the
    // binary path changed.
    let out = std::process::Command::new("schtasks")
        .args([
            "/Create",
            "/TN",
            TASK_NAME,
            "/XML",
            &xml_path.to_string_lossy(),
            "/F",
        ])
        .output()
        .map_err(|e| format!("spawn schtasks: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "schtasks /Create {TASK_NAME} failed: {}{}",
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout).trim()
        ));
    }

    // The logon trigger fires at the NEXT logon; the other two platforms
    // start the daemon as part of installing it (`launchctl load` honours
    // RunAtLoad, `systemctl enable --now` has the --now), so this one runs
    // it too rather than leaving the caller a service that exists and is
    // not serving.
    let out = std::process::Command::new("schtasks")
        .args(["/Run", "/TN", TASK_NAME])
        .output()
        .map_err(|e| format!("spawn schtasks: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "schtasks /Run {TASK_NAME} failed: {}{}",
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout).trim()
        ));
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn uninstall_schtasks() -> Result<(), String> {
    // Idempotent, like the other two: a task that was never registered is
    // not an error, it is the state the caller asked for.
    let out = std::process::Command::new("schtasks")
        .args(["/Delete", "/TN", TASK_NAME, "/F"])
        .output()
        .map_err(|e| format!("spawn schtasks: {e}"))?;
    if !out.status.success() {
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
        if !combined.to_lowercase().contains("cannot find") {
            return Err(format!(
                "schtasks /Delete {TASK_NAME} failed: {}",
                combined.trim()
            ));
        }
    }
    // The staged XML is ours; leaving it behind would make a later
    // `service_installed()` reading of the filesystem lie.
    if let Ok(xml_path) = schtasks_xml_path() {
        let _ = std::fs::remove_file(xml_path);
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
        .args(["--user", "disable", "--now", "svrnmesh.service"])
        .output();

    std::fs::remove_file(&unit_path).map_err(|e| format!("remove {}: {e}", unit_path.display()))?;

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();

    Ok(())
}

/// Service names to probe, current name FIRST and the legacy name
/// second.
///
/// Both platforms need the legacy entry for the same reason: the
/// rebrand shipped `migrate_legacy_service`, but migration only runs
/// on install, so hosts that never re-ran `svrn install-service` are
/// still serving under the old name today. This host is one — it runs
/// `sovereign.service` with four drop-ins. A probe that knows only the
/// current name concludes "nothing owns the daemon" and hands
/// lifecycle back to the detached-child path, which is precisely the
/// bug this module is here to prevent.
#[cfg(target_os = "linux")]
pub(crate) const CANDIDATE_SERVICES: [&str; 2] = ["svrnmesh.service", "sovereign.service"];

#[cfg(target_os = "macos")]
pub(crate) const CANDIDATE_SERVICES: [&str; 2] = ["com.svrnmesh.daemon", "com.sovereign.daemon"];

// Windows has no pre-rebrand registration to find — the platform had no
// service backend at all before sv-surface — so the legacy slot repeats the
// current name rather than inventing a name no host has ever carried. The
// array shape is shared so `managing_service` walks one loop (§10.6).
#[cfg(target_os = "windows")]
pub(crate) const CANDIDATE_SERVICES: [&str; 2] = ["SvrnmeshDaemon", "SvrnmeshDaemon"];

/// A service manager that is RUNNING the daemon right now.
///
/// Distinct from [`service_installed`], which probes `is-enabled` and
/// answers "will a crash be auto-restarted?" — a question about boot.
/// This probes `is-active` and answers "must lifecycle commands go
/// through the service manager?", which is what `daemon
/// start`/`stop`/`restart` need to know.
///
/// Why the distinction earns its keep: a service-managed daemon
/// inherits `Environment=` and the unit's `ExecStart` wrapper, and on
/// a real install that is where the operational configuration lives.
/// On this host five vars (`SOVEREIGN_N_UBATCH`,
/// `SOVEREIGN_RPC_WORKER_ALLOWLIST`, `_SETTLE_SECS`,
/// `_FLAP_THRESHOLD`, `SOVEREIGN_RPC_BLOCK_SPLIT`) exist ONLY in the
/// drop-ins — no config file, no shell rc. A `daemon restart` that
/// SIGTERMs the unit's child and spawns its own detached replacement
/// silently drops all five, changing the distributed-inference shard
/// boundary and worker policy while reporting success. Delegating
/// keeps them, because systemd re-applies the unit.
pub struct ManagingService {
    /// Unit or label name — printed so the operator can see which
    /// manager the verb decided to defer to.
    pub name: String,
    pub(crate) mgr: Manager,
}

/// Which service manager a candidate name belongs to.
///
/// All three variants exist on all three platforms on purpose — the argv
/// deciders below are not `cfg`-gated, so a Mac can type-check and unit-test
/// the `schtasks` command lines and a Windows box can do the same for
/// `launchctl`. The cost is that two of the three are never CONSTRUCTED in
/// any single build, which is what the allow names.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Manager {
    Systemd,
    Launchd,
    /// Windows Task Scheduler. The odd one out in exactly one way: it has no
    /// atomic restart verb, so [`ManagingService::restart`] runs stop then
    /// start for it rather than pretending one command exists.
    Schtasks,
}

impl Manager {
    /// The binary that drives it.
    pub(crate) fn program(self) -> &'static str {
        match self {
            Manager::Systemd => "systemctl",
            Manager::Launchd => "launchctl",
            Manager::Schtasks => "schtasks",
        }
    }
}

/// The argv for one lifecycle verb — pure, and compiled on EVERY
/// platform.
///
/// Deliberately not behind `#[cfg]`, so the launchd command lines are
/// type-checked and unit-tested when building on Linux too. The
/// interesting decisions live here — `kickstart -k` for restart but
/// bare `kickstart` for start, and the `gui/<uid>/` domain target —
/// and they are easy to get wrong. Only the `Command` invocation
/// below is platform-dependent.
pub(crate) fn lifecycle_argv(mgr: Manager, verb: &str, name: &str, uid: u32) -> Vec<String> {
    match mgr {
        Manager::Systemd => vec!["--user".into(), verb.into(), name.into()],
        // Task Scheduler names the two verbs differently and has no third.
        // `restart` never reaches here — `ManagingService::restart` runs stop
        // then start for this manager, because pretending an atomic restart
        // exists is how a caller ends up believing a job cycled when it did
        // not.
        Manager::Schtasks => {
            let verb = match verb {
                "stop" => "/End",
                _ => "/Run",
            };
            vec![verb.into(), "/TN".into(), name.into()]
        }
        Manager::Launchd => {
            // launchd has no `restart` verb; `kickstart -k` IS the
            // documented equivalent — it kills a running job and
            // starts it again, re-reading the plist so the job's
            // EnvironmentVariables are re-applied, exactly as systemd
            // re-applies a unit. That re-application is the entire
            // reason to delegate rather than spawn our own child.
            //
            // kickstart REQUIRES the `gui/<uid>/` domain target; the
            // bare-label form is legacy syntax and rejects `-k`.
            let target = format!("gui/{uid}/{name}");
            match verb {
                "restart" => vec!["kickstart".into(), "-k".into(), target],
                "stop" => vec!["kill".into(), "SIGTERM".into(), target],
                _ => vec!["kickstart".into(), target],
            }
        }
    }
}

/// Is this stderr the manager saying "it was already stopped"?
/// Treated as success: that is the outcome the caller asked for.
/// `stop_service` learned these shapes the hard way; the delegation
/// path must not regress them.
pub(crate) fn stop_stderr_means_already_stopped(stderr: &str) -> bool {
    stderr.contains("No such process")
        || stderr.contains("not running")
        || stderr.contains("Could not find service")
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid(2) cannot fail and is thread-safe.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

/// Argv that asks the manager "why is this job not running?".
///
/// Platform-independent by construction, like [`lifecycle_argv`] — the two
/// managers express the same question in different vocabularies.
pub(crate) fn diagnose_argv(mgr: Manager, name: &str, uid: u32) -> Vec<String> {
    match mgr {
        // `is-active` reports the state; `--property=Result` names WHY a unit
        // is not running (`start-limit-hit`, `exit-code`, …).
        Manager::Systemd => vec![
            "--user".into(),
            "show".into(),
            name.into(),
            "--property=ActiveState,Result,ExecMainStatus,NeedDaemonReload".into(),
        ],
        Manager::Launchd => vec!["print".into(), format!("gui/{uid}/{name}")],
        // `/Query /V /FO LIST` is the only schtasks form that prints "Last
        // Result" and "Scheduled Task State" — the two facts the other
        // managers answer with `show` and `print`.
        Manager::Schtasks => vec![
            "/Query".into(),
            "/TN".into(),
            name.into(),
            "/V".into(),
            "/FO".into(),
            "LIST".into(),
        ],
    }
}

/// Argv that re-registers the job so the manager re-reads it from disk.
///
/// Returned as a SEQUENCE because both managers need two steps, and the first
/// is allowed to fail: launchd's `bootout` is a no-op when the job is already
/// unloaded, and systemd's `reset-failed` is a no-op when the unit is not in a
/// failed state. Callers run them in order and judge only the last.
pub(crate) fn reregister_argv(
    mgr: Manager,
    name: &str,
    uid: u32,
    unit_path: &str,
) -> Vec<Vec<String>> {
    match mgr {
        // `daemon-reload` picks up an edited unit file; `reset-failed` clears
        // a start-limit lockout, which is systemd's version of "the manager
        // will not spawn this no matter how many times you ask".
        Manager::Systemd => vec![
            vec!["--user".into(), "daemon-reload".into()],
            vec!["--user".into(), "reset-failed".into(), name.into()],
        ],
        Manager::Launchd => vec![
            vec!["bootout".into(), format!("gui/{uid}/{name}")],
            vec!["bootstrap".into(), format!("gui/{uid}"), unit_path.into()],
        ],
        // The same two steps in Task Scheduler's vocabulary: drop the
        // registration, then create it again from the XML on disk. `/F` on
        // the create is what makes the second step survive the first having
        // been a no-op.
        Manager::Schtasks => vec![
            vec!["/Delete".into(), "/TN".into(), name.into(), "/F".into()],
            vec![
                "/Create".into(),
                "/TN".into(),
                name.into(),
                "/XML".into(),
                unit_path.into(),
                "/F".into(),
            ],
        ],
    }
}

/// Does this diagnosis mean "the manager will not spawn this job as
/// registered" — i.e. re-registering is the repair, not retrying?
///
/// # The failure class this names
///
/// Both managers can reach a state where they ACCEPT a start command and then
/// never run the job, while every surface reports success: the start verb
/// exits 0 because the manager took the request, the daemon log keeps an old
/// timestamp because the process dies before opening it, and the caller waits
/// out its whole readiness budget and blames slow model loading. Three
/// green-looking signals over one dead service (ARCH §18.3).
///
/// - **launchd (macOS 13+)**: a registered job is pinned to the CODE IDENTITY
///   of the binary it was bootstrapped with. Rebuilding that binary in place —
///   which is what every `scripts/dev-build.sh` does, since
///   `target/debug/sovereign-cli-daemon` IS the deployed daemon on a dev host —
///   invalidates the Launch Constraints Registry entry. launchd then refuses
///   to spawn and records `EX_CONFIG` (78). Observed 2026-08-26 with
///   `runs = 1038`, `last exit code = 78: EX_CONFIG`, `needs LWCR update`.
/// - **systemd**: a unit whose file changed on disk without a `daemon-reload`
///   starts from the STALE definition, and a unit that has hit its start limit
///   (`Result=start-limit-hit`) is refused outright until `reset-failed`.
///
/// Different mechanisms, one shape — and one repair: make the manager re-read
/// the job.
pub(crate) fn diagnosis_needs_reregister(mgr: Manager, text: &str) -> bool {
    match mgr {
        Manager::Systemd => {
            text.contains("NeedDaemonReload=yes")
                || text.contains("Result=start-limit-hit")
                || (text.contains("ActiveState=failed") && text.contains("Result=exit-code"))
        }
        // No `pid = ` line means launchd is not running it right now; the LWCR
        // note is the cause and the 78 is the symptom, and launchd does not
        // always print both.
        Manager::Launchd => {
            !text.contains("pid = ")
                && (text.contains("needs LWCR update") || text.contains("last exit code = 78"))
        }
        // Task Scheduler has no code-identity pinning and no start limit, so
        // the class this predicate names does not exist on Windows: a task
        // that will not run says so in its Last Result and retrying is the
        // repair, not re-registering. Answering `false` is a MEASUREMENT of
        // that, not a stub — the Windows arm of `needs_reregister` therefore
        // never fires and the caller falls through to its ordinary timeout.
        Manager::Schtasks => false,
    }
}

impl ManagingService {
    /// Restart in place, letting the manager re-apply the unit.
    pub fn restart(&self) -> Result<(), String> {
        // Task Scheduler has no restart verb. The other two DO, and the
        // reason to prefer theirs is not brevity — `launchctl kickstart -k`
        // and `systemctl --user restart` are atomic, so no window exists in
        // which the manager's own restart policy races ours. Windows has no
        // such window to close because a task that is `/End`ed has exited
        // cleanly and its RestartOnFailure policy does not fire on a clean
        // exit — the same reasoning as the launchd plist's
        // `KeepAlive.SuccessfulExit=false`.
        if self.mgr == Manager::Schtasks {
            self.act("stop")?;
            return self.act("start");
        }
        self.act("restart")
    }

    /// Stop, so the manager does not immediately restart it.
    pub fn stop(&self) -> Result<(), String> {
        self.act("stop")
    }

    /// Start under the manager.
    pub fn start(&self) -> Result<(), String> {
        self.act("start")
    }

    /// Ask the manager why the job is not running, and decide whether
    /// re-registering is the repair.
    ///
    /// `Some(reason)` means the manager will not spawn this job AS REGISTERED
    /// and retrying the start verb cannot help — see
    /// [`diagnosis_needs_reregister`] for the two mechanisms this covers.
    pub fn needs_reregister(&self) -> Option<String> {
        let argv = diagnose_argv(self.mgr, &self.name, current_uid());
        let out = std::process::Command::new(self.mgr.program())
            .args(&argv)
            .output()
            .ok()?;
        // launchd prints to stdout, systemd `show` likewise; merge for safety.
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        if diagnosis_needs_reregister(self.mgr, &text) {
            Some(match self.mgr {
                Manager::Launchd => "launchd is refusing to spawn it (EX_CONFIG 78, \
                    \"needs LWCR update\"): macOS pins a registered job to the code \
                    identity of the binary it was registered with, and this binary has \
                    been rebuilt since"
                    .to_string(),
                Manager::Systemd => "systemd is holding a stale or failed unit \
                    (NeedDaemonReload / start-limit-hit): the unit on disk and the one \
                    systemd will run are not the same"
                    .to_string(),
                // Unreachable: `diagnosis_needs_reregister` answers false for
                // Schtasks, because the failure class does not exist there.
                // Spelled out rather than wildcarded so ADDING a manager is a
                // compile error here, which is the only reason this match is
                // exhaustive by hand (ARCH §2.1).
                Manager::Schtasks => return None,
            })
        } else {
            None
        }
    }

    /// Make the manager re-read this job from disk.
    ///
    /// The repair for [`Self::needs_reregister`]. Runs the manager's sequence
    /// from [`reregister_argv`] and judges only the LAST step — the first is
    /// allowed to fail, because "already unloaded" and "not in a failed state"
    /// are the states it exists to reach.
    pub fn reregister(&self) -> Result<(), String> {
        let unit_path = self.unit_path().unwrap_or_default();
        let steps = reregister_argv(self.mgr, &self.name, current_uid(), &unit_path);
        let mut last: Result<(), String> = Err("no re-register steps".into());
        for argv in steps {
            let label = format!("{} {}", self.mgr.program(), argv.join(" "));
            let out = std::process::Command::new(self.mgr.program())
                .args(&argv)
                .output()
                .map_err(|e| format!("{label}: {e}"))?;
            last = if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "{label} failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            };
        }
        last
    }

    /// On-disk path of the unit/plist, when this platform needs one to
    /// re-register. launchd's `bootstrap` takes the plist path; systemd's
    /// `daemon-reload` finds the unit itself, so the value is unused there.
    fn unit_path(&self) -> Option<String> {
        match self.mgr {
            Manager::Launchd => {
                #[cfg(target_os = "macos")]
                {
                    launchd_plist_path().ok().map(|p| p.display().to_string())
                }
                #[cfg(not(target_os = "macos"))]
                {
                    None
                }
            }
            Manager::Systemd => None,
            // schtasks re-creates from the XML `install_schtasks` staged, so
            // the path IS needed here — unlike systemd, Task Scheduler does
            // not keep its own copy to reload.
            Manager::Schtasks => {
                #[cfg(target_os = "windows")]
                {
                    schtasks_xml_path().ok().map(|p| p.display().to_string())
                }
                #[cfg(not(target_os = "windows"))]
                {
                    None
                }
            }
        }
    }

    /// Platform-independent by construction — see [`lifecycle_argv`].
    fn act(&self, verb: &str) -> Result<(), String> {
        let argv = lifecycle_argv(self.mgr, verb, &self.name, current_uid());
        let label = format!("{} {}", self.mgr.program(), argv.join(" "));
        let out = std::process::Command::new(self.mgr.program())
            .args(&argv)
            .output()
            .map_err(|e| format!("{label}: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if verb == "stop" && stop_stderr_means_already_stopped(&stderr) {
            return Ok(());
        }
        Err(format!("{label} failed: {}", stderr.trim()))
    }
}

/// Argv that asks the manager "do you know this service?".
///
/// REGISTRATION, not liveness — and that distinction is the whole
/// point. An earlier cut of this probe asked systemd `is-active`,
/// which is wrong for exactly the case that matters: a `daemon start`
/// happens when the daemon is DOWN, so the unit is `inactive`, so the
/// probe said "unmanaged" and the caller spawned its own detached
/// child — losing every `Environment=` and the `ExecStart` wrapper.
/// The verb that most needs to delegate was the one guaranteed not
/// to. Verified the hard way on 2026-07-29: the restart stopped a
/// `sovereign.service`-managed daemon and then could not start it
/// back.
///
/// `systemctl --user cat` exits 0 for any unit systemd knows, active
/// or not. `launchctl list` exits 0 for any loaded job, running or
/// not — the same question in launchd's vocabulary.
pub(crate) fn probe_argv(mgr: Manager, name: &str) -> Vec<String> {
    match mgr {
        Manager::Systemd => vec!["--user".into(), "cat".into(), name.into()],
        Manager::Launchd => vec!["list".into(), name.into()],
        Manager::Schtasks => vec!["/Query".into(), "/TN".into(), name.into()],
    }
}

/// Is the installed unit the daemon THIS invocation is addressing?
///
/// THE INCIDENT (2026-07-30, caught by `first-run[7]` the hour that step was
/// given an assertion). The journey sandbox lane boots its own daemon under a
/// throwaway `HOME` on `:19741`. Its `daemon restart` step restarted the
/// OPERATOR's `sovereign.service`, and its `daemon stop` step then stopped it —
/// both reporting exit 0 from `systemctl`, both leaving the sandbox's own daemon
/// untouched. Two lanes' worth of "the daemon keeps dying under heavy lanes" was
/// this, mistaken for flakiness and hand-restarted three times.
///
/// The guard for exactly this existed and was bypassed. `SOVEREIGN_STOP_SANDBOXED`
/// was added 2026-06-10 after the phase3 test killed a developer's daemon twice,
/// and it confines the stop chain to its pidfile legs — but it is checked partway
/// down `stop_daemon`, and the service-manager leg added on 2026-07-29 was placed
/// ABOVE it, under an "OWNERSHIP FIRST" rationale that is right about ownership
/// and silent about *whose*. A per-call-site guard is the wrong shape: the
/// failure mode is a NEW call site that does not know to ask.
///
/// So the question is answered once, here, where every delegation path already
/// passes. Two signals, either sufficient:
///
///   * `SOVEREIGN_STOP_SANDBOXED=1` — automation stating it owns an isolated
///     daemon. Operators should never set it.
///   * a resolved client port other than the default — the unit serves the
///     default port, so an invocation addressed elsewhere is not addressing the
///     unit. This is the automatic half, and it is the half that would have
///     caught the incident: the journey lane sets no env var, it just runs on
///     `:19741`.
///
/// The asymmetry justifies the bias. Wrongly delegating kills a production
/// daemon from a sandbox; wrongly declining sends SIGTERM to a pid we own and
/// leaves the manager's restart policy to notice — which is what every version
/// before 2026-07-29 did.
/// Whether this invocation speaks for the operator's installed unit.
///
/// It used to be computed HERE, from the daemon's sandbox flag and its
/// `SetupConfig` port. Both are daemon-side facts, and a crate a desktop can
/// depend on must not know either — so the decision is the caller's and this
/// type is how it arrives. An argument rather than ambient state, because
/// the two answers have opposite failure modes and a default would silently
/// pick one (ARCH §7, §18.3).
///
/// The asymmetry that sets the bias: wrongly delegating kills a production
/// daemon from a sandbox; wrongly declining sends SIGTERM to a pid we own
/// and leaves the manager's restart policy to notice. So a caller that is
/// not sure says [`Addressing::SomethingElse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addressing {
    /// This process is addressing the unit the operator installed — the
    /// default data root, the default client port, no sandbox.
    TheInstalledUnit,
    /// A sandbox, a non-default port, or anything else that must not drive
    /// the operator's service manager.
    SomethingElse,
}

/// The service manager currently running the daemon, if any.
/// `None` means lifecycle is the caller's to own — the detached-child
/// path in `daemon start` is correct.
///
/// Also `None` when this invocation is not addressing the installed unit at all
/// (see [`service_manager_is_addressed`]) — a sandbox must not drive the
/// operator's service.
///
/// Both platforms walk [`CANDIDATE_SERVICES`] in order, so a host
/// still registered under the pre-rebrand name is recognised rather
/// than silently treated as unmanaged.
pub fn managing_service(addressing: Addressing) -> Option<ManagingService> {
    #[cfg(target_os = "macos")]
    let mgr = Manager::Launchd;
    #[cfg(target_os = "linux")]
    let mgr = Manager::Systemd;
    #[cfg(target_os = "windows")]
    let mgr = Manager::Schtasks;

    if addressing == Addressing::SomethingElse {
        return None;
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        CANDIDATE_SERVICES.iter().find_map(|name| {
            let out = std::process::Command::new(mgr.program())
                .args(probe_argv(mgr, name))
                .output()
                .ok()?;
            out.status.success().then(|| ManagingService {
                name: (*name).to_string(),
                mgr,
            })
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

/// True when the daemon is registered with the user's service manager
/// AND the registration is active — i.e. a crash WILL be auto-restarted.
///
/// macOS: the plist must exist on disk *and* be loaded into this
/// user's launchd session (`launchctl list <label>` exits 0) —
/// plist-on-disk-but-unloaded is NOT supervised, launchd only honors
/// KeepAlive for loaded jobs. Linux: `systemctl --user is-enabled`
/// exits 0. Anything else (other platforms, probe failures) reports
/// unsupervised, which is the safe direction for the doctor check and
/// the `daemon start` advisory that consume this.
///
/// Asks a different question from [`managing_service`]: this one is
/// about BOOT and crash-recovery, that one is about who must drive
/// lifecycle right now. A host can be managed-but-not-enabled (this
/// one is), so neither answer implies the other.
pub fn service_installed() -> bool {
    #[cfg(target_os = "macos")]
    {
        let Ok(plist) = launchd_plist_path() else {
            return false;
        };
        if !plist.exists() {
            return false;
        }
        std::process::Command::new("launchctl")
            .args(["list", "com.svrnmesh.daemon"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("systemctl")
            .args(["--user", "is-enabled", "svrnmesh.service"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(target_os = "windows")]
    {
        // Registration, not liveness — the same question the other two ask.
        // `/Query` exits non-zero when the task does not exist.
        std::process::Command::new("schtasks")
            .args(["/Query", "/TN", TASK_NAME])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        false
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod service_ownership_tests {
    use super::*;

    /// The launchd command lines, asserted from whatever platform the
    /// suite runs on. These are the lines a Mac user's `svrn daemon
    /// restart` actually executes, and until this test existed nothing
    /// checked them outside the release workflow.
    #[test]
    fn launchd_argv_uses_kickstart_dash_k_with_a_gui_domain_target() {
        assert_eq!(
            lifecycle_argv(Manager::Launchd, "restart", "com.svrnmesh.daemon", 501),
            ["kickstart", "-k", "gui/501/com.svrnmesh.daemon"],
            "restart must be kickstart -k: launchd has no restart verb, and -k is \
             what re-reads the plist so EnvironmentVariables are re-applied"
        );
        assert_eq!(
            lifecycle_argv(Manager::Launchd, "start", "com.svrnmesh.daemon", 501),
            ["kickstart", "gui/501/com.svrnmesh.daemon"],
            "start is kickstart WITHOUT -k — -k would kill a healthy daemon"
        );
        assert_eq!(
            lifecycle_argv(Manager::Launchd, "stop", "com.svrnmesh.daemon", 501),
            ["kill", "SIGTERM", "gui/501/com.svrnmesh.daemon"]
        );
        assert_eq!(Manager::Launchd.program(), "launchctl");
    }

    #[test]
    fn systemd_argv_is_user_scoped() {
        assert_eq!(
            lifecycle_argv(Manager::Systemd, "restart", "svrnmesh.service", 501),
            ["--user", "restart", "svrnmesh.service"],
            "--user matters: the daemon is a user unit, and the system-scoped \
             command would target a different (nonexistent) unit"
        );
        assert_eq!(Manager::Systemd.program(), "systemctl");
    }

    /// A stop that finds nothing to stop got what it asked for.
    #[test]
    fn already_stopped_stderr_is_success_on_both_managers() {
        for s in [
            "launchctl: No such process",
            "Unit svrnmesh.service is not running",
            "Could not find service \"com.svrnmesh.daemon\"",
        ] {
            assert!(stop_stderr_means_already_stopped(s), "{s:?}");
        }
        assert!(!stop_stderr_means_already_stopped("Permission denied"));
    }

    /// The regression that made service ownership invisible on a real
    /// host: the probe knew only the CURRENT service name, so a daemon
    /// still registered under the legacy name looked unmanaged and
    /// every lifecycle verb fell through to the detached-child path.
    /// Asserted on both platforms because both rebranded, and on both
    /// `migrate_legacy_service` only runs at install time — so both
    /// have hosts still serving under the old name.
    #[test]
    fn both_platforms_probe_current_name_first_then_legacy() {
        assert_eq!(CANDIDATE_SERVICES.len(), 2, "current + legacy");
        assert!(
            !CANDIDATE_SERVICES[0].contains("sovereign"),
            "current (rebranded) name is probed first: {CANDIDATE_SERVICES:?}"
        );
        assert!(
            CANDIDATE_SERVICES[1].contains("sovereign"),
            "legacy name must still be probed — real installs run under it: \
             {CANDIDATE_SERVICES:?}"
        );
    }

    /// The probe must ask about REGISTRATION, not liveness. Asking
    /// systemd `is-active` broke `daemon start` specifically: start
    /// runs when the daemon is down, so the unit reads `inactive`, so
    /// the probe reported "unmanaged" and the caller spawned a
    /// detached child with none of the unit's environment. Guard the
    /// verb, not just the intent.
    #[test]
    fn the_probe_asks_about_registration_not_liveness() {
        assert_eq!(
            probe_argv(Manager::Systemd, "sovereign.service"),
            ["--user", "cat", "sovereign.service"],
            "`cat` exits 0 for a known-but-inactive unit; `is-active` does not, \
             and a down daemon is exactly when `start` needs to delegate"
        );
        assert_eq!(
            probe_argv(Manager::Launchd, "com.svrnmesh.daemon"),
            ["list", "com.svrnmesh.daemon"],
            "`launchctl list` exits 0 for a loaded job whether or not it is running"
        );
    }

    // ── The Windows backend, tested on every platform ──────────────
    //
    // The argv deciders are deliberately not `cfg`-gated (see
    // `lifecycle_argv`), which is what lets a Scheduled Task's command
    // lines be checked on a Mac. That matters more here than for the
    // other two: nobody on this project can run Windows on demand, so an
    // untested `schtasks` spelling would ship as a guess. What these do
    // NOT prove is that Task Scheduler accepts the XML — that is a real
    // run on a real Windows host and it is owed.

    #[test]
    fn schtasks_start_and_stop_are_run_and_end() {
        assert_eq!(
            lifecycle_argv(Manager::Schtasks, "start", "SvrnmeshDaemon", 0),
            ["/Run", "/TN", "SvrnmeshDaemon"],
        );
        assert_eq!(
            lifecycle_argv(Manager::Schtasks, "stop", "SvrnmeshDaemon", 0),
            ["/End", "/TN", "SvrnmeshDaemon"],
        );
    }

    /// The asymmetry worth pinning: Task Scheduler has NO restart verb, so
    /// `lifecycle_argv` must not invent one. `ManagingService::restart`
    /// runs stop-then-start for this manager instead, and a future edit
    /// that quietly maps "restart" onto `/Run` alone would cycle nothing
    /// while reporting success (§18.3).
    #[test]
    fn schtasks_has_no_restart_verb_and_does_not_fake_one() {
        assert_eq!(
            lifecycle_argv(Manager::Schtasks, "restart", "SvrnmeshDaemon", 0),
            ["/Run", "/TN", "SvrnmeshDaemon"],
            "an unrecognised verb falls through to /Run; `restart` never \
             reaches here because ManagingService::restart intercepts it"
        );
        assert_ne!(
            lifecycle_argv(Manager::Schtasks, "restart", "SvrnmeshDaemon", 0),
            lifecycle_argv(Manager::Schtasks, "stop", "SvrnmeshDaemon", 0),
            "if these ever coincide, restart would stop the daemon and \
             report that it restarted it"
        );
    }

    #[test]
    fn schtasks_probes_registration_not_liveness() {
        assert_eq!(
            probe_argv(Manager::Schtasks, "SvrnmeshDaemon"),
            ["/Query", "/TN", "SvrnmeshDaemon"],
            "`/Query` on a name exits 0 for a registered task whether or not \
             an instance is running — the same question `launchctl list` and \
             `systemctl cat` answer"
        );
    }

    /// Re-registration is two steps on every manager, and on Windows the
    /// first is allowed to fail the same way the other two are: deleting a
    /// task that is not there is the state the caller wanted. `/F` on the
    /// create is what makes the second step independent of the first.
    #[test]
    fn schtasks_reregisters_by_delete_then_create_from_the_staged_xml() {
        let steps = reregister_argv(Manager::Schtasks, "SvrnmeshDaemon", 0, "C:\\x\\task.xml");
        assert_eq!(steps.len(), 2, "delete, then create");
        assert_eq!(steps[0], ["/Delete", "/TN", "SvrnmeshDaemon", "/F"]);
        assert_eq!(
            steps[1],
            [
                "/Create",
                "/TN",
                "SvrnmeshDaemon",
                "/XML",
                "C:\\x\\task.xml",
                "/F"
            ],
            "the XML path is load-bearing: unlike systemd, Task Scheduler \
             keeps no copy of the definition to reload"
        );
    }

    /// The launchd code-identity wedge and the systemd start-limit lockout
    /// are platform failure classes that Windows does not have. Answering
    /// `false` is a measurement, and the test says so — a future edit that
    /// makes this `true` would send every Windows user through a repair
    /// path for a condition that cannot occur.
    #[test]
    fn schtasks_never_reports_the_reregister_wedge() {
        for text in [
            "Last Result: 267011",
            "Scheduled Task State: Disabled",
            "needs LWCR update",
            "last exit code = 78",
            "Result=start-limit-hit",
            "",
        ] {
            assert!(
                !diagnosis_needs_reregister(Manager::Schtasks, text),
                "no Windows diagnosis is a re-register wedge; got one for {text:?}"
            );
        }
    }

    #[test]
    fn every_manager_names_its_own_program() {
        assert_eq!(Manager::Systemd.program(), "systemctl");
        assert_eq!(Manager::Launchd.program(), "launchctl");
        assert_eq!(Manager::Schtasks.program(), "schtasks");
    }

    /// The XML template must carry the two substitutions the installer
    /// makes and the settings that make it a DAEMON registration rather
    /// than a scheduled job — checked against the file itself, on every
    /// platform, because a template edit is the likeliest way to break a
    /// backend nobody here can run.
    #[test]
    fn the_windows_task_template_is_a_daemon_registration() {
        let xml = include_str!("../../../contrib/windows/SvrnmeshDaemon.xml");
        assert!(xml.contains("{BINARY}"), "installer substitutes the binary");
        assert!(
            xml.contains("{WORKDIR}"),
            "installer substitutes the workdir"
        );
        assert!(
            xml.contains("<LogonTrigger>"),
            "per-user, starts at logon — the analogue of RunAtLoad and \
             WantedBy=default.target"
        );
        assert!(
            xml.contains("<RunLevel>LeastPrivilege</RunLevel>"),
            "no elevation, like the LaunchAgent and the systemd --user unit"
        );
        assert!(
            xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"),
            "Task Scheduler's DEFAULT is to kill a task after three days; a \
             long-lived daemon must opt out or it is SIGKILLed while healthy"
        );
        assert!(
            xml.contains("<RestartOnFailure>"),
            "the analogue of KeepAlive / Restart=on-failure — without it the \
             task is registered and unsupervised, which is the state \
             install-service exists to leave behind"
        );
        assert!(
            xml.contains("<Count>5</Count>"),
            "the crash-loop brake, matching systemd's StartLimitBurst=5"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_probes_systemd_unit_names() {
        assert_eq!(
            CANDIDATE_SERVICES,
            ["svrnmesh.service", "sovereign.service"]
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_probes_launchd_labels() {
        assert_eq!(
            CANDIDATE_SERVICES,
            ["com.svrnmesh.daemon", "com.sovereign.daemon"]
        );
    }
}

#[cfg(test)]
mod reregister_decision_tests {
    use super::*;

    /// The exact `launchctl print` signature observed 2026-08-26 after ~10
    /// rebuilds of the deployed debug binary in one session.
    const LAUNCHD_WEDGED: &str = "\tstate = spawn scheduled\n\truns = 1038\n\t         last exit code = 78: EX_CONFIG\n\tproperties = runatload | needs LWCR update";

    /// The same job after `bootout` + `bootstrap` — this is what healthy looks
    /// like, and the detector must NOT fire on it.
    const LAUNCHD_HEALTHY: &str =
        "\tstate = running\n\tpid = 43984\n\truns = 1\n\tlast exit code = (never exited)";

    #[test]
    fn launchd_lwcr_wedge_is_detected() {
        assert!(diagnosis_needs_reregister(Manager::Launchd, LAUNCHD_WEDGED));
    }

    #[test]
    fn launchd_running_job_is_not_a_wedge() {
        assert!(!diagnosis_needs_reregister(
            Manager::Launchd,
            LAUNCHD_HEALTHY
        ));
    }

    /// A job that is running but happens to have exited 78 EARLIER is not
    /// wedged — the `pid = ` line is what distinguishes "refused to spawn"
    /// from "spawned fine, once had a bad run".
    #[test]
    fn launchd_running_after_a_past_78_is_not_a_wedge() {
        let text = "\tpid = 5150\n\tlast exit code = 78: EX_CONFIG";
        assert!(!diagnosis_needs_reregister(Manager::Launchd, text));
    }

    #[test]
    fn systemd_stale_unit_needs_reload() {
        let text = "ActiveState=inactive\nResult=success\nNeedDaemonReload=yes";
        assert!(diagnosis_needs_reregister(Manager::Systemd, text));
    }

    #[test]
    fn systemd_start_limit_needs_reset_failed() {
        let text = "ActiveState=failed\nResult=start-limit-hit\nNeedDaemonReload=no";
        assert!(diagnosis_needs_reregister(Manager::Systemd, text));
    }

    #[test]
    fn systemd_healthy_unit_is_left_alone() {
        let text = "ActiveState=active\nResult=success\nNeedDaemonReload=no";
        assert!(!diagnosis_needs_reregister(Manager::Systemd, text));
    }

    /// A unit that is merely stopped is NOT a re-register case — that is the
    /// ordinary "daemon is down, start it" path, and re-registering it would
    /// be a heavier action than the situation calls for.
    #[test]
    fn systemd_cleanly_stopped_unit_is_not_a_wedge() {
        let text = "ActiveState=inactive\nResult=success\nNeedDaemonReload=no";
        assert!(!diagnosis_needs_reregister(Manager::Systemd, text));
    }

    #[test]
    fn reregister_sequences_are_two_steps_per_manager() {
        let launchd = reregister_argv(Manager::Launchd, "com.svrnmesh.daemon", 502, "/tmp/x.plist");
        assert_eq!(launchd.len(), 2);
        assert_eq!(launchd[0][0], "bootout");
        assert_eq!(launchd[1][0], "bootstrap");
        // bootstrap needs the domain AND the on-disk plist path.
        assert_eq!(launchd[1][1], "gui/502");
        assert_eq!(launchd[1][2], "/tmp/x.plist");

        let systemd = reregister_argv(Manager::Systemd, "sovereign.service", 502, "");
        assert_eq!(systemd.len(), 2);
        assert!(systemd[0].contains(&"daemon-reload".to_string()));
        assert!(systemd[1].contains(&"reset-failed".to_string()));
    }
}

/// The linger decision, checked from whatever platform the suite runs on.
///
/// Pure by construction (see the block comment above `linger_probe_argv`),
/// so the Linux-only half of `install_service` is not the only thing that
/// ever exercises it — nobody here installs a systemd unit on demand, and
/// an unchecked spelling of `loginctl` would ship as a guess.
#[cfg(test)]
mod linger_tests {
    use super::*;

    #[test]
    fn the_probe_asks_logind_for_the_linger_property() {
        assert_eq!(
            linger_probe_argv("alex"),
            ["show-user", "alex", "--property=Linger"],
            "show-user answers for a user with no active session; `list-users` \
             does not list one, which is the state a remote install runs in"
        );
    }

    #[test]
    fn enabling_is_per_user_and_asks_for_no_elevation() {
        assert_eq!(linger_enable_argv("alex"), ["enable-linger", "alex"]);
    }

    #[test]
    fn linger_is_read_from_the_whole_line_not_a_substring() {
        assert!(linger_is_enabled("Linger=yes"));
        assert!(linger_is_enabled("IdleHint=no\nLinger=yes\nState=active\n"));
        assert!(!linger_is_enabled("Linger=no"));
        assert!(!linger_is_enabled(""));
        // The trap this guards: a `contains("Linger=yes")`-shaped check is
        // fine, but a `contains("Linger")` one reads "no" as "yes" and the
        // install then silently skips the only step that matters.
        assert!(!linger_is_enabled("Linger=no\nLingerHint=yes-ish"));
    }

    /// A refusal must arrive as its consequence plus its repair, never as
    /// logind's stderr alone (ARCH principle 6).
    #[test]
    fn the_warning_names_the_consequence_and_the_exact_fix() {
        let w = linger_refusal_warning("alex", "Access denied\n");
        assert!(
            w.contains("loginctl enable-linger alex"),
            "the fix must be a command the user can paste: {w}"
        );
        assert!(
            w.contains("STOP AT LOGOUT"),
            "the consequence must be in plain words: {w}"
        );
        assert!(
            w.contains("ABSENT"),
            "and in mesh terms — the node leaves, it does not idle: {w}"
        );
        assert!(
            w.contains("Access denied"),
            "logind's own words survive, they are just not the whole message: {w}"
        );
        assert!(
            w.contains("WARNING"),
            "never reported as a failed install: {w}"
        );
    }

    /// An empty detail must not leave a dangling `()` in the sentence.
    #[test]
    fn the_warning_reads_cleanly_with_no_detail() {
        let w = linger_refusal_warning("alex", "   ");
        assert!(w.contains("for alex."), "{w}");
        assert!(!w.contains("()"), "{w}");
    }
}
