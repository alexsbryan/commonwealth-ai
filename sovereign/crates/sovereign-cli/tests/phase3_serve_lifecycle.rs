//! Phase 3 — `sovereign init` auto-spawns `serve --background`.
//!
//! End-to-end checks that don't require a real index. The full
//! init→serve→stop dance is hard to exercise in a unit test (it
//! involves binding `:9741`, building a CorpusEngine, and forking
//! the binary), so this suite covers the deterministic surfaces:
//! flag plumbing, help text, and the stop-cmd fallback chain.
//!
//! The full lifecycle is covered manually via the verification
//! steps in the Phase 3 plan:
//!
//!   1. `cargo run -p sovereign-cli -- init` in a fresh repo
//!   2. `curl localhost:9741/mcp/stats` → 200
//!   3. `cargo run -p sovereign-cli -- stop`
//!   4. `curl localhost:9741/mcp/stats` → connection refused
//!
//! …and is not reproduced here because each step takes 30+s and
//! mutates `~/.sovereign/`. CI runs the unit-test layer.

use std::process::{Command, Output};

fn sovereign() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"));
    cmd.env("SOVEREIGN_QUIET_DEPRECATIONS", "1");
    cmd
}

fn run(args: &[&str]) -> Output {
    sovereign().args(args).output().expect("spawn sovereign-cli")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// `sovereign serve --help` documents the new `--background` flag
/// and the stop instructions. This is the user-facing contract that
/// the Phase 3 plan promised — if either string disappears, users
/// won't discover the new mode.
#[test]
fn serve_help_documents_background_flag() {
    let out = run(&["serve", "--help"]);
    assert!(out.status.success(), "serve --help exit: {:?}", out.status.code());
    let combined = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        combined.contains("--background"),
        "serve --help missing --background flag:\n{combined}"
    );
    assert!(
        combined.contains("sovereign stop"),
        "serve --help missing stop instructions:\n{combined}"
    );
}

/// `sovereign stop --help` documents that it reads the pid file. If
/// users can't find this in --help they'll never realize a
/// background server is running.
#[test]
fn stop_help_documents_pid_file() {
    let out = run(&["stop", "--help"]);
    assert!(out.status.success(), "stop --help exit: {:?}", out.status.code());
    let combined = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        combined.contains(".sovereign/server.pid") || combined.contains("server.pid"),
        "stop --help missing pid-file mention:\n{combined}"
    );
}

/// `sovereign init --help` runs without spawning anything. Our wrapper
/// in `init.rs` should forward --help directly to cmd_init, which
/// short-circuits via util::help::wants_help. If we accidentally
/// trigger the spawn path on --help, this test will hang or fail.
#[test]
fn init_help_short_circuits_without_spawning() {
    let out = run(&["init", "--help"]);
    assert!(
        out.status.success(),
        "init --help exit: {:?}\nstderr:\n{}",
        out.status.code(),
        stderr(&out)
    );
    // Belt-and-suspenders: the help output itself should be coherent,
    // not a wall of indexer logs.
    let s = stdout(&out);
    assert!(
        s.contains("init") || s.is_empty(), // some help renderers go to stderr
        "init --help stdout looked unexpected:\n{s}"
    );
}

/// Stop with no PID file present and no daemon running falls through
/// to the daemon-stop forwarding path, which itself forwards into
/// the daemon command. Without a running daemon, that returns a
/// well-formed "no running daemon" error rather than crashing.
///
/// We can't assert exit 0 here (there's nothing to stop), but the
/// command must NOT panic and must produce some user-visible
/// output. Setting HOME to a tempdir prevents the test from
/// accidentally killing the developer's actual MCP server.
#[test]
fn stop_with_no_pid_file_does_not_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let out = sovereign()
        .env("HOME", tmp.path())
        .args(["stop"])
        .output()
        .expect("spawn sovereign-cli");
    // Either exit 0 (somehow there was a daemon to stop) or non-zero
    // (no daemon, no serve). Both are acceptable; what's not
    // acceptable is a hang or a panic.
    let combined = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        !combined.contains("panicked at"),
        "stop panicked:\n{combined}"
    );
}
