// SPDX-License-Identifier: AGPL-3.0-or-later
//! api-gate — committed public-API surface snapshots for the hub/contract
//! crates ("the highest-signal legibility gate": encapsulation erosion
//! becomes a reviewable diff instead of an archaeology project).
//!
//! Wraps `cargo public-api` (rustdoc JSON — needs the PINNED nightly from
//! `quality/nightly-pin.txt`; the workspace itself never touches nightly).
//! Each tracked crate's surface is committed at
//! `quality/baselines/api/<crate>.txt`; the gate diffs current vs committed.
//! Intentional API changes: `cargo xtask api-gate --update-baseline` and
//! defend the snapshot diff in review.
//!
//! Runs locally + in the weekly lane only — NEVER on the PR critical path
//! (it is the one gate that costs a rustdoc build).
//!
//! Setup (one-time):
//!   rustup toolchain install $(cat quality/nightly-pin.txt)
//!   cargo install cargo-public-api --locked

use std::path::Path;

use crate::common;

/// The crates whose public surface is contract: the wire/contract leaves +
/// the high-fan-in hubs (fan-in per quality/baselines/fan_in.tsv).
///
/// The hub crates are PARKED until the nightly pin moves: rustc
/// nightly-2026-07-01 ICEs compiling `lance-index-4.0.0` (opaque-type
/// trait-selection panic in `stream_spill_reader`), which sits under
/// corpus-engine → sovereign-core/tools; commonwealth-core is parked with
/// them so the set expands in one deliberate step. To un-park: bump
/// quality/nightly-pin.txt to a nightly that builds lance-index, uncomment,
/// and run `api-gate --update-baseline`.
const API_CRATES: &[&str] = &[
    "oicp-types",
    "oicp-client",
    "sovereign-contracts",
    // "corpus-engine",
    // "sovereign-core",
    // "sovereign-tools",
    // "commonwealth-core",
];

pub fn run(args: &[String]) -> i32 {
    let root = common::repo_root();
    let flags = common::baseline_flags(args);
    let api_dir = common::baselines_dir(&root).join("api");

    let pin = match std::fs::read_to_string(root.join("quality/nightly-pin.txt")) {
        Ok(p) => p.trim().to_string(),
        Err(e) => {
            eprintln!("error: cannot read quality/nightly-pin.txt: {e}");
            return 1;
        }
    };

    if flags.tighten {
        // Snapshots are diffs, not counters — there is nothing to tighten.
        eprintln!("api-gate --tighten: snapshots have no counts; nothing to do");
        return 0;
    }

    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for &krate in API_CRATES {
        let current = match current_surface(&root, &pin, krate) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        let baseline_path = api_dir.join(format!("{krate}.txt"));

        if flags.update {
            let body = format!(
                "# api-gate snapshot — public API surface of `{krate}` (cargo public-api,\n\
                 # {pin}). MACHINE-WRITTEN; regenerate after an INTENTIONAL API change:\n\
                 #   cargo run -p xtask -- api-gate --update-baseline\n\
                 # The PR diff of this file IS the API review artifact.\n{current}"
            );
            if let Some(dir) = baseline_path.parent() {
                if let Err(e) = std::fs::create_dir_all(dir) {
                    eprintln!("error: cannot create {}: {e}", dir.display());
                    return 1;
                }
            }
            if let Err(e) = std::fs::write(&baseline_path, body) {
                eprintln!("error: cannot write {}: {e}", baseline_path.display());
                return 1;
            }
            eprintln!("wrote {}", baseline_path.display());
            continue;
        }

        checked += 1;
        let baseline = load_snapshot(&baseline_path);
        if baseline.is_empty() {
            failures.push(format!(
                "{krate}: no committed snapshot at {} — run \
                 `cargo run -p xtask -- api-gate --update-baseline`",
                baseline_path.display()
            ));
            continue;
        }
        let current_lines: Vec<&str> = current.lines().filter(|l| !l.is_empty()).collect();
        let added: Vec<&str> = current_lines
            .iter()
            .filter(|l| !baseline.contains(**l))
            .copied()
            .collect();
        let current_set: std::collections::BTreeSet<&str> = current_lines.iter().copied().collect();
        let removed: Vec<&String> = baseline
            .iter()
            .filter(|l| !current_set.contains(l.as_str()))
            .collect();
        if !added.is_empty() || !removed.is_empty() {
            let mut msg = format!(
                "{krate}: public API surface changed (+{} / -{} items) vs committed snapshot:",
                added.len(),
                removed.len()
            );
            for a in added.iter().take(8) {
                msg.push_str(&format!("\n      + {a}"));
            }
            for r in removed.iter().take(8) {
                msg.push_str(&format!("\n      - {r}"));
            }
            failures.push(msg);
        }
    }

    if flags.update {
        eprintln!(
            "api-gate: snapshots regenerated for {} crates",
            API_CRATES.len()
        );
        return 0;
    }

    eprintln!("api-gate: {checked} crate surfaces diffed vs committed snapshots ({pin})");
    for f in &failures {
        eprintln!("  ✗ {f}");
    }
    if failures.is_empty() {
        eprintln!("  ✓ every tracked public API matches its committed snapshot");
        0
    } else {
        eprintln!();
        eprintln!(
            "api-gate FAILED ({} surfaces changed). An API change must ship WITH its \
             snapshot: review the diff, then\n  cargo run -p xtask -- api-gate --update-baseline",
            failures.len()
        );
        1
    }
}

/// The sorted public-API line set of one crate via `cargo public-api`.
fn current_surface(root: &Path, pin: &str, krate: &str) -> Result<String, String> {
    let out = std::process::Command::new("cargo")
        .args([
            &format!("+{pin}"),
            "public-api",
            "-p",
            krate,
            "--simplified",
        ])
        .current_dir(root)
        .output()
        .map_err(|e| format!("cargo +{pin} public-api failed to launch: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "cargo public-api -p {krate} failed (is the pinned nightly installed — \
             `rustup toolchain install {pin}` — and cargo-public-api itself — \
             `cargo install cargo-public-api --locked`?):\n{}",
            stderr.lines().take(6).collect::<Vec<_>>().join("\n")
        ));
    }
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    lines.sort();
    lines.dedup();
    Ok(lines.join("\n") + "\n")
}

fn load_snapshot(path: &Path) -> std::collections::BTreeSet<String> {
    common::load_line_set(path)
}
