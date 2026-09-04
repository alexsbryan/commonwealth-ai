// SPDX-License-Identifier: AGPL-3.0-or-later
//! The extractable-PACKAGE half of the layer map — `[[package]]` and
//! `[[package_leaf]]`.
//!
//! Split out of `lib.rs` when the two halves together crossed ARCH §3.1's
//! 1200-line ceiling. The division is by QUESTION, not by size: `lib.rs`
//! answers "may this edge exist anywhere in the workspace" (direction, over
//! ordered layers); this module answers "is this edge inside MY closure"
//! (membership, over a hand-curated crate set). An edge can be layer-legal and
//! still take a package out of liftable range, which is why the two are
//! separate rules rather than one tighter one.
//!
//! [`Violation`] stays in `lib.rs` and carries both halves' variants: callers
//! render one list, and one enum is one decider (ARCH §10.6).

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

use crate::{wildcard_match, DepEdge, LayerMap, Violation};

/// An extractable package — the crate set a third party could lift out of the
/// monorepo and build against nothing but the shared leaves.
///
/// The overlap with layers is deliberate and partial. Layers answer "may this
/// edge exist at all, anywhere in the workspace"; a package answers "is this
/// edge inside MY closure". An edge can be layer-legal and still break a
/// package, which is the whole reason this is a separate rule and not a
/// tighter layer.
///
/// Membership is exact names, never patterns. A package is a curated closure,
/// and a `code-*` wildcard would silently admit the next crate someone names
/// that way — the opposite of a pinned contract.
#[derive(Debug, Deserialize)]
pub struct Package {
    pub name: String,
    pub crates: Vec<String>,
    /// Repo-relative path to the package's contract document. Printed with
    /// every violation so the reader lands on the rules, not just the edge.
    pub doc: String,
}

/// A shared contract leaf, with its own tight internal-dependency budget.
///
/// What qualifies a leaf is its CLOSURE, not its directory: `corpus-engine-
/// sections` crosses because its whole third-party budget is `regex` +
/// `tracing`, while `corpus-engine` drags LanceDB, Tantivy and rusqlite. Most
/// budgets here are empty, and empty is usually the contract rather than a
/// coincidence — a kernel that may name a product crate is not a kernel.
#[derive(Debug, Deserialize)]
pub struct PackageLeaf {
    pub name: String,
    /// Internal crates this leaf may itself depend on.
    #[serde(default)]
    pub allow: Vec<String>,
}

/// The pseudo-package an `[[exception]]` names to grandfather a SHARED LEAF's
/// budget, which belongs to no single package.
pub const SHARED_LEAVES_SCOPE: &str = "shared-leaves";

/// Validate the package half of a parsed map. Called from [`crate::parse`] so
/// a malformed package declaration is refused at the same moment a malformed
/// layer one is.
pub(crate) fn validate(map: &LayerMap) -> Result<(), String> {
    // Same argument as `backstage` in `lib.rs`, one rung out. `packages` is
    // `#[serde(default)]`, so a v3 map that lost its package blocks (a bad
    // merge, a truncated file) deserializes to an empty vec and every package
    // rule becomes VACUOUSLY TRUE — boundary-gate would print a clean bill of
    // health on a contract it never read. v1/v2 maps legitimately have none.
    if map.schema_version >= 3 && map.packages.is_empty() {
        return Err(
            "ARCH_LAYERS.toml declares schema_version >= 3 but no [[package]] \
             entries. v3 exists to carry the extractable-package boundaries, \
             and an empty list would make boundary-gate vacuous — it would \
             check nothing and report success. Declare the packages, or drop \
             back to schema_version = 2 if they are genuinely not wanted here."
                .to_string(),
        );
    }

    // Package membership must be unambiguous: one crate, one owner. Two
    // packages claiming a crate makes its budget depend on iteration order,
    // which is the "one decider, one name" rule (ARCH §10.6) applied to the
    // policy file itself.
    let mut owner: BTreeMap<&str, &str> = BTreeMap::new();
    let mut names: BTreeSet<&str> = BTreeSet::new();
    for pkg in &map.packages {
        if !names.insert(pkg.name.as_str()) {
            return Err(format!(
                "ARCH_LAYERS.toml declares two [[package]] blocks named `{}`",
                pkg.name
            ));
        }
        for c in &pkg.crates {
            if let Some(prev) = owner.insert(c.as_str(), pkg.name.as_str()) {
                return Err(format!(
                    "crate `{c}` is claimed by two packages (`{prev}` and \
                     `{}`) — a crate belongs to exactly one closure",
                    pkg.name
                ));
            }
        }
    }

    let mut leaves: BTreeSet<&str> = BTreeSet::new();
    for leaf in &map.package_leaves {
        if !leaves.insert(leaf.name.as_str()) {
            return Err(format!(
                "ARCH_LAYERS.toml declares two [[package_leaf]] blocks named `{}`",
                leaf.name
            ));
        }
        // A crate cannot be both a package member and a shared leaf: the two
        // carry different budgets, so the pair would silently pick one.
        if let Some(pkg) = owner.get(leaf.name.as_str()) {
            return Err(format!(
                "crate `{}` is both a [[package_leaf]] and a member of \
                 package `{pkg}` — it must be one or the other",
                leaf.name
            ));
        }
    }

    // An exception scoped to a package nobody declared silently protects
    // nothing, and reads in review as though it does.
    for exc in &map.exceptions {
        if let Some(scope) = exc.package.as_deref() {
            if scope != SHARED_LEAVES_SCOPE && !names.contains(scope) {
                return Err(format!(
                    "[[exception]] {} → {} names package `{scope}`, which no \
                     [[package]] block declares (expected one of: {}, or \
                     `{SHARED_LEAVES_SCOPE}`)",
                    exc.from,
                    exc.to,
                    names.iter().copied().collect::<Vec<_>>().join(", ")
                ));
            }
        }
    }

    Ok(())
}

/// Package crates the map declares that the workspace does not (yet) contain.
///
/// Not a violation: packages are declared BEFORE the extraction finishes, and
/// a crate that has not arrived cannot be checked. It is reported rather than
/// skipped in silence, because the same shape covers a typo — and a typo'd
/// crate name is a rule that quietly governs nothing.
pub fn missing_package_crates(map: &LayerMap, crates: &BTreeSet<String>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for pkg in &map.packages {
        for c in &pkg.crates {
            if !crates.contains(c) {
                out.push((pkg.name.clone(), c.clone()));
            }
        }
    }
    for leaf in &map.package_leaves {
        if !crates.contains(&leaf.name) {
            out.push((SHARED_LEAVES_SCOPE.to_string(), leaf.name.clone()));
        }
    }
    out
}

/// Check every package crate and shared leaf against its pinned budget.
///
/// Two differences from [`crate::evaluate`], both deliberate:
///
/// 1. **Dev edges are enforced here and ignored there.** The layer map governs
///    what a SHIPPED artifact carries, and a dev-dep cannot leak into one. A
///    package governs what a third party LIFTS, and they carry its tests.
/// 2. **Direction is irrelevant.** A package edge pointing down the layer
///    stack is still a violation if the target is outside the closure. "Below
///    me" and "inside my closure" are different questions.
///
/// The filesystem half of the contract — no `build.rs`, no crate-escaping
/// `include_str!` — is not expressible over dependency edges and lives in
/// `xtask boundary-gate`.
pub fn evaluate_packages(map: &LayerMap, edges: &[DepEdge]) -> Vec<Violation> {
    let mut violations = Vec::new();
    let leaf_names: BTreeSet<&str> = map.package_leaves.iter().map(|l| l.name.as_str()).collect();

    let mut owner: BTreeMap<&str, &Package> = BTreeMap::new();
    for pkg in &map.packages {
        for c in &pkg.crates {
            owner.insert(c.as_str(), pkg);
        }
    }

    // Which exceptions actually did work. An entry that protected nothing is
    // reported so the burn-down cannot stall behind stale grandfathering.
    let mut used = vec![false; map.exceptions.len()];

    for edge in edges {
        let (scope, doc, allowed): (&str, &str, BTreeSet<&str>) =
            if let Some(pkg) = owner.get(edge.from.as_str()) {
                let mut a: BTreeSet<&str> = pkg.crates.iter().map(String::as_str).collect();
                a.extend(leaf_names.iter().copied());
                (pkg.name.as_str(), pkg.doc.as_str(), a)
            } else if let Some(leaf) = map.package_leaves.iter().find(|l| l.name == edge.from) {
                (
                    SHARED_LEAVES_SCOPE,
                    "the [[package_leaf]] budgets in quality/ARCH_LAYERS.toml",
                    leaf.allow.iter().map(String::as_str).collect(),
                )
            } else {
                // Not governed by any package — the layer map owns this edge.
                continue;
            };

        if allowed.contains(edge.to.as_str()) {
            continue;
        }

        let excepted = map.exceptions.iter().enumerate().any(|(i, exc)| {
            let hit = exc.package.as_deref() == Some(scope)
                && wildcard_match(&exc.from, &edge.from)
                && wildcard_match(&exc.to, &edge.to);
            if hit {
                used[i] = true;
            }
            hit
        });
        if excepted {
            continue;
        }

        violations.push(Violation::PackageEdge {
            package: scope.to_string(),
            doc: doc.to_string(),
            from: edge.from.clone(),
            to: edge.to.clone(),
            kind: edge.kind,
        });
    }

    for (i, exc) in map.exceptions.iter().enumerate() {
        if let Some(scope) = exc.package.as_deref() {
            if !used[i] {
                violations.push(Violation::StalePackageException {
                    package: scope.to_string(),
                    from: exc.from.clone(),
                    to: exc.to.clone(),
                });
            }
        }
    }

    violations
}

#[cfg(test)]
mod package_tests {
    use super::*;
    use crate::{parse, DepEdge, DepKind, Violation};

    const MAP: &str = r#"
schema_version = 3
backstage = ["xtask"]
[[layer]]
name = "all"
crates = ["*"]
[[package_leaf]]
name = "oicp-types"
allow = []
[[package_leaf]]
name = "sovereign-contracts"
allow = ["oicp-types"]
[[package]]
name = "demo"
doc = "studio/BOUNDARY.md"
crates = ["pkg-a", "pkg-b"]
"#;

    fn edge(from: &str, to: &str) -> DepEdge {
        DepEdge {
            from: from.to_string(),
            to: to.to_string(),
            kind: DepKind::Normal,
            optional: false,
        }
    }

    /// The guard that stops the whole package half from going quietly
    /// vacuous. An empty `packages` list checks nothing and reports success,
    /// which reads exactly like a clean bill of health (ARCH §18.3).
    #[test]
    fn v3_map_without_packages_is_refused() {
        let err = parse(
            "schema_version = 3\nbackstage = [\"x\"]\n[[layer]]\nname = \"a\"\ncrates = [\"*\"]\n",
        )
        .unwrap_err();
        assert!(err.contains("no [[package]]"), "{err}");
        // v2 legitimately has none — the version is what distinguishes them.
        assert!(parse(
            "schema_version = 2\nbackstage = [\"x\"]\n[[layer]]\nname = \"a\"\ncrates = [\"*\"]\n"
        )
        .is_ok());
    }

    /// One crate, one owner: two packages claiming a crate would make its
    /// budget depend on iteration order (ARCH §10.6).
    #[test]
    fn a_crate_cannot_belong_to_two_packages() {
        let text = MAP.replace(
            "crates = [\"pkg-a\", \"pkg-b\"]",
            "crates = [\"pkg-a\", \"pkg-b\"]\n[[package]]\nname = \"other\"\ndoc = \"d\"\ncrates = [\"pkg-a\"]",
        );
        let err = parse(&text).unwrap_err();
        assert!(err.contains("claimed by two packages"), "{err}");
    }

    /// The two carry different budgets, so being both would silently pick one.
    #[test]
    fn a_leaf_cannot_also_be_a_package_member() {
        let text = MAP.replace(
            "crates = [\"pkg-a\", \"pkg-b\"]",
            "crates = [\"pkg-a\", \"oicp-types\"]",
        );
        let err = parse(&text).unwrap_err();
        assert!(err.contains("both a [[package_leaf]]"), "{err}");
    }

    /// A package-scoped exception answers to THIS pass and to no other. The
    /// layer pass must neither be suppressed by it nor report it stale —
    /// until 2026-09-03 `evaluate` read every entry, so the first package
    /// exception ever declared (layer-legal, so no layer violation could use
    /// it) was flagged "no longer matches any edge" by layer-gate while
    /// boundary-gate was relying on it.
    #[test]
    fn a_package_scoped_exception_is_invisible_to_the_layer_pass() {
        let text = format!(
            "{MAP}\n[[exception]]\npackage = \"demo\"\nfrom = \"pkg-a\"\nto = \"outside\"\n\
             reason = \"grandfathered for the test\"\ntracking = \"t\"\n"
        );
        let map = parse(&text).unwrap();
        let crates: std::collections::BTreeSet<String> = ["pkg-a", "pkg-b", "outside"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Layer pass, no edges at all: a layer exception would be stale here;
        // a package-scoped one is not the layer pass's to judge.
        let v = crate::evaluate(&map, &crates, &[]);
        assert!(
            !v.iter()
                .any(|x| matches!(x, Violation::StaleException { .. })),
            "layer pass reported a package-scoped exception as stale: {v:?}"
        );

        // Package pass: the same entry does its work (edge grandfathered) …
        let v = evaluate_packages(&map, &[edge("pkg-a", "outside")]);
        assert!(v.is_empty(), "{v:?}");
        // … and is the package pass's own stale row when the edge is gone.
        let v = evaluate_packages(&map, &[]);
        assert!(
            v.iter()
                .any(|x| matches!(x, Violation::StalePackageException { .. })),
            "{v:?}"
        );
    }

    /// An exception scoped to a package nobody declared protects nothing, and
    /// reads in review as though it does.
    #[test]
    fn exception_naming_an_undeclared_package_is_refused() {
        let text = format!(
            "{MAP}\n[[exception]]\npackage = \"typo\"\nfrom = \"pkg-a\"\nto = \"x\"\nreason = \"r\"\n"
        );
        let err = parse(&text).unwrap_err();
        assert!(err.contains("names package `typo`"), "{err}");
    }

    /// The soft-landing path: declare a dirty package, grandfather its edges,
    /// and have the ledger fail once an entry stops protecting anything.
    #[test]
    fn package_exception_grandfathers_then_goes_stale() {
        let dirty = [edge("pkg-a", "sovereign-core")];

        let bare = parse(MAP).unwrap();
        let v = evaluate_packages(&bare, &dirty);
        assert_eq!(v.len(), 1);
        assert!(matches!(v[0], Violation::PackageEdge { .. }));

        let text = format!(
            "{MAP}\n[[exception]]\npackage = \"demo\"\nfrom = \"pkg-a\"\nto = \"sovereign-core\"\nreason = \"r\"\n"
        );
        let excepted = parse(&text).unwrap();
        assert!(
            evaluate_packages(&excepted, &dirty).is_empty(),
            "the grandfathered edge must stop failing"
        );

        // Edge gone, entry left behind — the burn-down must not stall behind
        // stale grandfathering, so this is itself a failure.
        let stale = evaluate_packages(&excepted, &[]);
        assert_eq!(stale.len(), 1);
        assert!(matches!(stale[0], Violation::StalePackageException { .. }));
    }

    /// A leaf's budget is tighter than a package's and is enforced the same
    /// way — widening one widens every package at once.
    #[test]
    fn leaf_budget_is_enforced_and_scoped_to_the_leaf_pseudo_package() {
        let map = parse(MAP).unwrap();
        // Inside the pinned budget.
        assert!(evaluate_packages(&map, &[edge("sovereign-contracts", "oicp-types")]).is_empty());
        // Outside it.
        let v = evaluate_packages(&map, &[edge("sovereign-contracts", "corpus-engine")]);
        assert_eq!(v.len(), 1);
        match &v[0] {
            Violation::PackageEdge { package, .. } => assert_eq!(package, SHARED_LEAVES_SCOPE),
            other => panic!("expected a package edge, got {other:?}"),
        }
        // A pure leaf's budget is empty, so ANY internal dep breaches it.
        assert_eq!(
            evaluate_packages(&map, &[edge("oicp-types", "sovereign-contracts")]).len(),
            1
        );
    }
}
