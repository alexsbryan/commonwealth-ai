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

/// ARCH §3.1's middle band: "800–1200 — has to justify itself."
///
/// WHY THIS RATCHET EXISTS. The >1200 baseline freezes the TAIL and says
/// nothing about the APPROACH, so a file travels 400 → 1199 with nothing
/// reported and then fails as a "NEW oversized file" — blaming whoever wrote
/// line 1201 for accretion that took months. Worse, it lets a SPLIT refill:
/// measured 2026-08-27 against the sizes §3.3 records for files this repo
/// already decomposed, every one had regrown — `runtime/streaming.rs` 1,950 →
/// 4,596 (2.36x), `model_slot.rs` 3,475 → 5,727, `notes.rs` 5,634 → 7,794,
/// 1.43x in aggregate — and `runtime/prompts.rs` (733 → 1,058) and
/// `runtime/turn.rs` (680 → 849), themselves PRODUCTS of the June split, had
/// climbed back into this band unremarked. Splitting without watching the
/// approach is a one-time payment on a recurring bill.
///
/// The one file that did NOT regrow (`daemon_cmd/mod.rs`, 2,378 → 1,372) is
/// the one whose §3.3 entry declares an accepted END STATE. That is the real
/// lesson and this ratchet is its cheap half: a split's products stay the size
/// the split gave them.
const APPROACH_FLOOR: usize = 800;
/// Files (unlike counters) legitimately grow a little during unrelated work;
/// the slack keeps the gate about NEW debt, not about ±10-line noise. Count
/// ratchets (fan-in, lock dups, lint counts) get NO slack.
const GROWTH_SLACK: usize = 50;

/// The compass every session loads. `AGENTS.md` is the cross-harness source;
/// `.claude/CLAUDE.md` imports it and adds this harness's specifics.
///
/// WHY THIS RATCHET EXISTS. Work order `claude-md-slim` (landed 2026-08-07)
/// cut the instruction surface from 55,439 chars against a <= 22,000 target,
/// reached 30,010, and shipped NO ratchet. Measured 2026-08-27 it is back to
/// 42,142 — 76% of the way to where it started, in nineteen days, and nothing
/// reported the regrowth. This is the one size budget whose cost is paid
/// per-AGENT rather than per-reader: every byte is re-read by every session
/// and every spawned worker, so it is charged again on each fan-out.
const INSTRUCTION_SURFACE: &[&str] = &["AGENTS.md", ".claude/CLAUDE.md"];

/// Bytes per instruction file. A counter ratchet, so — like fan-in and lock
/// dups and unlike `oversized.txt` — it gets NO growth slack.
fn instruction_surface_bytes(root: &Path) -> BTreeMap<String, usize> {
    INSTRUCTION_SURFACE
        .iter()
        .filter_map(|rel| {
            std::fs::metadata(root.join(rel))
                .ok()
                .map(|m| ((*rel).to_string(), m.len() as usize))
        })
        .collect()
}

/// Files in ARCH §3.1's 800–1200 band, and their total mass.
///
/// TWO NUMBERS, NOT A PER-FILE BASELINE, and the reason is the handoff: a file
/// crossing 1200 LEAVES this band, which would read as an improvement here
/// while being a regression. It cannot hide, because crossing 1200 fails the
/// oversized ratchet in the same run — the two gates cover both directions
/// between them, and neither needs to know about the other.
fn approach_band(root: &Path, scope: &common::SourceTree) -> BTreeMap<String, usize> {
    let mut files: Vec<(String, usize)> = Vec::new();
    collect_band(root, root, scope, &mut files);
    let mut m = BTreeMap::new();
    m.insert("files".to_string(), files.len());
    m.insert("lines".to_string(), files.iter().map(|(_, n)| n).sum());
    m
}

fn collect_band(
    dir: &Path,
    root: &Path,
    scope: &common::SourceTree,
    out: &mut Vec<(String, usize)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !scope.excludes_dir(&common::rel_path(&path, root)) {
                collect_band(&path, root, scope, out);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let n = text.lines().count();
                if n >= APPROACH_FLOOR && n <= LINE_LIMIT {
                    out.push((common::rel_path(&path, root), n));
                }
            }
        }
    }
}

pub fn run(args: &[String]) -> i32 {
    let root = common::repo_root();
    let baseline_path = common::baselines_dir(&root).join("oversized.txt");
    let instr_path = common::baselines_dir(&root).join("instruction_surface.txt");
    let flags = common::baseline_flags(args);

    let scope = match common::SourceTree::discover(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("arch-gate: cannot resolve this repo's source tree: {e}");
            return 1;
        }
    };

    let instr_now = instruction_surface_bytes(&root);
    let instr_base = common::load_count_map(&instr_path);

    let band_path = common::baselines_dir(&root).join("approach_band.txt");
    let band_now = approach_band(&root, &scope);
    let band_base = common::load_count_map(&band_path);

    let mut oversized: Vec<(String, usize)> = Vec::new();
    collect_oversized(&root, &root, &scope, &mut oversized);
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
        if let Err(e) = common::write_count_map(
            &instr_path,
            "arch-gate",
            "instruction-surface bytes (the compass every session loads), frozen so it can only shrink",
            &instr_now,
        ) {
            eprintln!("error: {e}");
            return 1;
        }
        if let Err(e) =
            common::write_count_map(&band_path, "arch-gate", "files and total lines in ARCH §3.1's 800-1200 approach band, frozen so a split cannot refill", &band_now)
        {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "wrote {} ({} oversized files frozen), {} ({} instruction file(s)) and {} ({} files / \
             {} lines in the approach band)",
            baseline_path.display(),
            current.len(),
            instr_path.display(),
            instr_now.len(),
            band_path.display(),
            band_now.get("files").copied().unwrap_or(0),
            band_now.get("lines").copied().unwrap_or(0),
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
        // Bank instruction-surface shrinkage first — same rule, never raise.
        if !instr_base.is_empty() {
            let tight: BTreeMap<String, usize> = instr_base
                .iter()
                .map(|(k, &b)| (k.clone(), instr_now.get(k).copied().unwrap_or(b).min(b)))
                .collect();
            if tight != instr_base {
                if let Err(e) = common::write_count_map(
                    &instr_path,
                    "arch-gate",
                    "instruction-surface bytes (the compass every session loads), frozen so it can only shrink",
                    &tight,
                ) {
                    eprintln!("error: {e}");
                    return 1;
                }
                eprintln!(
                    "arch-gate --tighten: instruction surface lowered → {}",
                    instr_path.display()
                );
            }
        }
        // Same rule for the approach band: bank a fall, never a rise.
        if !band_base.is_empty() {
            let tight: BTreeMap<String, usize> = band_base
                .iter()
                .map(|(k, &b)| (k.clone(), band_now.get(k).copied().unwrap_or(b).min(b)))
                .collect();
            if tight != band_base {
                if let Err(e) =
                    common::write_count_map(&band_path, "arch-gate", "files and total lines in ARCH §3.1's 800-1200 approach band, frozen so a split cannot refill", &tight)
                {
                    eprintln!("error: {e}");
                    return 1;
                }
                eprintln!(
                    "arch-gate --tighten: approach band lowered -> {}",
                    band_path.display()
                );
            }
        }
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

    // No slack: this is a counter ratchet, and its cost is per-agent.
    for (rel, &now) in &instr_now {
        match instr_base.get(rel) {
            None => failures.push(format!(
                "NEW instruction file not budgeted: {rel} ({now} bytes). \
                 Re-baseline: cargo run -p xtask -- arch-gate --update-baseline"
            )),
            Some(&b) if now > b => failures.push(format!(
                "instruction surface GREW: {rel} {b} → {now} bytes (+{}). Every session and every \
                 spawned worker re-reads this. Distill and point (ARCH §1), or bank a real cut with \
                 `arch-gate --tighten`.",
                now - b
            )),
            _ => {}
        }
    }

    // The approach band. A COUNTER ratchet, so no slack -- same rule as the
    // instruction surface above and for the same reason: this number exists to
    // stop a trend, and a trend with slack is a trend.
    if band_base.is_empty() {
        eprintln!(
            "arch-gate: NEVER-RAN for the approach band -- no baseline at {}. This is not a \
             pass. Mint it: cargo run -p xtask -- arch-gate --update-baseline",
            band_path.display()
        );
    } else {
        for (k, &now) in &band_now {
            if let Some(&b) = band_base.get(k) {
                if now > b {
                    failures.push(format!(
                        "approach band GREW: {k} {b} -> {now} (+{}). These are the files between \
                         ARCH 3.1's \"has to justify itself\" line ({APPROACH_FLOOR}) and the \
                         {LINE_LIMIT} ceiling -- the queue that becomes next month's backlog, and \
                         where a split refills. Trim one back under {APPROACH_FLOOR}, or bank a \
                         real cut with `arch-gate --tighten`.",
                        now - b
                    ));
                }
            }
        }
    }

    let doc_fails = doc_contract_failures(&root);

    eprintln!(
        "arch-gate: {} oversized files tracked vs baseline; {} file(s) / {} lines in the \
         {APPROACH_FLOOR}-{LINE_LIMIT} approach band; §1 doc-contract checked \
         (source scope: {} non-source trees excluded)",
        oversized.len(),
        band_now.get("files").copied().unwrap_or(0),
        band_now.get("lines").copied().unwrap_or(0),
        scope.ignored_dir_count()
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

/// Collect (repo-relative path, line count) for every `.rs` over the limit
/// **within this repo's own source** — `scope` is the single decider for what
/// that means (`common::SourceTree`), so vendored dependency trees, build
/// outputs and agent worktree copies of this repo are never counted.
fn collect_oversized(
    dir: &Path,
    root: &Path,
    scope: &common::SourceTree,
    out: &mut Vec<(String, usize)>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if scope.excludes_dir(&common::rel_path(&path, root)) {
                continue;
            }
            collect_oversized(&path, root, scope, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let lines = text.lines().count();
                if lines > LINE_LIMIT {
                    out.push((common::rel_path(&path, root), lines));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// An identical oversized file planted in this repo's source and in a
    /// vendored tree must produce EXACTLY ONE failure. Watching only the
    /// quiet arm cannot tell an exclusion from a broken walk (ARCH §18.1).
    #[test]
    fn walk_counts_source_and_ignores_vendored_copies_of_the_same_file() {
        let tmp = std::env::temp_dir().join(format!("archgate-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let oversized = "// line\n".repeat(LINE_LIMIT + 1);

        for dir in [
            "corpus-engine/src",                 // source
            ".cargo-container/registry/foo-1.0", // vendored dependency
            ".claude/worktrees/agent-abc/src",   // agent copy of this repo
            "vendor/llama-cpp-4/src",            // tracked, not authored here
        ] {
            std::fs::create_dir_all(tmp.join(dir)).expect("mkdir");
            std::fs::write(tmp.join(dir).join("big.rs"), &oversized).expect("write");
        }

        let scope = common::SourceTree::from_parts(
            [".cargo-container", ".claude/worktrees"]
                .iter()
                .map(|s| (*s).to_string())
                .collect::<BTreeSet<_>>(),
            ["vendor", ".git"]
                .iter()
                .map(|s| (*s).to_string())
                .collect::<BTreeSet<_>>(),
        );

        let mut out = Vec::new();
        collect_oversized(&tmp, &tmp, &scope, &mut out);
        let _ = std::fs::remove_dir_all(&tmp);

        let found: Vec<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            found,
            vec!["corpus-engine/src/big.rs"],
            "the walk must count this repo's source and nothing else"
        );
        assert_eq!(out[0].1, LINE_LIMIT + 1);
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
