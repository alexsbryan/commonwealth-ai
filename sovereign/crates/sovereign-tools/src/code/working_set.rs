//! Working-set detection — what files are "in scope" for a session?
//!
//! Three deterministic strategies, no LLM, no heuristics that drift:
//!
//! - [`Strategy::BranchDiff`] — files differing between two branches.
//!   Default for an interactive session: compare current branch to the
//!   repo's default branch (typically `main`). Returns the set of
//!   files an engineer is actively changing.
//! - [`Strategy::RecentCommits`] — files touched by any commit within
//!   the last N hours. Useful for sessions opened mid-flow on the
//!   default branch where there's no diff yet.
//! - [`Strategy::Explicit`] — caller passes a vector. Used by the
//!   daemon HTTP endpoint when the caller already knows the scope
//!   (e.g. files mentioned in the user's prompt).
//!
//! All three return **repo-root-relative paths**, sorted, deduplicated.
//! That format matches what `git_archaeology::batch_harvest_all_commits`
//! emits, so downstream joins are trivial.
//!
//! Path semantics + git subprocess style mirror
//! [`corpus_engine::git_archaeology`] and
//! [`crate::code::recent_changes`] — explicit `current_dir`,
//! `Result`-typed errors, no libgit2.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

// ── Errors ─────────────────────────────────────────────────────

#[derive(Debug)]
pub enum WorkingSetError {
    NotGitRepo(PathBuf),
    GitNotInstalled(std::io::Error),
    GitCommandFailed { cmd: String, stderr: String },
    NoDefaultBranch,
}

impl std::fmt::Display for WorkingSetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotGitRepo(p) => write!(f, "{} is not a git repository", p.display()),
            Self::GitNotInstalled(e) => write!(f, "git is not installed or not on PATH: {e}"),
            Self::GitCommandFailed { cmd, stderr } => {
                write!(f, "`{cmd}` failed: {}", stderr.trim())
            }
            Self::NoDefaultBranch => write!(
                f,
                "couldn't resolve a default branch (no origin/HEAD, no `main`, no `master`)"
            ),
        }
    }
}

impl std::error::Error for WorkingSetError {}

// ── Strategy ───────────────────────────────────────────────────

/// How to compute the working set. The brief assembler accepts any of
/// these and renders the output identically — this enum just lets the
/// caller pick the lens.
#[derive(Debug, Clone)]
pub enum Strategy {
    /// `git diff --name-only <from>..<to>`. When `to` is `None`,
    /// uses `HEAD`. When `from` is `None`, the repo's default branch
    /// is auto-resolved.
    BranchDiff {
        from: Option<String>,
        to: Option<String>,
    },
    /// `git log --since=<N hours ago> --name-only`. Returns every
    /// file touched by any commit in the window, deduplicated.
    RecentCommits { hours: u64 },
    /// Caller-provided file list. No git access. Files are normalised
    /// (relative to repo root if absolute, sorted, deduped).
    Explicit(Vec<PathBuf>),
}

impl Strategy {
    pub fn default_branch_diff() -> Self {
        Self::BranchDiff {
            from: None,
            to: None,
        }
    }

    pub fn recent_commits_24h() -> Self {
        Self::RecentCommits { hours: 24 }
    }
}

// ── Public entry point ─────────────────────────────────────────

/// Compute the working set per `strategy` for the git repository
/// rooted at `repo_root`. Returns repo-root-relative paths,
/// alphabetically sorted, deduplicated.
pub fn detect_working_set(
    repo_root: &Path,
    strategy: Strategy,
) -> Result<Vec<PathBuf>, WorkingSetError> {
    match strategy {
        Strategy::BranchDiff { from, to } => detect_branch_diff(repo_root, from, to),
        Strategy::RecentCommits { hours } => detect_recent_commits(repo_root, hours),
        Strategy::Explicit(files) => Ok(normalise(files, repo_root)),
    }
}

// ── BranchDiff ─────────────────────────────────────────────────

fn detect_branch_diff(
    repo_root: &Path,
    from: Option<String>,
    to: Option<String>,
) -> Result<Vec<PathBuf>, WorkingSetError> {
    let from = match from {
        Some(b) => b,
        None => resolve_default_branch(repo_root)?,
    };
    let to = to.unwrap_or_else(|| "HEAD".into());

    let range = format!("{from}..{to}");
    // Two-dot `..` semantics: files changed on `to` since branching
    // from `from`. That's the engineer's intent — "what am I changing
    // on this branch?"
    let out = Command::new("git")
        .args(["diff", "--name-only", &range])
        .current_dir(repo_root)
        .output()
        .map_err(WorkingSetError::GitNotInstalled)?;
    if !out.status.success() {
        return Err(WorkingSetError::GitCommandFailed {
            cmd: format!("git diff --name-only {range}"),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        });
    }
    Ok(parse_path_lines(&out.stdout))
}

/// Resolve the repo's default branch. Order:
///   1. `origin/HEAD` — if remote tracking is configured.
///   2. `main` — if the ref exists locally.
///   3. `master` — likewise.
///   4. Error.
fn resolve_default_branch(repo_root: &Path) -> Result<String, WorkingSetError> {
    if let Some(b) = symbolic_ref(repo_root, "refs/remotes/origin/HEAD") {
        // `refs/remotes/origin/main` → `origin/main`
        if let Some(stripped) = b.strip_prefix("refs/remotes/") {
            return Ok(stripped.to_string());
        }
    }
    for candidate in ["main", "master"] {
        if ref_exists(repo_root, &format!("refs/heads/{candidate}")) {
            return Ok(candidate.to_string());
        }
    }
    Err(WorkingSetError::NoDefaultBranch)
}

fn symbolic_ref(repo_root: &Path, name: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["symbolic-ref", "--quiet", name])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn ref_exists(repo_root: &Path, refname: &str) -> bool {
    Command::new("git")
        .args(["show-ref", "--verify", "--quiet", refname])
        .current_dir(repo_root)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ── RecentCommits ──────────────────────────────────────────────

fn detect_recent_commits(repo_root: &Path, hours: u64) -> Result<Vec<PathBuf>, WorkingSetError> {
    let since = format!("{hours} hours ago");
    let out = Command::new("git")
        .args([
            "log",
            "--since",
            &since,
            "--name-only",
            "--pretty=format:",
        ])
        .current_dir(repo_root)
        .output()
        .map_err(WorkingSetError::GitNotInstalled)?;
    if !out.status.success() {
        return Err(WorkingSetError::GitCommandFailed {
            cmd: format!("git log --since=\"{since}\" --name-only"),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        });
    }
    Ok(parse_path_lines(&out.stdout))
}

// ── Helpers ────────────────────────────────────────────────────

fn parse_path_lines(stdout: &[u8]) -> Vec<PathBuf> {
    let mut set: BTreeSet<PathBuf> = BTreeSet::new();
    for line in String::from_utf8_lossy(stdout).lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        set.insert(PathBuf::from(l));
    }
    set.into_iter().collect()
}

/// Normalise an explicit caller-provided file list. Absolute paths
/// inside `repo_root` are stripped to their relative form; paths
/// outside the repo are kept verbatim (the brief assembler will skip
/// them in joins). Sorted + deduped.
fn normalise(files: Vec<PathBuf>, repo_root: &Path) -> Vec<PathBuf> {
    let mut set: BTreeSet<PathBuf> = BTreeSet::new();
    for p in files {
        let rel = if p.is_absolute() {
            p.strip_prefix(repo_root).unwrap_or(&p).to_path_buf()
        } else {
            p
        };
        set.insert(rel);
    }
    set.into_iter().collect()
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as Cmd;

    fn init_repo(dir: &Path) {
        assert!(Cmd::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        for (k, v) in [("user.email", "test@example.com"), ("user.name", "Test")] {
            assert!(Cmd::new("git")
                .args(["config", k, v])
                .current_dir(dir)
                .status()
                .unwrap()
                .success());
        }
    }

    fn write_and_commit(dir: &Path, rel: &str, body: &str, msg: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
        assert!(Cmd::new("git")
            .args(["add", rel])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        assert!(Cmd::new("git")
            .args(["commit", "-m", msg])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
    }

    fn checkout_branch(dir: &Path, branch: &str) {
        assert!(Cmd::new("git")
            .args(["checkout", "-b", branch])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
    }

    #[test]
    fn explicit_strategy_normalises_and_dedupes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        let files = vec![
            PathBuf::from("a.rs"),
            PathBuf::from("b.rs"),
            PathBuf::from("a.rs"), // dup
            repo.join("c.rs"),     // absolute → strips to relative
        ];
        let ws = detect_working_set(repo, Strategy::Explicit(files)).unwrap();
        assert_eq!(
            ws,
            vec![
                PathBuf::from("a.rs"),
                PathBuf::from("b.rs"),
                PathBuf::from("c.rs"),
            ]
        );
    }

    #[test]
    fn branch_diff_finds_only_branch_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        // Baseline on main.
        write_and_commit(repo, "common.rs", "fn a() {}\n", "init");
        // Branch.
        checkout_branch(repo, "feature/x");
        write_and_commit(repo, "feature.rs", "fn b() {}\n", "feat");
        write_and_commit(repo, "common.rs", "fn a() { /* ! */ }\n", "tweak");

        let ws = detect_working_set(
            repo,
            Strategy::BranchDiff {
                from: Some("main".into()),
                to: None,
            },
        )
        .unwrap();
        // Both files were touched on the branch; common.rs at baseline,
        // tweaked on branch; feature.rs is branch-only.
        assert_eq!(
            ws,
            vec![PathBuf::from("common.rs"), PathBuf::from("feature.rs")]
        );
    }

    #[test]
    fn branch_diff_resolves_default_branch_automatically() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        write_and_commit(repo, "lib.rs", "fn a() {}\n", "init");
        checkout_branch(repo, "feature/auto");
        write_and_commit(repo, "feat.rs", "fn b() {}\n", "feat");

        // No `from` — should auto-resolve to `main`.
        let ws =
            detect_working_set(repo, Strategy::BranchDiff { from: None, to: None }).unwrap();
        assert_eq!(ws, vec![PathBuf::from("feat.rs")]);
    }

    #[test]
    fn recent_commits_includes_window_only() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        // Backdate two commits a year ago so a 24h window excludes them.
        let year_ago = "2024-05-01T12:00:00 +0000";
        let p = repo.join("old.rs");
        std::fs::write(&p, "fn old() {}\n").unwrap();
        Cmd::new("git").args(["add", "old.rs"]).current_dir(repo).status().unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "old"])
            .env("GIT_AUTHOR_DATE", year_ago)
            .env("GIT_COMMITTER_DATE", year_ago)
            .current_dir(repo)
            .status()
            .unwrap();
        // Then a fresh commit (now).
        write_and_commit(repo, "fresh.rs", "fn fresh() {}\n", "fresh");

        // 24h window should catch only fresh.rs.
        let ws = detect_working_set(repo, Strategy::RecentCommits { hours: 24 }).unwrap();
        assert_eq!(ws, vec![PathBuf::from("fresh.rs")]);
        // 100,000h ≈ 11 years should catch both.
        let wide =
            detect_working_set(repo, Strategy::RecentCommits { hours: 100_000 }).unwrap();
        assert_eq!(wide, vec![PathBuf::from("fresh.rs"), PathBuf::from("old.rs")]);
    }

    #[test]
    fn no_default_branch_errors_when_neither_main_nor_master_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        // Init with a non-conventional branch name so neither main nor master exists.
        assert!(Cmd::new("git")
            .args(["init", "--initial-branch=trunk"])
            .current_dir(repo)
            .status()
            .unwrap()
            .success());
        for (k, v) in [("user.email", "t@e.com"), ("user.name", "T")] {
            Cmd::new("git")
                .args(["config", k, v])
                .current_dir(repo)
                .status()
                .unwrap();
        }
        // Need at least one commit so refs/heads/trunk exists.
        std::fs::write(repo.join("x.rs"), "fn x() {}\n").unwrap();
        Cmd::new("git").args(["add", "."]).current_dir(repo).status().unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(repo)
            .status()
            .unwrap();

        let res = detect_working_set(repo, Strategy::default_branch_diff());
        assert!(matches!(res, Err(WorkingSetError::NoDefaultBranch)));
    }
}
