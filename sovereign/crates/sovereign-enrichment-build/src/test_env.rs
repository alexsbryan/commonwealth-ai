// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared test helpers, behind the `test-support` feature.
//!
//! `std::env::set_var("HOME", …)` is process-wide state; tests that scope
//! `HOME` to a tempdir must acquire this lock before doing so to avoid racing
//! each other.
//!
//! It lives here, and not in each crate that needs it, so there is ONE lock
//! definition (ARCH §10.6). `sovereign-cli-llm` still has four test modules
//! that scope `HOME` — `egress_reds`, `errors`, `reset`, `integration_tests` —
//! and reaches this one through a re-export in `enrich_cmd/mod.rs` rather than
//! keeping a second copy. The `static` is per-process either way, which is the
//! semantics a HOME lock wants: two test binaries do not share a `HOME`.

use std::sync::{Mutex, MutexGuard};

static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Handle holding both the tempdir and the `HOME` lock. Drop to release.
pub struct HomeGuard {
    dir: tempfile::TempDir,
    _guard: MutexGuard<'static, ()>,
}

impl HomeGuard {
    pub fn path(&self) -> &std::path::Path {
        self.dir.path()
    }
}

/// Acquire the `HOME` lock and point `HOME` at a fresh tempdir.
pub fn scoped_home() -> HomeGuard {
    let guard = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", dir.path());
    HomeGuard { dir, _guard: guard }
}
