// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn install-service` — register the daemon with launchd /
//! systemd as a user-level service.
//!
//! Phase 4 split: prior to this, service registration was an implicit
//! step inside `svrn setup`. The user could never run setup
//! without also installing the service, even on dev boxes where they
//! wanted to test the daemon in the foreground first. Splitting this
//! out makes service registration explicit + scriptable, and lets
//! `svrn daemon` work as a foreground process for casual use.
//!
//! The actual platform-specific registration (writing the plist on
//! macOS, the unit file on Linux, then loading it) lives in
//! `service_install.rs`. This module is the CLI surface — it
//! resolves the binary path, calls `install_service`, and prints
//! human-readable feedback.

use crate::service_install;

pub async fn run(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP);
        return 0;
    }
    if let Some(unknown) = args.iter().find(|a| a.starts_with('-')) {
        eprintln!("error: unknown flag '{unknown}' for `svrn install-service`");
        sovereign_cli_shared::help::print(&HELP);
        return 2;
    }

    let bin_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot resolve current binary path: {e}");
            eprintln!("hint: pass the binary path explicitly is not yet supported.");
            return 1;
        }
    };

    // Double-start guard. Install does unload→load, and launchd
    // immediately spawns `daemon run` — which loses the :9741 bind to
    // a manually-started daemon and then crash-loops under
    // `KeepAlive.SuccessfulExit = false`. Refuse loudly instead (same
    // posture as `daemon start`'s bind-collision detector). The
    // pidfile is written ONLY by the manual `daemon start` path, so a
    // live pid there means exactly the conflicting case.
    if let Some(pid) = crate::daemon_cmd::read_daemon_pid() {
        eprintln!(
            "error: a manually-started daemon is running (pid {pid}).\n\
             Installing the service now would spawn a second daemon that \
             loses the :9741 bind and crash-loops.\n\
             Stop it first, then re-run:\n\
             \n  svrn daemon stop && svrn install-service\n"
        );
        return 1;
    }

    eprintln!("Registering {} as a system service…", bin_path.display());
    match service_install::install_service(&bin_path) {
        Ok(()) => {
            eprintln!("\u{2713} service registered.");
            // The service manager spawns `svrn daemon run` in
            // the background. Print one line so the user knows the
            // daemon should already be coming up; they can verify
            // with `svrn daemon status` (or `svrn status`).
            eprintln!("  Verify with: svrn daemon status");
            0
        }
        Err(e) => {
            eprintln!("error: service registration failed: {e}");
            // Common reasons: launchctl/systemctl not on PATH, no
            // ~/Library/LaunchAgents permission, etc. Tell the user
            // they can still run the daemon manually so they aren't
            // stuck.
            eprintln!("hint: you can still run the daemon foreground via `svrn daemon`.");
            1
        }
    }
}

const HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn install-service",
    summary: "Register the svrnmesh daemon with launchd (macOS) or systemd (Linux).",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("svrn install-service"),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Writes a launchd plist (macOS) or systemd user unit (Linux) that runs \
             `svrn daemon run` on login + restarts on crash. The daemon's setup \
             wizard must have been completed first (config at ~/.svrnmesh/config.toml). \
             Run `svrn daemon --setup-only` if not.",
        ),
    ],
};
