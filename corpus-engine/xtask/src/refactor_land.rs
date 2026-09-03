// SPDX-License-Identifier: AGPL-3.0-or-later
//! refactor-land — the post-split landing chain, run after a solve split
//! (or any test-moving refactor) and before commit.
//!
//! Two of the three steps a split lands with are mechanical and prescribed
//! by the gates themselves; this verb runs them in the order the gates
//! expect and refuses to paper over either:
//!
//!   1. Conformance-tag freshness — moving tests moves line numbers, so
//!      `quality/conformance/*.toml` goes stale and the freshness ratchet
//!      fails the workspace gate AFTER the split already looked green
//!      (watched 2026-09-02: the grounding split landed, then the full
//!      lane failed on `conformance_tags_are_fresh`). When stale, run the
//!      ratchet's own prescribed regeneration and verify it took.
//!   2. `arch-gate --tighten` — banks the extraction into the oversized
//!      baseline. Tighten is always safe: it lowers, never raises.
//!
//! The third step is human and stays that way — the SYSTEM_OVERVIEW §1.1
//! touch needs judgment about what the change MEANT. Printed as a reminder.

use std::process::Command;

const CONFORMANCE_TARGET: &[&str] = &[
    "test",
    "-p",
    "kernel-types",
    "--test",
    "conformance_tags",
];

fn run_conformance(regen: bool) -> std::process::Output {
    let mut c = Command::new("cargo");
    c.args(CONFORMANCE_TARGET);
    if regen {
        c.env("UPDATE_CONFORMANCE_TAGS", "1");
    }
    c.output().expect("failed to spawn cargo for conformance tags")
}

fn fail_with_tail(stage: &str, out: &std::process::Output) -> i32 {
    eprintln!("refactor-land: {stage} FAILED");
    let tail: String = String::from_utf8_lossy(&out.stderr)
        .lines()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    eprintln!("{tail}");
    1
}

pub fn run(_args: &[String]) -> i32 {
    println!("refactor-land [1/2]: conformance tag freshness");
    let probe = run_conformance(false);
    if !probe.status.success() {
        println!("  stale — regenerating (the ratchet's own prescribed repair)...");
        let regen = run_conformance(true);
        if !regen.status.success() {
            return fail_with_tail("conformance regeneration", &regen);
        }
        let verify = run_conformance(false);
        if !verify.status.success() {
            return fail_with_tail("conformance verification after regeneration", &verify);
        }
        println!("  regenerated + verified");
        // Glassbox: show WHAT the regeneration moved, so the landing commit
        // carries an intentional diff rather than a mystery rewrite.
        let stat = Command::new("git")
            .args(["diff", "--stat", "quality/conformance"])
            .output();
        if let Ok(s) = stat {
            print!("{}", String::from_utf8_lossy(&s.stdout));
        }
    } else {
        println!("  fresh");
    }

    println!("refactor-land [2/2]: arch-gate --tighten (banks the extraction)");
    let tighten = crate::arch_gate::run(&["--tighten".to_string()]);

    println!();
    println!("Remaining (human): the SYSTEM_OVERVIEW entry for the changed subsystem (§1.1),");
    println!("then commit the scoped paths — code + baseline + conformance together.");

    tighten
}
