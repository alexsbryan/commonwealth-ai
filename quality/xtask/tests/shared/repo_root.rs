// SPDX-License-Identifier: AGPL-3.0-or-later
//! Repo-root resolution for the two workspace-hygiene generators in
//! `xtask/tests/`. ONE derivation, shared by both test binaries (ARCH §10.6):
//! getting it wrong is the exact defect that moved these files here, and two
//! copies is two chances to half-fix the next relayout.
//!
//! It is not `xtask::common::repo_root` because `xtask` is a bin-only package
//! — there is no lib target for an integration test to link, and adding a
//! `[lib]` purely for six lines is the larger change. The derivation is
//! identical and the check below is what keeps them honest.

use std::path::PathBuf;

/// Repo root — the grandparent of `quality/xtask/`.
///
/// PANICS when the resolved directory does not look like this workspace.
/// A generator that cannot find the repo must say so in one line, not fail
/// six frames deeper with an `ENOENT` on a path nobody expected (ARCH §18.3
/// — absence is reported, never defaulted, and never skipped past).
pub fn repo_root() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("xtask manifest has no grandparent")
        .to_path_buf();
    assert!(
        root.join("quality").is_dir() && root.join("Cargo.toml").is_file(),
        "repo root did not resolve: {} has no quality/ and Cargo.toml. \
         These generators read the whole workspace and cannot run outside it.",
        root.display()
    );
    root
}

/// Every workspace member's directory, read from the root `Cargo.toml`
/// `members` — the one list of where crates live, at any depth.
///
/// Census walks scan these rather than a parent like `sovereign/crates/`:
/// a parent held only some crates, and stopped meaning anything when the
/// top level moved to one dir per program (a walk over a parent that lost
/// half its children narrows without failing). `quality/` members — xtask
/// itself and arch-layers — are left out: they are the census code, and
/// hold the literals the walks search for.
#[allow(dead_code)] // each test binary mounts this file; not all walk members
pub fn member_dirs() -> Vec<PathBuf> {
    let root = repo_root();
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).expect("root Cargo.toml");
    let doc: toml::Value = manifest.parse().expect("root Cargo.toml parses");
    let dirs: Vec<PathBuf> = doc["workspace"]["members"]
        .as_array()
        .expect("[workspace] members")
        .iter()
        .filter_map(|m| m.as_str())
        .filter(|m| !m.starts_with("quality/"))
        .map(|m| root.join(m))
        .collect();
    assert!(
        dirs.len() > 50,
        "only {} workspace members outside quality/ — not this workspace's Cargo.toml",
        dirs.len()
    );
    dirs
}
