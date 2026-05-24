//! Copy a workdir to a sibling scratch dir, skipping heavy build
//! output directories. Used to make per-candidate snapshots so test
//! runs don't pollute sibling candidates.

use std::path::Path;

pub fn snapshot_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    if dst.exists() {
        std::fs::remove_dir_all(dst)?;
    }
    std::fs::create_dir_all(dst)?;
    copy_dir_filtered(src, dst)
}

fn copy_dir_filtered(src: &Path, dst: &Path) -> std::io::Result<()> {
    const SKIP: &[&str] = &["target", "node_modules", ".git", "__pycache__", ".pytest_cache"];
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
        assert_eq!(std::fs::read_to_string(dst.join("a.py")).unwrap(), "x = 1\n");
        assert_eq!(
            std::fs::read_to_string(dst.join("tests/test_a.py")).unwrap(),
            "def test_a(): pass\n"
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
