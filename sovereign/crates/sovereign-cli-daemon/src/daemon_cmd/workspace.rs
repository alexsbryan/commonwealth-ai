// SPDX-License-Identifier: AGPL-3.0-or-later
//! Workspace auto-detection — extracted from `daemon_cmd` (§3.2).
//! Resolves the repo the daemon should watch for lint/test changes
//! (env → ~/.svrnmesh/workspace → ascend from exe/cwd).

use std::path::PathBuf;

use super::sovereign_root;

/// Resolve the workspace directory the daemon should watch for
/// lint/test changes. Returns `None` when the user has not opted in,
/// in which case `lint_status` / `test_status` report
/// `watcher_active: false` and `never_run` — the honest signal.
///
/// Lookup order:
/// 1. `SOVEREIGN_WORKSPACE_DIR` environment variable. Preferred for
///    launchd/systemd: set it in the service's environment block so
///    every daemon launch picks it up automatically.
/// 2. `~/.svrnmesh/workspace` — single-line text file containing
///    the workspace path. Useful for users who can't easily edit
///    their service environment.
///
/// Both forms are validated to point at an existing directory; a
/// missing or non-directory path is treated as "no workspace
/// configured" (with a warning log so the misconfiguration is
/// visible in the daemon log without breaking startup).
pub(super) fn resolve_workspace_dir() -> Option<PathBuf> {
    // 1. Explicit env override — preferred for launchd / systemd /
    //    container setups where the daemon doesn't know its own
    //    repo path at build time.
    if let Ok(val) = std::env::var("SOVEREIGN_WORKSPACE_DIR") {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            if path.is_dir() {
                return Some(path);
            } else {
                tracing::warn!(
                    path = %path.display(),
                    "SOVEREIGN_WORKSPACE_DIR set but not a directory — ignoring"
                );
            }
        }
    }

    // 2. User-pinned path written to `~/.svrnmesh/workspace`.
    //    Honoured when the user wants a non-default location (e.g.
    //    multi-checkout dev with switching).
    let workspace_file = sovereign_root().join("workspace");
    if let Ok(contents) = std::fs::read_to_string(&workspace_file) {
        let trimmed = contents.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            if path.is_dir() {
                return Some(path);
            } else {
                tracing::warn!(
                    path = %path.display(),
                    file = %workspace_file.display(),
                    "~/.svrnmesh/workspace path is not a directory — ignoring"
                );
            }
        }
    }

    // 3. **Auto-detect** — works on a fresh checkout on any machine
    //    without per-host configuration. Two sources, in order:
    //      a. The daemon binary's own location. When sovereign-cli
    //         was built from this repo at `<repo>/target/release/sovereign-cli`,
    //         walking up from `current_exe()` finds the repo root.
    //         Robust across host paths because it doesn't hard-code
    //         a username or home dir.
    //      b. Walk up from the daemon's CWD looking for the
    //         sovereign-workspace signature. Lets a developer run
    //         `cargo run --bin sovereign-cli` from anywhere inside
    //         the tree.
    //    The signature is `scripts/sovereign-lint.sh` + a
    //    workspace-shaped `Cargo.toml` at the same root — strict
    //    enough that a generic Cargo workspace in `$HOME` doesn't
    //    accidentally trip the lint runner.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(found) = ascend_for_sovereign_workspace(&exe) {
            tracing::info!(
                workspace = %found.display(),
                source = "current_exe",
                "resolve_workspace_dir: auto-detected from daemon binary"
            );
            return Some(found);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(found) = ascend_for_sovereign_workspace(&cwd) {
            tracing::info!(
                workspace = %found.display(),
                source = "current_dir",
                "resolve_workspace_dir: auto-detected from cwd"
            );
            return Some(found);
        }
    }

    None
}

/// Walk up from `start` looking for a directory that holds both
/// `scripts/sovereign-lint.sh` and a workspace-shaped `Cargo.toml`.
/// Returns the directory when found. Bounded ascent (12 hops) so
/// we don't sweep `/` on weird CWDs.
fn ascend_for_sovereign_workspace(start: &std::path::Path) -> Option<PathBuf> {
    let mut cur: Option<&std::path::Path> = Some(start);
    for _ in 0..12 {
        let dir = cur?;
        if looks_like_sovereign_workspace(dir) {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

fn looks_like_sovereign_workspace(dir: &std::path::Path) -> bool {
    let lint_script = dir.join("scripts").join("sovereign-lint.sh");
    if !lint_script.is_file() {
        return false;
    }
    let cargo_toml = dir.join("Cargo.toml");
    let Ok(contents) = std::fs::read_to_string(&cargo_toml) else {
        return false;
    };
    // Cheap shape check — a `[workspace]` table is the load-bearing
    // signal for "this is the monorepo root", not a single-crate
    // Cargo.toml that happens to live next to a `scripts/` dir.
    contents.contains("[workspace]")
}
#[cfg(test)]
mod workspace_autodetect_tests {
    use super::{ascend_for_sovereign_workspace, looks_like_sovereign_workspace};
    use std::fs;
    use tempfile::TempDir;

    fn make_workspace(root: &std::path::Path) {
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(
            root.join("scripts").join("sovereign-lint.sh"),
            "#!/usr/bin/env bash\n",
        )
        .unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nmembers = []\n",
        )
        .unwrap();
    }

    #[test]
    fn looks_like_workspace_accepts_workspace_with_lint_script() {
        let tmp = TempDir::new().unwrap();
        make_workspace(tmp.path());
        assert!(looks_like_sovereign_workspace(tmp.path()));
    }

    #[test]
    fn looks_like_workspace_rejects_single_crate_cargo_toml() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("scripts")).unwrap();
        fs::write(
            tmp.path().join("scripts").join("sovereign-lint.sh"),
            "#!/usr/bin/env bash\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"lone\"\nversion = \"0.0.1\"\n",
        )
        .unwrap();
        assert!(!looks_like_sovereign_workspace(tmp.path()));
    }

    #[test]
    fn looks_like_workspace_rejects_workspace_without_lint_script() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        assert!(!looks_like_sovereign_workspace(tmp.path()));
    }

    #[test]
    fn ascend_finds_workspace_from_nested_path() {
        let tmp = TempDir::new().unwrap();
        make_workspace(tmp.path());
        let deep = tmp
            .path()
            .join("target")
            .join("release")
            .join("sovereign-cli");
        fs::create_dir_all(deep.parent().unwrap()).unwrap();
        fs::write(&deep, "binary").unwrap();
        // Canonicalise both sides — macOS' /var ↔ /private/var
        // symlink makes the raw paths differ even though they
        // resolve to the same inode.
        let found = ascend_for_sovereign_workspace(&deep).unwrap();
        assert_eq!(
            fs::canonicalize(&found).unwrap(),
            fs::canonicalize(tmp.path()).unwrap()
        );
    }

    #[test]
    fn ascend_returns_none_when_no_workspace_above() {
        let tmp = TempDir::new().unwrap();
        let plain = tmp.path().join("not-a-workspace").join("nested");
        fs::create_dir_all(&plain).unwrap();
        assert!(ascend_for_sovereign_workspace(&plain).is_none());
    }
}
