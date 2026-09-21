// SPDX-License-Identifier: AGPL-3.0-or-later
//! One decider for "which repo, and from where". Every git shell goes through here.

use std::path::{Path, PathBuf};

/// `git rev-parse --show-toplevel` run IN `dir`. The caller says where.
pub fn repo_root_from(dir: &Path) -> Result<PathBuf, String> {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("running git rev-parse in {}: {e}", dir.display()))?;
    if !out.status.success() {
        return Err(format!("{} is not inside a git repository", dir.display()));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// The repo containing the process's cwd — the ambient case, named once.
pub fn repo_root_here() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("reading cwd: {e}"))?;
    repo_root_from(&cwd)
}

/// `git <args>` run IN `root`; stdout on success, `None` on any failure.
pub fn git_stdout_in(root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        tracing::debug!(target: "cli_dev.git", root = %root.display(), ?args, "git exited non-zero");
        return None;
    }
    String::from_utf8(out.stdout).ok()
}
