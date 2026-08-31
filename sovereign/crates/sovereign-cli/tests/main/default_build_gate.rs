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
    // `code` and `project` deliberately left out — both are split surfaces.
    // See `project_is_served_in_process` and `code_refuses_without_code_intel`.
    "atos",
    "tools",
    "status",
    "charter",
    "design",
    "plan",
    "amend",
    "milestone",
    "drift",
    "audit",
    "serve",
    // `init` left the gate 2026-08-07 — `cmd_init` ships in the dispatcher
    // under `code-intel`. See `init_refuses_without_code_intel` and its
    // positive twin `shipped_build_serves_init`.
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
    // `init` is NOT in this loop: it is served in-process only under
    // `code-intel`, which this build may or may not have. Its own pair of
    // tests below covers both shapes.
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

/// This build has neither `dev-tools` nor `code-intel`, so `svrn code` must
/// refuse cleanly — naming `code-intel`, never falling through to a missing
/// sibling. The SHIPPED binary is built WITH `code-intel`
/// (`scripts/release-cli-local.sh`), so a real user does not see this path;
/// it exists so a developer who omits the feature gets told which one.
/// Scoped to the build it describes. WATCHED FAIL (§18.1): without this cfg
/// the test fails under `--features code-intel` — correctly, because there is
/// no refusal to assert once the verb is present. That failure is the proof
/// the assertion is real rather than vacuous.
#[cfg(not(feature = "code-intel"))]
#[test]
fn code_refuses_without_code_intel() {
    let out = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"))
        .args(["code", "index", "."])
        .env("SOVEREIGN_NO_STALE_WARN", "1")
        .output()
        .expect("spawn sovereign-cli");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("code-intel"),
        "refusal must name the feature that provides it, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("cannot find sibling binary"),
        "must refuse here, not fall through to the sibling, got:\n{stderr}"
    );
}

/// `svrn init` without an indexer must say so and stop — not half-run, and not
/// hunt for the `sovereign-cli-dev` sibling it used to spawn.
///
/// Note this drives bare `init`, not `init --help`: the help guard answers
/// before the feature branch, so `--help` exits 0 in BOTH build shapes and
/// would assert nothing. WATCHED FAIL (§18.1) — dropping the `cfg` makes this
/// fail under `--features code-intel`, correctly, because that build really
/// does run init.
#[cfg(not(feature = "code-intel"))]
#[test]
fn init_refuses_without_code_intel() {
    let tmp = std::env::temp_dir().join("svrn-init-gate-no-code-intel");
    std::fs::create_dir_all(&tmp).expect("temp dir");
    let out = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"))
        .arg("init")
        .current_dir(&tmp)
        .env("SOVEREIGN_NO_STALE_WARN", "1")
        .output()
        .expect("spawn sovereign-cli");
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("code-intel"),
        "refusal must name the feature that provides it, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("cannot find sibling binary") && !stderr.contains("sovereign-cli-dev"),
        "must refuse here, not reach for the workbench sibling, got:\n{stderr}"
    );
}

/// The positive twin: in the shape the release tarball ships, BOTH spellings
/// of init are served in-process. `project init` is the load-bearing assertion
/// — unlike `init --help` it exits 2 when the verb is absent, so it can tell
/// the two builds apart.
#[cfg(feature = "code-intel")]
#[test]
fn shipped_build_serves_init() {
    for args in [
        ["init", "--help"].as_slice(),
        ["project", "init", "--help"].as_slice(),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"))
            .args(args)
            .env("SOVEREIGN_NO_STALE_WARN", "1")
            .output()
            .expect("spawn sovereign-cli");
        assert_eq!(
            out.status.code(),
            Some(0),
            "`svrn {}` must be served in-process, got {:?}\nstderr:\n{}",
            args.join(" "),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr),
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("not available in this build")
                && !stderr.contains("cannot find sibling binary"),
            "`svrn {}` fell through instead of running here, got:\n{stderr}",
            args.join(" "),
        );
    }
}

/// The positive twin of `code_refuses_without_code_intel`: this is the build
/// shape the release tarball ships (`--features code-intel`, no `dev-tools`),
/// so `code index` and `refresh` must be served here with no sibling present.
/// If either regresses, a `curl | sh` user loses the ability to index a repo —
/// the defect this whole port exists to fix.
#[cfg(feature = "code-intel")]
#[test]
fn shipped_build_serves_code_index_and_refresh() {
    for args in [
        ["code", "index", "--help"].as_slice(),
        ["refresh", "--help"].as_slice(),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"))
            .args(args)
            .env("SOVEREIGN_NO_STALE_WARN", "1")
            .output()
            .expect("spawn sovereign-cli");
        assert_eq!(
            out.status.code(),
            Some(0),
            "`svrn {}` must exit 0 in a code-intel build, got {:?}\nstderr:\n{}",
            args.join(" "),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("cannot find sibling binary"),
            "`svrn {}` fell through to the sibling instead of running here",
            args.join(" "),
        );
    }
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
