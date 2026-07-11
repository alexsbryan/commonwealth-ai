// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cargo xtask quality` — run every local gate, one summary table.
//!
//! The single entry point for "am I clean?" before/after a cleanup session.
//! Check-mode only: baseline mutations stay explicit per-gate
//! (`--update-baseline` / `--tighten`), so a habit-run can never silently
//! move a ratchet.
//!
//! Compile-coupled gates (lint-gate over clippy JSON, api-gate over rustdoc
//! JSON) are deliberately NOT here — they cost a build and run on their own
//! cadence (see .github/workflows/weekly.yml). This command stays sub-second.

use crate::{arch_gate, boundary_gate, docs_gate, layer_gate, lock_gate};

pub fn run() -> i32 {
    let no_args: [String; 0] = [];
    let gates: &[(&str, &dyn Fn() -> i32)] = &[
        ("arch-gate", &|| arch_gate::run(&no_args)),
        ("docs-gate", &docs_gate::run),
        ("boundary-gate", &boundary_gate::run),
        ("layer-gate", &|| layer_gate::run(&no_args)),
        ("lock-gate", &|| lock_gate::run(&no_args)),
    ];

    let mut results: Vec<(&str, i32)> = Vec::new();
    for (name, gate) in gates {
        eprintln!("── {name} ────────────────────────────────────────────────");
        let code = gate();
        results.push((name, code));
        eprintln!();
    }

    eprintln!("── quality summary ─────────────────────────────────────────");
    let mut failed = 0;
    for (name, code) in &results {
        let verdict = if *code == 0 { "PASS" } else { "FAIL" };
        eprintln!("  {verdict}  {name}");
        if *code != 0 {
            failed += 1;
        }
    }
    if failed == 0 {
        eprintln!("  ✓ all {} gates green", results.len());
        0
    } else {
        eprintln!(
            "  ✗ {failed}/{} gates failing — each gate's output above ends with its fix command",
            results.len()
        );
        1
    }
}
