// SPDX-License-Identifier: AGPL-3.0-or-later
//! lock-gate — no NEW duplicate crate versions in `Cargo.lock`.
//!
//! Every semver-incompatible duplicate compiles (and audits, and debugs)
//! twice. cargo-deny's `multiple-versions = "warn"` makes duplicates visible;
//! THIS gate is the enforcement — a ratchet over the set of duplicated crate
//! NAMES, so today's ~120 (mostly `windows-*`) are grandfathered and the
//! failure mode is "this PR forked a crate that used to resolve to one
//! version". Parses `Cargo.lock` directly: no cargo invocation, no network,
//! platform-independent.

use std::collections::{BTreeMap, BTreeSet};

use crate::common;

pub fn run(args: &[String]) -> i32 {
    let root = common::repo_root();
    let flags = common::baseline_flags(args);
    let baseline_path = common::baselines_dir(&root).join("lock_dups.txt");

    let lock_text = match std::fs::read_to_string(root.join("Cargo.lock")) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read Cargo.lock: {e}");
            return 1;
        }
    };
    let dups = duplicated_names(&lock_text);

    if flags.update {
        if let Err(e) = common::write_line_set(
            &baseline_path,
            "lock-gate",
            "crate names resolved at >1 version in Cargo.lock (grandfathered; may only shrink)",
            &dups,
        ) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "wrote {} ({} duplicated crate names frozen)",
            baseline_path.display(),
            dups.len()
        );
        return 0;
    }

    let baseline = common::load_line_set(&baseline_path);
    if baseline.is_empty() && !baseline_path.exists() {
        eprintln!(
            "error: no baseline at {}.\n  Run: cargo run -p xtask -- lock-gate --update-baseline",
            baseline_path.display()
        );
        return 1;
    }

    if flags.tighten {
        let tightened: BTreeSet<String> = baseline.intersection(&dups).cloned().collect();
        if tightened == baseline {
            eprintln!(
                "lock-gate --tighten: baseline already tight ({} entries)",
                baseline.len()
            );
            return 0;
        }
        let cleared = baseline.len() - tightened.len();
        if let Err(e) = common::write_line_set(
            &baseline_path,
            "lock-gate",
            "crate names resolved at >1 version in Cargo.lock (grandfathered; may only shrink)",
            &tightened,
        ) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "lock-gate --tighten: {cleared} names de-duplicated since baseline → {}",
            baseline_path.display()
        );
        return 0;
    }

    let new_dups: Vec<&String> = dups.difference(&baseline).collect();
    let healed = baseline.difference(&dups).count();

    eprintln!(
        "lock-gate: {} duplicated crate names in Cargo.lock vs {} grandfathered \
         ({healed} healed since baseline — bank them with --tighten)",
        dups.len(),
        baseline.len()
    );
    for name in &new_dups {
        eprintln!(
            "  ✗ NEW duplicate: `{name}` now resolves at multiple versions. Unify the \
             requirement (often: bump the lagging dependent, or add it to \
             [workspace.dependencies]), or accept the fork explicitly by re-baselining."
        );
    }
    if new_dups.is_empty() {
        eprintln!("  ✓ no new duplicated crates");
        0
    } else {
        eprintln!();
        eprintln!("lock-gate FAILED ({} new duplicates).", new_dups.len());
        eprintln!("{}", common::fix_footer("lock-gate"));
        1
    }
}

/// Crate names appearing in more than one `[[package]]` block. `name = "…"`
/// is the first key of every block in lockfile v3/v4, so a plain line scan
/// keyed on `name = ` inside package blocks is sufficient and stable.
fn duplicated_names(lock_text: &str) -> BTreeSet<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut in_package = false;
    for line in lock_text.lines() {
        let t = line.trim();
        if t == "[[package]]" {
            in_package = true;
            continue;
        }
        if t.starts_with('[') {
            in_package = false;
            continue;
        }
        if in_package {
            if let Some(rest) = t.strip_prefix("name = \"") {
                if let Some(name) = rest.strip_suffix('"') {
                    *counts.entry(name).or_default() += 1;
                }
            }
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(k, _)| k.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_only_duplicated_names() {
        let lock = "\
version = 4\n\
\n\
[[package]]\n\
name = \"serde\"\n\
version = \"1.0.100\"\n\
\n\
[[package]]\n\
name = \"hashbrown\"\n\
version = \"0.14.5\"\n\
\n\
[[package]]\n\
name = \"hashbrown\"\n\
version = \"0.15.2\"\n\
\n\
[[package]]\n\
name = \"xtask\"\n\
version = \"0.1.0\"\n";
        let dups = duplicated_names(lock);
        assert_eq!(dups.len(), 1);
        assert!(dups.contains("hashbrown"));
    }

    #[test]
    fn name_lines_outside_package_blocks_are_ignored() {
        let lock = "\
[[package]]\n\
name = \"a\"\n\
\n\
[metadata]\n\
name = \"a\"\n";
        assert!(duplicated_names(lock).is_empty());
    }
}
