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
    let exit = crate::project_cmd::cmd_init(args).await;
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
        assert!(!should_skip_spawn(&["--port".to_string(), "9741".to_string()]));
    }
}
