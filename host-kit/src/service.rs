// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-user systemd unit writer: what a program's binary does to have the
//! OS start it at boot (FIVE_PROGRAMS §2c, §12 3a rung 4). Moved from
//! sovereign-service, whose svrn unit is its first caller; `svrn mesh up`'s
//! cw-rails unit is its second (pb-mesh-exit-transport). The caller supplies
//! the unit's name, path and text, and the words its warnings speak in, so
//! the kit names no program.
//!
//! A user unit lives inside `user@<uid>.service`, and systemd stops that
//! manager when the user's last session ends. So `WantedBy=default.target`
//! means "at login", not "at boot": without lingering the program dies at
//! logout and does not come back until someone logs in again.
//! `loginctl enable-linger <user>` is what makes the user manager start at
//! boot and outlive every session; it is also the only part of an install
//! that a distro's polkit may refuse, so it is a warning and never a fatal
//! error. Nothing here turns it back off: lingering is a property of the
//! USER, and another of their units may be living on it.
//!
//! The argv deciders and output parses are pure and compiled on every
//! platform, so a string is checkable from whatever host the suite runs on.

use std::path::{Path, PathBuf};

/// The two programs a user-unit install drives. [`Self::system`] is the
/// host's; a test points both at stand-ins.
#[derive(Debug, Clone)]
pub struct SystemdUser {
    systemctl: PathBuf,
    loginctl: PathBuf,
}

/// One unit, as its program spells it.
#[derive(Debug, Clone, Copy)]
pub struct UserUnit<'a> {
    /// The unit name, e.g. `svrnmesh.service`.
    pub name: &'a str,
    /// Where the unit file is written.
    pub path: &'a Path,
    /// The unit file's text, placeholders already substituted.
    pub content: &'a str,
    /// The prefix of every line the install prints (`svrnmesh`).
    pub prefix: &'a str,
    /// What stops at logout, in the warning's words (`daemon`).
    pub what: &'a str,
}

/// Whether the unit is started at boot, per `systemctl --user is-enabled`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitState {
    /// `enabled` (or `enabled-runtime`, `linked`, `alias`): the manager
    /// starts it.
    Enabled,
    /// A unit file the manager knows and will not start at boot, with the
    /// state it named (`disabled`, `masked`, `static`, …).
    Disabled(String),
    /// No unit file by this name.
    NotInstalled,
    /// The probe did not run or answered nothing readable; the text says why.
    Unknown(String),
}

impl SystemdUser {
    /// The host's `systemctl` and `loginctl`, resolved on `PATH`.
    pub fn system() -> Self {
        Self::at("systemctl", "loginctl")
    }

    /// Named programs, for a test's stand-ins.
    pub fn at(systemctl: impl Into<PathBuf>, loginctl: impl Into<PathBuf>) -> Self {
        Self {
            systemctl: systemctl.into(),
            loginctl: loginctl.into(),
        }
    }

    fn systemctl(&self, args: &[&str]) -> Result<std::process::Output, String> {
        std::process::Command::new(&self.systemctl)
            .args(args)
            .output()
            .map_err(|e| format!("systemctl {}: {e}", args.join(" ")))
    }

    /// Write the unit, reload the user manager, enable it (and start it now
    /// when `start_now`), then make the user's manager outlive their
    /// sessions ([`Self::ensure_linger`]). A lingering refusal warns and
    /// does not fail the install: the unit is correctly installed.
    pub fn install(&self, unit: &UserUnit<'_>, start_now: bool) -> Result<(), String> {
        if let Some(parent) = unit.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        std::fs::write(unit.path, unit.content)
            .map_err(|e| format!("write {}: {e}", unit.path.display()))?;
        let out = self.systemctl(&["--user", "daemon-reload"])?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!("systemctl daemon-reload failed: {}", stderr.trim()));
        }
        let out = self.systemctl(&enable_argv(unit.name, start_now))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let verb = if start_now { "enable --now" } else { "enable" };
            return Err(format!("systemctl {verb} failed: {}", stderr.trim()));
        }
        self.ensure_linger(unit.prefix, unit.what);
        Ok(())
    }

    /// Disable, stop and remove the unit. Idempotent: no file is `Ok`.
    pub fn uninstall(&self, name: &str, path: &Path) -> Result<(), String> {
        if !path.exists() {
            return Ok(());
        }
        let _ = self.systemctl(&["--user", "disable", "--now", name]);
        std::fs::remove_file(path).map_err(|e| format!("remove {}: {e}", path.display()))?;
        let _ = self.systemctl(&["--user", "daemon-reload"]);
        Ok(())
    }

    /// Whether the manager starts `name` at boot.
    pub fn state(&self, name: &str) -> UnitState {
        match self.systemctl(&["--user", "is-enabled", name]) {
            Ok(out) => parse_is_enabled(
                &String::from_utf8_lossy(&out.stdout),
                &String::from_utf8_lossy(&out.stderr),
            ),
            Err(e) => UnitState::Unknown(e),
        }
    }

    /// Make this user's systemd manager outlive their sessions, so the unit
    /// stays up across logout and comes back at boot. The probe runs first
    /// and an already-lingering user is left alone (a repeat polkit prompt
    /// on a desktop host is not harmless).
    pub fn ensure_linger(&self, prefix: &str, what: &str) {
        let Some(user) = current_username() else {
            eprintln!(
                "{}",
                linger_refusal_warning(
                    prefix,
                    what,
                    "$USER",
                    "cannot resolve the current username"
                )
            );
            return;
        };
        if let Ok(out) = std::process::Command::new(&self.loginctl)
            .args(linger_probe_argv(&user))
            .output()
        {
            if out.status.success() && linger_is_enabled(&String::from_utf8_lossy(&out.stdout)) {
                return;
            }
        }
        match std::process::Command::new(&self.loginctl)
            .args(linger_enable_argv(&user))
            .output()
        {
            Ok(out) if out.status.success() => eprintln!(
                "{prefix}: enabled lingering for {user} — the {what} now survives \
                 logout and starts at boot without a login"
            ),
            Ok(out) => eprintln!(
                "{}",
                linger_refusal_warning(prefix, what, &user, &String::from_utf8_lossy(&out.stderr))
            ),
            Err(e) => eprintln!(
                "{}",
                linger_refusal_warning(prefix, what, &user, &format!("spawn loginctl: {e}"))
            ),
        }
    }
}

/// `systemctl --user enable [--now] <name>`.
pub fn enable_argv(name: &str, start_now: bool) -> Vec<&str> {
    let mut argv = vec!["--user", "enable"];
    if start_now {
        argv.push("--now");
    }
    argv.push(name);
    argv
}

/// Read `systemctl --user is-enabled` output. The state word is the first
/// line of stdout; a unit file systemd cannot find prints `not-found` on
/// newer versions and names the missing file on stderr on older ones.
pub fn parse_is_enabled(stdout: &str, stderr: &str) -> UnitState {
    let word = stdout.lines().next().unwrap_or("").trim();
    match word {
        "enabled" | "enabled-runtime" | "linked" | "linked-runtime" | "alias" => UnitState::Enabled,
        "not-found" => UnitState::NotInstalled,
        "" if stderr.contains("No such file") || stderr.contains("not found") => {
            UnitState::NotInstalled
        }
        "" => UnitState::Unknown(format!(
            "is-enabled answered nothing on stdout: {}",
            stderr.trim()
        )),
        other => UnitState::Disabled(other.to_string()),
    }
}

/// `%` is a systemd unit-file specifier; escape it so a value containing one
/// survives the round-trip.
pub fn escape_specifiers(value: &str) -> String {
    value.replace('%', "%%")
}

/// PATH as seen by the shell that ran the install — the one environment
/// where the operator's toolchain is known to resolve. Service managers start
/// programs with a minimal PATH, which silently severed svrn's SCIP exporters
/// (live incident 2026-08-06). Capturing at install time generalizes across
/// toolchain managers instead of enumerating their directories.
pub fn captured_path() -> String {
    std::env::var("PATH")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".to_string())
}

/// Argv that asks logind "does this user's manager survive logout?".
///
/// `show-user --property=Linger` answers for a user with no active session
/// too, which is the state an install run from a remote shell is in.
pub fn linger_probe_argv(user: &str) -> Vec<String> {
    vec!["show-user".into(), user.into(), "--property=Linger".into()]
}

/// Argv that turns lingering on for one user. No `--now`, no elevation:
/// enabling it for ONESELF is what default polkit rules permit.
pub fn linger_enable_argv(user: &str) -> Vec<String> {
    vec!["enable-linger".into(), user.into()]
}

/// Is lingering already on, per `loginctl show-user` output?
///
/// Whole-line equality rather than `contains`: the block this parses also
/// carries the word in its negative form, so a laxer `contains("Linger")`
/// reads `Linger=no` as a yes and the install then skips the only step
/// that was the point.
pub fn linger_is_enabled(show_user_output: &str) -> bool {
    show_user_output
        .lines()
        .any(|line| line.trim() == "Linger=yes")
}

/// What the operator is told when lingering could not be turned on: the
/// absence in the words of its consequence, not in logind's (ARCH principle
/// 6). The manager's own stderr is kept, but the sentence that matters is
/// what the user loses and the one command that fixes it.
pub fn linger_refusal_warning(prefix: &str, what: &str, user: &str, detail: &str) -> String {
    let detail = detail.trim();
    let because = if detail.is_empty() {
        String::new()
    } else {
        format!(" ({detail})")
    };
    format!(
        "{prefix}: WARNING — could not enable lingering for {user}{because}.\n\
         {prefix}:   The {what} will STOP AT LOGOUT: systemd tears down your user\n\
         {prefix}:   manager when your last session ends, and it does not come back\n\
         {prefix}:   until you log in again. Peers see this node as ABSENT from the\n\
         {prefix}:   mesh for that whole window, not as busy.\n\
         {prefix}:   Fix it with:  loginctl enable-linger {user}\n\
         {prefix}:   (some distros' polkit rules require an administrator to run it)."
    )
}

/// Whose manager we are asking logind to keep alive. `$USER` is set by every
/// interactive shell; `id -un` covers the install run from something that is
/// not one.
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

#[cfg(test)]
#[path = "service/tests.rs"]
mod tests;
