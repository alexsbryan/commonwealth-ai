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
//! cadence (see .github/workflows/weekly.yml). This command stays fast.
//!
//! FOUR VERDICTS, NOT TWO (ARCH §18.2). A gate that could not reach its
//! evidence did not pass, and a gate that never ran did not pass either — so
//! the summary distinguishes PASS / FAIL / COULD-NOT-JUDGE / NEVER-RAN rather
//! than folding the last three into one red X. Only a HARD gate's non-zero
//! code makes this command exit non-zero.
//!
//! ENFORCEMENT is per-gate and declared in the table below. A gate is
//! `Advisory` when it reads evidence this command cannot make current —
//! concept-gate counts type definitions in the SCIP graph at the last indexed
//! commit, not in the working tree being gated, so a habit-run would go red for
//! an indexer that is minutes behind. Advisory means REPORTED, not ignored: the
//! row is printed with its verdict and the summary names how many advisories
//! spoke up. The same gate is hard where its evidence is authoritative — CI and
//! every landing verdict call `svrn code converge status` and gate on its exit.

use crate::{
    arch_gate, boundary_gate, concept_gate, docs_gate, env_gate, layer_gate, layout_gate, lock_gate,
};

/// Whether a gate's verdict may fail this command.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Enforcement {
    Hard,
    Advisory,
}

/// The four verdicts, from a gate's exit code. 3 and 4 are the two the binary
/// pass/fail split used to hide.
fn verdict(code: i32) -> &'static str {
    match code {
        0 => "PASS",
        3 => "COULD-NOT-JUDGE",
        4 => "NEVER-RAN",
        _ => "FAIL",
    }
}

pub fn run() -> i32 {
    let no_args: [String; 0] = [];
    let gates: &[(&str, Enforcement, &dyn Fn() -> i32)] = &[
        ("arch-gate", Enforcement::Hard, &|| arch_gate::run(&no_args)),
        ("docs-gate", Enforcement::Hard, &docs_gate::run),
        ("boundary-gate", Enforcement::Hard, &boundary_gate::run),
        ("layer-gate", Enforcement::Hard, &|| {
            layer_gate::run(&no_args)
        }),
        ("lock-gate", Enforcement::Hard, &|| lock_gate::run(&no_args)),
        // Hard: unlike the SCIP-backed concept-gate, this one reads the
        // WORKING TREE, so its evidence is authoritative wherever it runs.
        ("layout-gate", Enforcement::Hard, &|| {
            layout_gate::run(&no_args)
        }),
        ("env-gate", Enforcement::Hard, &|| env_gate::run(&no_args)),
        ("concept-gate", Enforcement::Advisory, &|| {
            concept_gate::run(&no_args)
        }),
    ];

    let mut results: Vec<(&str, Enforcement, i32)> = Vec::new();
    for (name, enforcement, gate) in gates {
        eprintln!("── {name} ────────────────────────────────────────────────");
        let code = gate();
        results.push((name, *enforcement, code));
        eprintln!();
    }

    eprintln!("── quality summary ─────────────────────────────────────────");
    let mut failed = 0;
    let mut advisory_flags = 0;
    for (name, enforcement, code) in &results {
        let advisory = *enforcement == Enforcement::Advisory;
        let tag = if advisory { "  (advisory)" } else { "" };
        eprintln!("  {:<16} {name}{tag}", verdict(*code));
        if *code != 0 {
            if advisory {
                advisory_flags += 1;
            } else {
                failed += 1;
            }
        }
    }
    let total = results.len();
    if failed == 0 {
        if advisory_flags == 0 {
            eprintln!("  ✓ all {total} gates green");
        } else {
            eprintln!(
                "  ✓ {} enforcing gates green — {advisory_flags} advisory gate(s) have something \
                 to say (read their block above; they do not fail this run)",
                total - advisory_flags
            );
        }
        0
    } else {
        eprintln!(
            "  ✗ {failed}/{total} enforcing gates failing — each gate's output above ends with \
             its fix command"
        );
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two verdicts the old binary split hid. A gate that could not reach
    /// its evidence must not render as a plain FAIL, or nobody can tell "your
    /// code is wrong" from "my instrument is down" (§18.2).
    #[test]
    fn four_verdicts_not_two() {
        assert_eq!(verdict(0), "PASS");
        assert_eq!(verdict(1), "FAIL");
        assert_eq!(verdict(3), "COULD-NOT-JUDGE");
        assert_eq!(verdict(4), "NEVER-RAN");
        assert_eq!(verdict(101), "FAIL");
    }
}
