// SPDX-License-Identifier: AGPL-3.0-or-later
//! Parity tests for the CLI namespace collapse.
//!
//! Every verb probed here is part of the developer toolchain, which
//! the default (end-user) build intercepts with an exit-2 pointer at
//! `--features dev-tools` (see `DEV_VERBS` in main.rs). The binary
//! under test is built with the same feature set as this test crate,
//! so the suite only compiles when the verbs actually dispatch. The
//! default build's intercept contract is covered by
//! `default_build_gate.rs` (the `cfg(not(...))` twin of this gate).
#![cfg(feature = "dev-tools")]
//!
//! Every command moved into the flat `svrn <leaf>` namespace
//! must ALSO keep working under its old `svrn project <leaf>`
//! / `svrn atos <leaf>` / `svrn reflect` form for the
//! indefinite alias period.
//!
//! These tests spawn the actual `sovereign-cli` binary so the
//! dispatch wiring (main.rs match arms + alias shims in
//! `project_cmd::run_project` / `atos_cmd::run_atos`) is exercised
//! end-to-end.
//!
//! Two probe styles, one per direction:
//!
//! 1. **New name `--help`**: the new top-level modules all check
//!    `util::help::wants_help` at the very top of `run`, so
//!    `svrn <leaf> --help` exits 0 without touching the
//!    filesystem. This verifies the dispatch arm exists.
//!
//! 2. **Old name no-args**: most underlying handlers (especially
//!    inside `atos_cmd`) don't recognise `--help` — they treat it
//!    as an unknown subcommand and exit non-zero. We can't change
//!    that without touching their behaviour, so the alias probe
//!    instead invokes the OLD command with no further args. The
//!    deprecation banner fires inside the alias shim BEFORE the
//!    underlying handler emits its usage error, so we only verify
//!    "banner appears in stderr" — exit code is irrelevant.
//!
//! `SOVEREIGN_QUIET_DEPRECATIONS=0` is set explicitly per-spawn so
//! the banner fires regardless of the developer's local env.

use std::process::{Command, Output};

fn sovereign() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"));
    // Force banners to fire even if the developer normally suppresses
    // them. The "0" / empty-string values are recognized as
    // not-quiet by `util::deprecation::is_quiet`.
    cmd.env("SOVEREIGN_QUIET_DEPRECATIONS", "0");
    cmd
}

fn run(args: &[&str]) -> Output {
    sovereign()
        .args(args)
        .output()
        .expect("spawn sovereign-cli")
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

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// The flat verbs delegate into sibling crate binaries
/// (`sovereign-cli-dev`, `-daemon`, `-llm`). `cargo test` builds the
/// dispatcher but NOT those sibling bin artifacts, so a bare `cargo
/// test` without a prior `cargo build --bins` can't exercise dispatch
/// end-to-end. When the siblings are absent we skip rather than
/// false-fail with "cannot find sibling binary" — the same
/// prerequisite-gate pattern the Python-validator tests use for
/// `python3_available()`.
fn siblings_built() -> bool {
    let dir = std::path::Path::new(env!("CARGO_BIN_EXE_sovereign-cli"))
        .parent()
        .expect("CARGO_BIN_EXE_sovereign-cli has a parent dir");
    [
        "sovereign-cli-dev",
        "sovereign-cli-daemon",
        "sovereign-cli-llm",
    ]
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

/// New-name probe: `svrn <args> --help` exits 0. Verifies that
/// the new top-level module exists, is wired into main.rs's match,
/// and routes through `util::help::wants_help` at the top of `run`.
fn help_runs(args: &[&str]) {
    let mut full = args.to_vec();
    full.push("--help");
    let out = run(&full);
    assert_exit_zero(&out, &format!("{:?}", args));
}

/// Old-name probe: invoking the legacy command (with whatever
/// `args_after_old` the underlying handler is willing to accept,
/// often empty) prints the deprecation banner before the handler
/// runs. We don't constrain the exit code — the banner is the
/// contract this test cares about.
fn banner_fires(legacy_args: &[&str], expected_old: &str, expected_new: &str) {
    let out = run(legacy_args);
    let err = stderr(&out);
    assert!(
        err.contains(&format!("`{expected_old}`")),
        "banner missing old name {expected_old:?} for {legacy_args:?}\nstderr:\n{err}"
    );
    assert!(
        err.contains(&format!("`{expected_new}`")),
        "banner missing new name {expected_new:?} for {legacy_args:?}\nstderr:\n{err}"
    );
}

// ─── Tier-1: daily commands ─────────────────────────────────────

#[test]
fn alias_init() {
    require_siblings!();
    help_runs(&["init"]);
    // `svrn project init --help` short-circuits cleanly; using
    // --help here exercises the alias path's banner with no side
    // effects on the filesystem.
    banner_fires(
        &["project", "init", "--help"],
        "svrn project init",
        "svrn init",
    );
}

#[test]
fn alias_status() {
    require_siblings!();
    help_runs(&["status"]);
    banner_fires(
        &["project", "status", "--help"],
        "svrn project status",
        "svrn status",
    );
}

#[test]
fn alias_audit() {
    require_siblings!();
    help_runs(&["audit"]);
    banner_fires(
        &["project", "audit", "--help"],
        "svrn project audit",
        "svrn audit",
    );
}

// ─── Tier-3: deeper commands ────────────────────────────────────

#[test]
fn alias_charter() {
    require_siblings!();
    help_runs(&["charter"]);
    banner_fires(
        &["project", "charter", "--help"],
        "svrn project charter",
        "svrn charter",
    );
}

#[test]
fn alias_amend() {
    require_siblings!();
    help_runs(&["amend"]);
    banner_fires(
        &["project", "amend", "--help"],
        "svrn project amend",
        "svrn amend",
    );
}

#[test]
fn alias_design() {
    require_siblings!();
    help_runs(&["design"]);
    banner_fires(
        &["project", "design", "--help"],
        "svrn project design",
        "svrn design",
    );
}

#[test]
fn alias_plan() {
    require_siblings!();
    help_runs(&["plan"]);
    banner_fires(
        &["project", "plan", "--help"],
        "svrn project plan",
        "svrn plan",
    );
}

#[test]
fn alias_serve() {
    require_siblings!();
    help_runs(&["serve"]);
    banner_fires(
        &["project", "serve", "--help"],
        "svrn project serve",
        "svrn serve",
    );
}

#[test]
fn alias_refresh() {
    require_siblings!();
    help_runs(&["refresh"]);
    banner_fires(
        &["project", "refresh", "--help"],
        "svrn project refresh",
        "svrn refresh",
    );
}

// ─── Tier-2: spec commands ──────────────────────────────────────
//
// `svrn atos end-milestone` and `svrn atos spec` don't
// recognise `--help` — they exit 2 with "missing <id>" /
// "unknown subcommand". The banner still fires inside the alias
// shim before the handler runs, so we probe with no args (and
// ignore the resulting non-zero exit).

#[test]
fn alias_milestone() {
    require_siblings!();
    help_runs(&["milestone"]);
    // The banner's "new name" hint is the full positional shape so
    // copy-paste lands the user in a working invocation.
    banner_fires(
        &["atos", "end-milestone"],
        "svrn atos end-milestone",
        "svrn milestone <feature-id> <N>",
    );
}

#[test]
fn alias_drift() {
    require_siblings!();
    help_runs(&["drift"]);
    banner_fires(&["atos", "spec"], "svrn atos spec", "svrn drift");
}

#[test]
fn alias_notes() {
    help_runs(&["notes"]);
    // `svrn reflect` is the legacy entry point for the
    // reflection view that `svrn notes` now owns. The banner
    // fires inside `run_reflect` regardless of the args passed —
    // `--help` is the cheapest probe (no DB / fs touch).
    banner_fires(&["reflect", "--help"], "svrn reflect", "svrn notes");
}

// ─── Quiet-mode suppression ─────────────────────────────────────

#[test]
fn quiet_env_suppresses_banner() {
    require_siblings!();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"));
    cmd.env("SOVEREIGN_QUIET_DEPRECATIONS", "1");
    let out = cmd
        .args(["project", "audit", "--help"])
        .output()
        .expect("spawn sovereign-cli");
    assert_exit_zero(&out, "project audit --help [QUIET=1]");
    let err = stderr(&out);
    assert!(
        !err.contains("(old name still works)"),
        "banner leaked despite SOVEREIGN_QUIET_DEPRECATIONS=1\nstderr:\n{err}"
    );
}

// ─── Banner is once-per-process ─────────────────────────────────
//
// Each test spawns a fresh process, so we can't easily verify
// "twice in the same process" here. The unit test in
// `util::deprecation::tests::record_first_time_is_idempotent_per_pair`
// covers the once-only contract directly.
