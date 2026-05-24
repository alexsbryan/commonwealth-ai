//! `Workdir` — structural safety gate for the solver loop.
//!
//! Per ARCH_PRINCIPLES §7.1 ("structural invariants"), a solver
//! that mutates a directory must take a *typed* token that can only
//! be constructed via the safety check. The token cannot be built
//! by hand. This makes it impossible to compile a call to `solve()`
//! against an unvetted path — the kind of "we forgot to validate
//! that one entry point" bug that runtime checks miss.
//!
//! The check refuses three concrete classes:
//!
//! 1. `SystemPath` — paths under well-known directories (`/`, `/etc`,
//!    `/usr`, `/var`, `/bin`, `/lib`, `/sbin`, `/boot`, `/root`,
//!    `$HOME`, `$HOME/.config`). A miswired call should fail loud,
//!    not start rewriting `~/.bashrc`.
//! 2. `UncommittedChanges` — running a multi-round solver on a dirty
//!    working tree mixes the user's WIP with model output and
//!    destroys the diff that justifies the solver's result. The
//!    `force: true` escape hatch is for the operator who has
//!    consciously staged unrelated work.
//! 3. `NotAGitRepo` — without git, there's no way to roll back when
//!    a round goes sideways. The solver loop assumes `git restore`
//!    is available as a safety net; the gate enforces that
//!    assumption up front.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Typed token granting permission to mutate a workdir. Only
/// constructible via [`Workdir::check_safe`]; no `pub` constructor
/// or `From<PathBuf>`. Solvers take `&Workdir`, not `&Path`.
#[derive(Debug, Clone)]
pub struct Workdir(PathBuf);

/// Reasons the safety check refused a path. Each variant carries
/// the offending path so the caller can render an actionable
/// message ("won't operate on /etc — pick a project dir").
#[derive(Debug, Clone, thiserror::Error)]
pub enum DirtyWorkdir {
    #[error("refusing to operate on system path: {}", .path.display())]
    SystemPath { path: PathBuf },

    #[error(
        "refusing to operate on dirty working tree: {} \
         (commit, stash, or pass force=true)",
        .path.display()
    )]
    UncommittedChanges { path: PathBuf },

    #[error(
        "refusing to operate on non-git directory: {} \
         (the solver loop needs `git restore` for rollback)",
        .path.display()
    )]
    NotAGitRepo { path: PathBuf },
}

impl Workdir {
    /// Vet `path` and construct a `Workdir` if it passes. `force`
    /// bypasses *only* the uncommitted-changes check — the
    /// system-path and git-repo checks are never bypassable, since
    /// neither has a legitimate "I know what I'm doing" path.
    pub fn check_safe(path: PathBuf, force: bool) -> Result<Self, DirtyWorkdir> {
        let canonical = path.canonicalize().unwrap_or(path.clone());
        if is_system_path(&canonical) {
            tracing::warn!(
                path = %canonical.display(),
                "workdir: refused — system path"
            );
            return Err(DirtyWorkdir::SystemPath { path: canonical });
        }
        if !is_git_repo(&canonical) {
            tracing::warn!(
                path = %canonical.display(),
                "workdir: refused — not a git repo"
            );
            return Err(DirtyWorkdir::NotAGitRepo { path: canonical });
        }
        if !force && has_uncommitted_changes(&canonical) {
            tracing::warn!(
                path = %canonical.display(),
                "workdir: refused — uncommitted changes (pass force=true to override)"
            );
            return Err(DirtyWorkdir::UncommittedChanges { path: canonical });
        }
        tracing::debug!(
            path = %canonical.display(),
            force,
            "workdir: accepted"
        );
        Ok(Self(canonical))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

fn is_system_path(path: &Path) -> bool {
    let normalized = path.to_string_lossy().to_string();
    // Refuse exact bare-root + well-known top-level system dirs and
    // their descendants. Match either the bare path or a `/`-prefixed
    // subpath so `/etc/foo` is caught but `/var-of-mine` is not.
    const ROOTS: &[&str] = &[
        "/", "/etc", "/usr", "/var", "/bin", "/sbin", "/lib",
        "/lib64", "/boot", "/root", "/sys", "/proc", "/dev",
    ];
    if normalized == "/" {
        return true;
    }
    for root in ROOTS {
        if *root == "/" {
            continue;
        }
        if normalized == *root || normalized.starts_with(&format!("{root}/")) {
            return true;
        }
    }
    // $HOME and $HOME/.config (the dotfile root) are also refused —
    // the solver should never overwrite shell configs even if the
    // user typo'd. A literal project under ~/.config is uncommon
    // enough that we accept this false-positive risk; the gate is
    // a safety net, not a precision tool.
    if let Some(home) = home_dir_string() {
        if normalized == home || normalized == format!("{home}/.config") {
            return true;
        }
    }
    false
}

fn home_dir_string() -> Option<String> {
    std::env::var("HOME").ok().or_else(|| std::env::var("USERPROFILE").ok())
}

fn is_git_repo(path: &Path) -> bool {
    // git rev-parse --is-inside-work-tree exits 0 iff `path` is
    // inside a git working tree. Cheaper than walking parents
    // ourselves and respects the user's `core.worktree` setting.
    Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .output()
        .ok()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn has_uncommitted_changes(path: &Path) -> bool {
    // `git status --porcelain` prints one line per dirty entry and
    // nothing when the tree is clean. Includes untracked files,
    // which is what we want — a solver that overwrites the user's
    // un-added scratch file is still destructive.
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("status")
        .arg("--porcelain")
        .output();
    match out {
        Ok(o) if o.status.success() => !o.stdout.is_empty(),
        // If git itself errored, the safest assumption is "dirty"
        // so we don't power on under uncertainty. The is_git_repo
        // check upstream means we don't normally hit this branch.
        Ok(_) | Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_init(path: &Path) {
        // Minimal git init for tests — set user identity locally so
        // `git commit` works on CI machines without a global config.
        let _ = Command::new("git").arg("-C").arg(path).arg("init").arg("--initial-branch=main").output();
        let _ = Command::new("git").arg("-C").arg(path).args(["config", "user.email", "t@t.t"]).output();
        let _ = Command::new("git").arg("-C").arg(path).args(["config", "user.name", "t"]).output();
        let _ = Command::new("git").arg("-C").arg(path).args(["commit", "--allow-empty", "-m", "init"]).output();
    }

    #[test]
    fn refuses_root_directory() {
        let result = Workdir::check_safe(PathBuf::from("/"), false);
        assert!(matches!(result, Err(DirtyWorkdir::SystemPath { .. })));
    }

    #[test]
    fn refuses_etc_subpath() {
        let result = Workdir::check_safe(PathBuf::from("/etc"), false);
        assert!(matches!(result, Err(DirtyWorkdir::SystemPath { .. })));
    }

    #[test]
    fn refuses_etc_nested_subpath() {
        let result = Workdir::check_safe(PathBuf::from("/etc/cron.d"), false);
        assert!(matches!(result, Err(DirtyWorkdir::SystemPath { .. })));
    }

    #[test]
    fn refuses_force_does_not_override_system_path() {
        // §7.1: force is for "I know my tree is dirty," NOT for
        // "I know I'm pointed at /etc." The system-path refusal is
        // not bypassable.
        let result = Workdir::check_safe(PathBuf::from("/etc"), true);
        assert!(matches!(result, Err(DirtyWorkdir::SystemPath { .. })));
    }

    #[test]
    fn refuses_home_directory() {
        // We need a synthetic HOME to make this test deterministic
        // — the runner's actual $HOME might be anything.
        let tmp = tempfile::tempdir().unwrap();
        let prior = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());
        let result = Workdir::check_safe(tmp.path().to_path_buf(), false);
        if let Some(p) = prior {
            std::env::set_var("HOME", p);
        }
        assert!(matches!(result, Err(DirtyWorkdir::SystemPath { .. })));
    }

    #[test]
    fn refuses_non_git_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let result = Workdir::check_safe(tmp.path().to_path_buf(), false);
        assert!(matches!(result, Err(DirtyWorkdir::NotAGitRepo { .. })));
    }

    #[test]
    fn refuses_dirty_working_tree() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        // Make the tree dirty with an untracked file. `git status
        // --porcelain` lists untracked entries, which is what we
        // want — the solver would clobber them on candidate restore.
        std::fs::write(tmp.path().join("scratch.txt"), "wip").unwrap();
        let result = Workdir::check_safe(tmp.path().to_path_buf(), false);
        assert!(matches!(result, Err(DirtyWorkdir::UncommittedChanges { .. })));
    }

    #[test]
    fn accepts_dirty_tree_under_force() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        std::fs::write(tmp.path().join("scratch.txt"), "wip").unwrap();
        let result = Workdir::check_safe(tmp.path().to_path_buf(), true);
        assert!(result.is_ok(), "force=true should accept a dirty tree");
    }

    #[test]
    fn accepts_clean_git_repo() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        let result = Workdir::check_safe(tmp.path().to_path_buf(), false);
        let w = result.expect("clean git repo should be accepted");
        // canonicalize() may resolve symlinks (e.g. /tmp → /private/tmp
        // on macOS), so we compare canonical-to-canonical.
        let expected = tmp.path().canonicalize().unwrap();
        assert_eq!(w.path(), expected);
    }
}
