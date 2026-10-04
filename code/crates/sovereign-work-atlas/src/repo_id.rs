// SPDX-License-Identifier: AGPL-3.0-or-later
//! Resolve a cross-node `repo_id` from a local working directory.
//!
//! Walks up to the enclosing git repo, reads `origin` remote, strips
//! protocol/credentials/`.git`/trailing slash, lowercases the host,
//! and hashes the result with SHA-256. The hex digest is stable
//! across workstations that point at the same logical repo.
//!
//! [`resolve_repo_id`] hard-fails on a missing remote — this is the §10 MUST
//! decision: without `origin` there is nothing making two workstations' copies
//! *the same repo*, so a cross-node claim cannot be honest.
//!
//! [`resolve_repo_id_allowing_local`] is for callers that do not need the
//! cross-node half. It answers with a machine-local id and **says so** in the
//! returned [`RepoIdSource`], so the caller can tell the user their claims stay
//! on this workstation. That is the difference between an alternative and a
//! silent fallback (ARCH §18.3), and it is why the two kinds of id cannot even
//! be confused for one another: an origin id is 64 bare hex characters, a local
//! one wears a `local-` prefix.
//!
//! It exists because the empty string was doing this job badly. Three call
//! sites already degraded to `repo_id = ""` on a repo with no origin — one of
//! them with a comment claiming `declare_scope` would reject it, which it does
//! not — so **every** origin-less repo on a machine shared one id and could see
//! each other's claims. A named local id is distinct per repo.

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

/// Where a `repo_id` came from — and therefore how far it travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoIdSource {
    /// Derived from the `origin` remote. Every workstation pointing at this
    /// repo computes the same digest, so a claim under it is visible mesh-wide.
    Origin,
    /// Derived from this machine's path to the repo, because there is no
    /// `origin`. Stable across restarts and distinct per repo, but **private
    /// to this workstation**: a peer has no way to compute the same id, and
    /// that is correct — nothing makes the two directories the same repo.
    LocalOnly,
}

impl RepoIdSource {
    /// One sentence a caller can print verbatim, or `None` when there is
    /// nothing to tell the user.
    pub fn caveat(self) -> Option<&'static str> {
        match self {
            Self::Origin => None,
            Self::LocalOnly => Some(
                "this repo has no `origin` remote, so its work-atlas id is \
                 machine-local — claims stay on this workstation and peers \
                 will not see them",
            ),
        }
    }
}

/// Resolve `(repo_root, repo_id)` for `cwd`. `repo_id` is the SHA-256
/// hex digest of the canonicalized origin URL.
///
/// Errors on a repo with no `origin`. Callers that can work with a
/// workstation-local identity want [`resolve_repo_id_allowing_local`].
pub fn resolve_repo_id(cwd: &Path) -> Result<(PathBuf, String), RepoIdError> {
    let root = discover_repo_root(cwd)?;
    let origin = read_origin_url(&root)?;
    let canonical = canonicalize_origin(&origin);
    if canonical.is_empty() {
        return Err(RepoIdError::NoOriginRemote);
    }
    Ok((root, sha256_hex(&canonical)))
}

/// [`resolve_repo_id`], but a repo with no `origin` gets a machine-local id
/// instead of an error — and the [`RepoIdSource`] says which happened.
///
/// Still errors when `cwd` is not inside a git repo at all: there is no
/// directory to be stable about.
pub fn resolve_repo_id_allowing_local(
    cwd: &Path,
) -> Result<(PathBuf, String, RepoIdSource), RepoIdError> {
    let root = discover_repo_root(cwd)?;
    let canonical = read_origin_url(&root)
        .map(|raw| canonicalize_origin(&raw))
        .unwrap_or_default();
    if !canonical.is_empty() {
        return Ok((root, sha256_hex(&canonical), RepoIdSource::Origin));
    }
    // The path, canonicalized so `.`/symlink spellings of one directory do
    // not become two repos on the same machine.
    let path = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    let id = format!("local-{}", sha256_hex(&path.to_string_lossy()));
    Ok((root, id, RepoIdSource::LocalOnly))
}

fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hex::encode(hasher.finalize())
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

    /// Not being in a repo is still an error even for the lenient variant —
    /// "optional origin" is not "optional repo".
    #[test]
    fn the_lenient_variant_still_needs_a_repo() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            resolve_repo_id_allowing_local(tmp.path()),
            Err(RepoIdError::NotARepo(_))
        ));
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// **The unblock.** A repo with no `origin` gets a usable id, and the
    /// caller is told it is machine-local rather than left to assume it
    /// travels.
    #[test]
    fn a_repo_without_an_origin_gets_a_named_local_id() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q"]);
        let (_, id, source) = resolve_repo_id_allowing_local(tmp.path()).unwrap();
        assert_eq!(source, RepoIdSource::LocalOnly);
        assert!(id.starts_with("local-"), "{id}");
        assert!(source.caveat().is_some(), "the user must be told");
        // And the strict variant still refuses, because a cross-node claim
        // under this id would be a lie.
        assert!(matches!(
            resolve_repo_id(tmp.path()),
            Err(RepoIdError::NoOriginRemote)
        ));
    }

    /// Two origin-less repos must not share an id. They used to: three call
    /// sites degraded to `""`, so every one of them collided.
    #[test]
    fn two_origin_less_repos_get_different_ids() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        git(a.path(), &["init", "-q"]);
        git(b.path(), &["init", "-q"]);
        let id_a = resolve_repo_id_allowing_local(a.path()).unwrap().1;
        let id_b = resolve_repo_id_allowing_local(b.path()).unwrap().1;
        assert_ne!(id_a, id_b);
        // Stable across calls — a claim must survive a restart.
        assert_eq!(id_a, resolve_repo_id_allowing_local(a.path()).unwrap().1);
    }

    /// An origin id and a local id can never be mistaken for one another.
    #[test]
    fn an_origin_id_and_a_local_id_are_not_confusable() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q"]);
        git(
            tmp.path(),
            &["remote", "add", "origin", "https://host.dev/org/repo.git"],
        );
        let (_, id, source) = resolve_repo_id_allowing_local(tmp.path()).unwrap();
        assert_eq!(source, RepoIdSource::Origin);
        assert!(!id.starts_with("local-"));
        assert_eq!(id.len(), 64, "bare sha-256 hex");
        assert_eq!(id, resolve_repo_id(tmp.path()).unwrap().1, "one derivation");
        assert!(source.caveat().is_none());
    }
}
