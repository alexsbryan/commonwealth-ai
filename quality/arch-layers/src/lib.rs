// SPDX-License-Identifier: AGPL-3.0-or-later
//! Schema + evaluator for `quality/ARCH_LAYERS.toml` — the declared layer map.
//!
//! The layer map is the workspace's dependency-direction contract: layers are
//! ordered bottom → top, a crate may depend only on crates in the same or a
//! lower layer, `[[forbid]]` expresses cross-family rules that ordering can't,
//! and `[[exception]]` grandfathers today's known violations as a reviewable
//! burn-down list (adding one requires editing the policy file in the PR).
//!
//! `backstage` names the quality controls, which sit OUTSIDE the ordered stack
//! rather than on top of it: they may observe every layer, and nothing may
//! depend on them. See [`LayerMap::backstage`] for the rule and the one thing
//! it cannot enforce.
//!
//! `[[package]]` names an EXTRACTABLE package — a crate set that must stay
//! liftable out of the monorepo against nothing but the `[[package_leaf]]`
//! set. Layers govern direction for the whole workspace; a package pins an
//! exact allowlist for a named subset, which is a different question: an edge
//! can point DOWN the stack (layer-legal) and still leave the package's
//! closure. See [`evaluate_packages`].
//!
//! Three consumers, one parser:
//! - `xtask layer-gate` feeds Cargo-DECLARED dependency edges (deterministic,
//!   runs in CI without a daemon).
//! - `xtask boundary-gate` feeds the same edges to [`evaluate_packages`], and
//!   adds the two filesystem rules a manifest cannot express (no `build.rs`,
//!   no crate-escaping `include_str!`).
//! - the code-intel `arch_report` feeds SCIP-OBSERVED symbol-reference edges
//!   (catches coupling that re-exports hide from Cargo).
//!
//! They call [`evaluate`] and [`evaluate_packages`]; the meaning of the
//! policy file lives here and only here.

mod packages;
mod violations;
pub use packages::{
    evaluate_packages, missing_package_crates, Package, PackageLeaf, SHARED_LEAVES_SCOPE,
};
pub use violations::Violation;

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// Highest `schema_version` this evaluator understands. A map declaring a
/// newer version fails loudly instead of being half-interpreted.
///
/// v2 added `backstage`. The bump is the load-bearing half of that feature: a
/// build that predates it would parse a v2 map, silently ignore the unknown
/// key, and report a clean bill of health on a map whose central rule it never
/// evaluated. Refusing the map is the only honest answer (ARCH §18.3 — absence
/// is reported, never defaulted).
///
/// v3 added `[[package]]` / `[[package_leaf]]`, moving the package boundary
/// out of `boundary_gate.rs`'s Rust consts and into the policy file beside the
/// layer map. Same argument for the bump, one rung sharper: an old build meets
/// a v3 map, ignores every package, and prints the same "boundary-gate: clean"
/// it prints when the packages are genuinely clean.
pub const MAX_SCHEMA_VERSION: u32 = 3;

// ── Schema ────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct LayerMap {
    pub schema_version: u32,
    /// Ordered bottom → top.
    #[serde(default, rename = "layer")]
    pub layers: Vec<Layer>,
    #[serde(default, rename = "forbid")]
    pub forbids: Vec<Forbid>,
    #[serde(default, rename = "exception")]
    pub exceptions: Vec<Exception>,
    /// The extractable packages. Each is a hand-curated closure that must
    /// stay liftable against `package_leaves` alone — see [`Package`].
    #[serde(default, rename = "package")]
    pub packages: Vec<Package>,
    /// The shared contract leaves every package may reach. GLOBAL, not
    /// per-package: widening one widens every package's contract surface at
    /// once, which is why the list is short and each entry is pinned by hand.
    #[serde(default, rename = "package_leaf")]
    pub package_leaves: Vec<PackageLeaf>,
    /// The quality controls — eval, benches, judges, gates, harnesses. Crate
    /// name patterns (`*` allowed), declared ONCE here and nowhere else.
    ///
    /// These sit outside the ordered stack, not on top of it. The rule is
    /// one-way: back-of-house may observe every layer, and nothing may depend
    /// on it. A bench you cannot ship without is not a bench.
    ///
    /// "Nothing may depend on it" is mechanical, not a matter of taste. The
    /// test is *does the product ship without it?*, and Cargo already answers
    /// it: a product crate may reach a back-of-house crate only through an
    /// `optional = true` dependency the default feature set does not turn on
    /// ([`DepEdge::optional`]). An unconditional edge means the shipped
    /// artifact carries its own instrument, which is the single thing this
    /// rule exists to prevent.
    ///
    /// WHAT THIS CANNOT ENFORCE. The unit is the CRATE. Where quality-control
    /// code shares a crate with product code, this rule can only speak about
    /// the crate's dependencies as a whole — it cannot stop one module from
    /// naming a back-of-house type while its neighbours stay clean, and Cargo
    /// still links the back-of-house crate into the product binary either way.
    /// Any such crate needs an `[[exception]]` saying so; the exception is the
    /// honest record that the boundary is drawn in the wrong place, not a
    /// waiver.
    #[serde(default)]
    pub backstage: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Layer {
    pub name: String,
    /// Crate-name patterns (`*` wildcards allowed). Every workspace member
    /// must match exactly one layer — unmatched and ambiguous are both errors.
    pub crates: Vec<String>,
}

/// A rule layer ordering can't express (e.g. "commonwealth-* must not reach
/// sovereign-* even where the layers would allow it").
#[derive(Debug, Deserialize)]
pub struct Forbid {
    pub from: String,
    pub to: String,
    /// Crate names (or patterns) exempt from this rule's `to` side.
    #[serde(default)]
    pub except: Vec<String>,
    pub reason: String,
}

/// A grandfathered violation — tolerated, counted, expected to disappear.
/// A stale entry (no longer matching any live edge) is itself a failure:
/// the PR that removes the last offending edge must also delete its entry.
#[derive(Debug, Deserialize)]
pub struct Exception {
    pub from: String,
    pub to: String,
    pub reason: String,
    #[serde(default)]
    pub tracking: Option<String>,
    /// Scopes this entry to a PACKAGE boundary instead of the layer map.
    /// `Some("code-intel")` grandfathers an edge out of that package's
    /// closure; `Some("shared-leaves")` grandfathers one out of a leaf's
    /// budget; `None` is a layer/forbid exception as before.
    ///
    /// This is what lets a package be declared while it is still dirty: the
    /// first run's failure list becomes entries here, new edges fail from day
    /// one, and the count only goes down.
    #[serde(default)]
    pub package: Option<String>,
}

// ── Input edges ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    Normal,
    Build,
    /// Dev edges are reported by callers but never enforced — a dev-dep
    /// cannot leak into a shipped artifact.
    Dev,
}

#[derive(Debug, Clone)]
pub struct DepEdge {
    pub from: String,
    pub to: String,
    pub kind: DepKind,
    /// True when this dependency is absent from the crate's DEFAULT build —
    /// declared `optional = true` and not switched on by the transitive
    /// closure of the `default` feature.
    ///
    /// Layer DIRECTION ignores this (the layer map governs the all-features
    /// union: an edge behind a feature is still a declared edge). It matters
    /// only to the `backstage` rule, where it is the mechanical form of "does
    /// the product ship without it?" — see [`LayerMap::backstage`].
    ///
    /// Feeds that cannot see Cargo features (SCIP symbol references) set this
    /// `false`: an observed reference is by definition in some build, and
    /// claiming otherwise would be a guess.
    pub optional: bool,
}

// ── Parsing ───────────────────────────────────────────────────────────────────

pub fn parse(toml_text: &str) -> Result<LayerMap, String> {
    let map: LayerMap =
        toml::from_str(toml_text).map_err(|e| format!("ARCH_LAYERS.toml parse error: {e}"))?;
    if map.schema_version > MAX_SCHEMA_VERSION {
        return Err(format!(
            "ARCH_LAYERS.toml declares schema_version {} but this build \
             understands at most {MAX_SCHEMA_VERSION} — rebuild the consumers \
             before raising the version",
            map.schema_version
        ));
    }
    if map.layers.is_empty() {
        return Err("ARCH_LAYERS.toml declares no [[layer]] entries".to_string());
    }
    // The mirror image of the version check above, and the one that actually
    // bites: NEW code meeting an OLD (or truncated) map. `backstage` is
    // `#[serde(default)]`, so an absent key deserializes to an empty vec — and
    // an empty back-of-house set makes the one-way rule VACUOUSLY TRUE. Every
    // edge passes, layer-gate prints a clean bill of health, and the green
    // means "the rule was never configured" while reading exactly like "the
    // rule found nothing". Those two must never be spelled the same way
    // (ARCH §18.3 — absence is reported, never defaulted).
    //
    // v1 maps legitimately have no `backstage` key: the concept did not exist.
    // That is the ONE case where an empty set is a real answer, and the
    // version is what distinguishes it.
    if map.schema_version >= 2 && map.backstage.is_empty() {
        return Err(
            "ARCH_LAYERS.toml declares schema_version >= 2 but no `backstage` \
             crates. v2 exists to carry the one-way back-of-house rule, and an \
             empty list would make that rule vacuous — every edge would pass \
             and the gate would report success on a rule it never evaluated. \
             Declare the list, or drop back to schema_version = 1 if the rule \
             is genuinely not wanted here."
                .to_string(),
        );
    }
    packages::validate(&map)?;

    Ok(map)
}

/// `*`-wildcard match (any number of `*`, each matching any substring).
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    fn inner(p: &[u8], n: &[u8]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some(b'*') => (0..=n.len()).any(|i| inner(&p[1..], &n[i..])),
            Some(c) => n.first() == Some(c) && inner(&p[1..], &n[1..]),
        }
    }
    inner(pattern.as_bytes(), name.as_bytes())
}

// ── Evaluation ────────────────────────────────────────────────────────────────

/// Assign every crate to a layer and check every edge. `crates` must be the
/// COMPLETE workspace-member name set (totality is part of the contract);
/// `edges` are internal (member → member) dependency edges of any kind.
pub fn evaluate(map: &LayerMap, crates: &BTreeSet<String>, edges: &[DepEdge]) -> Vec<Violation> {
    let mut violations = Vec::new();

    // Layer assignment — total and unambiguous.
    let mut assignment: BTreeMap<&str, usize> = BTreeMap::new();
    for name in crates {
        let matches: Vec<usize> = map
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.crates.iter().any(|p| wildcard_match(p, name)))
            .map(|(i, _)| i)
            .collect();
        match matches.as_slice() {
            [] => violations.push(Violation::UnassignedCrate { name: name.clone() }),
            [i] => {
                assignment.insert(name.as_str(), *i);
            }
            many => violations.push(Violation::AmbiguousCrate {
                name: name.clone(),
                layers: many.iter().map(|&i| map.layers[i].name.clone()).collect(),
            }),
        }
    }

    // Edge checks. Dev edges are never enforced (see DepKind::Dev).
    //
    // Exceptions carrying `package = "…"` belong to `evaluate_packages`: they
    // neither suppress a layer violation nor count as stale here. Until
    // 2026-09-03 this loop read every entry, so the first package-scoped
    // exception ever declared (corpus-mcp → corpus-engine, layer-legal) was
    // reported "no longer matches any edge" by layer-gate while boundary-gate
    // was using it — two gates, opposite verdicts, one row.
    let mut used_exceptions: BTreeSet<(String, String)> = BTreeSet::new();
    let mut excepted = |from: &str, to: &str| -> bool {
        for e in map.exceptions.iter().filter(|e| e.package.is_none()) {
            if e.from == from && e.to == to {
                used_exceptions.insert((e.from.clone(), e.to.clone()));
                return true;
            }
        }
        false
    };

    let is_backstage =
        |name: &str| -> bool { map.backstage.iter().any(|p| wildcard_match(p, name)) };

    for edge in edges {
        if edge.kind == DepKind::Dev {
            continue;
        }
        // The one-way back-of-house rule, checked first: it is the sharpest
        // statement in the map and the only one that survives a crate sitting
        // in the "right" layer. An edge INTO backstage is legal only from
        // backstage itself (observing anything is the whole point) or when the
        // default build does not carry it.
        if is_backstage(&edge.to) && !is_backstage(&edge.from) && !edge.optional {
            if !excepted(&edge.from, &edge.to) {
                violations.push(Violation::BackstageEdge {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    kind: edge.kind,
                });
            }
            continue;
        }
        // [[forbid]] rules first — they're the sharper statement.
        let forbidden = map.forbids.iter().find(|f| {
            wildcard_match(&f.from, &edge.from)
                && wildcard_match(&f.to, &edge.to)
                && !f.except.iter().any(|x| wildcard_match(x, &edge.to))
        });
        if let Some(rule) = forbidden {
            if !excepted(&edge.from, &edge.to) {
                violations.push(Violation::ForbiddenEdge {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    reason: rule.reason.clone(),
                });
            }
            continue; // don't double-report as an upward edge
        }
        // Layer direction: to-layer must be <= from-layer.
        if let (Some(&fi), Some(&ti)) = (
            assignment.get(edge.from.as_str()),
            assignment.get(edge.to.as_str()),
        ) {
            if ti > fi && !excepted(&edge.from, &edge.to) {
                violations.push(Violation::UpwardEdge {
                    from: edge.from.clone(),
                    from_layer: map.layers[fi].name.clone(),
                    to: edge.to.clone(),
                    to_layer: map.layers[ti].name.clone(),
                    kind: edge.kind,
                });
            }
        }
    }

    // Exceptions that suppressed nothing are debt already paid — flag them.
    // Package-scoped ones answer to `evaluate_packages` (StalePackageException).
    for e in map.exceptions.iter().filter(|e| e.package.is_none()) {
        if !used_exceptions.contains(&(e.from.clone(), e.to.clone())) {
            violations.push(Violation::StaleException {
                from: e.from.clone(),
                to: e.to.clone(),
            });
        }
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(from: &str, to: &str, kind: DepKind) -> DepEdge {
        DepEdge {
            from: from.into(),
            to: to.into(),
            kind,
            optional: false,
        }
    }

    /// An edge the DEFAULT build does not carry — `optional = true`, not
    /// reachable from `default`.
    fn opt_edge(from: &str, to: &str) -> DepEdge {
        DepEdge {
            optional: true,
            ..edge(from, to, DepKind::Normal)
        }
    }

    const MAP: &str = r#"
schema_version = 1

[[layer]]
name = "wire"
crates = ["oicp-*"]

[[layer]]
name = "runtime"
crates = ["sovereign-core", "sovereign-store"]

[[layer]]
name = "hosts"
crates = ["sovereign-cli*", "commonwealth-daemon"]

[[forbid]]
from = "commonwealth-*"
to = "sovereign-*"
except = ["sovereign-contracts"]
reason = "families meet only at the contracts seam"

[[exception]]
from = "sovereign-core"
to = "sovereign-cli"
reason = "grandfathered upward edge"
tracking = "R99"
"#;

    fn crates() -> BTreeSet<String> {
        [
            "oicp-types",
            "sovereign-core",
            "sovereign-store",
            "sovereign-cli",
            "commonwealth-daemon",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn wildcard_semantics() {
        assert!(wildcard_match("oicp-*", "oicp-types"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("sovereign-cli*", "sovereign-cli-dev"));
        assert!(wildcard_match("sovereign-cli*", "sovereign-cli"));
        assert!(!wildcard_match("oicp-*", "sovereign-core"));
        assert!(!wildcard_match("corpus-engine", "corpus-engine-scip"));
    }

    #[test]
    fn newer_schema_version_is_rejected() {
        let err =
            parse("schema_version = 99\n[[layer]]\nname = \"x\"\ncrates = [\"*\"]\n").unwrap_err();
        assert!(err.contains("schema_version 99"));
    }

    #[test]
    fn downward_and_same_layer_edges_pass_upward_fails() {
        let map = parse(MAP).unwrap();
        let edges = vec![
            edge("sovereign-cli", "sovereign-core", DepKind::Normal), // down: ok
            edge("sovereign-core", "sovereign-store", DepKind::Normal), // same: ok
            edge("sovereign-store", "sovereign-cli", DepKind::Normal), // UP: fails
        ];
        let v = evaluate(&map, &crates(), &edges);
        // One upward edge + the stale (unused) exception in MAP.
        assert_eq!(
            v.iter()
                .filter(|x| matches!(x, Violation::UpwardEdge { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn dev_edges_are_never_enforced() {
        let map = parse(MAP).unwrap();
        let edges = vec![edge("sovereign-store", "sovereign-cli", DepKind::Dev)];
        let v = evaluate(&map, &crates(), &edges);
        assert!(!v.iter().any(|x| matches!(x, Violation::UpwardEdge { .. })));
    }

    #[test]
    fn forbid_rule_fires_across_families_and_respects_except() {
        let map = parse(MAP).unwrap();
        let edges = vec![edge(
            "commonwealth-daemon",
            "sovereign-core",
            DepKind::Normal,
        )];
        let v = evaluate(&map, &crates(), &edges);
        assert!(v.iter().any(|x| matches!(
            x,
            Violation::ForbiddenEdge { from, to, .. }
                if from == "commonwealth-daemon" && to == "sovereign-core"
        )));
        // The except list would have allowed sovereign-contracts.
        let edges = vec![edge(
            "commonwealth-daemon",
            "sovereign-contracts",
            DepKind::Normal,
        )];
        let crates2: BTreeSet<String> = crates()
            .into_iter()
            .chain(["sovereign-contracts".to_string()])
            .collect();
        // sovereign-contracts is unassigned in MAP — expect only that finding,
        // no ForbiddenEdge.
        let v = evaluate(&map, &crates2, &edges);
        assert!(!v
            .iter()
            .any(|x| matches!(x, Violation::ForbiddenEdge { .. })));
    }

    #[test]
    fn exception_suppresses_and_stale_exception_reports() {
        let map = parse(MAP).unwrap();
        // The exception (sovereign-core → sovereign-cli) is USED here.
        let edges = vec![edge("sovereign-core", "sovereign-cli", DepKind::Normal)];
        let v = evaluate(&map, &crates(), &edges);
        assert!(!v.iter().any(|x| matches!(x, Violation::UpwardEdge { .. })));
        assert!(!v
            .iter()
            .any(|x| matches!(x, Violation::StaleException { .. })));
        // With no edges, the same exception is stale.
        let v = evaluate(&map, &crates(), &[]);
        assert!(v
            .iter()
            .any(|x| matches!(x, Violation::StaleException { .. })));
    }

    #[test]
    fn totality_unassigned_and_ambiguous() {
        let map = parse(MAP).unwrap();
        let mut names = crates();
        names.insert("mystery-crate".to_string());
        let v = evaluate(&map, &names, &[]);
        assert!(v.iter().any(|x| matches!(
            x,
            Violation::UnassignedCrate { name } if name == "mystery-crate"
        )));

        let overlapping = r#"
schema_version = 1
[[layer]]
name = "a"
crates = ["sovereign-*"]
[[layer]]
name = "b"
crates = ["*-core"]
"#;
        let map = parse(overlapping).unwrap();
        let names: BTreeSet<String> = ["sovereign-core".to_string()].into_iter().collect();
        let v = evaluate(&map, &names, &[]);
        assert!(v
            .iter()
            .any(|x| matches!(x, Violation::AmbiguousCrate { .. })));
    }

    // ── the one-way back-of-house rule ────────────────────────────────────

    /// Same layer for every crate, so NOTHING here can fail on direction —
    /// any violation these tests see is the `backstage` rule and nothing else.
    const BACKSTAGE_MAP: &str = r#"
schema_version = 2
backstage = ["sovereign-eval", "xtask", "*-bench"]

[[layer]]
name = "everything"
crates = ["*"]
"#;

    fn backstage_crates() -> BTreeSet<String> {
        [
            "sovereign-eval",
            "xtask",
            "agent-bench",
            "sovereign-cli",
            "sovereign-core",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn product_depending_on_backstage_in_the_default_build_is_a_violation() {
        let map = parse(BACKSTAGE_MAP).unwrap();
        let v = evaluate(
            &map,
            &backstage_crates(),
            &[edge("sovereign-cli", "sovereign-eval", DepKind::Normal)],
        );
        assert_eq!(
            v.len(),
            1,
            "expected exactly the backstage violation: {v:?}"
        );
        assert!(matches!(
            &v[0],
            Violation::BackstageEdge { from, to, .. }
                if from == "sovereign-cli" && to == "sovereign-eval"
        ));
        // The gate must NAME its own limit where it speaks — a reader who sees
        // only this line must not walk away thinking the crate boundary is
        // tighter than it is.
        let msg = v[0].describe();
        assert!(
            msg.contains("still links"),
            "gate must disclose that Cargo still links the backstage crate: {msg}"
        );
        assert!(
            msg.contains("CRATE"),
            "gate must disclose its unit is the crate: {msg}"
        );
    }

    #[test]
    fn backstage_may_depend_on_backstage() {
        let map = parse(BACKSTAGE_MAP).unwrap();
        let v = evaluate(
            &map,
            &backstage_crates(),
            &[
                edge("xtask", "sovereign-eval", DepKind::Normal),
                edge("agent-bench", "xtask", DepKind::Normal),
            ],
        );
        assert!(v.is_empty(), "observing anything is legal: {v:?}");
    }

    #[test]
    fn a_dep_the_default_build_does_not_carry_is_not_a_violation() {
        let map = parse(BACKSTAGE_MAP).unwrap();
        let v = evaluate(
            &map,
            &backstage_crates(),
            &[opt_edge("sovereign-cli", "sovereign-eval")],
        );
        assert!(
            v.is_empty(),
            "the product ships without it — that IS the test: {v:?}"
        );
    }

    #[test]
    fn a_backstage_edge_can_be_grandfathered_and_the_entry_goes_stale_when_fixed() {
        let with_exception = r#"
schema_version = 2
backstage = ["sovereign-eval"]
[[layer]]
name = "everything"
crates = ["*"]
[[exception]]
from = "sovereign-cli"
to = "sovereign-eval"
reason = "one module in a mixed crate; the boundary is drawn in the wrong place"
"#;
        let map = parse(with_exception).unwrap();
        let live = edge("sovereign-cli", "sovereign-eval", DepKind::Normal);
        assert!(
            evaluate(&map, &backstage_crates(), std::slice::from_ref(&live)).is_empty(),
            "an excepted backstage edge is tolerated"
        );
        // …and once the edge is gone the entry must fail as stale, so the win
        // is forced to be recorded rather than quietly accruing dead policy.
        let v = evaluate(&map, &backstage_crates(), &[]);
        assert!(
            v.iter()
                .any(|x| matches!(x, Violation::StaleException { .. })),
            "a fixed backstage violation must retire its own exception: {v:?}"
        );
    }

    #[test]
    fn dev_dependencies_on_backstage_are_never_enforced() {
        let map = parse(BACKSTAGE_MAP).unwrap();
        let v = evaluate(
            &map,
            &backstage_crates(),
            &[edge("sovereign-cli", "sovereign-eval", DepKind::Dev)],
        );
        assert!(
            v.is_empty(),
            "a dev-dep cannot reach a shipped artifact: {v:?}"
        );
    }

    #[test]
    fn a_v2_map_with_no_backstage_list_is_refused_not_silently_vacuous() {
        // The mirror-image hole: new code, old/truncated map. An empty set
        // would make the rule vacuously true and the gate green on a rule it
        // never ran.
        let vacuous = r#"
schema_version = 2
[[layer]]
name = "everything"
crates = ["*"]
"#;
        let err = parse(vacuous).expect_err("a v2 map with no backstage must not parse");
        assert!(err.contains("backstage"), "{err}");
        assert!(err.contains("vacuous"), "the error must name WHY: {err}");
    }

    #[test]
    fn a_v1_map_legitimately_has_no_backstage_rule() {
        // The one case where an empty set is a real answer — the concept did
        // not exist in v1. It must parse, and it must not evaluate the rule.
        let m = parse(MAP).expect("v1 maps keep parsing");
        assert_eq!(m.schema_version, 1);
        assert!(m.backstage.is_empty());
    }

    #[test]
    fn a_map_declaring_a_newer_schema_is_refused_not_half_interpreted() {
        let future = r#"
schema_version = 99
backstage = ["sovereign-eval"]
[[layer]]
name = "everything"
crates = ["*"]
"#;
        let err = parse(future).expect_err("a v99 map must not parse");
        assert!(err.contains("schema_version"), "{err}");
    }

    #[test]
    fn an_empty_backstage_list_declares_no_rule() {
        // v1 maps have no `backstage` key at all. They must keep evaluating
        // exactly as before — the feature is opt-in by declaration.
        let map = parse(MAP).unwrap();
        assert!(map.backstage.is_empty());
        let v = evaluate(
            &map,
            &crates(),
            &[edge("sovereign-cli", "sovereign-store", DepKind::Normal)],
        );
        assert!(!v
            .iter()
            .any(|x| matches!(x, Violation::BackstageEdge { .. })));
    }
}
