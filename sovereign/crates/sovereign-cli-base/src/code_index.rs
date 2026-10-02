// SPDX-License-Identifier: AGPL-3.0-or-later
//! `tempfile_dir`, the one per-run scratch directory `svrn code index` and
//! `project init` write their ephemeral recipe and SCIP output under. The
//! verb itself moved to the code program, `sovereign-cli-dev` (pb-code-index).

use std::path::PathBuf;

pub fn tempfile_dir() -> std::io::Result<PathBuf> {
    // Avoid pulling in the `tempfile` crate — sovereign-cli doesn't
    // already use it, and a one-shot per-run dir is enough. Use the
    // system temp dir plus a pid-derived suffix for uniqueness.
    let base = std::env::temp_dir();
    let suffix = format!("sovereign-code-{}", std::process::id());
    let path = base.join(suffix);
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
