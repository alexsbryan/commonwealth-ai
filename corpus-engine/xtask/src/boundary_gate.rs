// SPDX-License-Identifier: AGPL-3.0-or-later
//! boundary-gate — enforces the studio-package extraction boundary
//! (studio/BOUNDARY.md): the package crates may reach only for each other +
//! the shared contract leaves, and the leaves have a hand-pinned budget of
//! their own. Kept green, the package stays liftable out of the monorepo
//! against just the OICP contract.
//!
//! Overlap with layer-gate is deliberate and partial: layer-gate governs
//! DIRECTION for the whole workspace; this gate pins an exact allowlist for
//! five crates plus rules direction can't express (no build.rs, no
//! crate-escaping include_str!).

use std::collections::BTreeSet;
use std::path::Path;

use crate::common;
use crate::manifests;

/// Crates that make up the extractable studio package.
pub const PACKAGE_SET: &[&str] = &[
    "sovereign-tools-base",
    "sovereign-workflow",
    "sovereign-workflow-host",
    "sovereign-recipe-author",
    "sovereign-studio",
];

/// The shared contract leaves the package depends on (each with its own tight
/// budget, pinned in `allowed_leaf_deps`).
pub const SHARED_LEAVES: &[&str] = &[
    "sovereign-contracts",
    "oicp-client",
    "oicp-types",
    "kernel-types",
    // The section detectors. Admitted 2026-08-20 (noun-convergence rung 2),
    // when `sovereign-tools-base` amended its own budget comment to permit it
    // and nothing taught this gate — so the edge stood as a violation for a
    // week under a gate that had no caller. It qualifies on the same test as
    // the others: a leaf whose ENTIRE budget is `regex` + `tracing`, taking it
    // does not drag LanceDB/Tantivy/rusqlite (which is what the "no
    // corpus-engine" rule was written against), and it is what lets both the
    // studio `SectionTool` and corpus-engine's own chunker reach ONE
    // implementation downward instead of parking it in the layer above.
    "corpus-engine-sections",
    // The wall clock. Admitted 2026-09-01, when clock-gate started routing
    // every non-core sovereign crate at `sovereign_time::` and
    // `sovereign-recipe-author` followed that instruction into a boundary
    // violation — two gates pointing opposite ways, which is a defect in the
    // allowlist and not in the crate that obeyed. It qualifies more strongly
    // than any other leaf here: `[dependencies]` is EMPTY — not "no in-repo
    // deps", no deps at all — so admitting it widens the package's closure by
    // exactly three functions and zero crates.
    "sovereign-time",
];

/// The internal (in-repo) deps each SHARED_LEAF is allowed. `None` for a package
/// crate — those get the union `PACKAGE_SET ∪ SHARED_LEAVES`, computed in the gate.
fn allowed_leaf_deps(crate_name: &str) -> Option<&'static [&'static str]> {
    match crate_name {
        "oicp-types" => Some(&[]),
        // The neutral kernel — identity + provenance, layer 0 beside
        // oicp-types. ZERO internal deps, and that is the contract, not an
        // accident: a kernel that may name a product crate is not a kernel
        // (noun-convergence rung nc-1-kernel).
        "kernel-types" => Some(&[]),
        // `regex` + `tracing` and nothing in-repo. Empty BY CONTRACT: the
        // whole reason this leaf may cross the boundary is that its closure is
        // provably tiny, and a single internal dep would make that untrue.
        "corpus-engine-sections" => Some(&[]),
        // Zero dependencies at all — see the SHARED_LEAVES note. Empty BY
        // CONTRACT for the same reason as `corpus-engine-sections`: the whole
        // basis for crossing the boundary is a provably empty closure.
        "sovereign-time" => Some(&[]),
        "sovereign-contracts" => Some(&["oicp-types", "kernel-types"]),
        "oicp-client" => Some(&["sovereign-contracts", "oicp-types"]),
        _ => None,
    }
}

pub fn run() -> i32 {
    let root = common::repo_root();
    let internal = manifests::workspace_internal_crates(&root);
    let pkg_union: BTreeSet<&str> = PACKAGE_SET.iter().chain(SHARED_LEAVES).copied().collect();

    let mut fails: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for &name in PACKAGE_SET.iter().chain(SHARED_LEAVES) {
        let Some(rel) = internal.get(name) else {
            // Not present yet (e.g. `sovereign-studio` arrives in B:P9e). A
            // missing member is not a violation — skip it.
            continue;
        };
        checked += 1;
        let dir = root.join(rel);

        // Rules 1/2 — dependency edges.
        let allowed: BTreeSet<&str> = match allowed_leaf_deps(name) {
            Some(list) => list.iter().copied().collect(),
            None => pkg_union.clone(),
        };
        for dep in manifests::cargo_internal_deps(&dir.join("Cargo.toml"), &internal) {
            if !allowed.contains(dep.as_str()) {
                let mut ok: Vec<&str> = allowed.iter().copied().collect();
                ok.sort_unstable();
                fails.push(format!(
                    "{name} → {dep}: dependency crosses the studio boundary \
                     (allowed for {name}: {})",
                    ok.join(", ")
                ));
            }
        }

        // Rule 3a — no build.rs (a build script is a source-tree reach-in no
        // package boundary survives; see B:P0's syn-walk removal).
        if dir.join("build.rs").exists() {
            fails.push(format!(
                "{name}: has a build.rs — package/leaf crates must not carry one"
            ));
        }

        // Rule 3b — include_str!/include_bytes! may not escape the crate root
        // except into the checked-in `sovereign-recipes/` tree (the one shared
        // data source the contract crate vendors).
        include_escapes(&dir, name, &mut fails);
    }

    eprintln!(
        "boundary-gate: checked {checked}/{} studio-boundary crates \
         (dep edges, build.rs, include_str escapes)",
        PACKAGE_SET.len() + SHARED_LEAVES.len()
    );
    for f in &fails {
        eprintln!("  ✗ {f}");
    }
    if fails.is_empty() {
        eprintln!("  ✓ the studio package reaches only for itself + the shared leaves");
        0
    } else {
        eprintln!();
        eprintln!(
            "boundary-gate FAILED ({} violations). The studio package must stay \
             liftable against only the OICP contract — see studio/BOUNDARY.md.",
            fails.len()
        );
        1
    }
}

/// Flag `include_str!` / `include_bytes!` literals that escape the crate root
/// (climb two+ levels) unless they target the checked-in `sovereign-recipes/`
/// tree. Grep-level (per-line), recursing the crate's `src/`.
fn include_escapes(dir: &Path, crate_name: &str, fails: &mut Vec<String>) {
    fn walk(dir: &Path, crate_name: &str, fails: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, crate_name, fails);
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
                            "{crate_name}: {} embeds a file outside the crate root \
                             and outside sovereign-recipes/: `{t}`",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        ));
                    }
                }
            }
        }
    }
    walk(&dir.join("src"), crate_name, fails);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn leaf_budgets_are_pinned() {
        assert_eq!(allowed_leaf_deps("oicp-types"), Some(&[][..]));
        // The kernel's budget is empty BY CONTRACT — if this ever gains an
        // entry, the layer-0 promise has been broken and the failing test is
        // the point.
        assert_eq!(allowed_leaf_deps("kernel-types"), Some(&[][..]));
        // Same contract, same reason: the section leaf is admitted across the
        // boundary only because its closure is `regex` + `tracing`.
        assert_eq!(allowed_leaf_deps("corpus-engine-sections"), Some(&[][..]));
        assert_eq!(allowed_leaf_deps("sovereign-time"), Some(&[][..]));
        assert_eq!(
            allowed_leaf_deps("sovereign-contracts"),
            Some(&["oicp-types", "kernel-types"][..])
        );
        assert_eq!(
            allowed_leaf_deps("oicp-client"),
            Some(&["sovereign-contracts", "oicp-types"][..])
        );
        // A package crate has no fixed leaf budget — it gets the union at runtime.
        assert_eq!(allowed_leaf_deps("sovereign-workflow-host"), None);
    }

    #[test]
    fn package_budget_flags_a_breach() {
        let internal: HashMap<String, String> = [
            ("sovereign-contracts", "x"),
            ("sovereign-core", "y"),
            ("oicp-types", "z"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let cargo = "\
[package]\n\
name = \"sovereign-tools-base\"\n\
[dependencies]\n\
sovereign-contracts = { workspace = true }\n\
[build-dependencies]\n\
sovereign-core = { workspace = true }\n";
        let deps = manifests::parse_cargo_internal_deps(cargo, &internal);

        // The package budget forbids sovereign-core (a build-dep breach counts).
        let pkg_union: BTreeSet<&str> = PACKAGE_SET.iter().chain(SHARED_LEAVES).copied().collect();
        let breaches: Vec<&String> = deps
            .iter()
            .filter(|d| !pkg_union.contains(d.as_str()))
            .collect();
        assert_eq!(breaches, vec![&"sovereign-core".to_string()]);
    }
}
