//! Thin shell-out wrappers around the system `git` binary.
//!
//! We do NOT depend on `git2` / `libgit2`: it's heavy, has its own
//! link-time quirks on macOS, and we only need two read-only checks
//! for v1 (Obsidian vault has a repo, which branch). The write-side
//! helper (`git_commit_before_write`) is also shell-out — it runs
//! `git add -A` + `git commit -m "..."` inside the vault directory.
//!
//! Every invocation:
//!   - uses an explicit `current_dir` (the vault path),
//!   - returns `Result<_, Error>` so the caller can degrade
//!     gracefully if git isn't on PATH,
//!   - NEVER writes to stdin.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use sovereign_core::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStatus {
    pub current_branch: String,
    pub has_uncommitted_changes: bool,
}

/// Returns `Some(status)` if `vault_path` sits inside a git repo,
/// `None` otherwise. Missing `git` binary or non-git directory are
/// both reported as `None` — the UI treats "no git" as the common
/// case rather than an error.
pub fn check_git_repo(vault_path: &Path) -> Option<GitStatus> {
    // Are we inside a work tree?
    let inside = Command::new("git")
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .current_dir(vault_path)
        .output()
        .ok()?;
    if !inside.status.success() {
        return None;
    }
    if String::from_utf8_lossy(&inside.stdout).trim() != "true" {
        return None;
    }

    // Branch name. `HEAD` can be detached; in that case we show the
    // short commit hash instead, which is more useful than a literal
    // "HEAD".
    let branch_out = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(vault_path)
        .output()
        .ok()?;
    let mut branch = String::from_utf8_lossy(&branch_out.stdout).trim().to_string();
    if branch == "HEAD" {
        // Detached HEAD — substitute the short SHA.
        if let Ok(short) = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(vault_path)
            .output()
        {
            if short.status.success() {
                branch = format!(
                    "(detached {})",
                    String::from_utf8_lossy(&short.stdout).trim()
                );
            }
        }
    }

    let status_out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(vault_path)
        .output()
        .ok()?;
    let has_changes = !status_out.stdout.is_empty();

    Some(GitStatus {
        current_branch: branch,
        has_uncommitted_changes: has_changes,
    })
}

/// Stage + commit everything in the vault with a supplied message.
/// Returns the short commit hash on success. Errors propagate so the
/// caller (write-back flow) can abort before touching frontmatter.
pub fn git_commit_before_write(vault_path: &Path, message: &str) -> Result<String> {
    let add = Command::new("git")
        .args(["add", "-A"])
        .current_dir(vault_path)
        .output()
        .map_err(|e| Error::Execution(format!("git add: {e}")))?;
    if !add.status.success() {
        return Err(Error::Execution(format!(
            "git add failed: {}",
            String::from_utf8_lossy(&add.stderr)
        )));
    }

    // `--allow-empty` so a user whose vault has no pending changes
    // still gets a snapshot commit anchored to their pre-Sovereign
    // state.
    let commit = Command::new("git")
        .args(["commit", "--allow-empty", "-m", message])
        .current_dir(vault_path)
        .output()
        .map_err(|e| Error::Execution(format!("git commit: {e}")))?;
    if !commit.status.success() {
        return Err(Error::Execution(format!(
            "git commit failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        )));
    }

    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(vault_path)
        .output()
        .map_err(|e| Error::Execution(format!("git rev-parse: {e}")))?;
    if !sha.status.success() {
        return Err(Error::Execution(format!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&sha.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&sha.stdout).trim().to_string())
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn non_git_dir_returns_none() {
        let dir = tempdir().unwrap();
        assert!(check_git_repo(dir.path()).is_none());
    }

    #[test]
    fn init_then_detect() {
        let dir = tempdir().unwrap();
        // Skip the test silently when `git` isn't available on the
        // test machine — CI for the sovereign repo presumably has it,
        // but we don't want this test to be a portability landmine.
        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .output();
        let Ok(out) = init else {
            return;
        };
        if !out.status.success() {
            return;
        }
        // Config a user so commit is possible (GH CI runners don't
        // have one). Local-scope, temp dir only.
        let _ = Command::new("git")
            .args(["config", "user.email", "t@t.test"])
            .current_dir(dir.path())
            .output();
        let _ = Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(dir.path())
            .output();

        let status = check_git_repo(dir.path()).expect("fresh git repo should detect");
        assert!(!status.current_branch.is_empty());
    }
}
