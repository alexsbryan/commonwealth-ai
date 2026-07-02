// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn serve` — foreground or background MCP server.
//!
//! Renamed from `svrn project serve` per the CLI refactor plan.
//!
//! Two run modes:
//!
//! - **Foreground (default)** — same as the legacy
//!   `svrn project serve` path. Blocks the terminal; ctrl-c
//!   stops the server. Forwards directly to
//!   [`crate::project_cmd::cmd_serve`].
//!
//! - **`--background`** — fork+exec the same binary with `serve`
//!   (no `--background`), redirect stdout/stderr to a rotating log
//!   under `~/.sovereign/logs/serve.log`, write the child PID to
//!   the project's `.sovereign/server.pid` AND `~/.sovereign/server.pid`,
//!   detach from the parent's controlling terminal via `setsid()`,
//!   and exit 0 once the child is up. The dual-PID-file write lets
//!   `svrn stop` find the process from inside *or* outside the
//!   project tree, and lets `svrn daemon` take over `:9741`
//!   without guessing.
//!
//! ## Why setsid()
//!
//! Without `setsid()` the spawned child stays in the parent shell's
//! session. Closing the launching terminal sends SIGHUP to every
//! process in the session — including our orphaned child — which
//! kills the MCP server as soon as the user closes the window that
//! ran `svrn init`. `setsid()` puts the child in its own
//! session with no controlling terminal, immune to that signal
//! cascade.

use std::path::{Path, PathBuf};

/// Public entry point. The `--background` flag is parsed here so the
/// foreground path stays a thin delegation to `cmd_serve`.
pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }

    let (mode, forwarded) = parse_mode(args);
    match mode {
        // `project-serve` lives in the sovereign-cli-dev sibling now.
        Mode::Foreground => crate::dev_bin::exec("project-serve", &forwarded),
        Mode::Background => match spawn_background(&forwarded).await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("svrn serve --background: {e}");
                1
            }
        },
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Foreground,
    Background,
}

/// Strip `--background` (anywhere in argv) and return whatever's left
/// to be forwarded to the foreground path. Keeps order-stable so the
/// child sees the same flags the user typed minus our wrapper switch.
fn parse_mode(args: &[String]) -> (Mode, Vec<String>) {
    let mut mode = Mode::Foreground;
    let mut forwarded = Vec::with_capacity(args.len());
    for a in args {
        if a == "--background" {
            mode = Mode::Background;
        } else {
            forwarded.push(a.clone());
        }
    }
    (mode, forwarded)
}

/// Fork+exec the current binary with `serve <forwarded args>` and
/// return as soon as we've confirmed the child is alive. The child
/// performs the actual MCP-server work via `cmd_serve` in the
/// foreground.
async fn spawn_background(forwarded: &[String]) -> Result<(), String> {
    use std::process::{Command, Stdio};

    if daemon_is_running_via_dev_bin() {
        eprintln!("  daemon is already serving :9741 — no standalone serve needed.");
        return Ok(());
    }

    // Resolve the .sovereign dir the same way every other handler
    // does so the project pid file lands next to project.toml /
    // notes.db / features.db.
    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    let sovereign_dir = sovereign_cli_shared::repo::find_sovereign_dir(&cwd)
        .or_else(|| sovereign_cli_shared::repo::find_repo_root().map(|r| r.join(".sovereign")))
        .unwrap_or_else(|| cwd.join(".sovereign"));
    std::fs::create_dir_all(&sovereign_dir)
        .map_err(|e| format!("create {}: {e}", sovereign_dir.display()))?;

    let project_pid_path = sovereign_dir.join("server.pid");
    let home_pid_path = home_pid_path()?;
    let log_path = serve_log_path()?;

    // Refuse to re-spawn on top of a live serve. A stale PID (process
    // gone, file lingered) is fine — we overwrite below.
    if let Some(pid) = read_pid(&project_pid_path).or_else(|| read_pid(&home_pid_path)) {
        if process_alive(pid) {
            eprintln!("  serve already running (pid {pid}). Use `svrn stop` first.");
            return Ok(());
        }
        // Stale: clean both pointers; the new spawn will overwrite.
        let _ = std::fs::remove_file(&project_pid_path);
        let _ = std::fs::remove_file(&home_pid_path);
    }

    // The child writes here directly — open with append so the parent
    // can rotate without coordinating with the child. Best-effort
    // truncation cap below in `truncate_if_oversize`.
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    truncate_if_oversize(&log_path);

    let log_for_stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("open log {}: {e}", log_path.display()))?;
    let log_for_stderr = log_for_stdout
        .try_clone()
        .map_err(|e| format!("clone log fd: {e}"))?;

    // Spawn the sovereign-cli-dev sibling directly with
    // `project-serve` rather than re-execing through sovereign-cli.
    // Saves one exec hop and one process layer in the pgrep tree.
    let dev_bin = locate_dev_bin_for_spawn()
        .ok_or_else(|| "sovereign-cli-dev sibling binary not found".to_string())?;

    let mut cmd = Command::new(&dev_bin);
    cmd.arg("project-serve");
    for a in forwarded {
        cmd.arg(a);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log_for_stdout))
        .stderr(Stdio::from(log_for_stderr));

    // Also propagate the deprecation-banner suppress so the child
    // (which won't ever print one anyway) doesn't accidentally
    // re-introduce one if its argv path triggers an alias dispatch.
    cmd.env("SOVEREIGN_SPAWNED_BY_INIT", "1");

    #[cfg(unix)]
    detach_from_session(&mut cmd);

    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn `{}`: {e}", dev_bin.display()))?;
    let pid = child.id() as i32;

    // Don't `wait()` on the handle — that would block until the
    // child exits. Just drop it; on Unix, dropping a `Child` does
    // not kill the process.
    drop(child);

    write_pid(&project_pid_path, pid)?;
    write_pid(&home_pid_path, pid)?;

    // Confirm the child is reachable. Five-second budget (25×200ms)
    // is plenty for axum to bind on a warm machine. Honour any
    // `--port <n>` the user forwarded so the probe doesn't poll a
    // different port than the child is binding.
    let probe_port = port_from_args(forwarded).unwrap_or(9741);
    let alive_url = format!("http://127.0.0.1:{probe_port}/mcp/stats");
    let mut up = false;
    for _ in 0..25 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if !process_alive(pid) {
            break;
        }
        if reachable(&alive_url).await {
            up = true;
            break;
        }
    }

    if !up {
        eprintln!(
            "  serve spawned (pid {pid}) but {alive_url} did not respond within 5s.\n  \
             Check {} for errors.",
            log_path.display()
        );
        return Ok(());
    }

    eprintln!();
    eprintln!("  MCP server: http://localhost:{probe_port}/mcp (pid {pid})");
    eprintln!("  Log: {}", log_path.display());
    eprintln!("  Stop: sovereign stop");
    Ok(())
}

/// Pull `--port <n>` out of forwarded argv. Returns `None` for the
/// default-port case (no flag, or malformed value).
fn port_from_args(args: &[String]) -> Option<u16> {
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a == "--port" {
            return iter.next().and_then(|s| s.parse().ok());
        }
    }
    None
}

#[cfg(unix)]
fn detach_from_session(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: `pre_exec` runs in the child after fork() but before
    // exec(). We call only async-signal-safe functions (setsid is
    // explicitly listed in POSIX.1-2008). No allocation, no mutex,
    // no Rust runtime state touched. Returning a Result preserves
    // any error from setsid as an io::Error in the parent.
    unsafe {
        cmd.pre_exec(|| {
            // setsid() == -1 means "already a session leader",
            // which can happen if the parent is already detached
            // (e.g. systemd-launched daemon spawning subprocesses).
            // We tolerate that case; the child will inherit a
            // session that's already detached, which is what we
            // want.
            if libc::setsid() == -1 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::EPERM) {
                    return Err(err);
                }
            }
            Ok(())
        });
    }
}

fn home_pid_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("could not resolve $HOME")?;
    let dir = home.join(".sovereign");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    Ok(dir.join("server.pid"))
}

fn serve_log_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("could not resolve $HOME")?;
    Ok(home.join(".sovereign").join("logs").join("serve.log"))
}

/// Cap the log at ~5 MiB so a long-running misbehaving serve doesn't
/// fill the disk. Cheap atomic rename to `.1` retains the previous
/// run's tail; older rotations are dropped.
fn truncate_if_oversize(path: &Path) {
    const MAX: u64 = 5 * 1024 * 1024;
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > MAX {
            let backup = path.with_extension("log.1");
            let _ = std::fs::rename(path, backup);
        }
    }
}

fn write_pid(path: &Path, pid: i32) -> Result<(), String> {
    std::fs::write(path, format!("{pid}\n")).map_err(|e| format!("write {}: {e}", path.display()))
}

fn read_pid(path: &Path) -> Option<i32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
}

#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    // kill(pid, 0) — no signal sent, just permission check. Returns
    // 0 if the process exists and we have permission to signal it.
    // -1 with ESRCH means "no such pid"; -1 with EPERM means "exists
    // but we can't signal it" — for our purposes still "alive."
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if r == 0 {
        return true;
    }
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    )
}

#[cfg(not(unix))]
fn process_alive(_pid: i32) -> bool {
    false
}

/// Locate the `sovereign-cli-dev` sibling for the background spawn.
/// Same lookup shape as `crate::dev_bin::exec` but returns the
/// `PathBuf` rather than execing.
fn locate_dev_bin_for_spawn() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SOVEREIGN_CLI_DEV_BIN") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(real) = std::fs::canonicalize(&exe) {
            if let Some(dir) = real.parent() {
                let cand = dir.join("sovereign-cli-dev");
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    which::which("sovereign-cli-dev").ok()
}

/// Bool wrapper over `crate::dev_bin::exec("project-daemon-is-running",
/// &[])`. The sibling returns 0 if a daemon is serving :9741, 1 if not.
/// We spawn (not exec) to keep the parent alive.
fn daemon_is_running_via_dev_bin() -> bool {
    let Some(bin) = locate_dev_bin_for_spawn() else {
        return false;
    };
    std::process::Command::new(&bin)
        .arg("project-daemon-is-running")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn reachable(url: &str) -> bool {
    match reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
    {
        Ok(c) => c.get(url).send().await.is_ok(),
        Err(_) => false,
    }
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn serve",
    summary: "Run the MCP code-intelligence server. \
         Default is foreground; pass --background to detach.",
    sections: &[
        crate::util::help::HelpSection::Usage("svrn serve [--background] [--port <n>]"),
        crate::util::help::HelpSection::Notes(
            "Foreground (default): blocks; ctrl-c stops. \
             Background: detaches via setsid; pid in .sovereign/server.pid; \
             log at ~/.sovereign/logs/serve.log. Stop with `svrn stop`.",
        ),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_default_is_foreground() {
        let (mode, args) = parse_mode(&[]);
        assert_eq!(mode, Mode::Foreground);
        assert!(args.is_empty());
    }

    #[test]
    fn parse_mode_strips_background_flag() {
        let argv = vec![
            "--background".to_string(),
            "--port".to_string(),
            "9741".to_string(),
        ];
        let (mode, args) = parse_mode(&argv);
        assert_eq!(mode, Mode::Background);
        assert_eq!(args, vec!["--port".to_string(), "9741".to_string()]);
    }

    #[test]
    fn parse_mode_background_can_appear_anywhere() {
        let argv = vec![
            "--port".to_string(),
            "9741".to_string(),
            "--background".to_string(),
        ];
        let (mode, args) = parse_mode(&argv);
        assert_eq!(mode, Mode::Background);
        assert_eq!(args, vec!["--port".to_string(), "9741".to_string()]);
    }

    #[test]
    fn port_from_args_extracts_explicit_port() {
        let argv = vec!["--port".to_string(), "9999".to_string()];
        assert_eq!(port_from_args(&argv), Some(9999));
    }

    #[test]
    fn port_from_args_returns_none_when_unset() {
        let argv: Vec<String> = vec![];
        assert_eq!(port_from_args(&argv), None);
        let argv = vec!["--data-dir".to_string(), "/tmp/x".to_string()];
        assert_eq!(port_from_args(&argv), None);
    }

    #[test]
    fn port_from_args_returns_none_for_malformed_value() {
        let argv = vec!["--port".to_string(), "not-a-number".to_string()];
        assert_eq!(port_from_args(&argv), None);
    }

    #[test]
    fn read_pid_handles_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_pid(&dir.path().join("missing.pid")).is_none());
    }

    #[test]
    fn write_then_read_pid_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.pid");
        write_pid(&path, 12345).unwrap();
        assert_eq!(read_pid(&path), Some(12345));
    }

    #[test]
    #[cfg(unix)]
    fn process_alive_for_self() {
        let me = std::process::id() as i32;
        assert!(process_alive(me));
    }

    #[test]
    #[cfg(unix)]
    fn process_alive_for_unlikely_pid() {
        // PID 1 is init, pid 0 is the kernel scheduler; using a
        // very high pid keeps the test from depending on whichever
        // unrelated process happens to live at any given low pid.
        // 999_999 is well above the default Linux pid_max (32_768)
        // and macOS's default cap.
        assert!(!process_alive(999_999));
    }
}
