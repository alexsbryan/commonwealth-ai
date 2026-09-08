// SPDX-License-Identifier: AGPL-3.0-or-later
//! size-gate — the ninth ratchet: CODE lines per crate, which may only shrink.
//!
//! Every other gate here answers "is this correct?", and correctness is
//! monotone: no amount of added code can make a passing test fail. So the
//! definition of done has never had a size term, and the workspace has run
//! roughly +622k / −179k over 90 days (`quality/DELETION.md`) — it deletes
//! 29 lines per 100 it adds. This gate adds the missing term, in the shape
//! the other eight already use: a frozen baseline, `--update-baseline` to
//! accept a rise and defend the diff in review, `--tighten` to bank a
//! reduction permanently.
//!
//! `--tighten` is the incentive, not the check. Banking a reduction lowers a
//! ceiling that can never rise again without a reviewed line, and it shows up
//! in the commit as a baseline file that got smaller. That is what "finished
//! with less" looks like as an artifact instead of a claim in a summary.
//!
//! THREE COUNTING RULES, each closing a way to win without improving anything:
//!
//! 1. **Comments and blanks are not counted.** Stripping documentation to get
//!    under a ceiling is worth exactly zero.
//! 2. **Test lines are their own key** (`<crate>::tests`), with their own
//!    ceiling. Production headroom cannot be bought by deleting tests.
//! 3. **Per crate, not per workspace.** Growth is attributable; one crate's
//!    diet cannot pay for another's expansion.
//!
//! SCRIPT DIRECTORIES COUNT TOO, since 2026-09-04. The workspace runs about
//! +622k / −179k over 90 days and this gate could only see the Rust half of
//! it — so a campaign that deleted 107 lines of shell in one day registered on
//! nothing. `scripts/` and the desktop's e2e script tree are now keys of their
//! own over `.sh`/`.py`/`.mjs`/`.ts`, with the same comments-and-blanks-excluded
//! rule and the same shrink-only ceiling.
//!
//! They are ONE key each, with no `::tests` half: for a directory of harnesses
//! the whole directory is tooling, and a "production vs test" split there would
//! be a distinction the gate invents rather than one the tree makes.
//!
//! What it does NOT claim: lines are a proxy. The quantity that matters is how
//! many distinct things a reader must hold, and `concept-gate` is the ratchet
//! for that. A rising concept count is the more serious signal; this is the
//! cheap daily one. And the Python approximation is stated rather than hidden:
//! a `"""docstring"""` counts as code, because tracking string state across a
//! whole file to save a few lines would make the counter the thing most likely
//! to be wrong.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::common;

/// Suffix marking the test half of a crate's key.
const TEST_SUFFIX: &str = "::tests";

/// Script trees measured as keys of their own. Repo-relative, so the key a
/// failure prints is the path a reader chases — the same contract the crate
/// keys keep.
const SCRIPT_DIRS: [&str; 2] = [
    "scripts",
    "sovereign/crates/sovereign-desktop/tests/e2e/scripts",
];

/// Extensions counted inside a [`SCRIPT_DIRS`] tree.
const SCRIPT_EXTS: [&str; 4] = ["sh", "py", "mjs", "ts"];

pub fn run(args: &[String]) -> i32 {
    // `--root <path>`: measure a DIFFERENT checkout. This is what makes the
    // documented re-pin recipe work — freeze the baseline from a worktree at
    // `origin/main` so it holds nobody's uncommitted lines, yours included,
    // then copy `quality/baselines/lines.tsv` back. Same flag `arch-report`
    // already carries, same reason.
    let root = args
        .windows(2)
        .find(|w| w[0] == "--root")
        .map(|w| PathBuf::from(&w[1]))
        .unwrap_or_else(common::repo_root);
    let flags = common::baseline_flags(args);
    // `--accept <key>` raises ONE ceiling to its current value. It exists
    // because `--update-baseline` on a working tree absorbs every other
    // crate's growth along with yours — the trap AGENTS.md names for exactly
    // these ratchets. A rise should be one reviewable line naming one crate,
    // not a snapshot of whatever the tree happened to hold.
    let accept: Vec<String> = args
        .windows(2)
        .filter(|w| w[0] == "--accept")
        .map(|w| w[1].clone())
        .collect();
    let baseline_path = common::baselines_dir(&root).join("lines.tsv");
    let what = "code lines per crate and per script tree — comments and blanks excluded, \
                `<crate>::tests` counted separately (may only shrink)";

    let scope = match common::SourceTree::discover(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let current = measure(&root, &scope);
    if current.is_empty() {
        eprintln!(
            "error: size-gate measured zero crates — the scope resolved to nothing, \
             which is a broken instrument, not a clean repo (ARCH §18.3)"
        );
        return 1;
    }

    if flags.update {
        if let Err(e) = common::write_count_map(&baseline_path, "size-gate", what, &current) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "wrote {} ({} keys, {} code lines frozen)",
            baseline_path.display(),
            current.len(),
            total_of(&current, false)
        );
        return 0;
    }

    let baseline = common::load_count_map(&baseline_path);
    if baseline.is_empty() && !baseline_path.exists() {
        eprintln!(
            "error: no baseline at {}.\n  Run: cargo run -p xtask -- size-gate --update-baseline",
            baseline_path.display()
        );
        return 1;
    }

    if !accept.is_empty() {
        let mut next = baseline.clone();
        let mut raised = 0usize;
        for key in &accept {
            let Some(&now) = current.get(key.as_str()) else {
                eprintln!(
                    "error: --accept {key}: no such key. Keys are repo-relative crate \
                     directories, or one of the script trees {SCRIPT_DIRS:?}, exactly as \
                     printed by this gate (add `{TEST_SUFFIX}` for a crate's test half)."
                );
                return 1;
            };
            let cap = next.get(key).copied().unwrap_or(0);
            if now <= cap {
                eprintln!(
                    "--accept {key}: already at or under its ceiling ({cap}) — nothing to raise"
                );
                continue;
            }
            eprintln!("--accept {key}: {cap} → {now} (+{})", now - cap);
            next.insert(key.clone(), now);
            raised += 1;
        }
        if raised == 0 {
            return 0;
        }
        if let Err(e) = common::write_count_map(&baseline_path, "size-gate", what, &next) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "size-gate --accept: {raised} ceiling(s) raised → {}. Say in the commit what the \
             lines bought.",
            baseline_path.display()
        );
        return 0;
    }

    if flags.tighten {
        let mut tightened = baseline.clone();
        let mut lowered = 0usize;
        let mut banked = 0usize;
        for (key, cap) in baseline.iter() {
            let now = current.get(key).copied().unwrap_or(0);
            if now < *cap {
                banked += cap - now;
                lowered += 1;
                tightened.insert(key.clone(), now);
            }
        }
        // A crate that no longer exists cannot be a ceiling.
        tightened.retain(|k, _| current.contains_key(k));
        let cleared = baseline.len() - tightened.len();
        if tightened == baseline {
            eprintln!(
                "size-gate --tighten: baseline already tight ({} keys)",
                baseline.len()
            );
            return 0;
        }
        if let Err(e) = common::write_count_map(&baseline_path, "size-gate", what, &tightened) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "size-gate --tighten: {lowered} ceilings lowered, {cleared} cleared, \
             {banked} lines banked → {}",
            baseline_path.display()
        );
        return 0;
    }

    verify(&current, &baseline)
}

fn total_of(map: &BTreeMap<String, usize>, tests: bool) -> usize {
    map.iter()
        .filter(|(k, _)| k.ends_with(TEST_SUFFIX) == tests)
        .map(|(_, n)| *n)
        .sum()
}

fn verify(current: &BTreeMap<String, usize>, baseline: &BTreeMap<String, usize>) -> i32 {
    let mut failures: Vec<String> = Vec::new();
    for (key, &n) in current {
        let cap = baseline.get(key).copied().unwrap_or(0);
        if n > cap {
            let what = if baseline.contains_key(key) {
                "grew"
            } else {
                "is new and unbaselined"
            };
            failures.push(format!("{key}: {cap} → {n} (+{}) — {what}", n - cap));
        }
    }
    failures.sort_by_key(|f| std::cmp::Reverse(f.len()));

    let code_now = total_of(current, false);
    let test_now = total_of(current, true);
    let code_was = total_of(baseline, false);
    let improved = baseline
        .iter()
        .filter(|(k, &b)| current.get(*k).copied().unwrap_or(0) < b)
        .count();

    // The scoreboard line: size is a number this run PRINTS, next to the
    // count of what it is allowed to be. A gate whose result is invisible in
    // the closing summary is a gate nobody optimises against.
    eprintln!(
        "size-gate: {code_now} code lines (+{test_now} test) across {} crates; \
         baseline {code_was}; {improved} ceilings beatable — bank with --tighten",
        current.len() - current.keys().filter(|k| k.ends_with(TEST_SUFFIX)).count()
    );
    for f in failures.iter().take(12) {
        eprintln!("  ✗ {f}");
    }
    if failures.len() > 12 {
        eprintln!("  … and {} more", failures.len() - 12);
    }
    if failures.is_empty() {
        eprintln!("  ✓ no crate grew");
        0
    } else {
        eprintln!();
        eprintln!("size-gate FAILED ({} keys grew).", failures.len());
        eprintln!("{}", common::fix_footer("size-gate"));
        1
    }
}

/// `<crate>` → code lines, `<crate>::tests` → test lines, plus one key per
/// [`SCRIPT_DIRS`] tree.
fn measure(root: &Path, scope: &common::SourceTree) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for dir in SCRIPT_DIRS {
        let n = measure_scripts(&root.join(dir), scope, root);
        if n > 0 {
            out.insert(dir.to_string(), n);
        }
    }
    let mut files: Vec<PathBuf> = Vec::new();
    collect_rs(root, root, scope, &mut files);
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(krate) = owning_crate(&path, root) else {
            continue;
        };
        let rel = common::rel_path(&path, root);
        let (code, tests) = count(&text, in_test_tree(&rel, &krate));
        if code > 0 {
            *out.entry(krate.clone()).or_default() += code;
        }
        if tests > 0 {
            *out.entry(format!("{krate}{TEST_SUFFIX}")).or_default() += tests;
        }
    }
    out
}

/// Code lines under one script tree. A missing tree contributes 0 and the key
/// is then absent from the map entirely — which `--tighten` reads as "cleared"
/// rather than as a ceiling of zero nothing can ever meet.
fn measure_scripts(dir: &Path, scope: &common::SourceTree, root: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !scope.excludes_dir(&common::rel_path(&path, root)) {
                total += measure_scripts(&path, scope, root);
            }
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !SCRIPT_EXTS.contains(&ext) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        total += count_script(&text, ext);
    }
    total
}

/// Blank lines and comment-only lines are worth zero here too. `#` for
/// sh/py (a shebang included — it is a line the reader does not read), `//`
/// and `/* … */` for mjs/ts.
fn count_script(text: &str, ext: &str) -> usize {
    let hash = ext == "sh" || ext == "py";
    let mut code = 0usize;
    let mut in_block = false;
    for line in text.lines() {
        let t = line.trim();
        if hash {
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            code += 1;
        } else if !is_noise(t, &mut in_block) {
            code += 1;
        }
    }
    code
}

/// `<crate-dir>/tests/…` and `<crate-dir>/benches/…` are test mass whole.
fn in_test_tree(rel: &str, krate: &str) -> bool {
    let _ = krate;
    rel.contains("/tests/") || rel.contains("/benches/") || rel.contains("/examples/")
}

/// The nearest ancestor directory holding a `Cargo.toml`, by its dir name.
/// Directory name, not the manifest's `[package] name`: this gate reports
/// where the lines LIVE, and a reader chases a path.
fn owning_crate(path: &Path, root: &Path) -> Option<String> {
    let mut dir = path.parent()?;
    loop {
        if dir.join("Cargo.toml").is_file() && dir != root {
            return Some(common::rel_path(dir, root));
        }
        dir = dir.parent()?;
        if !dir.starts_with(root) {
            return None;
        }
    }
}

/// `(code, test)` lines. Blank and comment-only lines count as neither.
/// Inline test mass is everything from the first line-initial `#[cfg(test)]`
/// to end of file — the convention in this workspace, and an approximation
/// this gate states rather than hides.
fn count(text: &str, whole_file_is_test: bool) -> (usize, usize) {
    let mut code = 0usize;
    let mut tests = 0usize;
    let mut in_block = false;
    let mut in_cfg_test = whole_file_is_test;
    for line in text.lines() {
        let t = line.trim();
        if !in_cfg_test && t.starts_with("#[cfg(test)]") {
            in_cfg_test = true;
        }
        let counted = !is_noise(t, &mut in_block);
        if counted {
            if in_cfg_test {
                tests += 1;
            } else {
                code += 1;
            }
        }
    }
    (code, tests)
}

/// Blank, line comment, doc comment, or inside a block comment.
fn is_noise(t: &str, in_block: &mut bool) -> bool {
    if *in_block {
        if t.contains("*/") {
            *in_block = false;
        }
        return true;
    }
    if t.is_empty() {
        return true;
    }
    if t.starts_with("//") {
        return true;
    }
    if t.starts_with("/*") {
        if !t.contains("*/") {
            *in_block = true;
        }
        return true;
    }
    false
}

fn collect_rs(dir: &Path, root: &Path, scope: &common::SourceTree, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !scope.excludes_dir(&common::rel_path(&path, root)) {
                collect_rs(&path, root, scope, out);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_blanks_are_not_code() {
        let src = "// a comment\n\nfn f() {}\n/* block\n   still block */\nfn g() {}\n";
        assert_eq!(count(src, false), (2, 0));
    }

    #[test]
    fn inline_cfg_test_mass_lands_in_the_test_column() {
        let src = "fn f() {}\n\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n";
        assert_eq!(count(src, false), (1, 4));
    }

    #[test]
    fn a_test_tree_file_is_test_mass_whole() {
        let src = "use x;\nfn t() {}\n";
        assert_eq!(count(src, true), (0, 2));
        assert!(in_test_tree("a/tests/main.rs", "a"));
        assert!(in_test_tree("a/benches/b.rs", "a"));
        assert!(!in_test_tree("a/src/lib.rs", "a"));
    }

    /// Both comment styles, and the Python approximation the module doc
    /// states: a docstring counts as code.
    #[test]
    fn script_comments_and_blanks_are_not_code() {
        assert_eq!(
            count_script("#!/bin/sh\n\n# a note\nset -e\necho hi\n", "sh"),
            2
        );
        assert_eq!(count_script("# c\nimport os\n\nx = 1\n", "py"), 2);
        assert_eq!(
            count_script(
                "// c\nconst a = 1;\n/* block\n   still */\nconst b = 2;\n",
                "mjs"
            ),
            2
        );
        // Stated, not hidden: the docstring is code to this counter.
        assert_eq!(
            count_script("def f():\n    \"\"\"doc\"\"\"\n    return 1\n", "py"),
            3
        );
    }

    /// The two trees are keys, and only the four declared extensions count —
    /// a `.md` or a `.json` beside a harness is not shell.
    #[test]
    fn only_the_declared_script_extensions_count() {
        for ext in ["sh", "py", "mjs", "ts"] {
            assert!(SCRIPT_EXTS.contains(&ext), "{ext}");
        }
        for ext in ["md", "json", "toml", "rs"] {
            assert!(!SCRIPT_EXTS.contains(&ext), "{ext}");
        }
        assert_eq!(SCRIPT_DIRS.len(), 2);
    }

    #[test]
    fn totals_split_on_the_test_suffix() {
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), 10);
        m.insert(format!("a{TEST_SUFFIX}"), 4);
        assert_eq!(total_of(&m, false), 10);
        assert_eq!(total_of(&m, true), 4);
    }
}
