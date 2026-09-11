// SPDX-License-Identifier: AGPL-3.0-or-later
//! The desktop cannot BE a daemon — sv-surface `svt-2`'s floor instrument.
//!
//! # The state this makes unrepresentable
//!
//! A desktop binary that re-enters itself as the daemon. Until 2026-09-11
//! `main.rs` read `Launch::Daemon => exit(sovereign_cli_daemon::
//! daemon_child_main())` — the SAME entry as `svrn daemon run`, Tauri never
//! initialized — and `supervisor_setup.rs` spawned `current_exe()
//! --daemon-child` to reach it. The window supervised the daemon: restart
//! policy, backoff, health heartbeat, crash-loop breaker, shutdown budget,
//! exit handler. That is one component owning another's recovery, which is
//! the line ARCH principle 12 draws.
//!
//! # Why a source census and not a compile error
//!
//! The compiler already holds most of this door: `sovereign-cli-daemon` and
//! `sovereign-compute` are no longer dependencies of `sovereign-desktop`, so
//! `sovereign_cli_daemon::daemon_child_main()` does not resolve. That is the
//! structural half, and it is stronger than any test.
//!
//! What a compile error cannot catch is the RE-GRANT: someone adding the
//! dependency back to `Cargo.toml` to reach one function, at which point the
//! ability returns and nothing is red. Principle 12's own instruction is to
//! look where the ability is granted — the dependency, the import — not at
//! the sites that use it. This test looks there.
//!
//! # The failing inputs, each one watched red before this landed
//!
//! * `use sovereign_cli_daemon::…` (or `sovereign_compute::…`) anywhere in
//!   `src/` → `no_desktop_source_names_a_daemon_crate` fails, naming the
//!   file and line.
//! * Folding a daemon role into the GUI fall-through arm — the regression
//!   that LOOKS harmless, because the variant is still named and the match
//!   still compiles, while `--daemon-child` now opens a window →
//!   `every_daemon_role_is_refused_by_name` fails, naming the line.
//! * Dropping `ExitCode::FAILURE` or the `NOT_A_DAEMON` text, so the refusal
//!   is silent and launchd reads it as success →
//!   `the_refusal_is_not_silent` fails.
//! * Re-introducing a supervisor type in the desktop →
//!   `the_desktop_holds_no_supervisor` fails.
//!
//! This is a NEEDLE LIST over source text, with the weakness that shape
//! carries: it sees the names it knows. The dependency edge is the real
//! guard and `cargo tree -p sovereign-desktop` is how it is audited.

use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` under `src/`, as (path, contents).
fn rust_sources() -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        for entry in std::fs::read_dir(dir).expect("src/ is readable").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let body = std::fs::read_to_string(&path).expect("source is utf-8");
                out.push((path, body));
            }
        }
    }
    let mut out = Vec::new();
    walk(&src_dir(), &mut out);
    assert!(
        out.len() > 30,
        "the walk found only {} files — it is not reaching src/",
        out.len()
    );
    out
}

/// Lines with the `//` comment prefix stripped off, so prose ABOUT the
/// deletion (this file is full of it, and so is `main.rs`) does not read as
/// the deletion being undone. A `use` or a path expression is what grants
/// the ability; a sentence naming the crate grants nothing.
fn code_lines(body: &str) -> impl Iterator<Item = (usize, &str)> {
    body.lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l))
        .filter(|(_, l)| {
            let t = l.trim_start();
            !t.starts_with("//") && !t.starts_with("*") && !t.starts_with("#!")
        })
}

/// The two crates whose presence in this crate's tree IS the ability to be a
/// daemon or to supervise one.
///
/// `sovereign_compute` is the shared supervisor state machine. It is the
/// right dependency for `sovereign-cli-daemon`, which supervises the
/// daemon's OWN compute children — and the wrong one for a window. The
/// deletion is of the desktop CALLER, never of the shared supervisor.
const DAEMON_CRATES: &[&str] = &["sovereign_cli_daemon", "sovereign_compute"];

#[test]
fn no_desktop_source_names_a_daemon_crate() {
    let mut found: Vec<String> = Vec::new();
    for (path, body) in rust_sources() {
        for (line_no, line) in code_lines(&body) {
            for crate_name in DAEMON_CRATES {
                if line.contains(crate_name) {
                    found.push(format!("{}:{line_no}: {}", path.display(), line.trim()));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "sovereign-desktop reaches a daemon crate again — the desktop can BE a daemon \
         (or supervise one) as soon as this edge exists, whatever the call site does \
         with it (ARCH principle 12). Sites:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn the_desktop_holds_no_supervisor() {
    // The desktop's own supervision surface, by the names it had. A window
    // holding any of these is holding a daemon's lifecycle.
    const GONE: &[&str] = &[
        "supervisor_setup",
        "SupervisedDaemon",
        "SupervisorConfig",
        "SupervisorState",
        "stop_daemon_child",
    ];
    let mut found: Vec<String> = Vec::new();
    for (path, body) in rust_sources() {
        for (line_no, line) in code_lines(&body) {
            for name in GONE {
                if line.contains(name) {
                    found.push(format!("{}:{line_no}: {}", path.display(), line.trim()));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "a daemon supervisor is back in sovereign-desktop. The daemon owns its own \
         lifetime; a client owns what it shows and what it asks for. Sites:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn every_daemon_role_is_refused_by_name() {
    let main_rs = std::fs::read_to_string(src_dir().join("main.rs")).expect("main.rs is readable");
    let code: Vec<(usize, &str)> = code_lines(&main_rs).collect();

    // `Launch::Desktop` names the GUI fall-through arm (`… => {}`). It also
    // names the `Launch::parse` default, which is the same answer — this
    // binary IS the desktop — so either occurrence is a line that means "open
    // a window", and a daemon role must never share one.
    for role in [
        "Launch::Daemon",
        "Launch::ComputeChild",
        "Launch::RpcWorker",
        "Launch::Worker",
    ] {
        assert!(
            code.iter().any(|(_, l)| l.contains(role)),
            "{role} is no longer named in main.rs's dispatch. An unnamed variant falls \
             into the GUI arm, so `--daemon-child` would open a WINDOW."
        );
        for (line_no, line) in &code {
            assert!(
                !(line.contains(role) && line.contains("Launch::Desktop")),
                "main.rs:{line_no} puts {role} on the same arm as Launch::Desktop — that \
                 argv form opens a window instead of being refused:\n  {}",
                line.trim()
            );
        }
    }
}

#[test]
fn the_refusal_is_not_silent() {
    let main_rs = std::fs::read_to_string(src_dir().join("main.rs")).expect("main.rs is readable");
    // An arm that refuses without saying so, or that exits 0, is the silent
    // substitution principle 6 forbids: a stale launchd plist pointing at this
    // binary would look like a daemon that started and did nothing.
    assert!(
        main_rs.contains("const NOT_A_DAEMON"),
        "main.rs no longer defines NOT_A_DAEMON — the refusal has no text."
    );
    assert!(
        code_lines(&main_rs).any(|(_, l)| l.contains("ExitCode::FAILURE")),
        "main.rs never returns ExitCode::FAILURE — a refused daemon role is exiting 0, \
         which reads as success to launchd, systemd and every wrapper script."
    );
}
