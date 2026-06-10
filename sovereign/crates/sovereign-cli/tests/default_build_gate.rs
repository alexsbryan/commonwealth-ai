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
    "project",
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
            "`sovereign {verb}` should exit 2 in the default build, got {:?}\nstdout:\n{}\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("--features dev-tools"),
            "`sovereign {verb}` intercept should point at dev-tools; got:\n{err}"
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
