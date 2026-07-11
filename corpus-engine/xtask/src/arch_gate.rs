// SPDX-License-Identifier: AGPL-3.0-or-later
//! arch-gate — a ratchet for ARCH_PRINCIPLES §3.1 ("> 1200 lines → split")
//! and §1.1 (the SYSTEM_OVERVIEW project map must resolve).
//!
//! It does NOT try to clean existing debt — it FREEZES it via a baseline so
//! the only allowed direction is down: a NEW oversized file or growth past
//! slack fails. Pair with the §10 roadmap (where each baselined file gets its
//! deferral rationale).

use crate::common;
use std::collections::BTreeMap;
use std::path::Path;

const LINE_LIMIT: usize = 1200;
/// Files (unlike counters) legitimately grow a little during unrelated work;
/// the slack keeps the gate about NEW debt, not about ±10-line noise. Count
/// ratchets (fan-in, lock dups, lint counts) get NO slack.
const GROWTH_SLACK: usize = 50;

pub fn run(args: &[String]) -> i32 {
    let root = common::repo_root();
    let baseline_path = common::baselines_dir(&root).join("oversized.txt");
    let flags = common::baseline_flags(args);

    let mut oversized: Vec<(String, usize)> = Vec::new();
    collect_oversized(&root, &root, &mut oversized);
    oversized.sort();
    let current: BTreeMap<String, usize> = oversized.iter().cloned().collect();

    if flags.update {
        if let Err(e) = common::write_count_map(
            &baseline_path,
            "arch-gate",
            "oversized .rs files (ARCH §3.1: > 1200 lines), frozen so debt can only shrink",
            &current,
        ) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "wrote {} ({} oversized files frozen)",
            baseline_path.display(),
            current.len()
        );
        return 0;
    }

    let baseline = common::load_count_map(&baseline_path);
    if baseline.is_empty() {
        eprintln!(
            "error: no baseline at {}.\n  Run: cargo run -p xtask -- arch-gate --update-baseline",
            baseline_path.display()
        );
        return 1;
    }

    if flags.tighten {
        // Keep only entries still oversized; lower counts that shrank. Never
        // add, never raise.
        let tightened: BTreeMap<String, usize> = baseline
            .iter()
            .filter_map(|(rel, &b)| current.get(rel).map(|&n| (rel.clone(), n.min(b))))
            .collect();
        let dropped = baseline.len() - tightened.len();
        let lowered = tightened
            .iter()
            .filter(|(k, &v)| baseline.get(*k).is_some_and(|&b| v < b))
            .count();
        if tightened == baseline {
            eprintln!(
                "arch-gate --tighten: baseline already tight ({} entries)",
                baseline.len()
            );
            return 0;
        }
        if let Err(e) = common::write_count_map(
            &baseline_path,
            "arch-gate",
            "oversized .rs files (ARCH §3.1: > 1200 lines), frozen so debt can only shrink",
            &tightened,
        ) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "arch-gate --tighten: {dropped} entries cleared, {lowered} lowered → {}",
            baseline_path.display()
        );
        return 0;
    }

    let mut failures: Vec<String> = Vec::new();
    for (rel, n) in &oversized {
        match baseline.get(rel) {
            None => failures.push(format!(
                "NEW oversized file: {rel} ({n} lines > {LINE_LIMIT}). \
                 Split it (ARCH §3.2), or — if deferring — add a SYSTEM_OVERVIEW §10 \
                 roadmap entry and re-baseline."
            )),
            Some(&b) if *n > b + GROWTH_SLACK => failures.push(format!(
                "GREW past slack: {rel} {b} → {n} lines (+{}, slack {GROWTH_SLACK}). \
                 Trim or split (ARCH §3.1).",
                n - b
            )),
            _ => {}
        }
    }

    let doc_fails = doc_contract_failures(&root);

    eprintln!(
        "arch-gate: {} oversized files tracked vs baseline; §1 doc-contract checked",
        oversized.len()
    );
    for f in &failures {
        eprintln!("  ✗ size: {f}");
    }
    for f in &doc_fails {
        eprintln!("  ✗ doc:  {f}");
    }
    if failures.is_empty() && doc_fails.is_empty() {
        eprintln!("  ✓ no new oversized files, none grown past slack, §1 project dirs all resolve");
        0
    } else {
        eprintln!();
        eprintln!(
            "arch-gate FAILED ({} size, {} doc). See ARCH_PRINCIPLES.md §3 + SYSTEM_OVERVIEW.md §10.",
            failures.len(),
            doc_fails.len()
        );
        eprintln!("{}", common::fix_footer("arch-gate"));
        1
    }
}

/// Collect (repo-relative path, line count) for every `.rs` over the limit,
/// skipping build/vendor/vcs trees.
fn collect_oversized(dir: &Path, root: &Path, out: &mut Vec<(String, usize)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                "target" | "vendor" | ".git" | "node_modules" | ".sovereign"
            ) {
                continue;
            }
            collect_oversized(&path, root, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let lines = text.lines().count();
                if lines > LINE_LIMIT {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push((rel, lines));
                }
            }
        }
    }
}

/// §1.1 contract: every project dir named in the SYSTEM_OVERVIEW §1 tree exists.
fn doc_contract_failures(root: &Path) -> Vec<String> {
    let overview = root.join("sovereign/SYSTEM_OVERVIEW.md");
    let text = match std::fs::read_to_string(&overview) {
        Ok(t) => t,
        Err(e) => return vec![format!("cannot read {}: {e}", overview.display())],
    };
    let mut fails = Vec::new();
    // §1 has two fenced blocks: the project tree (first) lists the dirs we
    // verify; the dependency diagram (second) is ASCII art, not paths. Only
    // parse the FIRST fence.
    let (mut in_sec1, mut fences_seen) = (false, 0u8);
    for line in text.lines() {
        if line.starts_with("## 1.") {
            in_sec1 = true;
            continue;
        }
        if in_sec1 && line.starts_with("## ") {
            break;
        }
        if in_sec1 && line.trim_start().starts_with("```") {
            fences_seen += 1;
            continue;
        }
        if in_sec1 && fences_seen == 1 {
            let trimmed = line.trim_start_matches(['│', ' ', '├', '└', '─']);
            if let Some(tok) = trimmed.split_whitespace().next() {
                let dir = tok.trim_end_matches('/');
                if dir.is_empty() || dir == "commonwealth-ai" {
                    continue;
                }
                if !root.join(dir).exists() {
                    fails.push(format!(
                        "§1 names project `{dir}/` but it does not exist on disk"
                    ));
                }
            }
        }
    }
    fails
}
