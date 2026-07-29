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
