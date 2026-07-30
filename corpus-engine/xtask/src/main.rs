// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cargo xtask` — repo maintenance gates.
//!
//! Usage:
//!   cargo xtask quality                            Run every local gate, print a summary table
//!   cargo xtask arch-gate [--update-baseline|--tighten]   ARCH §3.1 size ratchet + §1 doc-contract
//!   cargo xtask docs-gate                          Every repo path the narrative docs cite must resolve
//!   cargo xtask boundary-gate                      The studio package depends only on itself + shared leaves
//!   cargo xtask layer-gate [--update-baseline|--tighten]  Cargo-declared deps obey quality/ARCH_LAYERS.toml + fan-in ratchet
//!   cargo xtask lock-gate  [--update-baseline|--tighten]  No NEW duplicate crate versions in Cargo.lock
//!   cargo xtask env-gate   [--update-baseline|--tighten|--update-doc]  Observed env vars obey quality/env-flags.toml
//!
//! Ratchet contract (uniform across gates): baselines live in
//! `quality/baselines/`, are machine-written only (`--update-baseline`
//! snapshots current state; `--tighten` rewrites only entries that improved —
//! never adds, never raises), and every failure message ends with the exact
//! command that fixes it.

mod api_gate;
mod arch_gate;
mod boundary_gate;
mod common;
mod docs_gate;
mod env_gate;
mod layer_gate;
mod lint_gate;
mod lock_gate;
mod manifests;
mod quality_cmd;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");

    let exit_code = match cmd {
        "quality" => quality_cmd::run(),
        "arch-gate" => arch_gate::run(&args[1..]),
        "docs-gate" => docs_gate::run(),
        "boundary-gate" => boundary_gate::run(),
        "api-gate" => api_gate::run(&args[1..]),
        "env-gate" => env_gate::run(&args[1..]),
        "layer-gate" => layer_gate::run(&args[1..]),
        "lint-gate" => lint_gate::run(&args[1..]),
        "lock-gate" => lock_gate::run(&args[1..]),
        "help" | "--help" | "-h" => {
            print_usage();
            0
        }
        other => {
            eprintln!("Unknown xtask command: {other}");
            print_usage();
            1
        }
    };

    std::process::exit(exit_code);
}

fn print_usage() {
    eprintln!("Usage: cargo xtask <command>");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  quality                        Run every local gate; one summary table");
    eprintln!(
        "  arch-gate [--update-baseline|--tighten]   Enforce the §3.1 file-size ratchet + §1 doc-contract"
    );
    eprintln!(
        "  docs-gate                      Resolve every repo path cited by the narrative docs"
    );
    eprintln!(
        "  boundary-gate                  Enforce the studio-package dependency boundary (studio/BOUNDARY.md)"
    );
    eprintln!(
        "  layer-gate [--update-baseline|--tighten]  Enforce quality/ARCH_LAYERS.toml + the fan-in ratchet"
    );
    eprintln!(
        "  lock-gate [--update-baseline|--tighten]   No NEW duplicate crate versions in Cargo.lock"
    );
    eprintln!(
        "  env-gate [--update-baseline|--tighten|--update-doc]  Observed env vars obey quality/env-flags.toml; renders docs/ENV_FLAGS.md"
    );
    eprintln!(
        "  lint-gate --from <clippy.json> [--update-baseline|--tighten]  Per-crate/lint warning-count ratchet"
    );
    eprintln!(
        "  api-gate [--update-baseline]   Diff hub-crate public APIs vs committed snapshots (pinned nightly)"
    );
}
