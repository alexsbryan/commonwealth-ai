// SPDX-License-Identifier: AGPL-3.0-or-later
//! Journey dispatch replay: every step of every declared journey is
//! REACHABLE in the real binary, offline.
//!
//! This is the middle tier of the journey harness:
//!
//!  1. `cli_contract_journeys` — static. The manifest is coherent: steps
//!     bind to declared commands, docs exist, the stranded ledger shrinks.
//!  2. **this file** — offline. Each step is spawned against the real
//!     dispatcher with a temp `HOME`; none may fall through unrecognized.
//!  3. `cli-journey-verify.sh` — live. The sequence is *run in order*
//!     against a daemon and its state transitions are asserted.
//!
//! Be precise about what this tier buys: it proves each step of a sequence
//! is reachable, NOT that the sequence works. Ordering is only meaningful
//! once state exists, which is tier 3. What this catches cheaply is the
//! common regression — a verb renamed or re-routed out from under a journey
//! that the docs still teach.
//!
//! Two things it does that `cli_contract_code` deliberately does not:
//!
//!  - It asserts EXIT CODES for steps whose command promises `probe =
//!    "help"`. `cli_contract_code` ignores exit codes across the board
//!    because some leaf handlers treat `--help` as a positional; the
//!    manifest marks those `probe = "no-args"`, so within a journey the
//!    stricter bar is safe. This is the check that would have caught
//!    `proxy --help` exiting 2.
//!  - It runs with `HOME` pointed at a fresh temp dir, so a step cannot
//!    read or write the developer's real `~/.sovereign`.

use std::path::Path;
use std::process::{Command, Output};

use sovereign_cli_shared::cli_contract::{Contract, Probe};

/// Steps are probed with `--help`, which short-circuits before any daemon
/// call — but a few handlers act on bare invocation regardless. Anything
/// whose *verb* appears here is skipped by the replay; it is still covered
/// by the static tier and (where safe) the live tier.
const UNPROBED_VERBS: &[&str] = &[
    // `atos <sub> --help` routes into the subcommand handler; several
    // mutate (install-plugin) or read stdin (teardown). Same exclusion the
    // command-level conformance test makes, for the same reason.
    "atos",
];

fn sovereign(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"));
    cmd.env("SOVEREIGN_QUIET_DEPRECATIONS", "1");
    cmd.env("SOVEREIGN_NO_STALE_WARN", "1");
    // Hermetic: a replayed step must not touch the operator's real state.
    cmd.env("HOME", home);
    cmd
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The flat verbs exec sibling binaries. A bare `cargo test` without a
/// prior `cargo build --bins` cannot exercise dispatch, so skip rather than
/// false-fail — the same gate `cli_contract_code` and `aliases.rs` use.
fn siblings_built() -> bool {
    let dir = Path::new(env!("CARGO_BIN_EXE_sovereign-cli"))
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

/// Distinctive prefix of the dispatcher's top-level usage banner. It appears
/// only when a verb fell through to `print_usage` — i.e. was not recognized.
const DISPATCHER_MISS_MARKER: &str = "Local AI assistant";

fn looks_like_miss(text: &str) -> bool {
    if text.contains(DISPATCHER_MISS_MARKER) {
        return true;
    }
    text.to_lowercase()
        .lines()
        .any(|line| line.contains("unknown") && line.contains("subcommand"))
}

/// The argv a step is probed with: the path words (arguments and
/// placeholders stripped) plus `--help`.
///
/// Callers pass the DECLARED command path when the step binds exactly —
/// probing `model list --help` when the manifest declares `model` tests a
/// subcommand nobody promised, and `model` is deliberately declared
/// subcommand-shaped (list / set / unset / context) with the help on the
/// verb. Probe what the contract actually claims.
fn probe_argv(path: &str) -> Vec<String> {
    let mut a: Vec<String> = path
        .split_whitespace()
        .take_while(|w| !w.starts_with('-') && !w.starts_with('{'))
        .map(String::from)
        .collect();
    a.push("--help".into());
    a
}

fn verb_of(run: &str) -> &str {
    run.split_whitespace().next().unwrap_or("")
}

#[cfg(feature = "dev-tools")]
#[test]
fn every_journey_step_dispatches() {
    if !siblings_built() {
        eprintln!("skip: sibling CLI bins not built — run `cargo build --bins`");
        return;
    }
    let home = tempfile::tempdir().expect("temp HOME");
    let c = Contract::load_default().expect("docs/cli-contract.toml must parse");

    let mut misses = Vec::new();
    let mut bad_exits = Vec::new();
    let mut probed = 0usize;
    let mut skipped = 0usize;

    for j in &c.journeys {
        for (i, step) in j.steps.iter().enumerate() {
            let verb = verb_of(&step.run);
            if UNPROBED_VERBS.contains(&verb) {
                skipped += 1;
                continue;
            }
            let binding = c.resolve_step(step);
            if let Some(cmd) = binding.exact() {
                if cmd.probe == Probe::Skip {
                    skipped += 1;
                    continue;
                }
            }

            // Probe the DECLARED path when we have one; fall back to the
            // step's own words only for a VerbOnly binding, where by
            // definition no row declares the exact path.
            let argv = probe_argv(
                binding
                    .exact()
                    .map_or(step.run.as_str(), |c| c.path.as_str()),
            );
            let args: Vec<&str> = argv.iter().map(String::as_str).collect();
            let out = sovereign(home.path())
                .args(&args)
                .output()
                .expect("spawn sovereign-cli");
            probed += 1;
            let text = combined(&out);

            if looks_like_miss(&text) {
                misses.push(format!(
                    "{}[{}] `svrn {}` was not recognized — the journey drives \
                     a command the dispatcher does not route",
                    j.id,
                    i,
                    argv.join(" ")
                ));
                continue;
            }

            // Stricter than the command-level probe: a command that promises
            // `--help` must succeed at it. `probe = "no-args"` opts out.
            let promises_help = binding
                .exact()
                .map(|cmd| cmd.probe == Probe::Help)
                .unwrap_or(false);
            if promises_help && !out.status.success() {
                bad_exits.push(format!(
                    "{}[{}] `svrn {}` exited {} — a step whose command is \
                     probe=\"help\" must exit 0 on --help (mark it \
                     probe=\"no-args\" if it legitimately treats --help as a \
                     positional)",
                    j.id,
                    i,
                    argv.join(" "),
                    out.status.code().unwrap_or(-1)
                ));
            }
        }
    }

    eprintln!("cli_journey_dispatch: probed {probed} step(s), skipped {skipped}");
    assert!(
        misses.is_empty(),
        "journey steps the binary does not dispatch:\n  {}",
        misses.join("\n  ")
    );
    assert!(
        bad_exits.is_empty(),
        "journey steps that fail their own --help:\n  {}",
        bad_exits.join("\n  ")
    );
}

/// Under the shipped (default) build the dev-tools verbs are intercepted
/// before dispatch. A journey step landing on one must therefore be part of
/// an INTERNAL journey — a public journey promising a gated step is caught
/// statically by `cli_contract_journeys`, and this asserts the runtime half.
#[cfg(not(feature = "dev-tools"))]
#[test]
fn default_build_gates_dev_steps_and_dispatches_public_ones() {
    if !siblings_built() {
        eprintln!("skip: sibling CLI bins not built — run `cargo build --bins`");
        return;
    }
    use sovereign_cli_shared::cli_contract::{Feature, Visibility};

    let home = tempfile::tempdir().expect("temp HOME");
    let c = Contract::load_default().expect("docs/cli-contract.toml must parse");
    let mut fails = Vec::new();

    for j in c
        .journeys
        .iter()
        .filter(|j| j.visibility == Visibility::Public)
    {
        for (i, step) in j.steps.iter().enumerate() {
            if UNPROBED_VERBS.contains(&verb_of(&step.run)) {
                continue;
            }
            let binding = c.resolve_step(step);
            let Some(cmd) = binding.exact() else {
                continue;
            };
            if cmd.feature != Feature::Default || cmd.probe == Probe::Skip {
                continue;
            }
            let argv = probe_argv(&cmd.path);
            let args: Vec<&str> = argv.iter().map(String::as_str).collect();
            let out = sovereign(home.path())
                .args(&args)
                .output()
                .expect("spawn sovereign-cli");
            if looks_like_miss(&combined(&out)) {
                fails.push(format!(
                    "{}[{}] `svrn {}` is not dispatched in the shipped build, \
                     but the journey is public",
                    j.id,
                    i,
                    argv.join(" ")
                ));
            }
        }
    }
    assert!(
        fails.is_empty(),
        "public journey steps missing from the shipped build:\n  {}",
        fails.join("\n  ")
    );
}

#[test]
fn probe_argv_strips_arguments_and_placeholders() {
    assert_eq!(
        probe_argv("chat inspect --corpus {corpus} \"a question\""),
        vec!["chat", "inspect", "--help"]
    );
    assert_eq!(probe_argv("corpus list"), vec!["corpus", "list", "--help"]);
    assert_eq!(
        probe_argv("mesh join {join_key}"),
        vec!["mesh", "join", "--help"]
    );
}
