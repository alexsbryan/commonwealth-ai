// SPDX-License-Identifier: AGPL-3.0-or-later
//! clock-gate — a ratchet on hand-read wall clocks.
//!
//! # The leak, measured
//!
//! `sovereign_core::time`'s own module doc records that it was minted to
//! absorb "~40 copy-pasted private `fn unix_now()` / `now_secs()` /
//! `now_unix()` / `now_millis()` across the workspace". The consolidation
//! happened. Then it grew back: censused 2026-08-31, **35 local
//! re-implementations** of a zero-argument epoch accessor were live again,
//! under fifteen different names —
//!
//! | name | copies | name | copies |
//! |---|---|---|---|
//! | `now_unix` | 8 | `now_unix_secs` | 3 |
//! | `now` | 6 | `now_secs` | 3 |
//! | `unix_now` | 5 | ...and nine more | 1 each |
//!
//! That is the failure mode this repo names in `MEMORY.md` as the
//! creation-closure gap: a convergence with no ratchet is undone by the next
//! agent, because writing seven fresh lines is local and certain while finding
//! the accessor is discovery. The convergence is only as durable as the gate
//! that holds it.
//!
//! # What this gate is for
//!
//! Each dependency island has exactly one decider, and they are the only files
//! allowed to read the wall clock:
//!
//! | island | decider |
//! |---|---|
//! | sovereign (light crates) | `sovereign-time` |
//! | sovereign (on core) | `sovereign_core::time` |
//! | corpus-engine | `corpus_engine_yield::time` |
//! | commonwealth | `commonwealth_core::clock` |
//!
//! Everything else asks one of them. This freezes the remaining sites so the
//! only allowed direction is down — the same contract as `layout-gate` and
//! `arch-gate`, and the closure loop for the convergence rather than a hope
//! that the next reader remembers (ARCH §7, §10.6).
//!
//! Sub-second precision is the one thing no decider offers, so the sites that
//! need `as_nanos()` / `subsec_nanos()` ride the baseline rather than being
//! forced onto an accessor that cannot serve them.

use std::collections::BTreeMap;
use std::path::Path;

use crate::common;

/// The files ALLOWED to read the wall clock, because they are the deciders.
const DECIDERS: &[&str] = &[
    "sovereign/crates/sovereign-time/src/lib.rs",
    "sovereign/crates/sovereign-core/src/time.rs",
    "corpus-engine-yield/src/time.rs",
    "commonwealth/crates/commonwealth-core/src/clock.rs",
];

/// Which decider a new site should reach for, named by where the site lives.
fn decider_for(rel: &str) -> &'static str {
    if rel.starts_with("corpus-engine") {
        "corpus_engine_yield::time::{unix_now, unix_now_u64, unix_millis}"
    } else if rel.starts_with("commonwealth/") {
        "commonwealth_core::clock::{unix_now_secs, unix_now_millis}"
    } else {
        "sovereign_core::time::{unix_now, unix_now_u64, unix_millis} \
         (or `sovereign_time::` for a crate not on sovereign-core)"
    }
}

/// One hand-read wall clock. `Instant::now()` is monotonic and deliberately
/// not counted — it is not a date, and no accessor replaces it.
fn clock_hits(line: &str) -> usize {
    let trimmed = line.trim_start();
    // A comment describing the clock does not read it.
    if trimmed.starts_with("//") {
        return 0;
    }
    line.matches("SystemTime::now()").count()
}

fn collect(dir: &Path, root: &Path, scope: &common::SourceTree, out: &mut Vec<(String, usize)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let rel = common::rel_path(&path, root);
        if path.is_dir() {
            if !scope.excludes_dir(&rel) {
                collect(&path, root, scope, out);
            }
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs")
            || DECIDERS.contains(&rel.as_str())
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let n: usize = text.lines().map(clock_hits).sum();
        if n > 0 {
            out.push((rel, n));
        }
    }
}

const WHAT: &str = "hand-read wall clocks per file (`SystemTime::now()`) — each \
                    dependency island's time module is the decider; frozen so debt \
                    can only shrink";

pub fn run(args: &[String]) -> i32 {
    let root = common::repo_root();
    let baseline_path = common::baselines_dir(&root).join("clock_reads.txt");
    let flags = common::baseline_flags(args);

    let scope = match common::SourceTree::discover(&root) {
        Ok(s) => s,
        Err(e) => {
            // ARCH §18.3 — a gate that cannot resolve its own scope refuses
            // rather than rendering a verdict on the wrong tree.
            eprintln!("clock-gate: cannot resolve this repo's source tree: {e}");
            return 1;
        }
    };

    let mut sites: Vec<(String, usize)> = Vec::new();
    collect(&root, &root, &scope, &mut sites);
    sites.sort();
    let current: BTreeMap<String, usize> = sites.iter().cloned().collect();
    let total: usize = current.values().sum();

    if flags.update {
        if let Err(e) = common::write_count_map(&baseline_path, "clock-gate", WHAT, &current) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "wrote {} ({total} clock reads across {} files frozen)",
            baseline_path.display(),
            current.len()
        );
        return 0;
    }

    let baseline = common::load_count_map(&baseline_path);
    if baseline.is_empty() {
        eprintln!(
            "error: no baseline at {}.\n  Run: cargo run -p xtask -- clock-gate --update-baseline",
            baseline_path.display()
        );
        return 1;
    }

    if flags.tighten {
        let tightened: BTreeMap<String, usize> = baseline
            .iter()
            .filter_map(|(rel, &b)| current.get(rel).map(|&n| (rel.clone(), n.min(b))))
            .collect();
        if tightened == baseline {
            eprintln!(
                "clock-gate --tighten: baseline already tight ({} files)",
                baseline.len()
            );
            return 0;
        }
        let dropped = baseline.len() - tightened.len();
        let lowered = tightened
            .iter()
            .filter(|(k, &v)| baseline.get(*k).is_some_and(|&b| v < b))
            .count();
        if let Err(e) = common::write_count_map(&baseline_path, "clock-gate", WHAT, &tightened) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "clock-gate --tighten: {dropped} files cleared, {lowered} lowered → {}",
            baseline_path.display()
        );
        return 0;
    }

    let mut failures: Vec<String> = Vec::new();
    for (rel, n) in &sites {
        match baseline.get(rel) {
            None => failures.push(format!(
                "NEW hand-read clock: {rel} ({n}). Ask the decider instead — {}.",
                decider_for(rel)
            )),
            // No slack: unlike a file's line count, a clock read does not
            // drift in by accident. One more is one more decider.
            Some(&b) if *n > b => failures.push(format!(
                "GREW: {rel} {b} → {n} hand-read clocks. Ask the decider instead — {}.",
                decider_for(rel)
            )),
            _ => {}
        }
    }

    eprintln!(
        "clock-gate: {total} hand-read clocks across {} files vs baseline \
         ({} files, {} reads) — {} failure(s)",
        current.len(),
        baseline.len(),
        baseline.values().sum::<usize>(),
        failures.len()
    );
    if failures.is_empty() {
        return 0;
    }
    for f in &failures {
        eprintln!("  FAIL {f}");
    }
    eprintln!("{}", common::fix_footer("clock-gate"));
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hand_read_wall_clock_is_a_hit() {
        assert_eq!(clock_hits("    let n = SystemTime::now();"), 1);
        assert_eq!(
            clock_hits("        std::time::SystemTime::now()"),
            1,
            "the fully-qualified spelling is the same read"
        );
    }

    #[test]
    fn asking_the_decider_is_not_a_hit() {
        assert_eq!(
            clock_hits("    let n = sovereign_core::time::unix_now();"),
            0
        );
        assert_eq!(
            clock_hits("    let n = corpus_engine_yield::time::unix_now_u64();"),
            0
        );
        assert_eq!(
            clock_hits("    let n = commonwealth_core::clock::unix_now_secs();"),
            0
        );
    }

    #[test]
    fn a_monotonic_instant_is_not_a_wall_clock() {
        assert_eq!(
            clock_hits("    let t = Instant::now();"),
            0,
            "elapsed-time measurement must stay truthful under skew; no accessor replaces it"
        );
    }

    #[test]
    fn prose_about_the_clock_does_not_read_it() {
        assert_eq!(
            clock_hits("    /// Wraps SystemTime::now() for the caller."),
            0
        );
        assert_eq!(clock_hits("    // let t = SystemTime::now();"), 0);
    }

    #[test]
    fn each_island_is_pointed_at_its_own_decider() {
        assert!(decider_for("corpus-engine/src/facts_store.rs").starts_with("corpus_engine_yield"));
        assert!(decider_for("commonwealth/crates/commonwealth-api/src/x.rs")
            .starts_with("commonwealth_core"));
        assert!(
            decider_for("sovereign/crates/sovereign-mesh/src/x.rs").starts_with("sovereign_core")
        );
    }
}
