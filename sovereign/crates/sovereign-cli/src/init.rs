// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign init` — workspace setup for code intelligence.
//!
//! The new top-level entry point in the flat CLI namespace, renamed
//! from `sovereign project init`. After indexing finishes we
//! auto-spawn `sovereign serve --background` so the user gets a live
//! MCP server on `:9741` without typing a second command. The
//! daemon-takes-over check happens inside `serve_cmd::spawn_background`
//! — if the daemon already owns the port, we skip the spawn and tell
//! the user.
//!
//! ## Why the spawn lives here, not in cmd_init
//!
//! `cmd_init` is also the alias target for `sovereign project init`.
//! We don't want both `sovereign init` and `sovereign project init`
//! to spawn a server — only the new flat path should. Putting the
//! spawn here, in the `sovereign init` wrapper, keeps the alias path
//! a pure no-op on top of the original handler.

pub async fn run(args: &[String]) -> i32 {
    // `--help` is answered here, by the dispatcher, before any sibling
    // spawn — matching every other flat verb (serve_cmd, drift_cmd, …).
    // Help must never require the 240 MB sovereign-cli-dev sibling to be
    // built: before this guard, `sovereign init --help` blindly spawned
    // it and died with "cannot find sibling binary" when it wasn't.
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }

    // `project-init` lives in the sovereign-cli-dev sibling binary.
    // We spawn (not exec) so we can chain into `serve --background`
    // after a successful init.
    let exit = match std::process::Command::new(locate_dev_bin())
        .arg("project-init")
        .args(args)
        .status()
    {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("sovereign init: spawn sovereign-cli-dev: {e}");
            return 126;
        }
    };
    if exit != 0 {
        // Indexing failed — don't paper over it by lighting up a
        // server with no corpus to serve.
        return exit;
    }

    if should_skip_spawn(args) {
        return 0;
    }

    // Forward only the flags `serve` understands (--port, --data-dir,
    // --sovereign-dir). cmd_init has its own --no-scip / --name / etc
    // that mean nothing to serve and would trip its arg parser's
    // "unknown flag" warning — drop them.
    let serve_args = forward_serve_flags(args);
    let mut bg_args = vec!["--background".to_string()];
    bg_args.extend(serve_args);

    let _ = crate::serve_cmd::run(&bg_args).await;
    0
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign init",
    summary: "Index the current workspace for code intelligence, then start the MCP server.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign init [--no-serve] [--port N] [--data-dir DIR]   Index, then serve --background",
        ),
        crate::util::help::HelpSection::Notes(
            "Renamed from `sovereign project init` (the old name still works and forwards here). \
             After indexing succeeds, auto-spawns `sovereign serve --background` so a live MCP \
             server comes up on :9741 — pass --no-serve to skip that.",
        ),
    ],
};

/// Find the `sovereign-cli-dev` sibling. Mirrors the lookup in
/// `crate::dev_bin` but kept inline because `init` needs a
/// spawn-and-wait, not `exec`.
fn locate_dev_bin() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("SOVEREIGN_CLI_DEV_BIN") {
        return std::path::PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(real) = std::fs::canonicalize(&exe) {
            if let Some(dir) = real.parent() {
                let cand = dir.join("sovereign-cli-dev");
                if cand.is_file() {
                    return cand;
                }
            }
        }
    }
    std::path::PathBuf::from("sovereign-cli-dev")
}

/// Two ways to suppress the auto-spawn:
///
/// - `--no-serve` flag for users who want the legacy "init only"
///   behaviour without a background process.
/// - `SOVEREIGN_SPAWNED_BY_INIT=1` env var so a recursive invocation
///   from inside the spawned child can't trigger nested spawns. The
///   child sets this before exec'ing serve, but if anyone wedges
///   `sovereign init` inside the child for some reason, this stops
///   the loop.
fn should_skip_spawn(args: &[String]) -> bool {
    if std::env::var("SOVEREIGN_SPAWNED_BY_INIT").ok().as_deref() == Some("1") {
        return true;
    }
    args.iter().any(|a| a == "--no-serve")
}

fn forward_serve_flags(args: &[String]) -> Vec<String> {
    const PASS_THROUGH: &[&str] = &["--port", "--data-dir", "--sovereign-dir"];
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if PASS_THROUGH.contains(&a.as_str()) {
            out.push(a.clone());
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
                i += 1;
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_only_serve_flags() {
        let argv = vec![
            "--no-scip".to_string(),
            "--port".to_string(),
            "9741".to_string(),
            "--name".to_string(),
            "myproj".to_string(),
            "--data-dir".to_string(),
            "/tmp/x".to_string(),
        ];
        let forwarded = forward_serve_flags(&argv);
        assert_eq!(
            forwarded,
            vec![
                "--port".to_string(),
                "9741".to_string(),
                "--data-dir".to_string(),
                "/tmp/x".to_string(),
            ]
        );
    }

    #[test]
    fn no_serve_flag_skips_spawn() {
        assert!(should_skip_spawn(&["--no-serve".to_string()]));
        assert!(!should_skip_spawn(&[
            "--port".to_string(),
            "9741".to_string()
        ]));
    }
}
