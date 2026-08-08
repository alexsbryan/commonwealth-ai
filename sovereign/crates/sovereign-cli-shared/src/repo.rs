// SPDX-License-Identifier: AGPL-3.0-or-later
//! Git-repo and `.sovereign/` directory resolution.
//!
//! These shell out to `git rev-parse` rather than depending on
//! `gix` / `git2` — sovereign treats the CLI surface as truth, and
//! the existing daemon + tooling already require `git` on PATH.
//! Keeping it shell-based avoids pulling a 200KLOC dep into every
//! CLI binary that needs to know "am I in a repo?"

use std::path::{Path, PathBuf};

/// Walk upward from `start` looking for the first directory that contains a
/// `.sovereign/` subdirectory. Returns the `.sovereign/` path if found.
pub fn find_sovereign_dir(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        let candidate = current.join(".sovereign");
        if candidate.is_dir() {
            return Some(candidate);
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => return None,
        }
    }
}

/// The registry `corpus_id` for a repo root: its directory name.
///
/// Lives here rather than in `sovereign-mesh::projects` (where the
/// registry itself lives) because BOTH the dispatcher's `svrn project
/// register` and `svrn setup`'s direct registry write need it, and
/// `sovereign-cli` deliberately does not depend on `sovereign-mesh` —
/// keeping the shipped dispatcher off the workbench's heavy crates is
/// the point (`sovereign-cli/Cargo.toml`, dep-surface note). Two
/// derivations would mean `setup` registering one id and `project list`
/// showing another (ARCH §10.6).
pub fn derive_corpus_id(root: &Path) -> String {
    root.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string()
}

/// `git rev-parse --show-toplevel` from the current working directory.
/// `None` if not inside a git repo (or git is unavailable).
pub fn find_repo_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    find_repo_root_in(&cwd)
}

/// `git rev-parse --show-toplevel` as evaluated *from `start`*, rather than
/// from the process's own cwd.
///
/// Needed by any command that resolves a repo for a path the caller named
/// (`--project <path>`) instead of the directory it happens to be sitting in.
pub fn find_repo_root_in(start: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(start)
        .output()
        .ok()?;
    if output.status.success() {
        let s = String::from_utf8_lossy(&output.stdout);
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(PathBuf::from(trimmed))
    } else {
        None
    }
}

/// `git rev-parse --abbrev-ref HEAD` for the given repo. `None` on
/// unborn HEAD, detached HEAD, or git failure. Best-effort: callers
/// just leave the field empty when this returns `None`.
pub fn current_branch(repo_root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

// ─── Legacy post-commit hook ─────────────────────────────────────────────────
//
// Earlier sovereign versions installed a post-commit hook that shelled out to
// `svrn project refresh`. The daemon now owns freshness (FS watcher + git HEAD
// poll + startup catch-up), so the hook is redundant and was a common source of
// silent staleness when the binary path drifted. Nothing installs one anymore;
// `project init` removes any it finds.
//
// The marker lives here, not in either binary, because BOTH now touch hooks:
// `project init` ships in `sovereign-cli` (2026-08-07) while `project
// install-hooks` stayed in `sovereign-cli-dev`. Two copies of this string
// would mean one binary failing to recognise a hook the other wrote.

/// Marker line identifying a sovereign-owned `post-commit` hook block.
pub const SOVEREIGN_HOOK_MARKER: &str = "# SOVEREIGN_HOOK_V3";

/// Scan `.git/hooks/post-commit` for a `SOVEREIGN_HOOK_V*` marker and remove
/// the whole file (we were its sole owner). `Ok(true)` when a hook was removed,
/// `Ok(false)` when none was found.
///
/// If the file mixes sovereign content with anything else we leave it alone —
/// deleting a user's own hook to clean up ours is not a trade we make.
pub fn remove_legacy_hook(repo_root: &Path) -> std::io::Result<bool> {
    let hook_path = repo_root.join(".git/hooks/post-commit");
    if !hook_path.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(&hook_path)?;
    let is_sovereign_only = content.lines().any(|l| l.starts_with("# SOVEREIGN_HOOK_V"))
        && !content.contains("# non-sovereign");
    if !is_sovereign_only {
        return Ok(false);
    }
    std::fs::remove_file(&hook_path)?;
    Ok(true)
}
