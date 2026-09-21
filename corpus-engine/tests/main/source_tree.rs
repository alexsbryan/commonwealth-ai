// SPDX-License-Identifier: AGPL-3.0-or-later
//! One decider each for the two trees this crate's gates read outside itself.
//!
//! Both arrive as env knobs, never as a climb out of the crate root: a third
//! party who lifts this package carries these tests and has neither tree
//! (boundary-gate rule 3c). An absent or wrong knob PANICS naming what to set —
//! never a skip, which is a gate that cannot fail (ARCH §18.1).

use std::path::PathBuf;

/// Names the monorepo source tree. Set for in-repo builds by `.cargo/config.toml`.
pub const WORKSPACE_ROOT_ENV: &str = "SOVEREIGN_WORKSPACE_ROOT";

/// Names the canonical recipes tree — the SAME knob `build.rs` vendors from.
pub const RECIPES_DIR_ENV: &str = "CORPUS_ENGINE_RECIPES_DIR";

/// The monorepo source tree. Panics naming the knob when it is absent.
pub fn workspace_root() -> PathBuf {
    require_dir(WORKSPACE_ROOT_ENV, "the monorepo source tree")
}

/// The canonical `sovereign-recipes/` tree. Panics naming the knob when absent.
pub fn recipes_root() -> PathBuf {
    require_dir(RECIPES_DIR_ENV, "a sovereign-recipes checkout")
}

/// Absence is reported, never defaulted: unset and not-a-directory both panic.
fn require_dir(var: &str, what: &str) -> PathBuf {
    let raw = std::env::var(var).unwrap_or_else(|_| panic!("{var} is unset — point it at {what}"));
    let path = PathBuf::from(raw);
    assert!(
        path.is_dir(),
        "{var} points at {} — {what} is not there",
        path.display()
    );
    path
}
