// SPDX-License-Identifier: AGPL-3.0-or-later
//! boundary-gate — enforces the extractable-PACKAGE boundaries declared in
//! `quality/ARCH_LAYERS.toml`: a package crate may reach only its own package
//! plus the shared `[[package_leaf]]` set, and each leaf has a hand-pinned
//! budget of its own. Kept green, each package stays liftable out of the
//! monorepo against just the OICP contract.
//!
//! Until 2026-09-03 the boundary lived in Rust consts here (`PACKAGE_SET` /
//! `SHARED_LEAVES` / `allowed_leaf_deps`) and could describe exactly one
//! package — the studio one. It is policy, so it moved to the policy file
//! beside the layer map, behind the same parser (`quality/arch-layers`) that
//! layer-gate and the code-intel `arch_report` already share. That is ARCH
//! §10.6 (one decider, one name) and it is what makes N packages a TOML edit
//! rather than a code change.
//!
//! Overlap with layer-gate is deliberate and partial: layer-gate governs
//! DIRECTION for the whole workspace; this gate pins an exact allowlist per
//! declared package, and adds the two rules a dependency edge cannot express —
//! no `build.rs`, no crate-escaping `include_str!`.
//!
//! WHAT THIS GATE CANNOT SEE. Its unit is the CRATE. Where package-shaped code
//! shares a crate with everything else — `sovereign-core`, `sovereign-tools`,
//! `sovereign-cli-llm`, `sovereign-mesh`, which together are ~41% of the
//! workspace — declaring a package says nothing, because there is no edge to
//! check. Containment there is a module rule enforced by a test in the crate
//! itself; `sovereign-cli-llm/src/lib.rs`'s
//! `bench_cmd_is_the_only_module_naming_the_eval_harness` is the worked
//! example, and it is honest about being strictly weaker: Cargo still links
//! the crate either way.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::common;
use crate::manifests;

pub fn run() -> i32 {
    let root = common::repo_root();
    let map_path = root.join("quality/ARCH_LAYERS.toml");

    let map_text = match std::fs::read_to_string(&map_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "error: cannot read {} ({e}).\n  The package boundaries are declared there \
                 — see studio/BOUNDARY.md and docs/CODE_TOOLING_BOUNDARY.md.",
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
    let dir_of: BTreeMap<&str, &str> = members
        .iter()
        .map(|m| (m.name.as_str(), m.dir.as_str()))
        .collect();
    let edges = manifests::internal_dep_edges(&root, &members);

    let mut fails: Vec<String> = Vec::new();

    // Rules 1/2 — the dependency closure. Evaluated by the shared parser so
    // this gate and arch-report cannot drift on what a package MEANS.
    for v in arch_layers::evaluate_packages(&map, &edges) {
        fails.push(v.describe());
    }

    // Rules 3a/3b — the filesystem half, which no manifest can express.
    let mut checked = 0usize;
    for (scope, name) in governed_crates(&map) {
        let Some(rel) = dir_of.get(name) else {
            // Declared but not present yet — reported below, not here. A
            // package is declared BEFORE its extraction finishes.
            continue;
        };
        checked += 1;
        let dir = root.join(rel);

        // A build script is a source-tree reach-in no package boundary
        // survives (see studio B:P0's syn-walk removal).
        if dir.join("build.rs").exists() {
            fails.push(format!(
                "[{scope}] {name}: has a build.rs — package and leaf crates must not \
                 carry one; a third party lifting this crate carries its build scripts"
            ));
        }
        include_escapes(&dir, name, scope, &mut fails);
    }

    // Declared-but-absent crates. Reported rather than skipped in silence:
    // the same shape covers a typo, and a typo'd crate name is a rule that
    // quietly governs nothing (ARCH §18.3 — absence is reported, never
    // defaulted).
    let missing = arch_layers::missing_package_crates(&map, &names);

    // ── Report ────────────────────────────────────────────────────────────────
    eprintln!(
        "boundary-gate: {} package(s) + {} shared leaves, {checked} crate(s) checked \
         (dep closure incl. dev+build edges, build.rs, include_str escapes)",
        map.packages.len(),
        map.package_leaves.len()
    );
    for pkg in &map.packages {
        let present = pkg.crates.iter().filter(|c| names.contains(*c)).count();
        eprintln!(
            "  {:<14} {present}/{} crates present   {}",
            pkg.name,
            pkg.crates.len(),
            pkg.doc
        );
    }
    let leaves_present = map
        .package_leaves
        .iter()
        .filter(|l| names.contains(&l.name))
        .count();
    eprintln!(
        "  {:<14} {leaves_present}/{} present",
        arch_layers::SHARED_LEAVES_SCOPE,
        map.package_leaves.len()
    );

    for (scope, name) in &missing {
        eprintln!("  ! [{scope}] {name}: declared but not a workspace member (yet?)");
    }
    for f in &fails {
        eprintln!("  ✗ {f}");
    }

    // The closure, once per offending package. A newly declared package can
    // print a hundred-plus edges, and repeating this on every line buries
    // them; naming it zero times leaves the reader diffing the TOML by hand.
    if !fails.is_empty() {
        let leaves: Vec<&str> = map.package_leaves.iter().map(|l| l.name.as_str()).collect();
        for pkg in &map.packages {
            if fails
                .iter()
                .any(|f| f.starts_with(&format!("[{}]", pkg.name)))
            {
                eprintln!(
                    "\n  closure for [{}]: {} + shared leaves ({})",
                    pkg.name,
                    pkg.crates.join(", "),
                    leaves.join(", ")
                );
            }
        }
    }

    if fails.is_empty() {
        eprintln!("  ✓ every declared package reaches only itself + the shared leaves");
        0
    } else {
        eprintln!();
        eprintln!(
            "boundary-gate FAILED ({} violation(s)). A package must stay liftable against \
             only the shared leaves. Either move the code that needs the offending \
             dependency outside the package and inject it through a trait at the call \
             site, or — if the boundary is real but not yet clean — grandfather the edge \
             with an [[exception]] carrying `package = \"<name>\"` and a reason in \
             quality/ARCH_LAYERS.toml. Widening a [[package_leaf]] budget widens EVERY \
             package at once; do that deliberately, with the leaf's comment updated.",
            fails.len()
        );
        1
    }
}

/// Every crate the package rules govern, as `(scope, crate)` — package members
/// under their package's name, shared leaves under the pseudo-scope.
fn governed_crates(map: &arch_layers::LayerMap) -> Vec<(&str, &str)> {
    let mut out: Vec<(&str, &str)> = Vec::new();
    for pkg in &map.packages {
        for c in &pkg.crates {
            out.push((pkg.name.as_str(), c.as_str()));
        }
    }
    for leaf in &map.package_leaves {
        out.push((arch_layers::SHARED_LEAVES_SCOPE, leaf.name.as_str()));
    }
    out
}

/// Flag `include_str!` / `include_bytes!` literals that escape the crate root
/// (climb two+ levels) unless they target the checked-in `sovereign-recipes/`
/// tree. Grep-level (per-line), recursing the crate's `src/`.
///
/// The `sovereign-recipes/` carve-out is the one recorded exception, and it is
/// worth knowing that it is not free: when the studio closure was actually
/// lifted to a sandbox (2026-07-21) it built in 36 seconds with zero source
/// edits, but had to preserve the monorepo's directory shape to compile —
/// because of exactly this embed. A green gate is not a proven lift.
fn include_escapes(dir: &Path, crate_name: &str, scope: &str, fails: &mut Vec<String>) {
    fn walk(dir: &Path, crate_name: &str, scope: &str, fails: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, crate_name, scope, fails);
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for line in text.lines() {
                    let t = line.trim();
                    if !(t.contains("include_str!") || t.contains("include_bytes!")) {
                        continue;
                    }
                    // Escapes the crate root iff it climbs two+ levels.
                    if t.contains("../..") && !t.contains("sovereign-recipes") {
                        fails.push(format!(
                            "[{scope}] {crate_name}: {} embeds a file outside the crate root \
                             and outside sovereign-recipes/: `{t}`",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        ));
                    }
                }
            }
        }
    }
    walk(&dir.join("src"), crate_name, scope, fails);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live map must parse and must actually declare packages. Without
    /// this the whole gate can go quietly vacuous — an empty `packages` list
    /// checks nothing and prints success, which is the one failure mode the
    /// v3 schema bump exists to prevent.
    #[test]
    fn live_map_declares_packages_and_leaves() {
        let root = common::repo_root();
        let text = std::fs::read_to_string(root.join("quality/ARCH_LAYERS.toml"))
            .expect("the layer map must be readable");
        let map = arch_layers::parse(&text).expect("the layer map must parse");

        assert!(
            map.packages.len() >= 2,
            "expected at least the studio and code-intel packages, got {}",
            map.packages.len()
        );
        // The leaf budget is the tip of the DAG — losing it would silently
        // widen every package's contract surface to the whole workspace.
        assert!(
            !map.package_leaves.is_empty(),
            "no [[package_leaf]] declared"
        );

        // Every governed crate names a doc a reader can open.
        for pkg in &map.packages {
            assert!(
                root.join(&pkg.doc).exists(),
                "package `{}` names doc `{}`, which does not exist",
                pkg.name,
                pkg.doc
            );
        }
    }

    /// The budgets that are empty BY CONTRACT — each one's whole basis for
    /// crossing a package boundary is a provably empty closure, so a single
    /// entry here means the promise is broken and the failing test is the
    /// point.
    #[test]
    fn leaf_budgets_stay_pinned() {
        let root = common::repo_root();
        let text = std::fs::read_to_string(root.join("quality/ARCH_LAYERS.toml")).unwrap();
        let map = arch_layers::parse(&text).unwrap();
        let budget = |name: &str| -> Vec<String> {
            map.package_leaves
                .iter()
                .find(|l| l.name == name)
                .unwrap_or_else(|| panic!("leaf `{name}` is no longer declared"))
                .allow
                .clone()
        };

        assert!(budget("oicp-types").is_empty());
        assert!(budget("kernel-types").is_empty());
        assert!(budget("corpus-engine-sections").is_empty());
        assert!(budget("sovereign-time").is_empty());
        assert_eq!(
            budget("sovereign-contracts"),
            ["oicp-types", "kernel-types"]
        );
        assert_eq!(budget("oicp-client"), ["sovereign-contracts", "oicp-types"]);
    }

    /// A breach is caught, and a build- or dev-edge breach counts. The layer
    /// map ignores dev edges — a dev-dep cannot reach a shipped artifact — but
    /// a third party who lifts a package carries its tests, so this gate does
    /// not get to ignore them.
    #[test]
    fn package_budget_flags_a_breach_on_every_edge_kind() {
        use arch_layers::{DepEdge, DepKind};
        let map = arch_layers::parse(
            r#"
schema_version = 3
backstage = ["xtask"]
[[layer]]
name = "all"
crates = ["*"]
[[package_leaf]]
name = "oicp-types"
allow = []
[[package]]
name = "demo"
doc = "studio/BOUNDARY.md"
crates = ["pkg-a", "pkg-b"]
"#,
        )
        .unwrap();

        let edge = |to: &str, kind| DepEdge {
            from: "pkg-a".to_string(),
            to: to.to_string(),
            kind,
            optional: false,
        };
        // Inside the closure: the sibling crate and the shared leaf.
        assert!(arch_layers::evaluate_packages(
            &map,
            &[
                edge("pkg-b", DepKind::Normal),
                edge("oicp-types", DepKind::Normal)
            ]
        )
        .is_empty());

        // Outside it — on all three edge kinds.
        for kind in [DepKind::Normal, DepKind::Build, DepKind::Dev] {
            let v = arch_layers::evaluate_packages(&map, &[edge("sovereign-core", kind)]);
            assert_eq!(v.len(), 1, "{kind:?} edge should breach the closure");
            assert!(v[0].describe().contains("leaves the package closure"));
        }

        // A crate in no package is not this gate's business.
        let outside = DepEdge {
            from: "sovereign-cli".to_string(),
            to: "sovereign-core".to_string(),
            kind: DepKind::Normal,
            optional: false,
        };
        assert!(arch_layers::evaluate_packages(&map, &[outside]).is_empty());
    }
}
