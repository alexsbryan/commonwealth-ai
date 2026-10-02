// SPDX-License-Identifier: AGPL-3.0-or-later
//! Locating the `sovereign-cli` dispatcher from a sibling binary.

use std::path::{Path, PathBuf};

/// The dispatcher beside `current`, else the installed symlink. One decider.
pub fn dispatcher_exe(current: &Path) -> Result<PathBuf, String> {
    if let Some(dir) = current.parent() {
        for name in ["sovereign-cli", "svrn"] {
            let c = dir.join(name);
            if c.exists() {
                return Ok(c);
            }
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    for c in [
        home.join(".local/bin/sovereign"),
        home.join(".local/bin/svrn"),
    ] {
        if c.exists() {
            return Ok(c);
        }
    }
    Err("cannot find the `sovereign-cli` dispatcher beside this binary or on ~/.local/bin".into())
}
