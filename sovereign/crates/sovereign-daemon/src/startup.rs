// SPDX-License-Identifier: AGPL-3.0-or-later
//! Small startup helpers the assembly needs: the daemon's pidfile path. (The
//! one-shot orphaned-index warning went with the Reindexer's registry to the
//! code program at pb-code-daemon-exit.)
//!
//! Moved out of the binary's `daemon_cmd` at domains
//! `dm-daemon-cli-composition` (2026-09-17). `bootstrap` calls them, and a
//! library may not reach up into its host; they are here rather than in
//! `bootstrap` because that file is over ARCH §3.1's oversized ceiling and the
//! ratchet allows no growth (`quality/baselines/oversized.txt`).

use std::path::PathBuf;

/// Path to the pidfile written by `daemon start`.
///
/// The body is the one the binary used —
/// `sovereign_contracts::rebrand::svrnmesh_root().join("daemon.pid")`, which is
/// what `sovereign_cli_shared::dirs::sovereign_root` delegates to. The binary's
/// `daemon_cmd::lifecycle` keeps its own accessor delegating here so the
/// lifecycle verbs and this module agree on one derivation.
pub fn daemon_pid_path() -> PathBuf {
    sovereign_contracts::rebrand::svrnmesh_root().join("daemon.pid")
}
