// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared gate plumbing: repo-root resolution + the uniform baseline
//! (ratchet) file I/O every count-based gate uses.
//!
//! Baseline contract: files live under `quality/baselines/`, are written ONLY
//! by `--update-baseline` (snapshot current state) or `--tighten` (rewrite
//! only improved entries; never add, never raise), and carry a header naming
//! the exact regeneration command so a red X is always self-serviceable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Repo root = grandparent of `corpus-engine/xtask/`.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("xtask manifest has no grandparent")
        .to_path_buf()
}

/// A path rendered repo-relative with forward slashes — the spelling every
/// gate reports and every baseline key uses.
pub fn rel_path(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

// ─── Source-tree scope ──────────────────────────────────────────────
//
// THE one decider for "is this directory part of this repo's authored
// source?" — shared by every whole-tree gate walk (arch-gate, docs-gate,
// env-gate) so the three cannot drift on the answer (ARCH §10.6).
//
// It replaced three independent hardcoded skip-lists that had each rotted
// past four non-source trees (`.cargo-container/`, `.claude/worktrees/`,
// `target-container-linux/`, `…/target-xwin/`). On 2026-08-18 that made
// arch-gate report 2183 failures of which 2115 were not this repo's source —
// a gate that fails on everything is a gate nobody reads.

/// Declared residual exclusions — see the file's own header.
const SOURCE_SCOPE_PATH: &str = "quality/source-tree.toml";

/// Which directories a gate walk may descend into.
pub struct SourceTree {
    /// Repo-relative dirs git reports as ignored (no trailing slash).
    ignored: BTreeSet<String>,
    /// Declared dir NAMES excluded at any depth (what git cannot express).
    excluded_names: BTreeSet<String>,
}

impl SourceTree {
    /// Resolve the scope from its two declared inputs: git's ignore rules
    /// (primary — self-maintaining, so a new build tree is excluded the day
    /// it appears) and `quality/source-tree.toml` (residual — trees that are
    /// TRACKED here but not authored here, e.g. `vendor/`).
    ///
    /// Returns `Err` — never a partial or empty scope — when either input is
    /// unreadable. A gate that cannot tell source from vendored must refuse
    /// rather than render a verdict on the wrong tree (ARCH §18.3).
    pub fn discover(root: &Path) -> Result<Self, String> {
        let declared_path = root.join(SOURCE_SCOPE_PATH);
        let text = std::fs::read_to_string(&declared_path).map_err(|e| {
            format!(
                "cannot read {} — the gates cannot tell this repo's source from vendored trees without it: {e}",
                declared_path.display()
            )
        })?;
        let parsed: toml::Value = text
            .parse()
            .map_err(|e| format!("{}: {e}", declared_path.display()))?;
        let excluded_names: BTreeSet<String> = parsed
            .get("excluded_dirs")
            .and_then(|v| v.as_array())
            .ok_or_else(|| format!("{}: missing `excluded_dirs` array", declared_path.display()))?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();

        let out = std::process::Command::new("git")
            .args([
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
                "--no-empty-directory",
                "-z",
            ])
            .current_dir(root)
            .output()
            .map_err(|e| {
                format!(
                    "cannot run git to resolve ignored trees in {}: {e}",
                    root.display()
                )
            })?;
        if !out.status.success() {
            return Err(format!(
                "git ls-files failed in {} ({}): {}",
                root.display(),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        // `--directory` collapses a wholly-ignored dir to one entry with a
        // trailing slash; ignored FILES come through without one and are not
        // walk roots, so only the dirs matter here.
        let ignored = String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|s| s.ends_with('/'))
            .map(|s| s.trim_end_matches('/').to_string())
            .collect();

        Ok(Self {
            ignored,
            excluded_names,
        })
    }

    /// Build a scope directly — for tests, which must exercise both arms
    /// without depending on the host's git state.
    #[cfg(test)]
    pub fn from_parts(ignored: BTreeSet<String>, excluded_names: BTreeSet<String>) -> Self {
        Self {
            ignored,
            excluded_names,
        }
    }

    /// True when a walk must NOT descend into `rel` (a repo-relative dir
    /// path with forward slashes).
    pub fn excludes_dir(&self, rel: &str) -> bool {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        self.excluded_names.contains(name) || self.ignored.contains(rel)
    }

    /// How many ignored dirs git reported — for the glassbox one-liner each
    /// gate prints, so a scope that silently collapsed is visible.
    pub fn ignored_dir_count(&self) -> usize {
        self.ignored.len()
    }
}

/// Directory all machine-written ratchet baselines live in.
pub fn baselines_dir(root: &Path) -> PathBuf {
    root.join("quality/baselines")
}

/// Parse a `<count>\t<key>` baseline (blank lines + `#` comments skipped).
pub fn load_count_map(path: &Path) -> BTreeMap<String, usize> {
    let mut map = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((count, key)) = line.split_once('\t') {
                if let Ok(n) = count.trim().parse::<usize>() {
                    map.insert(key.trim().to_string(), n);
                }
            }
        }
    }
    map
}

/// Write a `<count>\t<key>` baseline, sorted, with the standard header.
pub fn write_count_map(
    path: &Path,
    gate: &str,
    what: &str,
    map: &BTreeMap<String, usize>,
) -> Result<(), String> {
    let mut body = header(gate, what);
    for (key, n) in map {
        body.push_str(&format!("{n}\t{key}\n"));
    }
    write_baseline(path, &body)
}

/// Parse a one-key-per-line baseline (blank lines + `#` comments skipped).
pub fn load_line_set(path: &Path) -> std::collections::BTreeSet<String> {
    std::fs::read_to_string(path)
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Write a one-key-per-line baseline, sorted, with the standard header.
pub fn write_line_set(
    path: &Path,
    gate: &str,
    what: &str,
    set: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    let mut body = header(gate, what);
    for key in set {
        body.push_str(key);
        body.push('\n');
    }
    write_baseline(path, &body)
}

fn header(gate: &str, what: &str) -> String {
    format!(
        "# {gate} baseline — {what}.\n\
         # MACHINE-WRITTEN. Regenerate (snapshot current state — defend the diff in review):\n\
         #   cargo run -p xtask -- {gate} --update-baseline\n\
         # Bank improvements only (never adds, never raises — safe to run anytime):\n\
         #   cargo run -p xtask -- {gate} --tighten\n"
    )
}

fn write_baseline(path: &Path, body: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, body).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The uniform failure footer: the exact command that makes the red X
/// self-serviceable.
pub fn fix_footer(gate: &str) -> String {
    format!(
        "To accept the current state as intentional (and defend the diff in review):\n  \
         cargo run -p xtask -- {gate} --update-baseline"
    )
}

/// Shared `--update-baseline` / `--tighten` flag parsing.
pub struct BaselineFlags {
    pub update: bool,
    pub tighten: bool,
}

pub fn baseline_flags(args: &[String]) -> BaselineFlags {
    BaselineFlags {
        update: args.iter().any(|a| a == "--update-baseline"),
        tighten: args.iter().any(|a| a == "--tighten"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    /// The scope the live repo produces, in miniature: git reports the four
    /// non-source trees that the old hardcoded skip-list had rotted past.
    fn scope() -> SourceTree {
        SourceTree::from_parts(
            set(&[
                ".cargo-container",
                ".claude/worktrees",
                "target",
                "target-container-linux",
                "sovereign/crates/sovereign-desktop/target-xwin",
            ]),
            set(&[".git", ".sovereign", "vendor", "node_modules"]),
        )
    }

    // Both arms, because an exclusion only ever watched go quiet is
    // indistinguishable from a walk that stopped working (ARCH §18.1).

    #[test]
    fn excludes_the_trees_that_are_not_this_repos_source() {
        let s = scope();
        // git-ignored, the arm that regressed: 2115 of 2183 arch-gate
        // failures on 2026-08-18 came from these four.
        assert!(s.excludes_dir(".cargo-container"));
        assert!(s.excludes_dir(".claude/worktrees"));
        assert!(s.excludes_dir("target-container-linux"));
        assert!(s.excludes_dir("sovereign/crates/sovereign-desktop/target-xwin"));
        // declared: tracked here, but not authored here.
        assert!(s.excludes_dir("vendor"));
        assert!(s.excludes_dir(".git"));
    }

    #[test]
    fn keeps_this_repos_own_source() {
        let s = scope();
        for rel in [
            "corpus-engine/src",
            "sovereign/crates/sovereign-core/src/runtime",
            "commonwealth/crates/commonwealth-api/src",
            "scripts",
            "docs",
            ".claude/hooks",
        ] {
            assert!(!s.excludes_dir(rel), "{rel} is source and must be walked");
        }
    }

    /// The declared exclusions are NAME-based at any depth (the semantics of
    /// the skip-list they replace); the git-ignored ones are exact paths, so
    /// a same-named source dir elsewhere is still walked.
    #[test]
    fn declared_names_match_at_depth_but_ignored_paths_are_exact() {
        let s = scope();
        assert!(s.excludes_dir("some/crate/vendor"));
        assert!(!s.excludes_dir("corpus-engine/src/target-container-linux-notes"));
        // `target` is a declared-by-git path here, not a name rule…
        assert!(s.excludes_dir("target"));
        // …so an unrelated dir that merely CONTAINS the word is walked.
        assert!(!s.excludes_dir("sovereign/crates/targeting"));
    }

    #[test]
    fn rel_path_is_repo_relative_with_forward_slashes() {
        let root = Path::new("/repo");
        assert_eq!(rel_path(&root.join("a").join("b.rs"), root), "a/b.rs");
    }
}
