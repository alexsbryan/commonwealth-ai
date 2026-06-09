// SPDX-License-Identifier: AGPL-3.0-or-later
//! Resolve a cross-node `repo_id` from a local working directory.
//!
//! Walks up to the enclosing git repo, reads `origin` remote, strips
//! protocol/credentials/`.git`/trailing slash, lowercases the host,
//! and hashes the result with SHA-256. The hex digest is stable
//! across workstations that point at the same logical repo.
//!
//! Hard-fails on missing remote — this is the §10 MUST decision: a
//! workstation without `origin` cannot participate in the atlas.
//! Surface the error clearly, don't silently fall back to a local-
//! only scheme.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RepoIdError {
    #[error("not inside a git repository: {0}")]
    NotARepo(PathBuf),

    /// §10 MUST gate: the spec rejects repo-less workstations.
    #[error("git repo has no `origin` remote (work atlas requires one)")]
    NoOriginRemote,

    #[error("git command failed: {0}")]
    GitFailed(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Resolve `(repo_root, repo_id)` for `cwd`. `repo_id` is the SHA-256
/// hex digest of the canonicalized origin URL.
pub fn resolve_repo_id(cwd: &Path) -> Result<(PathBuf, String), RepoIdError> {
    let root = discover_repo_root(cwd)?;
    let origin = read_origin_url(&root)?;
    let canonical = canonicalize_origin(&origin);
    if canonical.is_empty() {
        return Err(RepoIdError::NoOriginRemote);
    }
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hex::encode(hasher.finalize());
    Ok((root, digest))
}

fn discover_repo_root(cwd: &Path) -> Result<PathBuf, RepoIdError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| RepoIdError::GitFailed(e.to_string()))?;
    if !out.status.success() {
        return Err(RepoIdError::NotARepo(cwd.to_path_buf()));
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return Err(RepoIdError::NotARepo(cwd.to_path_buf()));
    }
    Ok(PathBuf::from(s))
}

fn read_origin_url(repo_root: &Path) -> Result<String, RepoIdError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .map_err(|e| RepoIdError::GitFailed(e.to_string()))?;
    if !out.status.success() {
        // exit-1 from `git config --get` means key not set — that's
        // a missing origin remote, the user's MUST gate.
        return Err(RepoIdError::NoOriginRemote);
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return Err(RepoIdError::NoOriginRemote);
    }
    Ok(s)
}

/// Lowercase host, strip protocol and credentials, drop trailing
/// `.git` and any trailing `/`. SSH and HTTPS forms of the same repo
/// canonicalize to the same string.
///
/// Examples:
/// - `git@github.com:org/repo.git`     → `github.com/org/repo`
/// - `https://GitHub.com/org/repo.git` → `github.com/org/repo`
/// - `https://user:tok@host.dev/x.git` → `host.dev/x`
fn canonicalize_origin(raw: &str) -> String {
    let s = raw.trim();

    // Strip `scp`-style SSH form: `git@github.com:org/repo.git` → split on first `:`.
    let s = if let Some(rest) = s.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            format!("{host}/{path}")
        } else {
            rest.to_string()
        }
    } else if let Some(rest) = s.strip_prefix("ssh://") {
        rest.to_string()
    } else if let Some(rest) = s.strip_prefix("https://") {
        rest.to_string()
    } else if let Some(rest) = s.strip_prefix("http://") {
        rest.to_string()
    } else if let Some(rest) = s.strip_prefix("git://") {
        rest.to_string()
    } else {
        s.to_string()
    };

    // Drop userinfo: anything before `@` in the host segment.
    let s = match s.split_once('@') {
        Some((_, rest)) => rest.to_string(),
        None => s,
    };

    // Lowercase the host portion (everything before the first `/`).
    let s = match s.split_once('/') {
        Some((host, path)) => format!("{}/{}", host.to_lowercase(), path),
        None => s.to_lowercase(),
    };

    // Drop trailing `.git` and trailing slash.
    let s = s.strip_suffix(".git").unwrap_or(&s).to_string();
    let s = s.trim_end_matches('/').to_string();

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_https_and_ssh_match() {
        let a = canonicalize_origin("git@github.com:org/repo.git");
        let b = canonicalize_origin("https://github.com/org/repo.git");
        let c = canonicalize_origin("https://GitHub.com/org/repo");
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert_eq!(a, "github.com/org/repo");
    }

    #[test]
    fn canonicalize_strips_credentials() {
        assert_eq!(
            canonicalize_origin("https://user:tok@host.dev/x.git"),
            "host.dev/x"
        );
    }

    #[test]
    fn canonicalize_handles_ssh_protocol() {
        assert_eq!(
            canonicalize_origin("ssh://git@host.dev/org/repo.git"),
            "host.dev/org/repo"
        );
    }

    #[test]
    fn resolve_repo_id_errors_outside_a_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let res = resolve_repo_id(tmp.path());
        assert!(matches!(res, Err(RepoIdError::NotARepo(_))));
    }
}
