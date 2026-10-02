// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `SOVEREIGN_*` <-> `SVRNMESH_*` env mirror every binary runs first.
//! Moved whole from `sovereign_contracts::rebrand`, which re-exports it at its
//! historical path, so cw-rails (whose `[[forbid]]` keeps it off every
//! sovereign-* crate) runs the same mirror rather than a twin.

/// Mirror the legacy/new env-var prefixes in both directions so that neither
/// old scripts (setting `SOVEREIGN_*`) nor not-yet-converted read sites (still
/// reading `SOVEREIGN_*`, or already reading `SVRNMESH_*`) break during the
/// transition.
///
/// MUST be called from each binary's `main()` *before* the async runtime is
/// built — mutating the process environment is only sound single-threaded.
/// Idempotent: a var already present under the target prefix is never
/// overwritten, so re-running (e.g. the dispatcher exec'ing a sibling that
/// re-runs the shim) is a no-op.
pub fn promote_legacy_env() {
    // Snapshot first: we mutate the environment inside the loop, and iterating
    // `vars()` while calling `set_var` would otherwise be unsound.
    let snapshot: Vec<(String, String)> = std::env::vars().collect();
    let mut promoted = 0usize;
    for (key, val) in &snapshot {
        if let Some(suffix) = key.strip_prefix("SOVEREIGN_") {
            let new_key = format!("SVRNMESH_{suffix}");
            if std::env::var_os(&new_key).is_none() {
                std::env::set_var(&new_key, val);
                promoted += 1;
            }
        } else if let Some(suffix) = key.strip_prefix("SVRNMESH_") {
            let old_key = format!("SOVEREIGN_{suffix}");
            if std::env::var_os(&old_key).is_none() {
                std::env::set_var(&old_key, val);
            }
        }
    }
    if promoted > 0 {
        eprintln!(
            "svrnmesh: bridged {promoted} legacy SOVEREIGN_* env var(s) to SVRNMESH_* \
             (the SOVEREIGN_* prefix is deprecated — update your scripts)"
        );
    }
}
