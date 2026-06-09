// SPDX-License-Identifier: AGPL-3.0-or-later
//! Copy a workdir to a sibling scratch dir, skipping heavy build
//! output directories. Used to make per-candidate snapshots so test
//! runs don't pollute sibling candidates.

use std::path::Path;

/// Snapshot `src` to `dst`, preserving any `.git` already in `dst`.
///
/// `.git` is the rollback target the §7.1 Workdir gate requires.
/// When the solver loop promotes a winning candidate's scratch dir
/// back to the canonical workdir, naively wiping `dst` would erase
/// the canonical `.git` (candidates don't carry git). Subsequent
/// re-vets (e.g., the green stage of `bdd_cycle`) would then refuse
/// the workdir as `NotAGitRepo`.
///
/// Solution: when `dst` already exists, wipe only its NON-`.git`
/// entries before copying. The copy itself still skips `.git`
/// (`copy_dir_filtered`'s SKIP set), so the `.git` already on `dst`
/// stays untouched while source files are refreshed from `src`.
pub fn snapshot_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    if dst.exists() {
        for entry in std::fs::read_dir(dst)? {
            let entry = entry?;
            if entry.file_name() == ".git" {
                continue;
            }
            let p = entry.path();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                std::fs::remove_dir_all(&p)?;
            } else {
                std::fs::remove_file(&p)?;
            }
        }
    } else {
        std::fs::create_dir_all(dst)?;
    }
    copy_dir_filtered(src, dst)
}

fn copy_dir_filtered(src: &Path, dst: &Path) -> std::io::Result<()> {
    const SKIP: &[&str] = &[
        "target",
        "node_modules",
        ".git",
        "__pycache__",
        ".pytest_cache",
    ];
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if SKIP.iter().any(|s| *s == name_str) {
            continue;
        }
        let s = entry.path();
        let d = dst.join(&name);
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_filtered(&s, &d)?;
        } else if ft.is_file() {
            std::fs::copy(&s, &d)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_content() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("a.py"), "x = 1\n").unwrap();
        std::fs::create_dir_all(src.path().join("tests")).unwrap();
        std::fs::write(src.path().join("tests/test_a.py"), "def test_a(): pass\n").unwrap();
        let dst_parent = tempfile::tempdir().unwrap();
        let dst = dst_parent.path().join("snap");
        snapshot_dir(src.path(), &dst).unwrap();
        assert_eq!(
            std::fs::read_to_string(dst.join("a.py")).unwrap(),
            "x = 1\n"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("tests/test_a.py")).unwrap(),
            "def test_a(): pass\n"
        );
    }

    #[test]
    fn preserves_dst_git_when_src_has_none() {
        // Models the canonical case: dst is the canonical workdir
        // with .git committed; src is a candidate scratch dir
        // (no .git). Promotion must preserve dst's .git so re-vet
        // accepts the result.
        let dst = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dst.path().join(".git/objects")).unwrap();
        std::fs::write(dst.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(dst.path().join("old.py"), "old\n").unwrap();

        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("new.py"), "new\n").unwrap();

        snapshot_dir(src.path(), dst.path()).unwrap();
        // .git survived
        assert!(dst.path().join(".git/HEAD").exists());
        // old.py was wiped, new.py was copied in
        assert!(!dst.path().join("old.py").exists());
        assert_eq!(
            std::fs::read_to_string(dst.path().join("new.py")).unwrap(),
            "new\n"
        );
    }

    #[test]
    fn skips_build_dirs() {
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("target/debug")).unwrap();
        std::fs::write(src.path().join("target/debug/big_artifact"), "junk").unwrap();
        std::fs::write(src.path().join("a.py"), "x = 1\n").unwrap();
        let dst_parent = tempfile::tempdir().unwrap();
        let dst = dst_parent.path().join("snap");
        snapshot_dir(src.path(), &dst).unwrap();
        assert!(dst.join("a.py").exists());
        assert!(!dst.join("target").exists());
    }
}
