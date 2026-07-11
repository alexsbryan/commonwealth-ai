// SPDX-License-Identifier: AGPL-3.0-or-later
//! layer-gate — Cargo-DECLARED dependency edges must obey the layer map
//! (`quality/ARCH_LAYERS.toml`), plus the crate fan-in ratchet.
//!
//! This is the deterministic half of dependency-direction enforcement: it
//! parses manifests only, runs in <1s, and needs no daemon. The code-intel
//! `arch_report` checks the same policy against SCIP-OBSERVED symbol
//! references (catching coupling that re-exports hide) — both halves share
//! the `arch-layers` parser/evaluator so they can't drift on semantics.
//!
//! Fan-in ratchet: `quality/baselines/fan_in.tsv` caps how many workspace
//! crates may depend on each tracked (god-)crate. One more dependent on
//! `corpus-engine` becomes a visible, reviewed baseline diff instead of
//! silent accretion.

use arch_layers::{DepEdge, DepKind};
use std::collections::{BTreeMap, BTreeSet};

use crate::common;
use crate::manifests;

/// Crates enter the fan-in baseline when first snapshotted at or above this
/// many dependents. (Below it, fan-in isn't the interesting signal.) 8, not
/// 9: sovereign-tools — the workspace's sharpest hub (fan-in 8 AND fan-out
/// 13) — must be inside the ratchet.
const FAN_IN_SEED_THRESHOLD: usize = 8;

pub fn run(args: &[String]) -> i32 {
    let root = common::repo_root();
    let flags = common::baseline_flags(args);
    let map_path = root.join("quality/ARCH_LAYERS.toml");
    let fan_in_path = common::baselines_dir(&root).join("fan_in.tsv");

    let map_text = match std::fs::read_to_string(&map_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "error: cannot read {} ({e}).\n  The layer map is the dependency-direction \
                 contract — see ARCH_PRINCIPLES.md §8.",
                map_path.display()
            );
            return 1;
        }
    };
    let map = match arch_layers::parse(&map_text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    let members = manifests::workspace_members(&root);
    let names: BTreeSet<String> = members.iter().map(|m| m.name.clone()).collect();
    let edges = manifests::internal_dep_edges(&root, &members);
    let fan_in = compute_fan_in(&edges);

    // ── Baseline maintenance modes ────────────────────────────────────────────
    if flags.update {
        let tracked: BTreeMap<String, usize> = if fan_in_path.exists() {
            // Keep the tracked SET stable; snapshot current counts for it.
            common::load_count_map(&fan_in_path)
                .keys()
                .map(|k| (k.clone(), fan_in.get(k).copied().unwrap_or(0)))
                .collect()
        } else {
            // First snapshot: seed with every crate at/above the threshold.
            fan_in
                .iter()
                .filter(|(_, &n)| n >= FAN_IN_SEED_THRESHOLD)
                .map(|(k, &n)| (k.clone(), n))
                .collect()
        };
        if let Err(e) = write_fan_in(&fan_in_path, &tracked) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "wrote {} ({} crates fan-in-ratcheted)",
            fan_in_path.display(),
            tracked.len()
        );
        return 0;
    }
    if flags.tighten {
        let baseline = common::load_count_map(&fan_in_path);
        let tightened: BTreeMap<String, usize> = baseline
            .iter()
            .map(|(k, &cap)| (k.clone(), fan_in.get(k).copied().unwrap_or(0).min(cap)))
            .collect();
        if tightened == baseline {
            eprintln!(
                "layer-gate --tighten: fan-in baseline already tight ({} entries)",
                baseline.len()
            );
            return 0;
        }
        let lowered = tightened
            .iter()
            .filter(|(k, &v)| baseline.get(*k).is_some_and(|&b| v < b))
            .count();
        if let Err(e) = write_fan_in(&fan_in_path, &tightened) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "layer-gate --tighten: {lowered} caps lowered → {}",
            fan_in_path.display()
        );
        return 0;
    }

    // ── Check mode ────────────────────────────────────────────────────────────
    let violations = arch_layers::evaluate(&map, &names, &edges);

    let baseline = common::load_count_map(&fan_in_path);
    let mut fan_in_fails: Vec<String> = Vec::new();
    for (name, &cap) in &baseline {
        let actual = fan_in.get(name).copied().unwrap_or(0);
        if actual > cap {
            fan_in_fails.push(format!(
                "fan-in of `{name}` grew {cap} → {actual}: one more crate now depends on \
                 this god-crate. Depend on a narrower crate instead, or accept the growth \
                 explicitly by re-baselining."
            ));
        }
    }

    // Dev-only upward edges are worth eyes but never enforcement.
    let dev_edges_up = edges.iter().filter(|e| e.kind == DepKind::Dev).count();

    eprintln!(
        "layer-gate: {} members, {} internal edges ({} dev, exempt) vs {} layers; \
         fan-in ratchet over {} crates",
        names.len(),
        edges.len(),
        dev_edges_up,
        map.layers.len(),
        baseline.len()
    );
    for v in &violations {
        eprintln!("  ✗ {}", v.describe());
    }
    for f in &fan_in_fails {
        eprintln!("  ✗ {f}");
    }
    if violations.is_empty() && fan_in_fails.is_empty() {
        eprintln!(
            "  ✓ every crate assigned, every edge points down or sideways, fan-in within caps"
        );
        0
    } else {
        eprintln!();
        eprintln!(
            "layer-gate FAILED ({} layer violations, {} fan-in). The layer map is the \
             dependency-direction contract — quality/ARCH_LAYERS.toml (ARCH_PRINCIPLES §8).",
            violations.len(),
            fan_in_fails.len()
        );
        eprintln!(
            "Layer/forbid violations: fix the edge or add a [[exception]] with a reason \
             (a reviewable policy diff). Stale exceptions: delete the entry."
        );
        eprintln!("{}", common::fix_footer("layer-gate"));
        1
    }
}

/// Distinct-dependent count per crate over Normal+Build edges (a dev-dep
/// can't leak into a shipped artifact, so it doesn't count as fan-in).
fn compute_fan_in(edges: &[DepEdge]) -> BTreeMap<String, usize> {
    let mut dependents: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for e in edges {
        if e.kind == DepKind::Dev {
            continue;
        }
        dependents.entry(&e.to).or_default().insert(&e.from);
    }
    dependents
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.len()))
        .collect()
}

fn write_fan_in(path: &std::path::Path, map: &BTreeMap<String, usize>) -> Result<(), String> {
    common::write_count_map(
        path,
        "layer-gate",
        "crate fan-in caps (distinct dependent workspace crates, normal+build deps)",
        map,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(from: &str, to: &str, kind: DepKind) -> DepEdge {
        DepEdge {
            from: from.into(),
            to: to.into(),
            kind,
        }
    }

    #[test]
    fn fan_in_counts_distinct_dependents_and_ignores_dev() {
        let edges = vec![
            edge("a", "core", DepKind::Normal),
            edge("b", "core", DepKind::Normal),
            edge("b", "core", DepKind::Normal), // duplicate edge — still one dependent
            edge("c", "core", DepKind::Dev),    // dev — not fan-in
            edge("a", "other", DepKind::Build), // build — counts
        ];
        let f = compute_fan_in(&edges);
        assert_eq!(f.get("core"), Some(&2));
        assert_eq!(f.get("other"), Some(&1));
    }
}
