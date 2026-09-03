// SPDX-License-Identifier: AGPL-3.0-or-later
//! The environment operations the driver needs, over the `git` binary. A
//! `Combine` child forks a worktree from the parent's commit; delivery
//! merges the siblings' branches back. The memo patch is a diff between two
//! tree hashes. Every failure carries git's own stderr.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, thiserror::Error)]
#[error("git {args} in {dir}: {stderr}")]
pub struct GitError {
    pub args: String,
    pub dir: String,
    pub stderr: String,
}

/// Trimmed stdout — for hashes and names.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, GitError> {
    git_raw(dir, args).map(|s| s.trim().to_string())
}

/// Untrimmed stdout — a patch's final newline is load-bearing.
pub fn git_raw(dir: &Path, args: &[&str]) -> Result<String, GitError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| GitError {
            args: args.join(" "),
            dir: dir.display().to_string(),
            stderr: e.to_string(),
        })?;
    if !out.status.success() {
        return Err(GitError {
            args: args.join(" "),
            dir: dir.display().to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn head(dir: &Path) -> Result<String, GitError> {
    git(dir, &["rev-parse", "HEAD"])
}

pub fn current_branch(dir: &Path) -> Result<String, GitError> {
    git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// Stage everything and commit (empty commits allowed, so a branch with no
/// edits still has a tip to merge).
pub fn commit_all(dir: &Path, msg: &str) -> Result<(), GitError> {
    git(dir, &["add", "-A"])?;
    git(dir, &["commit", "-q", "--allow-empty", "-m", msg])?;
    Ok(())
}

/// Hash of the working tree's content (staged into this worktree's index
/// first). Same content → same hash, whichever worktree it is in: that is
/// what makes it a memo key.
pub fn tree_hash(dir: &Path) -> Result<String, GitError> {
    git(dir, &["add", "-A"])?;
    git(dir, &["write-tree"])
}

/// The patch from tree `a` to tree `b`.
pub fn diff_trees(dir: &Path, a: &str, b: &str) -> Result<String, GitError> {
    if a == b {
        return Ok(String::new());
    }
    git_raw(dir, &["diff", "--binary", a, b])
}

/// Apply a patch to the working tree. An empty patch is a no-op.
pub fn apply(dir: &Path, patch: &str) -> Result<(), GitError> {
    if patch.trim().is_empty() {
        return Ok(());
    }
    let err = |stderr: String| GitError {
        args: "apply".into(),
        dir: dir.display().to_string(),
        stderr,
    };
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["apply", "--whitespace=nowarn"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| err(e.to_string()))?;
    let Some(mut stdin) = child.stdin.take() else {
        return Err(err("no stdin pipe".into()));
    };
    stdin
        .write_all(patch.as_bytes())
        .map_err(|e| err(e.to_string()))?;
    drop(stdin);
    let out = child.wait_with_output().map_err(|e| err(e.to_string()))?;
    if !out.status.success() {
        return Err(err(String::from_utf8_lossy(&out.stderr).into_owned()));
    }
    Ok(())
}

/// Fork a worktree on a new branch at `commit`. Works from any worktree of
/// the repo.
pub fn add_worktree(from: &Path, path: &Path, branch: &str, commit: &str) -> Result<(), GitError> {
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let path_s = path.display().to_string();
    git(
        from,
        &["worktree", "add", "-q", "-b", branch, &path_s, commit],
    )?;
    Ok(())
}

/// Merge `branches` into the checked-out branch of `dir`, one at a time so
/// a conflict names its branch. `Ok(Err(msg))` is a conflict (aborted, tree
/// restored); `Err` is git itself failing.
pub fn merge(dir: &Path, branches: &[String]) -> Result<Result<(), String>, GitError> {
    for b in branches {
        if let Err(e) = git(dir, &["merge", "-q", "--no-edit", b]) {
            let _ = git(dir, &["merge", "--abort"]);
            return Ok(Err(format!("{b}: {}", e.stderr.trim())));
        }
    }
    Ok(Ok(()))
}
