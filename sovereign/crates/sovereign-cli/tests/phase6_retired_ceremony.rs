//! Phase 6 retirement tests.
//!
//! The CLI refactor's Phase 6 retires the explicit "founding" /
//! "provision" ceremony. Both surfaces are now no-op + deprecation
//! banner — the user types them, sees the banner, and learns that
//! the new flow is just "init → write spec → commit → work."
//!
//! These tests spawn the actual `sovereign-cli` binary so the
//! retirement banners are exercised end-to-end. Style mirrors
//! `aliases.rs` (sibling test file).

use std::process::{Command, Output};

fn sovereign() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"));
    // Ensure the retired-banner pointer text fires regardless of
    // the developer's local env. `announce_retired` always prints
    // the title line; the helpful pointer is suppressed in quiet
    // mode, so we set quiet off here to test the full surface.
    cmd.env("SOVEREIGN_QUIET_DEPRECATIONS", "0");
    cmd
}

fn run(args: &[&str]) -> Output {
    sovereign().args(args).output().expect("spawn sovereign-cli")
}

/// Retired `project` / `atos` verbs print their banner from inside the
/// `sovereign-cli-dev` sibling (the dispatcher forwards there). `cargo
/// test` builds the dispatcher but not the sibling bin, so without a
/// prior `cargo build --bins` we skip rather than false-fail with
/// "cannot find sibling binary" — mirrors the python3_available() gate.
fn siblings_built() -> bool {
    let dir = std::path::Path::new(env!("CARGO_BIN_EXE_sovereign-cli"))
        .parent()
        .expect("CARGO_BIN_EXE_sovereign-cli has a parent dir");
    ["sovereign-cli-dev", "sovereign-cli-daemon", "sovereign-cli-llm"]
        .iter()
        .all(|b| dir.join(b).is_file())
}

macro_rules! require_siblings {
    () => {
        if !siblings_built() {
            eprintln!(
                "skip: sibling CLI bins not built next to sovereign-cli — \
                 run `cargo build --bins`"
            );
            return;
        }
    };
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn assert_exit_zero(out: &Output, label: &str) {
    if !out.status.success() {
        panic!(
            "expected exit 0 from {label}, got {:?}\nstdout:\n{}\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

/// `sovereign project found` is retired — the banner fires and the
/// command exits 0 without touching the filesystem. (No
/// `.sovereign/project.toml` is required; the old gate that
/// demanded it is gone with the rest of the body.)
#[test]
fn project_found_is_retired_no_op() {
    require_siblings!();
    let out = run(&["project", "found"]);
    assert_exit_zero(&out, "project found");
    let err = stderr(&out);
    assert!(
        err.contains("`sovereign project found`"),
        "retirement banner should reference the old name; got:\n{err}"
    );
    assert!(
        err.contains("retired"),
        "retirement banner should say 'retired'; got:\n{err}"
    );
    // Steer the user toward the new flow. Either the spec workflow
    // hint or the charter pointer is acceptable.
    assert!(
        err.contains("spec") || err.contains("charter"),
        "retirement banner should hint at the new flow (spec / charter); got:\n{err}"
    );
}

/// Same retirement contract for `sovereign atos provision`.
#[test]
fn atos_provision_is_retired_no_op() {
    require_siblings!();
    let out = run(&["atos", "provision"]);
    assert_exit_zero(&out, "atos provision");
    let err = stderr(&out);
    assert!(
        err.contains("`sovereign atos provision`"),
        "retirement banner should reference the old name; got:\n{err}"
    );
    assert!(
        err.contains("retired"),
        "retirement banner should say 'retired'; got:\n{err}"
    );
    assert!(
        err.contains("spec") || err.contains("features.db"),
        "retirement banner should hint at the new flow (spec / features.db not required); got:\n{err}"
    );
}

/// The retirement banners' helpful pointer text obeys
/// `SOVEREIGN_QUIET_DEPRECATIONS=1` (the title line still prints —
/// we never silently swallow a retired command).
#[test]
fn retirement_banner_pointer_obeys_quiet_env() {
    require_siblings!();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"));
    cmd.env("SOVEREIGN_QUIET_DEPRECATIONS", "1");
    let out = cmd
        .args(["project", "found"])
        .output()
        .expect("spawn sovereign-cli");
    assert_exit_zero(&out, "project found [QUIET=1]");
    let err = stderr(&out);
    // Title line still fires — operator must know the command is gone.
    assert!(
        err.contains("retired"),
        "title line must always print, even quiet; got:\n{err}"
    );
    // Pointer text suppressed.
    assert!(
        !err.contains("Founding is implicit"),
        "pointer line should be suppressed under QUIET=1; got:\n{err}"
    );
}
