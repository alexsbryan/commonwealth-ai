// SPDX-License-Identifier: AGPL-3.0-or-later
//! Small startup helpers the assembly needs: the daemon's pidfile path and the
//! one-shot orphaned-index warning.
//!
//! Both moved out of the binary's `daemon_cmd` at domains
//! `dm-daemon-cli-composition` (2026-09-17). `bootstrap` calls them, and a
//! library may not reach up into its host; they are here rather than in
//! `bootstrap` because that file is over ARCH §3.1's oversized ceiling and the
//! ratchet allows no growth (`quality/baselines/oversized.txt`).

use std::path::{Path, PathBuf};

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

/// Is this process armed to serve RPC workers? One reader of
/// `SOVEREIGN_RPC_DISCOVER`.
///
/// Three sites ask (`bootstrap`, `build/containment`, `doctor_cmd`) and two of
/// them feed a containment VERDICT, so a divergence would mean the doctor
/// reporting a containment posture the daemon does not actually run under. It
/// lives here rather than in `bootstrap` because `bootstrap` is gated on
/// `treesitter` and `build/containment` is not (domains
/// `dm-daemon-cli-composition`, 2026-09-17).
pub fn rpc_discovery_armed() -> bool {
    std::env::var("SOVEREIGN_RPC_DISCOVER").is_ok()
}

/// Surface orphaned per-corpus SCIP indexes at startup.
///
/// On an upgrade from a pre-registry sovereign, `~/.svrnmesh/
/// indexes/<corpus>/scip_graph.db` will often exist even though
/// `projects.json` is empty. The daemon can't safely auto-register
/// those — we don't know which filesystem path each one came
/// from, and guessing could point the FS watcher at the wrong
/// directory. Instead, log a one-shot hint so the operator knows
/// to re-register each repo manually.
pub fn warn_orphaned_indexes(
    indexes_dir: &Path,
    registry: &sovereign_contracts::watcher_projects::Registry,
) {
    let Ok(entries) = std::fs::read_dir(indexes_dir) else {
        return;
    };
    let registered: std::collections::HashSet<&str> = registry
        .entries()
        .iter()
        .map(|e| e.corpus_id.as_str())
        .collect();
    let mut orphans: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        // Skip flat files (project_docs.db, lint_results.db, etc.).
        if !entry.path().is_dir() {
            continue;
        }
        let scip = entry.path().join("scip_graph.db");
        if !scip.exists() {
            continue;
        }
        if registered.contains(name.as_str()) {
            continue;
        }
        orphans.push(name);
    }
    if orphans.is_empty() {
        return;
    }
    eprintln!();
    eprintln!(
        "  \u{26a0} Found {} SCIP index(es) on disk with no registry entry:",
        orphans.len()
    );
    for o in &orphans {
        eprintln!("      {o}");
    }
    eprintln!(
        "  Run `svrn project register` in each repo to resume watching.\n\
         (The daemon won't guess the filesystem path for you — bad guesses\n\
         point the FS watcher at the wrong directory.)"
    );
    eprintln!();
}
