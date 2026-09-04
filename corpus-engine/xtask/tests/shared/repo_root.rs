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

/// Repo root — the grandparent of `corpus-engine/xtask/`.
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
