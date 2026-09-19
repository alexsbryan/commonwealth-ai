// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one `git` read the composition needs: the current branch of a repo.
//!
//! Moved down from `sovereign-cli-shared::repo` at domains
//! `dm-daemon-cli-composition` (2026-09-17). The caller is the daemon's
//! work-atlas wiring, which needs the branch to scope observations, and
//! `sovereign-daemon` sits at `mesh-api` — one tier BELOW `sovereign-cli-shared`
//! (`hosts`), so it may not name the CLI helper crate. This crate is already a
//! dependency of both, so the move adds no edge.
//!
//! Shells out to `git rev-parse` rather than depending on `gix`/`git2`:
//! sovereign treats the CLI surface as truth, and every binary that asks this
//! question already requires `git` on PATH.

use std::path::Path;

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
