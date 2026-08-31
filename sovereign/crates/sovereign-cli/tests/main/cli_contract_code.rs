// SPDX-License-Identifier: AGPL-3.0-or-later
//! Code-conformance: the CLI contract manifest (`docs/cli-contract.toml`)
//! reconciled against the REAL binaries. The binary-side half of the harness;
//! the docs-side half is `cli_contract_docs.rs`.
//!
//! Direction 1 (manifest -> binary): every canonical command (documented,
//!   non-hidden, non-alias) is RECOGNIZED. Probed with `--help`, which is safe
//!   across the surface — the help check short-circuits before any daemon
//!   probe or side effect, so even daemon-backed commands (`chat ask --help`)
//!   pass offline with no daemon. A probe FAILS if the output is an
//!   "unknown subcommand" miss or the dispatcher's top-level usage banner
//!   (i.e. the verb fell through unrecognized). Exit codes are deliberately
//!   NOT asserted: some real subcommands treat `--help` as a positional and
//!   exit 2 — that is not a miss. Under the DEFAULT build, dev-tools commands
//!   are instead asserted INTERCEPTED (exit 2 + the `--features dev-tools`
//!   pointer), reusing the default_build_gate contract.
//!
//! Direction 2 (binary -> manifest): `svrn __dump-commands` enumerates
//!   every dispatched top-level verb; that set must equal the manifest's verb
//!   set — no untracked verb in the binary, no orphaned verb in the manifest.
//!
//! Invariant: no canonical command is visibility=public AND feature=dev-tools
//!   (it would exit 2 in the shipped binary, breaking a public promise).
//!
//! `atos` is in UNPROBED_VERBS: `atos <sub> --help` routes into the subcommand
//!   handler (run_atos only honors `--help` as argv[0]), and several mutate
//!   (install-plugin) or read stdin (teardown). atos is still tracked
//!   (Direction 2) and docs-checked. Under the default build it IS asserted
//!   intercepted — the dev-tools gate fires on the verb before any handler
//!   runs, so it is side-effect-free there.

use std::collections::BTreeSet;
use std::process::{Command, Output};

use sovereign_cli_shared::cli_contract::{Contract, Feature, Probe, Visibility};

fn sovereign() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"));
    cmd.env("SOVEREIGN_QUIET_DEPRECATIONS", "1");
    cmd.env("SOVEREIGN_NO_STALE_WARN", "1");
    cmd
}

pub(crate) fn run(args: &[&str]) -> Output {
    sovereign()
        .args(args)
        .output()
        .expect("spawn sovereign-cli")
}

pub(crate) fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The flat verbs exec sibling binaries (`-dev`, `-llm`, `-daemon`). A bare
/// `cargo test` without a prior `cargo build --bins` can't exercise dispatch,
/// so skip rather than false-fail — the same gate `aliases.rs` uses.
pub(crate) fn siblings_built() -> bool {
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
            eprintln!("skip: sibling CLI bins not built — run `cargo build --bins`");
            return;
        }
    };
}

pub(crate) fn contract() -> Contract {
    Contract::load_default().expect("docs/cli-contract.toml must parse")
}

/// Verbs whose subcommands are not safe to offline-probe (see module docs).
/// Only referenced by the dev-tools Direction-1 probe (the default build gates
/// these before any handler runs, so they're safe to assert there).
#[cfg_attr(not(feature = "dev-tools"), allow(dead_code))]
pub(crate) const UNPROBED_VERBS: &[&str] = &["atos"];

/// Distinctive prefix of the dispatcher's top-level usage banner (HELP.summary
/// in main.rs). It appears only when a verb fell through to `print_usage` —
/// i.e. it was not recognized. A recognized verb's `--help` shows its own help.
const DISPATCHER_MISS_MARKER: &str = "Local AI assistant";

fn looks_like_miss(text: &str) -> bool {
    if text.contains(DISPATCHER_MISS_MARKER) {
        return true;
    }
    // A real dispatch miss is "[Uu]nknown <verb> subcommand" / "unknown
    // subcommand '<x>'" — both contain "subcommand" on the same line. This
    // deliberately does NOT match "Unknown flag: --help" / "Unknown flag for
    // `mobile serve`: --help", which mean the command IS recognized (it routed
    // to its handler) but rejects --help — fine for an existence probe.
    text.to_lowercase()
        .lines()
        .any(|line| line.contains("unknown") && line.contains("subcommand"))
}

pub(crate) fn verb_of(path: &str) -> &str {
    path.split_whitespace().next().unwrap_or("")
}

pub(crate) fn help_argv(path: &str) -> Vec<String> {
    let mut a: Vec<String> = path.split_whitespace().map(String::from).collect();
    a.push("--help".to_string());
    a
}

pub(crate) fn refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

// ── Direction 1 ─────────────────────────────────────────────────────────

#[cfg(feature = "dev-tools")]
#[test]
fn direction1_dev_build_every_canonical_command_dispatches() {
    require_siblings!();
    let c = contract();
    let mut fails = Vec::new();
    for cmd in c.canonical() {
        if cmd.probe == Probe::Skip || UNPROBED_VERBS.contains(&verb_of(&cmd.path)) {
            continue;
        }
        let argv = help_argv(&cmd.path);
        let out = run(&refs(&argv));
        if looks_like_miss(&combined(&out)) {
            fails.push(format!(
                "`svrn {} --help` was not recognized (dispatcher miss)",
                cmd.path
            ));
        }
    }
    assert!(
        fails.is_empty(),
        "promised commands the binary does not dispatch:\n  {}",
        fails.join("\n  ")
    );
}

#[cfg(not(feature = "dev-tools"))]
#[test]
fn direction1_default_build_gates_dev_and_dispatches_public() {
    require_siblings!();
    let c = contract();
    let mut fails = Vec::new();
    for cmd in c.canonical() {
        if cmd.probe == Probe::Skip {
            continue;
        }
        let argv = help_argv(&cmd.path);
        let out = run(&refs(&argv));
        let text = combined(&out);
        match cmd.feature {
            // The shipped build intercepts dev-tools verbs before dispatch.
            Feature::DevTools => {
                let intercepted = !out.status.success() && text.contains("dev-tools");
                if !intercepted {
                    fails.push(format!(
                        "`svrn {}` is not gated in the default build \
                         (expected exit 2 + a `--features dev-tools` pointer)",
                        cmd.path
                    ));
                }
            }
            // Awareness commands are hidden (not canonical); nothing to do.
            Feature::Awareness => {}
            // Code-intel verbs are gated exactly like dev-tools in the shipped
            // build. (Arm added because `Feature::CodeIntel` was introduced
            // without updating this match — pre-existing break on `main`,
            // unrelated to the mesh work this branch carries.)
            Feature::CodeIntel => {
                let intercepted = !out.status.success() && text.contains("dev-tools");
                if !intercepted {
                    fails.push(format!(
                        "`svrn {}` is not gated in the default build \
                         (expected exit 2 + a `--features dev-tools` pointer)",
                        cmd.path
                    ));
                }
            }
            // Public + default-feature internal commands must dispatch.
            Feature::Default => {
                if looks_like_miss(&text) {
                    fails.push(format!(
                        "`svrn {} --help` was not recognized (dispatcher miss)",
                        cmd.path
                    ));
                }
            }
        }
    }
    assert!(
        fails.is_empty(),
        "default-build conformance failures:\n  {}",
        fails.join("\n  ")
    );
}

// ── Direction 2 ─────────────────────────────────────────────────────────

#[test]
fn direction2_dump_commands_matches_manifest_verbs() {
    // `__dump-commands` is handled in-process by the dispatcher (no siblings
    // needed) and runs in any build, so this check needs no feature gate.
    let out = run(&["__dump-commands"]);
    assert!(
        out.status.success(),
        "`svrn __dump-commands` failed:\n{}",
        combined(&out)
    );
    let dumped: BTreeSet<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    assert!(!dumped.is_empty(), "__dump-commands printed nothing");

    let manifest_verbs: BTreeSet<String> = contract()
        .commands
        .iter()
        .map(|c| verb_of(&c.path).to_string())
        .collect();

    let untracked: Vec<&String> = dumped.difference(&manifest_verbs).collect();
    let orphaned: Vec<&String> = manifest_verbs.difference(&dumped).collect();
    assert!(
        untracked.is_empty(),
        "verbs the binary dispatches but cli-contract.toml does not track \
         (add a row, hidden=true if internal): {untracked:?}"
    );
    assert!(
        orphaned.is_empty(),
        "verbs in cli-contract.toml the binary does not dispatch \
         (remove the row or fix ALL_VERBS): {orphaned:?}"
    );
}

// ── Invariant: public surface is never gated out of the shipped binary ────

#[test]
fn public_commands_are_not_dev_tools_gated() {
    let c = contract();
    let bad: Vec<String> = c
        .commands
        .iter()
        .filter(|cmd| cmd.visibility == Visibility::Public && cmd.feature == Feature::DevTools)
        .map(|cmd| cmd.path.clone())
        .collect();
    assert!(
        bad.is_empty(),
        "public commands gated behind dev-tools (they would exit 2 in the \
         shipped binary, breaking a public promise): {bad:?}"
    );
}
