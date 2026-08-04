// SPDX-License-Identifier: AGPL-3.0-or-later
//! Service-PATH-independent resolution of developer tool binaries.
//!
//! ## Why this module exists
//!
//! The daemon runs as a supervised service with a minimal PATH
//! (launchd/systemd give it something like `/usr/local/bin:/usr/bin:/bin`),
//! while the SCIP exporters it must spawn live in per-user toolchain
//! directories: rust-analyzer in `~/.cargo/bin`, scip-typescript under an
//! nvm version dir, scip-python in a pip user bin. The observed failure
//! (2026-08-04) was a watcher that correctly detected every commit, then
//! failed every rebuild with "exporter not found in PATH" — silently, for
//! ten days — while `doctor`, run from an interactive shell with the full
//! PATH, reported the same exporters as present. The instrument validated
//! the wrong environment.
//!
//! The fix is ONE decider for "where is this tool?" that does not depend on
//! who is asking: the process PATH is consulted first (an operator's
//! explicit PATH always wins), then a curated list of well-known per-user
//! and system toolchain directories. Both exporter *detection*
//! ([`resolve`]) and the *spawn environment* ([`augmented_path_env`]) use
//! the same list, because resolving the binary alone is not enough — the
//! exporters themselves need their runtimes on PATH (rust-analyzer shells
//! out to `cargo`; scip-typescript is a `#!/usr/bin/env node` script).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// How a tool was found — carried so surfaces like `doctor` can tell the
/// operator when a tool is visible only through the *current* process's
/// PATH (i.e. a service daemon with a different environment may miss it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedVia {
    /// Found through this process's own PATH.
    ProcessPath,
    /// Found by probing the well-known toolchain directories.
    WellKnownDir,
}

/// A tool resolved to an absolute path.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// Absolute path to the binary.
    pub path: PathBuf,
    /// Which probe found it.
    pub via: ResolvedVia,
}

/// Resolve `command` to an absolute path: process PATH first, then the
/// well-known toolchain directories. Returns `None` when the tool is
/// genuinely absent from both.
pub fn resolve(command: &str) -> Option<Resolved> {
    if let Ok(path) = which::which(command) {
        return Some(Resolved {
            path,
            via: ResolvedVia::ProcessPath,
        });
    }
    for dir in well_known_tool_dirs() {
        if let Some(path) = executable_in(&dir, command) {
            return Some(Resolved {
                path,
                via: ResolvedVia::WellKnownDir,
            });
        }
    }
    None
}

/// The PATH value child tool processes should run with: the current
/// process's PATH followed by every existing well-known toolchain
/// directory not already on it. Existing PATH entries keep priority so an
/// operator's explicit environment always wins over the probe list.
pub fn augmented_path_env() -> OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut entries: Vec<PathBuf> = std::env::split_paths(&current).collect();
    for dir in well_known_tool_dirs() {
        if !entries.contains(&dir) {
            entries.push(dir);
        }
    }
    std::env::join_paths(entries).unwrap_or(current)
}

/// Existing well-known toolchain directories for the current user.
pub fn well_known_tool_dirs() -> Vec<PathBuf> {
    well_known_tool_dirs_from(home_dir().as_deref())
}

/// Pure core of [`well_known_tool_dirs`] — takes the home directory
/// explicitly so tests can exercise the probe against a fixture tree.
/// Only directories that actually exist are returned.
pub fn well_known_tool_dirs_from(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(home) = home {
        // rustup shims (rust-analyzer, cargo) — the single most common miss.
        dirs.push(home.join(".cargo/bin"));
        // pip/pipx user installs on Linux, and a general user-tools spot.
        dirs.push(home.join(".local/bin"));
        // Node version managers. nvm keeps one bin dir per installed
        // version; probe newest first so the pick matches what an
        // interactive `nvm use` most plausibly resolves to.
        dirs.extend(versioned_bins(&home.join(".nvm/versions/node"), "bin"));
        dirs.push(home.join(".volta/bin"));
        dirs.push(home.join(".npm-global/bin"));
        // pyenv shims.
        dirs.push(home.join(".pyenv/shims"));
        // Go tools (scip-go default install target).
        dirs.push(home.join("go/bin"));
        // macOS pip --user layout.
        dirs.extend(versioned_bins(&home.join("Library/Python"), "bin"));
    }

    // System package managers a service PATH often lacks anyway on Linux
    // distros, and python.org framework installs on macOS.
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.extend(versioned_bins(
        Path::new("/Library/Frameworks/Python.framework/Versions"),
        "bin",
    ));

    dirs.retain(|d| d.is_dir());
    dirs
}

/// List `<parent>/<version>/<bin>` dirs, newest version first.
/// Handles `v20.20.2`-style and `3.13`-style names; non-versioned entries
/// sort last. Returns only what exists (the caller re-filters anyway).
fn versioned_bins(parent: &Path, bin: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut versions: Vec<(Vec<u64>, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (version_key(&name), e.path().join(bin))
        })
        .collect();
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    versions.into_iter().map(|(_, p)| p).collect()
}

/// Numeric components of a version-ish directory name, for descending
/// sort: "v20.20.2" → [20, 20, 2]. Names with no digits sort as empty
/// (i.e. last).
fn version_key(name: &str) -> Vec<u64> {
    name.split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// `dir/command` if it exists and is executable. On Windows also probes
/// the conventional launcher extensions.
fn executable_in(dir: &Path, command: &str) -> Option<PathBuf> {
    let candidate = dir.join(command);
    if is_executable(&candidate) {
        return Some(candidate);
    }
    #[cfg(windows)]
    for ext in ["exe", "cmd", "bat"] {
        let candidate = dir.join(format!("{command}.{ext}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn version_key_orders_numerically_not_lexically() {
        // Lexical sort would put v9 above v20; numeric must not.
        assert!(version_key("v20.20.2") > version_key("v9.11.0"));
        assert!(version_key("3.13") > version_key("3.9"));
        assert_eq!(version_key("no-digits-here"), Vec::<u64>::new());
    }

    #[cfg(unix)]
    #[test]
    fn probes_cargo_bin_under_home_fixture() {
        let home = tempfile::tempdir().unwrap();
        let cargo_bin = home.path().join(".cargo/bin");
        std::fs::create_dir_all(&cargo_bin).unwrap();
        make_executable(&cargo_bin.join("rust-analyzer"));

        let dirs = well_known_tool_dirs_from(Some(home.path()));
        assert!(dirs.contains(&cargo_bin), "dirs: {dirs:?}");
        assert_eq!(
            executable_in(&cargo_bin, "rust-analyzer"),
            Some(cargo_bin.join("rust-analyzer"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn nvm_versions_probe_newest_first() {
        let home = tempfile::tempdir().unwrap();
        for v in ["v9.11.0", "v20.20.2"] {
            std::fs::create_dir_all(home.path().join(".nvm/versions/node").join(v).join("bin"))
                .unwrap();
        }
        let dirs = well_known_tool_dirs_from(Some(home.path()));
        let nvm_dirs: Vec<&PathBuf> = dirs
            .iter()
            .filter(|d| d.to_string_lossy().contains(".nvm"))
            .collect();
        assert_eq!(nvm_dirs.len(), 2);
        assert!(
            nvm_dirs[0].to_string_lossy().contains("v20.20.2"),
            "newest nvm version must come first: {nvm_dirs:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_executable_file_is_not_resolved() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tool"), "data").unwrap();
        assert_eq!(executable_in(dir.path(), "tool"), None);
    }

    #[test]
    fn augmented_path_keeps_current_path_priority() {
        // The current PATH's entries must appear before any probe-added dir.
        let current = std::env::var_os("PATH").unwrap_or_default();
        let first_current = std::env::split_paths(&current).next();
        let augmented = augmented_path_env();
        let first_augmented = std::env::split_paths(&augmented).next();
        assert_eq!(first_current, first_augmented);
    }
}
