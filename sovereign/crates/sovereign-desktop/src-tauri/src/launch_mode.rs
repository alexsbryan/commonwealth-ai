// SPDX-License-Identifier: AGPL-3.0-or-later
//! What THIS desktop process became, decided once in `main` and read wherever
//! it is needed.
//!
//! `main.rs` already runs the one `Launch::parse` for this binary and
//! dispatches on it — child re-execs never reach Tauri. But `state::bootstrap`
//! runs deep inside the Tauri setup closure, far from that decision, and it
//! has to name a `Launch` when it commissions the in-process daemon through
//! `sovereign_mesh::assemble`.
//!
//! The two wrong answers are a literal `Launch::Desktop` at the commissioning
//! site (a second place deciding what the process is — the §10.6 duplicate
//! `Launch` exists to remove) and a second `Launch::parse` (the same call with
//! a different default arm, which is how two argv scans disagreed here in the
//! first place; `quality/TOPOLOGY.md` §1). So `main` publishes its answer and
//! everything downstream reads it.

use std::sync::OnceLock;

use sovereign_contracts::launch::{DaemonHost, Launch};

static LAUNCH: OnceLock<Launch> = OnceLock::new();
static DAEMON_HOST: OnceLock<DaemonHost> = OnceLock::new();

/// Publish `main`'s parse, and resolve the launch-topology environment ONCE
/// while we are here (`quality/TOPOLOGY.md` Phase 10, §6.2).
///
/// Called exactly once, immediately after `Launch::parse`, before any
/// subsystem starts — which is what makes "resolved at construction" true
/// rather than aspirational.
pub(crate) fn publish(launch: Launch) {
    let _ = LAUNCH.set(launch);
    let _ = DAEMON_HOST.set(DaemonHost::from_env());
}

/// What this process is.
///
/// Falls back to [`Launch::Desktop`] only if `publish` never ran, which means
/// a test or a harness drove `state::bootstrap` without going through `main`.
/// That is the honest default for this binary — reaching a Tauri bootstrap at
/// all means the launch fell through `main`'s dispatch, and `Desktop` is the
/// arm it falls through to.
pub(crate) fn get() -> Launch {
    LAUNCH.get().cloned().unwrap_or(Launch::Desktop)
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
