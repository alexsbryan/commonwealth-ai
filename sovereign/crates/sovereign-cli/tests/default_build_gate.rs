// SPDX-License-Identifier: AGPL-3.0-or-later
//! Default-build intercept contract for the developer toolchain.
//!
//! The default (end-user) build gates the dev verbs out: `async_main`
//! intercepts any first arg in `DEV_VERBS` and exits 2 with a pointer
//! at `--features dev-tools` BEFORE dispatch (so no sibling binary is
//! ever needed). This suite is the `cfg(not(...))` twin of
//! `aliases.rs` / `phase6_retired_ceremony.rs`: those verify the verbs
//! work when the feature is on; this verifies they are cleanly
//! refused when it is off.
#![cfg(not(feature = "dev-tools"))]

use std::process::Command;

/// Mirror of `DEV_VERBS` in main.rs (a bin-crate constant, not
/// importable from an integration test). If a verb leaves the gate,
/// this test fails and the two lists get re-synced — that coupling is
/// the point.
const DEV_VERBS: &[&str] = &[
    "code",
    // `project` deliberately left out — see `project_is_served_in_process`.
    "atos",
    "tools",
    "status",
    "charter",
    "design",
    "plan",
    "amend",
    "refresh",
    "milestone",
    "drift",
    "audit",
    "serve",
    "init",
    "notes",
    "reflect",
    "rough-edges",
    "git-archaeology",
    "archaeology-eval",
    "agent-bench",
    "claim",
    "nudge",
];

/// The registry half of `project` is served BY THIS BINARY in the default
/// build — no sibling, no feature flag. This is the negative control for the
/// gate above: if someone re-adds `project` to `DEV_VERBS`, these stop
/// working and a `curl | sh` user loses the only route to the daemon's code
/// intelligence.
///
/// `--help` short-circuits before any daemon call, so this asserts the
/// dispatch without needing a live daemon.
#[test]
fn project_is_served_in_process() {
    for sub in ["register", "unregister", "list", "watch"] {
        let out = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"))
            .args(["project", sub, "--help"])
            .env("SOVEREIGN_NO_STALE_WARN", "1")
            .output()
            .expect("spawn sovereign-cli");
        assert_eq!(
            out.status.code(),
            Some(0),
            "`svrn project {sub} --help` must be served in-process in the default build, got {:?}\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("cannot find sibling binary"),
            "`svrn project {sub}` fell through to the sibling instead of running here",
        );
    }
}

/// A `project` subcommand that still lives in the workbench refuses with a
/// message naming what this build CAN do — not with the old
/// `cargo build --features dev-tools` pointer, which a user who installed
/// via `curl | sh` has no checkout to act on.
#[test]
fn workbench_project_subcommands_refuse_with_a_usable_alternative() {
    let out = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"))
        .args(["project", "serve"])
        // Mute the dev-machine banner from `sibling::warn_if_dev_tools_missing`.
        // It fires only when a `sovereign-cli-dev` sits beside the binary
        // (sibling.rs:119-121) — true in a dev target dir, never true for the
        // `curl | sh` user this test speaks for. Without this the assertion
        // below trips on that banner's `cargo build` text and the test passes
        // in CI while failing locally, which is worse than failing outright.
        .env("SOVEREIGN_NO_STALE_WARN", "1")
        .output()
        .expect("spawn sovereign-cli");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("svrn project register"),
        "refusal must name the subcommands that do work here, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("cargo build"),
        "refusal must not point at a repo build the user does not have, got:\n{stderr}"
    );
}

/// Every dev verb is intercepted with exit 2 and a stderr pointer at
/// the rebuild flag — never a panic, a hang, or a fall-through into
/// "cannot find sibling binary".
#[test]
fn dev_verbs_are_intercepted_with_rebuild_pointer() {
    for verb in DEV_VERBS {
        let out = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"))
            .arg(verb)
            .output()
            .expect("spawn sovereign-cli");
        assert_eq!(
            out.status.code(),
            Some(2),
            "`svrn {verb}` should exit 2 in the default build, got {:?}\nstdout:\n{}\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("--features dev-tools"),
            "`svrn {verb}` intercept should point at dev-tools; got:\n{err}"
        );
    }
}

/// The intercept must not swallow non-dev verbs: top-level `--help`
/// still works in the default build.
#[test]
fn top_level_help_still_works() {
    let out = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"))
        .arg("--help")
        .output()
        .expect("spawn sovereign-cli");
    assert!(
        out.status.success(),
        "--help should exit 0 in the default build, got {:?}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
}
