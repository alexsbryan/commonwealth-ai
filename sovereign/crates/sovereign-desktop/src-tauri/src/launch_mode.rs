// SPDX-License-Identifier: AGPL-3.0-or-later
//! What THIS desktop process became, decided once in `main` and read wherever
//! it is needed.
//!
//! **Only half of it survives svt-3, and the half that went is the reason it
//! was written.** It existed so `state::bootstrap` could name a `Launch` when
//! it commissioned the in-process daemon through `sovereign_mesh::assemble`,
//! without either hardcoding `Launch::Desktop` at that site or running a
//! second `Launch::parse` (the §10.6 duplicate `quality/TOPOLOGY.md` §1
//! records). There is no commissioning site: the desktop assembles no daemon,
//! so nothing downstream asks what this process is.
//!
//! What remains is `DaemonHost`, which is a different question — where the
//! daemon RUNS — resolved once here rather than at each point of use.
//! `publish` still takes the `Launch` it is handed so `main` keeps one call
//! at one place; it no longer stores it.

use std::sync::OnceLock;

use sovereign_contracts::launch::{DaemonHost, Launch};

static DAEMON_HOST: OnceLock<DaemonHost> = OnceLock::new();

/// Publish `main`'s parse, and resolve the launch-topology environment ONCE
/// while we are here (`quality/TOPOLOGY.md` Phase 10, §6.2).
///
/// Called exactly once, immediately after `Launch::parse`, before any
/// subsystem starts — which is what makes "resolved at construction" true
/// rather than aspirational.
pub(crate) fn publish(_launch: Launch) {
    let _ = DAEMON_HOST.set(DaemonHost::from_env());
}

/// Where this desktop's daemon runs — supervised child, or in-process and why.
///
/// Resolved once by [`publish`]. The `from_env` fallback covers a harness that
/// drove a subsystem without going through `main`; it is the same answer, just
/// paid for again.
pub(crate) fn daemon_host() -> DaemonHost {
    DAEMON_HOST
        .get()
        .copied()
        .unwrap_or_else(DaemonHost::from_env)
}
