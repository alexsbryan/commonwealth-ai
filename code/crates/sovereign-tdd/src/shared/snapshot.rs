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
            // `node_modules` is preserved like `.git`: it is
            // heavyweight immutable infrastructure, not source. On
            // winner-promote (candidate → canonical workdir) wiping
            // it would leave the project unrunnable — and the
            // candidate side only carries a symlink to it anyway.
            if entry.file_name() == ".git" || entry.file_name() == "node_modules" {
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
    copy_dir_filtered(src, dst)?;
    share_node_modules(src, dst)
}

/// JS test commands need the dependency tree, but copying a
/// `node_modules` (hundreds of MB with browsers' drivers) per
/// candidate is prohibitive — so a fresh snapshot gets a SYMLINK to
/// the source's real dir instead. Canonicalized first, so a
/// candidate whose own `node_modules` is already a symlink (every
/// candidate) hands the next hop the real directory, never a chain.
/// Tests only read the tree; runtime caches inside it (vite's
/// `.vite`) are dependency-keyed, and Playwright trials run
/// candidates serially, so sharing is safe.
#[cfg(unix)]
fn share_node_modules(src: &Path, dst: &Path) -> std::io::Result<()> {
    let nm_src = src.join("node_modules");
    let nm_dst = dst.join("node_modules");
    // symlink_metadata, not exists(): an existing-but-dangling
    // symlink must count as present, or symlink() errors on it.
    if !nm_src.is_dir() || nm_dst.symlink_metadata().is_ok() {
        return Ok(());
    }
    let real = std::fs::canonicalize(&nm_src)?;
    std::os::unix::fs::symlink(real, nm_dst)
}

#[cfg(not(unix))]
fn share_node_modules(_src: &Path, _dst: &Path) -> std::io::Result<()> {
    Ok(())
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

    #[cfg(unix)]
    #[test]
    fn node_modules_is_shared_by_symlink_not_copied() {
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("node_modules/vite")).unwrap();
        std::fs::write(src.path().join("node_modules/vite/pkg.js"), "x").unwrap();
        std::fs::write(src.path().join("app.ts"), "export {}\n").unwrap();
        let dst_parent = tempfile::tempdir().unwrap();
        let dst = dst_parent.path().join("snap");
        snapshot_dir(src.path(), &dst).unwrap();
        let nm = dst.join("node_modules");
        assert!(nm.symlink_metadata().unwrap().file_type().is_symlink());
        // Deps resolve through the link.
        assert!(nm.join("vite/pkg.js").exists());
    }

    #[cfg(unix)]
    #[test]
    fn promote_preserves_dst_real_node_modules_and_never_chains() {
        // Candidate (src, with a symlinked node_modules) promotes
        // onto the canonical workdir (dst, with the REAL dir). The
        // real dir must survive and stay a real dir.
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("node_modules/vite")).unwrap();
        std::fs::write(base.path().join("main.ts"), "old\n").unwrap();

        let candidate = tempfile::tempdir().unwrap();
        std::fs::write(candidate.path().join("main.ts"), "new\n").unwrap();
        std::os::unix::fs::symlink(
            base.path().join("node_modules"),
            candidate.path().join("node_modules"),
        )
        .unwrap();

        snapshot_dir(candidate.path(), base.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(base.path().join("main.ts")).unwrap(),
            "new\n"
        );
        let meta = base.path().join("node_modules").symlink_metadata().unwrap();
        assert!(
            meta.file_type().is_dir(),
            "real dir survived, no symlink chain"
        );
        assert!(base.path().join("node_modules/vite").exists());
    }
}
