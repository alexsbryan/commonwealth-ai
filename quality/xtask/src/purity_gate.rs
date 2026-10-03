// SPDX-License-Identifier: AGPL-3.0-or-later
//! purity-gate — "links no filesystem code at all" is a gate, not a comment.
//!
//! Holds ROOT_CAUSE_FIXES B4: the ring rail's canon (`commonwealth-rail-core`)
//! and its envelope (`oplog-types`) perform NO I/O. The claim existed in two
//! Cargo.toml comments for months while `workspace-hack` and an `oplog` with
//! `std::fs` inside sat in the closure — a comment enforcing nothing is the
//! §5.1 smell.
//!
//! # What each leg covers, and what it cannot (the inventory)
//!
//! | instrument | covers | blind to |
//! |---|---|---|
//! | `quality/ARCH_LAYERS.toml` `[[forbid]]` (layer-gate) | dependency EDGES — `rail-core -> oplog` and kin | `std::fs` use hiding behind an ALLOWED edge |
//! | this gate, leg 1 (source census) | direct fs/net/clock reads in the pure trees | use inside a dependency's source |
//! | this gate, leg 2 (closure deny-list) | known-IO crates entering the closure | an innocuous-looking dep that quietly reads a socket |
//! | this gate, leg 3 (wasm32 check) | the closure building for the browser tier at all (the getrandom class) | `std::fs` itself — it compiles on wasm32 as stubs |
//!
//! Together they pin both halves of the split: no fs CODE in the pure trees,
//! no fs CRATES in the closure. The wasm32 leg is also the browser claim's
//! first gate — `ring-runtime` does not link rail-core, so its build proves
//! nothing here (the review's note, recorded in the plan).
//!
//! Red-watched the day it landed: a `std::fs::metadata` planted into
//! `view.rs` failed leg 1 by name; removing it went green.

use std::path::Path;
use std::process::Command;

use crate::common;

/// The trees that must stay pure: the fold and its envelope.
const PURE_TREES: &[&str] = &[
    "shared/crates/commonwealth-rail-core/src",
    "shared/crates/oplog-types/src",
];

/// Tokens that mean "this file talks to the machine". A line in a comment
/// describing one does not (the clock-gate rule).
const IO_TOKENS: &[&str] = &[
    "std::fs",
    "std::net",
    "SystemTime::now",
    "tokio::",
    "OpenOptions",
    "TcpStream",
    "UdpSocket",
    "std::time::Instant",
];

/// Crates that must never appear in the pure closure: the fs writer of the
/// type/writer split, and the known IO runtimes.
const DENY_LIST: &[&str] = &[
    "oplog ",
    "tokio ",
    "async-std ",
    "mio ",
    "hyper ",
    "reqwest ",
];

fn io_hits(line: &str) -> usize {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with("///") {
        return 0;
    }
    IO_TOKENS.iter().map(|t| line.matches(t).count()).sum()
}

fn collect(dir: &Path, hits: &mut Vec<(String, usize)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, hits);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let n: usize = text.lines().map(io_hits).sum();
        if n > 0 {
            hits.push((path.display().to_string(), n));
        }
    }
}

pub fn run(_args: &[String]) -> i32 {
    let root = common::repo_root();
    let mut failures = 0;

    // ── leg 1: the pure trees read no machine ─────────────────────────
    let mut hits = Vec::new();
    for tree in PURE_TREES {
        collect(&root.join(tree), &mut hits);
    }
    if hits.is_empty() {
        eprintln!("purity-gate: leg 1 PASS — no fs/net/clock reads in the pure trees");
    } else {
        failures += 1;
        for (file, n) in &hits {
            eprintln!(
                "  ✗ {file}: {n} machine-read token(s) — the fold must ask, never read \
                 (ROOT_CAUSE_FIXES B4)"
            );
        }
    }

    // ── leg 2: the pure closure contains no known-IO crate ────────────
    let tree = Command::new("cargo")
        .args([
            "tree",
            "-p",
            "commonwealth-rail-core",
            "-e",
            "normal",
            "--prefix",
            "none",
        ])
        .current_dir(&root)
        .output();
    match tree {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let offenders: Vec<&str> = DENY_LIST
                .iter()
                .copied()
                .filter(|d| text.lines().any(|l| l.contains(d)))
                .collect();
            if offenders.is_empty() {
                eprintln!("purity-gate: leg 2 PASS — the fold's closure is free of known IO");
            } else {
                failures += 1;
                for name in offenders {
                    eprintln!(
                        "  ✗ {name} is in commonwealth-rail-core's closure — the type/writer \
                         split or the [[forbid]] row regressed"
                    );
                }
            }
        }
        _ => {
            eprintln!(
                "purity-gate: leg 2 COULD-NOT-JUDGE — `cargo tree` failed; fix the workspace first"
            );
            return 3;
        }
    }

    // ── leg 3: the closure builds for the browser tier ────────────────
    if Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("wasm32-unknown-unknown"))
        .unwrap_or(false)
    {
        // getrandom 0.3 selects its wasm backend by explicit cfg — the same
        // flag scripts/build-ring-runtime.sh passes (and the same lesson).
        let check = Command::new("cargo")
            .args([
                "check",
                "-p",
                "commonwealth-rail-core",
                "--target",
                "wasm32-unknown-unknown",
            ])
            .env("RUSTFLAGS", "--cfg getrandom_backend=\"wasm_js\"")
            .current_dir(&root)
            .status();
        match check {
            Ok(s) if s.success() => {
                eprintln!(
                    "purity-gate: leg 3 PASS — the fold builds for wasm32 (the browser tier)"
                );
            }
            Ok(_) => {
                failures += 1;
                eprintln!("  ✗ commonwealth-rail-core does not build for wasm32 — the browser claim is dead");
            }
            Err(_) => {
                eprintln!("purity-gate: leg 3 COULD-NOT-JUDGE — cargo unavailable");
                return 3;
            }
        }
    } else {
        eprintln!(
            "purity-gate: leg 3 COULD-NOT-JUDGE — wasm32-unknown-unknown not installed; \
             `rustup target add wasm32-unknown-unknown`"
        );
        return 3;
    }

    if failures == 0 {
        eprintln!("purity-gate PASSED (3 legs)");
        0
    } else {
        eprintln!(
            "purity-gate FAILED ({failures} leg(s)) — see ROOT_CAUSE_FIXES B4 for the fix shape"
        );
        1
    }
}
