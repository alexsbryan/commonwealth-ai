//! §7.1 Workdir-gate end-to-end tests. The gate is the structural
//! safety boundary the whole solver takes a typed token through —
//! these pin the refusal classes and the bypass scope of `force`.

use std::path::Path;
use std::process::Command;

use commonwealth_tdd::{DirtyWorkdir, Workdir};

fn fresh_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    init_git(tmp.path());
    tmp
}

fn init_git(path: &Path) {
    let _ = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("init")
        .arg("--initial-branch=main")
        .output();
    let _ = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["config", "user.email", "t@t.t"])
        .output();
    let _ = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["config", "user.name", "t"])
        .output();
    let _ = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["commit", "--allow-empty", "-m", "init"])
        .output();
}

#[test]
fn gate_refuses_dirty_repo_at_request_boundary() {
    let tmp = fresh_repo();
    std::fs::write(tmp.path().join("uncommitted.txt"), "wip").unwrap();
    let result = Workdir::check_safe(tmp.path().to_path_buf(), false);
    assert!(matches!(
        result,
        Err(DirtyWorkdir::UncommittedChanges { .. })
    ));
}

#[test]
fn gate_force_unlocks_dirty_repo_only() {
    // Force overrides UncommittedChanges but NOT system-path
    // refusal — the gate enforces the bypass scope structurally.
    let tmp = fresh_repo();
    std::fs::write(tmp.path().join("uncommitted.txt"), "wip").unwrap();
    assert!(Workdir::check_safe(tmp.path().to_path_buf(), true).is_ok());

    // System path refusal is unbypassable.
    let system_result = Workdir::check_safe(std::path::PathBuf::from("/etc"), true);
    assert!(matches!(
        system_result,
        Err(DirtyWorkdir::SystemPath { .. })
    ));
}

#[test]
fn gate_refuses_non_git_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let result = Workdir::check_safe(tmp.path().to_path_buf(), true);
    // NotAGitRepo is also unbypassable — the solver needs git as a
    // rollback target.
    assert!(matches!(result, Err(DirtyWorkdir::NotAGitRepo { .. })));
}
