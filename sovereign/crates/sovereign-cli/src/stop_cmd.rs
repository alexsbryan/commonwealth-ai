// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn stop` — stop the background MCP server or daemon.
//!
//! New top-level command. `svrn init` spawns `svrn serve
//! --background`, which writes the child PID to two pointer files:
//!
//! - `<project>/.sovereign/server.pid` — for `svrn stop`
//!   invocations from inside the project directory tree.
//! - `~/.sovereign/server.pid` — for invocations from anywhere
//!   else, and as the canonical handle the daemon checks before
//!   binding `:9741`.
//!
//! This command's resolve order:
//!
//! 1. Walk up from cwd looking for a `.sovereign/server.pid`.
//! 2. If none found, try `~/.sovereign/server.pid`.
//! 3. If neither yields a live PID, forward to `svrn daemon stop`
//!    in case the daemon is what's actually holding the port.

use std::path::PathBuf;

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }

    // Resolve the project's .sovereign/server.pid by walking up from
    // cwd, then fall back to ~/.sovereign/server.pid for invocations
    // from outside any project tree. Either pointer is authoritative
    // — they're written together by `serve --background`.
    let candidates = pid_file_candidates();
    for path in &candidates {
        match try_stop_via_pid_file(path) {
            StopOutcome::Stopped => return 0,
            StopOutcome::Stale => continue,
            StopOutcome::Missing => continue,
            StopOutcome::Malformed => return 1,
            StopOutcome::ReadError(code) => return code,
        }
    }

    // Fallback: maybe the daemon owns :9741 instead of a standalone
    // serve. Forward to `daemon stop` in the sovereign-cli-daemon
    // sibling, with the original args.
    let mut forwarded = vec!["stop".to_string()];
    forwarded.extend(args.iter().cloned());
    crate::daemon_bin::exec("daemon", &forwarded)
}

/// Ordered list of pid-file paths to try. The cwd walk-up is checked
/// first because a project-local pid file is always more specific
/// than the home-dir one — and the home-dir file is the canonical
/// fallback when stop is invoked from outside any project tree.
fn pid_file_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        let mut current = cwd;
        loop {
            let candidate = current.join(".sovereign").join("server.pid");
            if candidate.exists() {
                out.push(candidate);
                break;
            }
            match current.parent() {
                Some(p) => current = p.to_path_buf(),
                None => break,
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        let home_pid = home.join(".sovereign").join("server.pid");
        if home_pid.exists() && !out.iter().any(|p| p == &home_pid) {
            out.push(home_pid);
        }
    }
    out
}

#[derive(Debug)]
enum StopOutcome {
    Stopped,
    Stale,
    Missing,
    Malformed,
    ReadError(i32),
}

fn try_stop_via_pid_file(path: &std::path::Path) -> StopOutcome {
    let contents = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return StopOutcome::Missing,
        Err(e) => {
            eprintln!("  read {}: {e}", path.display());
            return StopOutcome::ReadError(1);
        }
    };
    let trimmed = contents.trim();
    let Ok(pid) = trimmed.parse::<i32>() else {
        eprintln!(
            "  malformed pid file at {} (got {trimmed:?})",
            path.display()
        );
        return StopOutcome::Malformed;
    };
    if signal_term(pid) {
        let _ = std::fs::remove_file(path);
        eprintln!("  stopped MCP server (pid {pid}).");
        StopOutcome::Stopped
    } else {
        eprintln!(
            "  stale pid file at {} — process {pid} not running.",
            path.display()
        );
        let _ = std::fs::remove_file(path);
        StopOutcome::Stale
    }
}

#[cfg(unix)]
fn signal_term(pid: i32) -> bool {
    // Shell out to /bin/kill rather than pulling in `libc` for one
    // syscall. `kill -TERM <pid>` is POSIX-portable; non-zero exit
    // means the process didn't accept the signal (gone or unowned),
    // which we surface to the caller as "stale pid".
    std::process::Command::new("/bin/kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn signal_term(_pid: i32) -> bool {
    eprintln!("  sovereign stop: process termination is unimplemented on this platform.");
    false
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn stop",
    summary: "Stop the background MCP server or the running daemon.",
    sections: &[
        crate::util::help::HelpSection::Usage("svrn stop"),
        crate::util::help::HelpSection::Notes(
            "Reads .sovereign/server.pid (written by `svrn init` in Phase 3); \
             falls back to `svrn daemon stop` when the daemon owns the port.",
        ),
    ],
};
