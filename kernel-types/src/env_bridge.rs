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
    for removed in removed_set(snapshot.iter().map(|(k, _)| k.as_str())) {
        eprintln!(
            "svrnmesh: {} is set but no longer read, so it has no effect — {}",
            removed.name, removed.instead
        );
    }
}

/// An env var nothing reads any more, and what to do instead.
#[derive(Debug, PartialEq, Eq)]
pub struct RemovedEnv {
    /// Canonical `SOVEREIGN_*` name; the `SVRNMESH_*` spelling matches too.
    pub name: &'static str,
    /// The successor to set, or why nothing replaces it.
    pub instead: &'static str,
}

/// Every `removed` row of `quality/env-flags.toml`, name and `replacement`
/// verbatim — `cargo xtask env-gate` fails when the two disagree, so the
/// registry stays the one decider and this is its runtime copy.
pub const REMOVED_ENV: &[RemovedEnv] = &[
    RemovedEnv { name: "SOVEREIGN_AGENTIC_KQ", instead: "retired: the agentic evidence loop was deleted (cc78b933b)" },
    RemovedEnv { name: "SOVEREIGN_AGENTIC_KQ_THRESHOLD", instead: "retired with SOVEREIGN_AGENTIC_KQ's loop (cc78b933b)" },
    RemovedEnv { name: "SOVEREIGN_BIND", instead: "set [daemon] client_bind in config.toml; its reader, sovereign-server, was deleted (5cb09f22b)" },
    RemovedEnv { name: "SOVEREIGN_CONV_PPR_WEIGHT", instead: "retired: conversation entity PPR was deleted at its default, 0 (cc78b933b)" },
    RemovedEnv { name: "SOVEREIGN_DB_PATH", instead: "set SOVEREIGN_DATA_DIR; the state DB lives under the data root, and this var's reader, sovereign-server, was deleted (5cb09f22b)" },
    RemovedEnv { name: "SOVEREIGN_DECOMP_DECAY", instead: "retired with SOVEREIGN_QUERY_DECOMP's step (ac032e5bc)" },
    RemovedEnv { name: "SOVEREIGN_DEMAND_PLAN", instead: "retired: the demand-plan retrieval step was deleted (ac032e5bc)" },
    RemovedEnv { name: "SOVEREIGN_DEMAND_PLAN_FANOUT", instead: "retired with SOVEREIGN_DEMAND_PLAN's step (ac032e5bc)" },
    RemovedEnv { name: "SOVEREIGN_FRONTDOOR", instead: "set SOVEREIGN_HARNESS=opencode; the alias was cut (cc78b933b)" },
    RemovedEnv { name: "SOVEREIGN_GRAPH_NEIGHBOR_EXPAND", instead: "retired: the graph-neighbour expansion step was deleted (ac032e5bc)" },
    RemovedEnv { name: "SOVEREIGN_META_BRIDGE", instead: "retired: the meta-atlas bridge boost was deleted (ac032e5bc)" },
    RemovedEnv { name: "SOVEREIGN_QUERY_DECOMP", instead: "retired: the query-decomposition retrieval step was deleted (ac032e5bc)" },
    RemovedEnv { name: "SOVEREIGN_SERVER_PATH", instead: "retired: the mobile host that read it was deleted (efa709871)" },
    RemovedEnv { name: "SOVEREIGN_SUFFICIENCY_CHUNKS", instead: "retired with SOVEREIGN_AGENTIC_KQ's loop (cc78b933b)" },
    RemovedEnv { name: "SOVEREIGN_TITLE_EXPAND", instead: "retired: the title-expansion retrieval step was deleted (ac032e5bc)" },
];

/// The [`REMOVED_ENV`] rows the given env keys set, under either prefix, once
/// each and in table order.
pub fn removed_set<'k>(keys: impl Iterator<Item = &'k str>) -> Vec<&'static RemovedEnv> {
    let set: std::collections::BTreeSet<String> = keys
        .filter_map(|k| {
            k.strip_prefix("SVRNMESH_")
                .or_else(|| k.strip_prefix("SOVEREIGN_"))
        })
        .map(|suffix| format!("SOVEREIGN_{suffix}"))
        .collect();
    REMOVED_ENV
        .iter()
        .filter(|r| set.contains(r.name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_removed_var_is_found_under_either_prefix_once() {
        let first = REMOVED_ENV[0].name;
        let suffix = first.trim_start_matches("SOVEREIGN_");
        let branded = format!("SVRNMESH_{suffix}");
        let live = format!("{first}_DEBUG");
        let keys = [first, branded.as_str(), live.as_str(), "PATH"];
        assert_eq!(removed_set(keys.into_iter()), vec![&REMOVED_ENV[0]]);
        assert!(removed_set(["PATH", live.as_str()].into_iter()).is_empty());
    }
}
